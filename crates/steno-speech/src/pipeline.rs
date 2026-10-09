//! The pipeline above the tensors, in order: VAD regions, the pause-aligned
//! layout, one decode per chunk with the empty-decode recovery, the LCS
//! merge, pieces to words to segments, and the language tag. A chunk is a
//! range the layout chose; a window is the range actually decoded, the
//! chunk itself or, after a recovery, the chunk with more audio around it.
//! Generic over [`SpeechBackend`]: the ONNX and the `CoreML` engines run
//! it, and a fake backend drives it in tests.
//! Swift: `Sources/StenoSpeech/Engines/ParakeetEngine.swift` and
//! `ParakeetMapping.swift`, with `FluidAudio`'s `ChunkProcessor` in between.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use steno_core::{LanguageTag, RawSegment, TimedWord};

use crate::backend::{FRAME_SAMPLES, SAMPLE_RATE, SpeechBackend, sample_count};
use crate::chunker::{Chunk, ChunkerConfig, layout};
use crate::decoder::{DecodeStats, DecoderConfig, Token, decode_window};
use crate::error::SpeechError;
use crate::language::LanguageTagger;
use crate::merge::merge_all;
use crate::segmentation::{TokenAggregator, TranscriptSegmenter, timed_pieces};
use crate::vad::VoiceActivityDetector;
use crate::vocab::{Vocab, starts_word};

/// When a chunk with speech decodes to (almost) nothing, the window is
/// decoded again with more audio around it (decision 1: extend, do not
/// shift) and the candidate with the most words inside the chunk wins.
/// Decision 1 extends by 5 to 12 s; the defaults try 6, 12 and 18 s in
/// total, at most 6 s before and 12 s after the chunk, the 5 s earlier
/// start and 12 s later end that spike D's probe table
/// (`.plans/spikes/2026-10-01-spike-chunker-voting.md`) shows escaping the
/// zero-token window.
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
        join_words(&self.words)
    }
}

