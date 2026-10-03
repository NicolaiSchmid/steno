//! From merged tokens to what Steno's Swift engine returns: FluidAudio's
//! `createTokenTimings` and `convertTokensToText`
//! (`AsrManager+TokenProcessing.swift`, `AsrManager.swift`), then
//! `StenoSpeech`'s `TokenAggregator` (pieces to words) and
//! `TranscriptSegmenter` (words to `RawSegment`s). The parity harness
//! compares at this level because the Swift baseline files are the
//! segments `ParakeetEngine.transcribe` returned.
//!
//! The aggregator and the segmenter belong to the shared speech crate
//! once WP4a lands; they live here until then so the harness has them.

use steno_core::{RawSegment, TimedWord, WordTiming};

use crate::Token;
use crate::chunking::{FRAME_SECONDS, frame_seconds};
use crate::vocab::{Vocab, WORD_BOUNDARY, is_punctuation, is_symbol};

/// One token with its time, as FluidAudio's `TokenTiming`.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenTiming {
    /// The piece with the SentencePiece marker replaced by a space.
    pub text: String,
    /// SentencePiece id.
    pub id: usize,
    /// Seconds, one frame before emission.
    pub start: f64,
    /// Seconds, from the token's duration.
    pub end: f64,
    /// The joint's probability.
    pub confidence: f32,
}

/// Swift's `CharacterSet.whitespaces`, which `TokenAggregator` trims:
/// Unicode `Zs` plus tab. No v3 piece carries anything but a space, so
/// the set matters for the rule, not for today's output.
fn is_swift_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// TDT emits about one encoder frame after the acoustic event; FluidAudio
/// shifts every token one frame earlier (`TDT_EMISSION_DELAY_FRAMES`).
const EMISSION_DELAY_FRAMES: usize = 1;

/// The transcript text: pieces joined, marker to space, trimmed
/// (`AsrManager.convertTokensToText`). Unknown ids contribute nothing.
#[must_use]
pub fn transcript_text(tokens: &[Token], vocab: &Vocab) -> String {
    let joined: String = tokens.iter().map(|token| vocab.piece(token.id)).collect();
    joined.replace(WORD_BOUNDARY, " ").trim().to_owned()
}

/// Token timings as `AsrManager.createTokenTimings` builds them: stable
/// sort by frame, one-frame emission delay, the end from the token's
/// duration when it has one, else the next token's start, at least one
/// frame and never before the start.
#[must_use]
pub fn token_timings(tokens: &[Token], vocab: &Vocab) -> Vec<TokenTiming> {
    let mut sorted: Vec<Token> = tokens.to_vec();
    sorted.sort_by_key(|token| token.frame);
    let start_of = |token: &Token| frame_seconds(token.frame.saturating_sub(EMISSION_DELAY_FRAMES));
    sorted
        .iter()
        .enumerate()
        .map(|(i, token)| {
            let start = start_of(token);
            let end = if token.duration > 0 {
                start + frame_seconds(token.duration).max(FRAME_SECONDS)
            } else if let Some(next) = sorted.get(i + 1) {
                start_of(next).max(start + FRAME_SECONDS)
            } else {
                start + FRAME_SECONDS
            };
            let text = if vocab.contains(token.id) {
                vocab.piece(token.id).replace(WORD_BOUNDARY, " ")
            } else {
                format!("token_{}", token.id)
            };
            TokenTiming {
                text,
                id: token.id,
                start,
                end: end.max(start + 0.001),
                confidence: token.confidence,
            }
        })
        .collect()
}

