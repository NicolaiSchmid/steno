//! The pipeline above the tensors, in order: VAD regions, the pause-aligned
//! layout, one decode per chunk with the empty-decode recovery, the LCS
//! merge, pieces to words to segments, and the language tag. Generic over
//! [`SpeechBackend`], so a fake backend drives it in tests and the `CoreML`
//! backend of `WP4b` slots in unchanged.
//! Swift: `Sources/StenoSpeech/Engines/ParakeetEngine.swift` and
//! `ParakeetMapping.swift`, with `FluidAudio`'s `ChunkProcessor` in between.

use std::ops::Range;

use steno_core::{LanguageTag, RawSegment, TimedWord};

use crate::backend::{FRAME_SAMPLES, SpeechBackend};
use crate::chunker::{Chunk, ChunkerConfig, SAMPLE_RATE, layout, samples};
use crate::decoder::{DecodeStats, DecoderConfig, Token, decode_window};
use crate::error::SpeechError;
use crate::language::LanguageTagger;
use crate::merge::merge_all;
use crate::segmentation::{TokenAggregator, TranscriptSegmenter, timed_pieces};
use crate::vad::VoiceActivityDetector;
use crate::vocab::{Vocab, starts_word};

/// When a chunk with speech decodes to (almost) nothing, the window is
/// decoded again with more audio around it (decision 1: extend, do not
/// shift) and the candidate with the most words wins.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryConfig {
    /// Chunks with less VAD speech than this are not retried.
    pub min_speech_seconds: f32,
    /// Fewer words per second of speech than this counts as empty.
    pub min_words_per_speech_second: f32,
    /// Seconds added before and after the chunk, tried in order.
    pub extensions_seconds: Vec<(f32, f32)>,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        RecoveryConfig {
            min_speech_seconds: 3.0,
            min_words_per_speech_second: 0.4,
            extensions_seconds: vec![(0.0, 6.0), (6.0, 6.0), (6.0, 12.0)],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PipelineConfig {
    pub chunker: ChunkerConfig,
    pub decoder: DecoderConfig,
    pub recovery: RecoveryConfig,
    pub segmenter: TranscriptSegmenter,
}

/// Everything one transcription produced, from the segments the engine
/// returns down to the chunk layout for diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub segments: Vec<RawSegment>,
    pub words: Vec<TimedWord>,
    pub tokens: Vec<Token>,
    pub chunks: Vec<Chunk>,
    pub speech: Vec<Range<usize>>,
    pub stats: DecodeStats,
}

impl Transcript {
    /// The words joined by spaces.
    #[must_use]
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// One backend, its vocabulary, a detector and the configuration; owns
/// everything it needs, so it can move to a worker thread.
pub struct Transcriber<B: SpeechBackend> {
    backend: B,
    vocab: Vocab,
    vad: Box<dyn VoiceActivityDetector>,
    tagger: LanguageTagger,
    config: PipelineConfig,
}

impl<B: SpeechBackend> Transcriber<B> {
    #[must_use]
    pub fn new(
        backend: B,
        vocab: Vocab,
        vad: Box<dyn VoiceActivityDetector>,
        tagger: LanguageTagger,
        config: PipelineConfig,
    ) -> Self {
        Transcriber {
            backend,
            vocab,
            vad,
            tagger,
            config,
        }
    }

    #[must_use]
    pub fn backend(&self) -> &B {
        &self.backend
    }

    #[must_use]
    pub fn vocab(&self) -> &Vocab {
        &self.vocab
    }

    #[must_use]
    pub fn config(&self) -> &PipelineConfig {
        &self.config
    }

    /// Transcribes 16 kHz mono samples. `hint` steers the language tagger
    /// only; the model runs unpinned.
    pub fn transcribe(
        &mut self,
        samples: &[f32],
        hint: Option<&LanguageTag>,
    ) -> Result<Transcript, SpeechError> {
        let speech = self.vad.speech_regions(samples)?;
        let chunks = layout(samples, &speech, &self.config.chunker);
        let mut stats = DecodeStats::default();
        let mut windows = Vec::with_capacity(chunks.len());
        for chunk in &chunks {
            let mut tokens = self.decode_range(samples, chunk.range.clone(), &mut stats)?;
            if self.looks_empty(&tokens, &speech, &chunk.range) {
                tokens = self.recover(samples, &chunk.range, tokens, &mut stats)?;
            }
            windows.push(tokens);
        }
        let tokens = merge_all(
            &windows,
            f64::from(self.config.chunker.overlap_seconds),
            &self.vocab,
        );
        let words = TokenAggregator.words(&timed_pieces(&tokens, &self.vocab));
        let segments = self
            .tagger
            .tag(self.config.segmenter.segments(&words), hint);
        Ok(Transcript {
            segments,
            words,
            tokens,
            chunks,
            speech,
            stats,
        })
    }

