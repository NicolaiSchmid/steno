//! From decoder tokens to [`RawSegment`]s: token timings as `FluidAudio`
//! reports them, `SentencePiece` pieces joined into words, words cut into
//! segments at pauses, sentence ends and a length cap.
//! Swift: `Sources/StenoSpeech/Segmentation/TokenAggregator.swift`,
//! `Sources/StenoSpeech/Segmentation/TranscriptSegmenter.swift` and
//! `FluidAudio`'s `createTokenTimings`.

use steno_core::{RawSegment, TimedWord};

use crate::backend::FRAME_SECONDS;
use crate::decoder::Token;
use crate::vocab::{Vocab, starts_word, strip_boundary};

/// One timed piece per token: start one frame before the emission frame
/// (TDT emits after the frame it describes), end after the token's
/// duration, at least one frame.
#[must_use]
pub fn timed_pieces(tokens: &[Token], vocab: &Vocab) -> Vec<TimedWord> {
    tokens
        .iter()
        .map(|token| {
            let start = token.frame.saturating_sub(1) as f64 * FRAME_SECONDS;
            let length = token.duration.max(1) as f64 * FRAME_SECONDS;
            TimedWord {
                text: vocab.piece(token.id).to_owned(),
                start,
                end: start + length,
                confidence: token.confidence,
            }
        })
        .collect()
}

/// Joins pieces into words. A piece starting with a space or the marker
/// begins a word; punctuation-only pieces glue to the word before them;
/// pieces that are only a boundary carry it to the next piece. A word's
/// confidence is the mean of its pieces'.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenAggregator;

impl TokenAggregator {
    #[must_use]
    pub fn words(&self, pieces: &[TimedWord]) -> Vec<TimedWord> {
        let mut words: Vec<TimedWord> = Vec::new();
        let mut current: Option<TimedWord> = None;
        let mut confidences: Vec<f32> = Vec::new();
        let mut boundary_pending = true;

        let flush = |current: &mut Option<TimedWord>,
                     confidences: &mut Vec<f32>,
                     words: &mut Vec<TimedWord>| {
            if let Some(mut word) = current.take()
                && !word.text.is_empty()
            {
                word.confidence = if confidences.is_empty() {
                    1.0
                } else {
                    confidences.iter().sum::<f32>() / confidences.len() as f32
                };
                words.push(word);
            }
            confidences.clear();
        };

        for piece in pieces {
            let is_word_start = starts_word(&piece.text);
            let text = strip_boundary(&piece.text).trim();
            if text.is_empty() {
                if is_word_start {
                    boundary_pending = true;
                }
                continue;
            }
            let punctuation_only = text.chars().all(is_punctuation_or_symbol);
            match current.as_mut() {
                Some(word) if punctuation_only || !(is_word_start || boundary_pending) => {
                    word.text.push_str(text);
                    word.end = word.end.max(piece.end);
                }
                _ => {
                    flush(&mut current, &mut confidences, &mut words);
                    current = Some(TimedWord {
                        text: text.to_owned(),
                        start: piece.start,
                        end: piece.end,
                        confidence: piece.confidence,
                    });
                }
            }
            confidences.push(piece.confidence);
            boundary_pending = false;
        }
        flush(&mut current, &mut confidences, &mut words);
        words
    }
}

/// Swift's `punctuationCharacters` and `symbols` sets, near enough: not a
/// letter, digit, space or control character.
fn is_punctuation_or_symbol(c: char) -> bool {
    c.is_ascii_punctuation() || !(c.is_alphanumeric() || c.is_whitespace() || c.is_control())
}

/// Cuts words into segments: a new one after a gap over `split_gap_seconds`,
/// after sentence-final punctuation, and when the next word would push the
/// segment past `max_segment_seconds`. `language` stays `None`; the tagger
/// fills it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TranscriptSegmenter {
    pub max_segment_seconds: f64,
    pub split_gap_seconds: f64,
}

/// Swift's defaults (30 s, 0.7 s).
impl Default for TranscriptSegmenter {
    fn default() -> Self {
        TranscriptSegmenter {
            max_segment_seconds: 30.0,
            split_gap_seconds: 0.7,
        }
    }
}