/// Pieces to words (`StenoSpeech.TokenAggregator.words(from:)`): a piece
/// starting with a space or the marker begins a word; punctuation-only
/// pieces glue to the word before them; a bare boundary carries to the
/// next piece. A word's confidence is the mean over its pieces.
#[must_use]
pub fn words(timings: &[TokenTiming]) -> Vec<TimedWord> {
    fn flush(
        current: &mut Option<TimedWord>,
        confidences: &mut Vec<f32>,
        out: &mut Vec<TimedWord>,
    ) {
        if let Some(mut word) = current.take()
            && !word.text.is_empty()
        {
            word.confidence = if confidences.is_empty() {
                1.0
            } else {
                // A word has a handful of pieces; the count is exact.
                #[allow(clippy::cast_precision_loss)]
                let count = confidences.len() as f32;
                confidences.iter().sum::<f32>() / count
            };
            out.push(word);
        }
        confidences.clear();
    }

    let mut out: Vec<TimedWord> = Vec::new();
    let mut current: Option<TimedWord> = None;
    let mut confidences: Vec<f32> = Vec::new();
    let mut boundary_pending = true;

    for timing in timings {
        let mut text = timing.text.as_str();
        let starts_word = text.starts_with(' ') || text.starts_with(WORD_BOUNDARY);
        if starts_word {
            let mut chars = text.chars();
            chars.next();
            text = chars.as_str();
        }
        let text = text.trim_matches(is_swift_whitespace);
        if text.is_empty() {
            if starts_word {
                boundary_pending = true;
            }
            continue;
        }
        // Punctuation glues to the word before it even across a boundary;
        // anything else glues only when nothing says a new word starts.
        let punctuation_only = text.chars().all(|c| is_punctuation(c) || is_symbol(c));
        let glues = punctuation_only || !(starts_word || boundary_pending);
        match current.as_mut() {
            Some(word) if glues => {
                word.text.push_str(text);
                word.end = word.end.max(timing.end);
            }
            _ => {
                flush(&mut current, &mut confidences, &mut out);
                current = Some(TimedWord {
                    text: text.to_owned(),
                    start: timing.start,
                    end: timing.end,
                    confidence: 1.0,
                });
            }
        }
        confidences.push(timing.confidence);
        boundary_pending = false;
    }
    flush(&mut current, &mut confidences, &mut out);
    out
}

/// A new segment starts after a pause longer than this
/// (`TranscriptSegmenter.splitGapSeconds`).
pub const SPLIT_GAP_SECONDS: f64 = 0.7;
/// No segment runs longer than this (`TranscriptSegmenter.maxSegmentSeconds`).
pub const MAX_SEGMENT_SECONDS: f64 = 30.0;

/// Words to segments (`StenoSpeech.TranscriptSegmenter.segments(from:)`):
/// split after a pause over [`SPLIT_GAP_SECONDS`], after sentence-final
/// punctuation, and before a word that would push the segment past
/// [`MAX_SEGMENT_SECONDS`]. `language` stays `None`; the tagger fills it.
#[must_use]
pub fn segments(words: &[TimedWord]) -> Vec<RawSegment> {
    fn flush(current: &mut Vec<&TimedWord>, out: &mut Vec<RawSegment>) {
        if let (Some(first), Some(last)) = (current.first(), current.last()) {
            out.push(RawSegment {
                start: first.start,
                end: last.end.max(first.start),
                text: current
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                language: None,
                word_timings: Some(
                    current
                        .iter()
                        .map(|word| WordTiming {
                            word: word.text.clone(),
                            start: word.start,
                            end: word.end,
                        })
                        .collect(),
                ),
            });
        }
        current.clear();
    }

    let mut out: Vec<RawSegment> = Vec::new();
    let mut current: Vec<&TimedWord> = Vec::new();

    for word in words.iter().filter(|word| !word.text.is_empty()) {
        if let (Some(previous), Some(first)) = (current.last(), current.first()) {
            let gap = word.start - previous.end;
            let ends_sentence = previous
                .text
                .chars()
                .last()
                .is_some_and(|c| ".?!".contains(c));
            let too_long = word.end - first.start > MAX_SEGMENT_SECONDS;
            if gap > SPLIT_GAP_SECONDS || ends_sentence || too_long {
                flush(&mut current, &mut out);
            }
        }
        current.push(word);
    }
    flush(&mut current, &mut out);
    out
}