fn join_words(words: &[TimedWord]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One backend or more, the vocabulary, a detector and the configuration;
/// owns everything it needs, so it can move to a worker thread. With more
/// than one backend ([`Transcriber::with_workers`]) the chunks decode in
/// parallel, one backend per thread; the merge sees them in order, so the
/// transcript does not depend on the number of workers.
pub struct Transcriber<B: SpeechBackend> {
    /// The first decodes every chunk when it is alone and the probes of
    /// [`Transcriber::decode_range`].
    workers: Vec<B>,
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
            workers: vec![backend],
            vocab,
            vad,
            tagger,
            config,
        }
    }

    /// Adds backends that decode chunks in parallel with the first, one
    /// thread each; `CoreML` runs four, as `FluidAudio` does
    /// (`parallelChunkConcurrency`). ONNX Runtime spreads one call over
    /// its own threads, so the ONNX engine runs one.
    #[must_use]
    pub fn with_workers(mut self, more: impl IntoIterator<Item = B>) -> Self {
        self.workers.extend(more);
        self
    }

    /// The first backend.
    #[must_use]
    pub fn backend(&self) -> &B {
        &self.workers[0]
    }

    /// How many chunks decode at once.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.workers.len()
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
        let windows = self.decode_chunks(samples, &chunks, &speech, &mut stats)?;
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

    /// Decodes one window of the recording on the first backend; token
    /// frames are absolute.
    pub fn decode_range(
        &mut self,
        samples: &[f32],
        range: Range<usize>,
        stats: &mut DecodeStats,
    ) -> Result<Vec<Token>, SpeechError> {
        self.chunk_decoder(0).decode_range(samples, range, stats)
    }

    /// The text of `tokens`, for probes and tests.
    #[must_use]
    pub fn render(&self, tokens: &[Token]) -> String {
        join_words(&TokenAggregator.words(&timed_pieces(tokens, &self.vocab)))
    }

    fn chunk_decoder(&mut self, worker: usize) -> ChunkDecoder<'_, B> {
        ChunkDecoder {
            backend: &mut self.workers[worker],
            vocab: &self.vocab,
            config: &self.config,
        }
    }

    #[cfg(test)]
    fn recover(
        &mut self,
        samples: &[f32],
        range: &Range<usize>,
        original: Vec<Token>,
        stats: &mut DecodeStats,
    ) -> Result<Vec<Token>, SpeechError> {
        self.chunk_decoder(0)
            .recover(samples, range, original, stats)
    }

    /// Every chunk's tokens, in chunk order: on the one backend in turn,
    /// or on every backend at once, each taking the next chunk as it
    /// finishes one. After an error no worker starts another chunk, and
    /// the error of the earliest chunk is returned.
    fn decode_chunks(
        &mut self,
        samples: &[f32],
        chunks: &[Chunk],
        speech: &[Range<usize>],
        stats: &mut DecodeStats,
    ) -> Result<Vec<Vec<Token>>, SpeechError> {
        if self.workers.len() == 1 || chunks.len() <= 1 {
            let mut decoder = self.chunk_decoder(0);
            return chunks
                .iter()
                .map(|chunk| decoder.decode_chunk(samples, &chunk.range, speech, stats))
                .collect();
        }
        let (vocab, config) = (&self.vocab, &self.config);
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let mut slots: Vec<Option<Result<Vec<Token>, SpeechError>>> =
            chunks.iter().map(|_| None).collect();
        std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .workers
                .iter_mut()
                .take(chunks.len())
                .map(|backend| {
                    let (next, failed) = (&next, &failed);
                    scope.spawn(move || {
                        let mut decoder = ChunkDecoder {
                            backend,
                            vocab,
                            config,
                        };
                        let mut stats = DecodeStats::default();
                        let mut done = Vec::new();
                        while !failed.load(Ordering::Relaxed) {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(chunk) = chunks.get(index) else {
                                break;
                            };
                            let tokens =
                                decoder.decode_chunk(samples, &chunk.range, speech, &mut stats);
                            if tokens.is_err() {
                                failed.store(true, Ordering::Relaxed);
                            }
                            done.push((index, tokens));
                        }
                        (done, stats)
                    })
                })
                .collect();
            for handle in handles {
                // A worker's panic is the caller's, as on one thread.
                let (done, worker_stats) = handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                stats.add(&worker_stats);
                for (index, tokens) in done {
                    slots[index] = Some(tokens);
                }
            }
        });
        // Workers take chunks in order and finish every chunk they take,
        // so without an error every slot is filled.
        let mut windows = Vec::with_capacity(chunks.len());
        for slot in slots {
            match slot {
                Some(Ok(tokens)) => windows.push(tokens),
                Some(Err(error)) => return Err(error),
                None => {}
            }
        }
        Ok(windows)
    }
}

/// One backend's view of a transcription: decodes a chunk and, when it
/// looks empty, retries it with more audio around it.
struct ChunkDecoder<'a, B: ?Sized> {
    backend: &'a mut B,
    vocab: &'a Vocab,
    config: &'a PipelineConfig,
}