impl TranscriptSegmenter {
    #[must_use]
    pub fn segments(&self, words: &[TimedWord]) -> Vec<RawSegment> {
        let mut segments = Vec::new();
        let mut current: Vec<&TimedWord> = Vec::new();
        let flush = |current: &mut Vec<&TimedWord>, segments: &mut Vec<RawSegment>| {
            if let (Some(first), Some(last)) = (current.first(), current.last()) {
                segments.push(RawSegment {
                    start: first.start,
                    end: last.end.max(first.start),
                    text: current
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    language: None,
                    word_timings: Some(current.iter().map(|w| w.word_timing()).collect()),
                });
            }
            current.clear();
        };
        for word in words.iter().filter(|w| !w.text.is_empty()) {
            if let (Some(previous), Some(first)) = (current.last(), current.first()) {
                let gap = word.start - previous.end;
                let ends_sentence = previous.text.ends_with(['.', '?', '!']);
                let too_long = word.end - first.start > self.max_segment_seconds;
                if gap > self.split_gap_seconds || ends_sentence || too_long {
                    flush(&mut current, &mut segments);
                }
            }
            current.push(word);
        }
        flush(&mut current, &mut segments);
        segments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(text: &str, start: f64, end: f64, confidence: f32) -> TimedWord {
        TimedWord {
            text: text.to_owned(),
            start,
            end,
            confidence,
        }
    }

    fn texts(words: &[TimedWord]) -> Vec<&str> {
        words.iter().map(|w| w.text.as_str()).collect()
    }

    #[test]
    fn three_pieces_join_into_one_word_with_the_mean_confidence() {
        let words = TokenAggregator.words(&[
            piece("▁Produkt", 0.0, 0.1, 0.9),
            piece("strat", 0.1, 0.25, 0.7),
            piece("egie", 0.25, 0.4, 0.8),
        ]);
        assert_eq!(words.len(), 1);
        assert_eq!(
            (words[0].text.as_str(), words[0].start, words[0].end),
            ("Produktstrategie", 0.0, 0.4)
        );
        assert!((words[0].confidence - 0.8).abs() < 1e-6);
    }

    #[test]
    fn space_prefixed_pieces_start_words() {
        let words = TokenAggregator.words(&[
            piece(" Wir", 0.0, 0.1, 1.0),
            piece(" müssen", 0.3, 0.5, 1.0),
            piece(" das", 0.6, 0.7, 1.0),
            piece(" Onboarding", 0.8, 1.0, 1.0),
            piece(".", 1.1, 1.15, 1.0),
            piece(" Dann", 1.3, 1.5, 1.0),
        ]);
        assert_eq!(
            texts(&words),
            ["Wir", "müssen", "das", "Onboarding.", "Dann"]
        );
        assert_eq!(
            words.iter().map(|w| w.start).collect::<Vec<_>>(),
            [0.0, 0.3, 0.6, 0.8, 1.3]
        );
        assert!(words[1].end == 0.5 && words[3].end == 1.15);
    }

    #[test]
    fn the_sentencepiece_marker_is_a_boundary_and_punctuation_glues() {
        let words = TokenAggregator.words(&[
            piece("▁Wir", 0.0, 0.1, 1.0),
            piece("▁müssen", 0.2, 0.4, 1.0),
            piece(".", 0.4, 0.45, 1.0),
        ]);
        assert_eq!(texts(&words), ["Wir", "müssen."]);
        let words = TokenAggregator.words(&[
            piece("▁Hallo", 0.0, 0.2, 1.0),
            piece(",", 0.2, 0.25, 1.0),
            piece("▁Welt", 0.3, 0.5, 1.0),
            piece("…", 0.5, 0.55, 1.0),
        ]);
        assert_eq!(texts(&words), ["Hallo,", "Welt…"]);
        assert_eq!(words.last().unwrap().end, 0.55);
        // With the marker too: the vocabulary has `▁,`.
        let words =
            TokenAggregator.words(&[piece("▁Hallo", 0.0, 0.2, 1.0), piece("▁,", 0.2, 0.25, 1.0)]);
        assert_eq!(texts(&words), ["Hallo,"]);
    }

    #[test]
    fn bare_boundaries_and_empty_tokens_only_steer_the_next_word() {
        let words = TokenAggregator.words(&[
            piece("▁", 0.0, 0.05, 1.0),
            piece("OK", 0.05, 0.2, 1.0),
            piece("▁", 0.2, 0.25, 1.0),
            piece("go", 0.25, 0.4, 1.0),
            piece(" ", 0.4, 0.45, 1.0),
            piece("on", 0.45, 0.6, 1.0),
        ]);
        assert_eq!(texts(&words), ["OK", "go", "on"]);
        assert_eq!(words[0].start, 0.05);
        let words = TokenAggregator.words(&[
            piece("", 0.0, 0.1, 1.0),
            piece("ab", 0.1, 0.2, 1.0),
            piece(" ", 0.2, 0.3, 1.0),
        ]);
        assert_eq!(texts(&words), ["ab"]);
        let words =
            TokenAggregator.words(&[piece("hi", 0.0, 0.1, 1.0), piece("▁there", 0.1, 0.2, 1.0)]);
        assert_eq!(texts(&words), ["hi", "there"]);
        assert!(TokenAggregator.words(&[]).is_empty());
    }

    fn words(specs: &[(&str, f64, f64)]) -> Vec<TimedWord> {
        specs.iter().map(|&(t, s, e)| piece(t, s, e, 1.0)).collect()
    }

    #[test]
    fn splits_on_gaps_and_sentence_ends_and_caps_length() {
        let segments = TranscriptSegmenter::default().segments(&words(&[
            ("eins", 0.0, 0.3),
            ("zwei", 0.4, 0.7),
            ("drei", 1.6, 1.9),
        ]));
        assert_eq!(
            segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            ["eins zwei", "drei"]
        );
        assert_eq!(
            segments.iter().map(|s| s.start).collect::<Vec<_>>(),
            [0.0, 1.6]
        );
        assert_eq!(
            segments.iter().map(|s| s.end).collect::<Vec<_>>(),
            [0.7, 1.9]
        );
        assert_eq!(segments[0].word_timings.as_ref().unwrap().len(), 2);
        assert!(segments.iter().all(|s| s.language.is_none()));

        let segments = TranscriptSegmenter::default().segments(&words(&[
            ("Ja.", 0.0, 0.2),
            ("Gut", 0.3, 0.5),
            ("so?", 0.5, 0.7),
            ("Fein!", 0.8, 1.0),
        ]));
        assert_eq!(
            segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            ["Ja.", "Gut so?", "Fein!"]
        );

        let long: Vec<(String, f64, f64)> = (0..40)
            .map(|i| (format!("w{i}"), f64::from(i), f64::from(i) + 0.5))
            .collect();
        let long: Vec<TimedWord> = long.iter().map(|(t, s, e)| piece(t, *s, *e, 1.0)).collect();
        let segments = TranscriptSegmenter::default().segments(&long);
        assert_eq!(segments.len(), 2);
        assert!(segments[0].end - segments[0].start <= 30.0);
        assert_eq!(segments[0].word_timings.as_ref().unwrap().len(), 30);
        assert!(segments[1].text.starts_with("w30"));
        assert!(TranscriptSegmenter::default().segments(&[]).is_empty());
        assert!(
            TranscriptSegmenter::default()
                .segments(&words(&[("", 0.0, 1.0)]))
                .is_empty()
        );
    }

    #[test]
    fn pieces_get_fluid_audio_timings() {
        let vocab = Vocab::from_pieces(["▁ja", "<blk>"].map(str::to_owned).to_vec()).unwrap();
        let pieces = timed_pieces(
            &[
                Token {
                    id: 0,
                    frame: 0,
                    confidence: 0.5,
                    duration: 0,
                },
                Token {
                    id: 0,
                    frame: 10,
                    confidence: 0.5,
                    duration: 2,
                },
            ],
            &vocab,
        );
        assert_eq!(pieces[0].text, "▁ja");
        assert_eq!(pieces[0].start, 0.0);
        assert!((pieces[0].end - 0.08).abs() < 1e-9);
        assert!((pieces[1].start - 0.72).abs() < 1e-9);
        assert!((pieces[1].end - 0.88).abs() < 1e-9);
    }
}