    /// Decodes one window of the recording; token frames are absolute.
    pub fn decode_range(
        &mut self,
        samples: &[f32],
        range: Range<usize>,
        stats: &mut DecodeStats,
    ) -> Result<Vec<Token>, SpeechError> {
        let range = range.start.min(samples.len())..range.end.min(samples.len());
        let features = self.backend.features(&samples[range.clone()])?;
        let encoded = self.backend.encode(&features)?;
        decode_window(
            &mut self.backend,
            &encoded,
            range.start / FRAME_SAMPLES,
            &self.config.decoder,
            stats,
        )
    }

    /// The text of `tokens`, for probes and tests.
    #[must_use]
    pub fn render(&self, tokens: &[Token]) -> String {
        TokenAggregator
            .words(&timed_pieces(tokens, &self.vocab))
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn word_count(&self, tokens: &[Token]) -> usize {
        tokens
            .iter()
            .filter(|t| starts_word(self.vocab.piece(t.id)))
            .count()
    }

    fn looks_empty(&self, tokens: &[Token], speech: &[Range<usize>], range: &Range<usize>) -> bool {
        let speech_seconds = speech_inside(speech, range);
        // Word counts are small.
        #[allow(clippy::cast_precision_loss)]
        let words = self.word_count(tokens) as f32;
        speech_seconds >= self.config.recovery.min_speech_seconds
            && words < self.config.recovery.min_words_per_speech_second * speech_seconds
    }

    fn recover(
        &mut self,
        samples: &[f32],
        range: &Range<usize>,
        original: Vec<Token>,
        stats: &mut DecodeStats,
    ) -> Result<Vec<Token>, SpeechError> {
        let max = samples_of(self.config.chunker.max_seconds);
        let mut best = original;
        let mut best_words = self.word_count(&best);
        let mut accepted = false;
        let extensions = self.config.recovery.extensions_seconds.clone();
        for (before, after) in extensions {
            let start = range.start.saturating_sub(samples_of(before));
            let end = (range.end + samples_of(after)).min(samples.len());
            if end - start > max || (start == range.start && end == range.end) {
                continue;
            }
            stats.recoveries_tried += 1;
            let candidate = self.decode_range(samples, start..end, stats)?;
            let words = self.word_count(&candidate);
            if words > best_words {
                best = candidate;
                best_words = words;
                accepted = true;
            }
        }
        if accepted {
            stats.recoveries_accepted += 1;
        }
        Ok(best)
    }
}

fn samples_of(seconds: f32) -> usize {
    samples(seconds)
}

/// Seconds of VAD speech inside `range`.
#[must_use]
pub fn speech_inside(speech: &[Range<usize>], range: &Range<usize>) -> f32 {
    let count: usize = speech
        .iter()
        .map(|region| {
            region
                .end
                .min(range.end)
                .saturating_sub(region.start.max(range.start))
        })
        .sum();
    // Sample counts fit f32's integer range well enough for seconds.
    #[allow(clippy::cast_precision_loss)]
    let seconds = count as f32 / SAMPLE_RATE as f32;
    seconds
}

#[cfg(test)]
pub(crate) mod fake {
    //! A backend that reads the "token per frame" the test audio encodes:
    //! each 80 ms frame of audio holds a constant sample value `k / 1000`,
    //! which the fake decodes as piece `k` (0 for silence); a chunk's frames
    //! are not aligned to the recording's, so a frame takes the piece most
    //! of its samples carry. The joint emits the frame's piece once
    //! (duration 0) and then blank, so the decoder loop, the chunker and the
    //! merge are exercised end to end with a known answer.

    use super::*;
    use crate::backend::{
        DecoderState, DecoderStep, EncoderOutput, Features, JointDecision, ModelShape,
    };

    pub struct FrameTokenBackend {
        pub shape: ModelShape,
    }