impl<B: SpeechBackend + ?Sized> ChunkDecoder<'_, B> {
    /// The chunk's tokens, recovered when the first decode looks empty.
    fn decode_chunk(
        &mut self,
        samples: &[f32],
        range: &Range<usize>,
        speech: &[Range<usize>],
        stats: &mut DecodeStats,
    ) -> Result<Vec<Token>, SpeechError> {
        let tokens = self.decode_range(samples, range.clone(), stats)?;
        if self.looks_empty(&tokens, speech, range) {
            return self.recover(samples, range, tokens, stats);
        }
        Ok(tokens)
    }

    /// Decodes one window of the recording; token frames are absolute.
    fn decode_range(
        &mut self,
        samples: &[f32],
        range: Range<usize>,
        stats: &mut DecodeStats,
    ) -> Result<Vec<Token>, SpeechError> {
        let range = range.start.min(samples.len())..range.end.min(samples.len());
        let features = self.backend.features(&samples[range.clone()])?;
        let encoded = self.backend.encode(&features)?;
        decode_window(
            &mut *self.backend,
            &encoded,
            range.start / FRAME_SAMPLES,
            &self.config.decoder,
            stats,
        )
    }

    /// Word starts among `tokens` whose frame lies inside `range`, so a
    /// recovery candidate decoded over a wider window is scored on the
    /// chunk's own audio and not on the neighbour's speech it also saw.
    fn words_inside(&self, tokens: &[Token], range: &Range<usize>) -> usize {
        let frames = range.start / FRAME_SAMPLES..=range.end / FRAME_SAMPLES;
        tokens
            .iter()
            .filter(|t| frames.contains(&t.frame) && starts_word(self.vocab.piece(t.id)))
            .count()
    }

    fn looks_empty(&self, tokens: &[Token], speech: &[Range<usize>], range: &Range<usize>) -> bool {
        let speech_seconds = speech_inside(speech, range);
        let words = self.words_inside(tokens, range) as f32;
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
        let config = self.config;
        let max = sample_count(config.chunker.max_seconds);
        let original_words = self.words_inside(&original, range);
        let (mut best, mut best_words) = (original, original_words);
        for &(before, after) in &config.recovery.extensions_seconds {
            let start = range.start.saturating_sub(sample_count(before));
            let end = (range.end + sample_count(after)).min(samples.len());
            if end - start > max || (start == range.start && end == range.end) {
                continue;
            }
            stats.recoveries_tried += 1;
            let candidate = self.decode_range(samples, start..end, stats)?;
            let words = self.words_inside(&candidate, range);
            if words > best_words {
                best = candidate;
                best_words = words;
            }
        }
        if best_words > original_words {
            stats.recoveries_accepted += 1;
            let overlap = sample_count(config.chunker.overlap_seconds);
            keep_chunk_and_overlap(&mut best, range, overlap, self.vocab);
        }
        Ok(best)
    }
}

/// Trims an accepted recovery to what the merge expects of a chunk: its
/// range plus `overlap` either side, since the wider window also decoded
/// the neighbours' speech. A word with a piece inside those bounds is kept
/// whole, from its first piece to its last, so the merge never meets half
/// a word. `tokens` come from one decode, so their frames do not decrease.
fn keep_chunk_and_overlap(
    tokens: &mut Vec<Token>,
    range: &Range<usize>,
    overlap: usize,
    vocab: &Vocab,
) {
    let frames =
        range.start.saturating_sub(overlap) / FRAME_SAMPLES..=(range.end + overlap) / FRAME_SAMPLES;
    let inside = |t: &Token| frames.contains(&t.frame);
    let (Some(first), Some(last)) = (
        tokens.iter().position(inside),
        tokens.iter().rposition(inside),
    ) else {
        tokens.clear();
        return;
    };
    let splice_safe = |t: &Token| vocab.is_splice_safe(t.id);
    let start = tokens[..=first].iter().rposition(splice_safe).unwrap_or(0);
    let end = tokens[last + 1..]
        .iter()
        .position(splice_safe)
        .map_or(tokens.len(), |n| last + 1 + n);
    tokens.truncate(end);
    tokens.drain(..start);
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
    count as f32 / SAMPLE_RATE as f32
}

#[cfg(test)]
mod fake {
    //! A backend that reads the "token per frame" the test audio encodes:
    //! each 80 ms frame of audio holds a constant sample value `k / 1000`,
    //! which the fake decodes as piece `k` (0 for silence); a chunk's frames
    //! are not aligned to the recording's, so a frame takes the piece most
    //! of its samples carry. The joint emits the frame's piece once
    //! (duration 0) and then blank, so the decoder loop, the chunker and the
    //! merge are exercised end to end with a known answer. A window shorter
    //! than `min_frames` encodes to silence, which stands in for spike D's
    //! zero-token window so the recovery has something to recover.