/// Tokens to segments, the whole Swift chain; with text but no words
/// (nothing aggregated) one segment over the audio, as `ParakeetMapping`.
/// `language` stays `None`: `ParakeetMapping` runs `LanguageTagger` here,
/// which arrives with WP4a's shared crate.
#[must_use]
pub fn raw_segments(tokens: &[Token], vocab: &Vocab, duration: f64) -> Vec<RawSegment> {
    let mut result = segments(&words(&token_timings(tokens, vocab)));
    if result.is_empty() {
        let text = transcript_text(tokens, vocab);
        if !text.is_empty() {
            result.push(RawSegment {
                start: 0.0,
                end: duration.max(0.0),
                text,
                language: None,
                word_timings: None,
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocab::tests::sample;

    fn token(id: usize, frame: usize, duration: usize) -> Token {
        Token {
            id,
            frame,
            confidence: 0.8,
            duration,
        }
    }

    #[test]
    fn text_joins_pieces_and_trims() {
        let vocab = sample();
        let tokens = [token(12, 1, 1), token(13, 2, 1), token(4, 3, 1)];
        assert_eq!(transcript_text(&tokens, &vocab), "hello world.");
        assert_eq!(transcript_text(&[], &vocab), "");
        assert_eq!(transcript_text(&[token(999, 1, 1)], &vocab), "");
        // A lone boundary piece at the end becomes a trailing space, which
        // the trim removes too.
        let marker = Vocab::from_pieces(
            [(1, "\u{2581}hello".to_owned()), (2, "\u{2581}".to_owned())].into(),
        );
        assert_eq!(
            transcript_text(&[token(1, 1, 1), token(2, 2, 1)], &marker),
            "hello"
        );
    }

    #[test]
    fn timings_shift_one_frame_and_use_the_duration() {
        let vocab = sample();
        let tokens = [token(12, 10, 2), token(13, 13, 0), token(4, 14, 0)];
        let timings = token_timings(&tokens, &vocab);
        assert_eq!(timings[0].text, " hello");
        assert!((timings[0].start - 0.72).abs() < 1e-9);
        assert!((timings[0].end - 0.88).abs() < 1e-9);
        // Zero duration: the next token's start, at least one frame.
        assert!((timings[1].start - 0.96).abs() < 1e-9);
        assert!((timings[1].end - 1.04).abs() < 1e-9);
        // Last token with zero duration: one frame.
        assert!((timings[2].end - 1.12).abs() < 1e-9);
        // Frame zero does not go negative; unknown ids get a placeholder.
        let first = token_timings(&[token(999, 0, 1)], &vocab);
        assert_eq!(first[0].start, 0.0);
        assert_eq!(first[0].text, "token_999");
    }

    fn timing(text: &str, start: f64, confidence: f32) -> TokenTiming {
        TokenTiming {
            text: text.to_owned(),
            id: 0,
            start,
            end: start + 0.08,
            confidence,
        }
    }

    #[test]
    fn aggregator_joins_pieces_into_words() {
        let timings = [
            timing(" hel", 0.0, 0.6),
            timing("lo", 0.08, 1.0),
            timing(" world", 0.2, 0.9),
            timing(".", 0.3, 0.5),
            timing(" ", 0.4, 1.0),
            timing("next", 0.5, 0.7),
            timing("\u{2581}marker", 0.6, 0.7),
        ];
        let words = words(&timings);
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, vec!["hello", "world.", "next", "marker"]);
        assert!((words[0].confidence - 0.8).abs() < 1e-6);
        assert!((words[0].end - 0.16).abs() < 1e-9);
        assert!((words[1].end - 0.38).abs() < 1e-9);
        // A leading punctuation piece with no word before it starts a word
        // and the continuation glues to it, as the Swift aggregator does.
        let only = words_of(&[timing(".", 0.0, 1.0), timing("a", 0.1, 1.0)]);
        assert_eq!(only, vec![".a"]);
        assert_eq!(super::words(&[]), Vec::new());
    }

    fn words_of(timings: &[TokenTiming]) -> Vec<String> {
        words(timings).into_iter().map(|w| w.text).collect()
    }

    fn word(text: &str, start: f64, end: f64) -> TimedWord {
        TimedWord {
            text: text.to_owned(),
            start,
            end,
            confidence: 1.0,
        }
    }

    #[test]
    fn segmenter_splits_on_gaps_sentences_and_length() {
        let words = [
            word("Hallo", 0.0, 0.4),
            word("zusammen.", 0.5, 1.0),
            word("Wie", 1.1, 1.3),
            word("geht's", 1.4, 1.8),
            word("gut", 3.0, 3.2),
            word("", 3.3, 3.4),
        ];
        let segments = segments(&words);
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["Hallo zusammen.", "Wie geht's", "gut"]);
        assert_eq!(segments[0].word_timings.as_ref().unwrap().len(), 2);
        assert_eq!(segments[1].start, 1.1);
        assert_eq!(segments[1].end, 1.8);
        let long: Vec<TimedWord> = (0..40)
            .map(|i| word("w", f64::from(i), f64::from(i) + 0.5))
            .collect();
        let segments = super::segments(&long);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].word_timings.as_ref().unwrap().len(), 30);
    }

    #[test]
    fn raw_segments_run_the_whole_chain() {
        let vocab = sample();
        let tokens = [
            token(12, 10, 1),
            token(13, 12, 1),
            token(4, 13, 1),
            token(14, 40, 1),
        ];
        let segments = raw_segments(&tokens, &vocab, 10.0);
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["hello world.", "a"]);
        assert_eq!(raw_segments(&[], &vocab, 10.0), Vec::new());
    }
}