    impl FrameTokenBackend {
        pub fn new(vocab: &Vocab) -> Self {
            FrameTokenBackend {
                shape: ModelShape {
                    vocab_size: vocab.len(),
                    blank_id: vocab.blank_id(),
                    durations: vec![0, 1, 2, 3, 4],
                    decoder_layers: 1,
                    decoder_hidden: 1,
                    encoder_hidden: 1,
                },
            }
        }
    }

    impl SpeechBackend for FrameTokenBackend {
        fn shape(&self) -> &ModelShape {
            &self.shape
        }
        fn features(&mut self, samples: &[f32]) -> Result<Features, SpeechError> {
            let frames = samples.len() / FRAME_SAMPLES;
            let data = (0..frames)
                .map(|f| {
                    let frame = &samples[f * FRAME_SAMPLES..(f + 1) * FRAME_SAMPLES];
                    let mut counts = std::collections::BTreeMap::new();
                    for x in frame {
                        *counts.entry((x * 1000.0).round() as i64).or_insert(0usize) += 1;
                    }
                    counts
                        .into_iter()
                        .max_by_key(|&(_, n)| n)
                        .map_or(0.0, |(k, _)| k as f32)
                })
                .collect();
            Ok(Features {
                mels: 1,
                frames,
                data,
            })
        }
        fn encode(&mut self, features: &Features) -> Result<EncoderOutput, SpeechError> {
            Ok(EncoderOutput {
                hidden: 1,
                len: features.frames,
                data: features.data.clone(),
            })
        }
        fn decoder_step(
            &mut self,
            token: u32,
            _state: &DecoderState,
        ) -> Result<DecoderStep, SpeechError> {
            Ok(DecoderStep {
                projection: vec![token as f32],
                state: DecoderState::zeros(1, 1),
            })
        }
        fn joint_step(
            &mut self,
            encoder_frame: &[f32],
            projection: &[f32],
        ) -> Result<JointDecision, SpeechError> {
            let piece = encoder_frame[0] as u32;
            let blank = JointDecision {
                token: self.shape.blank_id,
                probability: 1.0,
                duration_bin: 1,
            };
            if piece == 0 || piece >= self.shape.blank_id || projection[0] == encoder_frame[0] {
                return Ok(blank);
            }
            Ok(JointDecision {
                token: piece,
                probability: 0.9,
                duration_bin: 0,
            })
        }
    }