    use super::*;
    use crate::backend::{
        DecoderState, DecoderStep, EncoderOutput, Features, JointDecision, ModelShape,
    };

    pub struct FrameTokenBackend {
        shape: ModelShape,
        min_frames: usize,
    }

    impl FrameTokenBackend {
        pub fn new(vocab: &Vocab, min_frames: usize) -> Self {
            FrameTokenBackend {
                min_frames,
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
            let data = if features.frames < self.min_frames {
                vec![0.0; features.frames]
            } else {
                features.data.clone()
            };
            Ok(EncoderOutput {
                hidden: 1,
                len: features.frames,
                data,
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
        let mut out = vec![0.0f32; sample_count(seconds)];
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
        Vocab::from_pieces(pieces).unwrap()
    }

    fn transcriber(config: PipelineConfig, min_frames: usize) -> Transcriber<FrameTokenBackend> {
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
        let backend = FrameTokenBackend::new(&vocab, min_frames);
        Transcriber::new(backend, vocab, Box::new(vad), tagger, config)
    }

    /// Copies the non-silent samples of `other` over `samples`.
    fn overlay(samples: &mut [f32], other: &[f32]) {
        for (x, &y) in samples.iter_mut().zip(other) {
            if y != 0.0 {
                *x = y;
            }
        }
    }

    /// Fills the silence inside `range` with the fixtures' noise.
    fn fill_noise(samples: &mut [f32], range: Range<usize>) {
        for (i, x) in samples.iter_mut().enumerate() {
            if *x == 0.0 && range.contains(&i) {
                *x = 0.0004 * if i % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
    }

    /// 9 s of speech energy the fake decodes to nothing (piece 0), one word
    /// at `word_frame`; chunks of about 8 s with 1.5 s of overlap.
    fn empty_chunk_fixture(word_frame: usize) -> (PipelineConfig, Vec<f32>) {
        let config = PipelineConfig {
            chunker: ChunkerConfig {
                target_seconds: 8.0,
                search_seconds: 1.0,
                overlap_seconds: 1.5,
                long_pause_seconds: 100.0,
                ..ChunkerConfig::default()
            },
            ..PipelineConfig::default()
        };
        let mut samples = audio(14.0, word_frame, &[5, 25]);
        fill_noise(&mut samples, 0..9 * SAMPLE_RATE);
        (config, samples)
    }

    #[test]
    fn a_short_recording_is_one_chunk_with_words_and_timings() {
        let mut t = transcriber(PipelineConfig::default(), 0);
        // "w1 s21 s22" then a pause then "w2".
        let mut samples = audio(6.0, 10, &[1, 21, 22]);
        overlay(&mut samples, &audio(6.0, 40, &[2]));
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
        let mut t = transcriber(config, 0);
        // 20 words, one every 1.6 s (20 frames), starting at 1 s: 32 s of speech with no pauses
        // long enough for the VAD, so the chunker cuts on energy inside the words.
        let mut samples = vec![0.0f32; 36 * SAMPLE_RATE];
        for word in 1..=20u32 {
            let frame = 12 + (word as usize - 1) * 20;
            overlay(&mut samples, &audio(36.0, frame, &[word, 20 + word]));
        }
        // Fill the gaps with low-level noise so the VAD sees one region.
        fill_noise(&mut samples, SAMPLE_RATE + 1..34 * SAMPLE_RATE);
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
    fn chunks_decoded_on_several_workers_merge_as_on_one() {
        let config = PipelineConfig {
            chunker: ChunkerConfig {
                target_seconds: 6.0,
                search_seconds: 1.0,
                max_seconds: 9.0,
                ..ChunkerConfig::default()
            },
            ..PipelineConfig::default()
        };
        let mut samples = vec![0.0f32; 36 * SAMPLE_RATE];
        for word in 1..=20u32 {
            let frame = 12 + (word as usize - 1) * 20;
            overlay(&mut samples, &audio(36.0, frame, &[word, 20 + word]));
        }
        fill_noise(&mut samples, SAMPLE_RATE + 1..34 * SAMPLE_RATE);
        let mut one = transcriber(config.clone(), 0);
        let alone = one.transcribe(&samples, None).unwrap();
        let vocab = vocab();
        let mut three =
            transcriber(config, 0).with_workers((0..2).map(|_| FrameTokenBackend::new(&vocab, 0)));
        assert_eq!(three.workers(), 3);
        let parallel = three.transcribe(&samples, None).unwrap();
        assert!(alone.chunks.len() >= 4, "{:?}", alone.chunks);
        assert_eq!(parallel, alone);
    }

    #[test]
    fn an_empty_chunk_with_speech_is_retried_with_a_wider_window() {
        // The word sits at 4 s inside the one 9.25 s chunk (116 frames); the
        // fake encodes windows under 130 frames to silence, so only the
        // 6 s extension after the chunk decodes it.
        let (config, samples) = empty_chunk_fixture(50);
        let mut t = transcriber(config, 130);
        let transcript = t.transcribe(&samples, None).unwrap();
        assert_eq!(transcript.chunks.len(), 1, "{:?}", transcript.chunks);
        assert!(transcript.stats.recoveries_tried >= 1);
        assert_eq!(transcript.stats.recoveries_accepted, 1);
        assert_eq!(transcript.text(), "w5s25");
    }

    #[test]
    fn a_word_in_the_neighbour_chunk_does_not_count_as_a_recovery() {
        // The word at 10.4 s lies after the first chunk's end; the widened
        // window decodes it, but it is the second chunk's word, so the retry
        // is not accepted and the merge sees it once.
        let (config, samples) = empty_chunk_fixture(130);
        let mut t = transcriber(config, 0);
        let transcript = t.transcribe(&samples, None).unwrap();
        assert_eq!(transcript.chunks.len(), 2, "{:?}", transcript.chunks);
        assert!(transcript.stats.recoveries_tried >= 1);
        assert_eq!(transcript.stats.recoveries_accepted, 0);
        assert_eq!(transcript.text(), "w5s25");
    }

    #[test]
    fn speech_inside_counts_only_the_overlap() {
        assert_eq!(
            speech_inside(&[0..16_000, 32_000..48_000], &(8_000..40_000)),
            1.0
        );
    }

    /// Recovers the 1 to 9 s range of the empty-chunk fixture, with a
    /// second word at 10.8 s, under `extensions`; windows under 160 frames
    /// (12.8 s) decode to silence.
    fn recover_with(extensions: &[(f32, f32)]) -> (Vec<Token>, Transcriber<FrameTokenBackend>) {
        let (mut config, mut samples) = empty_chunk_fixture(50);
        config.recovery.extensions_seconds = extensions.to_vec();
        overlay(&mut samples, &audio(14.0, 135, &[6]));
        let mut t = transcriber(config, 160);
        let range = SAMPLE_RATE..9 * SAMPLE_RATE;
        let tokens = t
            .recover(&samples, &range, Vec::new(), &mut DecodeStats::default())
            .unwrap();
        (tokens, t)
    }

    #[test]
    fn an_accepted_recovery_keeps_only_the_chunk_and_its_overlap() {
        // The 1 to 15 s window decodes both words; the one at 10.8 s lies
        // past 9 s plus the 1.5 s overlap and belongs to the next chunk.
        let (tokens, t) = recover_with(&[(0.0, 6.0)]);
        assert_eq!(t.render(&tokens), "w5s25");
    }

    #[test]
    fn a_later_extension_replaces_an_earlier_one_only_with_more_words() {
        // The 3 s extension (11 s window) decodes nothing: fewer words, so
        // the 6 s one stays.
        let (tokens, t) = recover_with(&[(0.0, 6.0), (0.0, 3.0)]);
        assert_eq!(t.render(&tokens), "w5s25");
        // A tie keeps the first: starting half a frame earlier moves the
        // frame grid, and with it the word's frame.
        let (first, _) = recover_with(&[(0.0, 6.0)]);
        let (later, _) = recover_with(&[(0.04, 6.0)]);
        assert_ne!(first[0].frame, later[0].frame);
        let (tied, _) = recover_with(&[(0.0, 6.0), (0.04, 6.0)]);
        assert_eq!(tied, first);
    }

    #[test]
    fn a_recovery_trim_through_a_word_keeps_the_whole_word() {
        // The trim ends on frame 131 (9 s plus the 1.5 s overlap) and "w7
        // s27" starts on it. The trimmed window keeps the whole word, so the
        // merge with the next window, which heard all of it, keeps it whole.
        let (mut config, mut samples) = empty_chunk_fixture(50);
        config.recovery.extensions_seconds = vec![(0.0, 6.0)];
        samples.resize(18 * SAMPLE_RATE, 0.0);
        overlay(&mut samples, &audio(18.0, 132, &[7, 27]));
        let mut t = transcriber(config, 110);
        let mut stats = DecodeStats::default();
        let range = SAMPLE_RATE..9 * SAMPLE_RATE;
        let left = t.recover(&samples, &range, Vec::new(), &mut stats).unwrap();
        assert_eq!(stats.recoveries_accepted, 1);
        assert_eq!(t.render(&left), "w5s25 w7s27");
        let next = 15 * SAMPLE_RATE / 2..35 * SAMPLE_RATE / 2;
        let right = t.decode_range(&samples, next, &mut stats).unwrap();
        assert_eq!(t.render(&right), "w7s27");
        let merged = merge_all(&[left, right], 1.5, t.vocab());
        assert_eq!(t.render(&merged), "w5s25 w7s27");
    }

    #[test]
    fn the_trim_keeps_tokens_up_to_the_overlap_and_words_it_cuts_whole() {
        // The 3 to 9 s chunk with 1.5 s overlap keeps frames 18 to 131.
        let token = |id, frame| Token {
            id,
            frame,
            confidence: 1.0,
            duration: 1,
        };
        let ids = |tokens: &[Token]| tokens.iter().map(|t| (t.id, t.frame)).collect::<Vec<_>>();
        let range = 3 * SAMPLE_RATE..9 * SAMPLE_RATE;
        let overlap = 3 * SAMPLE_RATE / 2;
        let vocab = vocab();
        let mut words = vec![token(1, 17), token(2, 18), token(3, 131), token(4, 132)];
        keep_chunk_and_overlap(&mut words, &range, overlap, &vocab);
        assert_eq!(ids(&words), [(2, 18), (3, 131)]);
        // "w1 s21" ends on frame 18 and "w3 s23 s24" starts on 131: both
        // are kept whole; "w4" and the lone "s25" past it are dropped.
        let mut cut = vec![
            token(1, 16),
            token(21, 18),
            token(3, 131),
            token(23, 132),
            token(24, 133),
            token(4, 134),
            token(25, 135),
        ];
        keep_chunk_and_overlap(&mut cut, &range, overlap, &vocab);
        assert_eq!(
            ids(&cut),
            [(1, 16), (21, 18), (3, 131), (23, 132), (24, 133)]
        );
        // Without a word start before the first kept piece, the trim keeps
        // from the window's first token.
        let mut headless = vec![token(21, 16), token(22, 18), token(3, 131)];
        keep_chunk_and_overlap(&mut headless, &range, overlap, &vocab);
        assert_eq!(ids(&headless), [(21, 16), (22, 18), (3, 131)]);
        let mut outside = vec![token(1, 10), token(2, 140)];
        keep_chunk_and_overlap(&mut outside, &range, overlap, &vocab);
        assert!(outside.is_empty());
    }

    #[test]
    fn silence_gives_an_empty_transcript() {
        let mut t = transcriber(PipelineConfig::default(), 0);
        let transcript = t.transcribe(&vec![0.0; 3 * SAMPLE_RATE], None).unwrap();
        assert!(transcript.segments.is_empty());
        assert!(transcript.chunks.is_empty());
        assert_eq!(t.render(&[]), "");
    }
}