    /// Audio of `seconds` seconds encoding `pieces` one per frame from
    /// `start_frame`, silence elsewhere.
    pub fn audio(seconds: f32, start_frame: usize, pieces: &[u32]) -> Vec<f32> {
        let mut out = vec![0.0f32; (seconds * SAMPLE_RATE as f32) as usize];
        for (i, &piece) in pieces.iter().enumerate() {
            let frame = start_frame + i;
            let range = frame * FRAME_SAMPLES..((frame + 1) * FRAME_SAMPLES).min(out.len());
            for x in &mut out[range] {
                *x = piece as f32 / 1000.0;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FrameTokenBackend, audio};
    use super::*;
    use crate::language::StopwordRecognizer;
    use crate::vad::{EnergyVad, VadConfig};

    /// Pieces 1..=20 are word starts, 21..=40 continuations, 41 blank.
    fn vocab() -> Vocab {
        let mut pieces = vec!["<unk>".to_owned()];
        pieces.extend((1..=20).map(|i| format!("▁w{i}")));
        pieces.extend((21..=40).map(|i| format!("s{i}")));
        pieces.push("<blk>".to_owned());
        Vocab::from_pieces(pieces)
    }

    fn transcriber(config: PipelineConfig) -> Transcriber<FrameTokenBackend> {
        let vocab = vocab();
        let tagger = LanguageTagger::with_recognizer(
            LanguageTagger::default_candidates(),
            4,
            Box::new(StopwordRecognizer),
        );
        // Noise in the fixtures sits at 0.0004: under the fake's rounding to
        // piece 0, over this threshold, so the VAD counts it as speech.
        let vad = EnergyVad {
            rms_threshold: 0.0003,
            config: VadConfig {
                pad_seconds: 0.0,
                min_speech_seconds: 0.05,
                ..VadConfig::default()
            },
        };
        Transcriber::new(
            FrameTokenBackend::new(&vocab),
            vocab,
            Box::new(vad),
            tagger,
            config,
        )
    }

    #[test]
    fn a_short_recording_is_one_chunk_with_words_and_timings() {
        let mut t = transcriber(PipelineConfig::default());
        // "w1 s21 s22" then a pause then "w2".
        let mut samples = audio(6.0, 10, &[1, 21, 22]);
        for (i, x) in audio(6.0, 40, &[2]).into_iter().enumerate() {
            if x != 0.0 {
                samples[i] = x;
            }
        }
        let transcript = t.transcribe(&samples, None).unwrap();
        assert_eq!(transcript.chunks.len(), 1);
        assert_eq!(transcript.text(), "w1s21s22 w2");
        assert_eq!(transcript.segments.len(), 2, "{:?}", transcript.segments);
        // Frame 10 starts at 0.72 s after the one-frame emission delay; the
        // chunk's frame grid may sit one frame off the recording's.
        assert!(
            (transcript.words[0].start - 0.72).abs() < 0.09,
            "{}",
            transcript.words[0].start
        );
        assert_eq!(transcript.stats.windows, 1);
        assert_eq!(transcript.stats.recoveries_tried, 0);
    }

    #[test]
    fn long_speech_is_chunked_with_overlap_and_merged_without_duplicates() {
        let config = PipelineConfig {
            chunker: ChunkerConfig {
                target_seconds: 8.0,
                search_seconds: 1.0,
                overlap_seconds: 1.5,
                max_seconds: 12.0,
                ..ChunkerConfig::default()
            },
            ..PipelineConfig::default()
        };
        let mut t = transcriber(config);
        // 20 words, one every 1.6 s (20 frames), starting at 1 s: 32 s of speech with no pauses
        // long enough for the VAD, so the chunker cuts on energy inside the words.
        let mut samples = vec![0.0f32; 36 * SAMPLE_RATE];
        for word in 1..=20u32 {
            let frame = 12 + (word as usize - 1) * 20;
            for (i, x) in audio(36.0, frame, &[word, 20 + word])
                .into_iter()
                .enumerate()
            {
                if x != 0.0 {
                    samples[i] = x;
                }
            }
        }
        // Fill the gaps with low-level noise so the VAD sees one region.
        for (i, x) in samples.iter_mut().enumerate() {
            if *x == 0.0 && i > SAMPLE_RATE && i < 34 * SAMPLE_RATE {
                *x = 0.0004 * if i % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
        let transcript = t.transcribe(&samples, None).unwrap();
        assert!(transcript.chunks.len() >= 3, "{:?}", transcript.chunks);
        let expected: Vec<String> = (1..=20).map(|w| format!("w{w}s{}", 20 + w)).collect();
        assert_eq!(
            transcript
                .words
                .iter()
                .map(|w| w.text.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            transcript
                .tokens
                .windows(2)
                .all(|w| w[0].frame <= w[1].frame)
        );
    }

    #[test]
    fn an_empty_chunk_with_speech_is_retried_with_a_wider_window() {
        // Speech energy without decodable pieces for 10 s (piece 0 is silence to the fake),
        // then a word just after the chunk end: the extension picks it up.
        let config = PipelineConfig {
            chunker: ChunkerConfig {
                target_seconds: 8.0,
                search_seconds: 1.0,
                long_pause_seconds: 100.0,
                ..ChunkerConfig::default()
            },
            ..PipelineConfig::default()
        };
        let mut t = transcriber(config);
        let mut samples = audio(14.0, 130, &[5, 25]);
        for (i, x) in samples.iter_mut().enumerate() {
            if *x == 0.0 && i < 9 * SAMPLE_RATE {
                *x = 0.0004 * if i % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
        let transcript = t.transcribe(&samples, None).unwrap();
        assert!(transcript.stats.recoveries_tried >= 1);
        assert_eq!(transcript.text(), "w5s25");
        assert_eq!(
            speech_inside(&[0..16_000, 32_000..48_000], &(8_000..40_000)),
            1.0
        );
    }

    #[test]
    fn silence_gives_an_empty_transcript() {
        let mut t = transcriber(PipelineConfig::default());
        let transcript = t.transcribe(&vec![0.0; 3 * SAMPLE_RATE], None).unwrap();
        assert!(transcript.segments.is_empty());
        assert!(transcript.chunks.is_empty());
        assert_eq!(t.render(&[]), "");
    }
}
