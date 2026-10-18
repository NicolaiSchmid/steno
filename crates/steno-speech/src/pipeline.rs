//! The pipeline above the tensors, in order: VAD regions, the partition of
//! the recording into owned spans ([`partition`]: the pause-aligned chunks
//! and, when [`ChunkerConfig::decode_gaps`] is on, extra windows over the
//! audio no chunk covers), one decode per span with the empty-decode
//! recovery for a chunk, the spans' words joined in order, pieces to words
//! to segments, and the language tag. A chunk is a range the layout chose;
//! a window is the range actually decoded: the chunk itself or, after a
//! recovery, the chunk with more audio around it, or an extra window.
//!
//! The spans' decodes are joined by runs (`join`). A run is the
//! consecutive spans of one kind: the chunks between two gaps' windows, or
//! one gap's windows. Inside a run the LCS merge stays (`merge_all`), as
//! before the partition, until after stable. Between runs the words are
//! concatenated, and one rule drops a word both sides kept. A gap's window
//! keeps its whole decode, and so does a chunk's plain decode; an accepted
//! recovery keeps the words with a piece in its chunk's reach
//! ([`Span::reach`], `own`), the range the merge before the partition kept.
//! Generic over [`SpeechBackend`]: the ONNX and the `CoreML` engines run
//! it, and a fake backend drives it in tests.
//! Swift: `Sources/StenoSpeech/Engines/ParakeetEngine.swift` and
//! `ParakeetMapping.swift`, with `FluidAudio`'s `ChunkProcessor` in between.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use steno_core::{LanguageTag, RawSegment, TimedWord};

use crate::backend::{FRAME_SAMPLES, SAMPLE_RATE, SpeechBackend, sample_count};
use crate::chunker::{ChunkerConfig, Span, partition};
use crate::decoder::{DecodeStats, DecoderConfig, Token, decode_window};
use crate::error::SpeechError;
use crate::language::LanguageTagger;
use crate::merge::merge_all;
use crate::segmentation::{TokenAggregator, TranscriptSegmenter, timed_pieces};
use crate::vad::VoiceActivityDetector;
use crate::vocab::{Vocab, starts_word};

/// When a chunk with speech decodes to (almost) nothing, the window is
/// decoded again with more audio around it (decision 1: extend, do not
/// shift) and the candidate with the most words starting in the chunk wins,
/// and keeps the words with a piece in the chunk's reach ([`Span::reach`]).
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
/// returns down to the partition for diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub segments: Vec<RawSegment>,
    pub words: Vec<TimedWord>,
    pub tokens: Vec<Token>,
    /// The partition ([`partition`]): the chunks and the extra windows over
    /// the gaps, in order.
    pub spans: Vec<Span>,
    /// Per span, the window its words came from: the span's own, or the
    /// wider one of an accepted recovery.
    pub windows: Vec<Range<usize>>,
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
/// owns everything it needs, so it can move to a blocking thread. With
/// more than one backend ([`Transcriber::with_more_workers`]) the windows
/// decode in parallel, one backend per thread; the merge sees them in
/// order, so the transcript does not depend on the number of workers.
pub struct Transcriber<B: SpeechBackend> {
    /// The first decodes every window when it is alone and the probes of
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

    /// Adds backends that decode windows in parallel with the first, one
    /// thread each, so `more` backends make `more + 1` workers; `CoreML`
    /// runs four, as `FluidAudio` does (`parallelChunkConcurrency`). ONNX
    /// Runtime spreads one call over its own threads, so the ONNX engine
    /// runs one.
    #[must_use]
    pub fn with_more_workers(mut self, more: impl IntoIterator<Item = B>) -> Self {
        self.workers.extend(more);
        self
    }

    /// The first backend.
    #[must_use]
    pub fn backend(&self) -> &B {
        &self.workers[0]
    }

    /// How many windows decode at once.
    #[must_use]
    pub fn worker_count(&self) -> usize {
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
        let spans = partition(samples, &speech, &self.config.chunker);
        let mut stats = DecodeStats::default();
        let (decoded, windows): (Vec<_>, Vec<_>) = self
            .decode_spans(samples, &spans, &speech, &mut stats)?
            .into_iter()
            .map(|d| (d.tokens, d.window))
            .unzip();
        let overlap = f64::from(self.config.chunker.overlap_seconds);
        let tokens = join(&spans, &decoded, overlap, &self.vocab);
        let words = TokenAggregator.words(&timed_pieces(&tokens, &self.vocab));
        let segments = self
            .tagger
            .tag(self.config.segmenter.segments(&words), hint);
        Ok(Transcript {
            segments,
            words,
            tokens,
            spans,
            windows,
            speech,
            stats,
        })
    }

    /// Decodes one window of the recording on the first backend; token
    /// frames are absolute and their phase places them where the window
    /// starts ([`Token::phase`]).
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
        span: &Span,
        original: Vec<Token>,
        stats: &mut DecodeStats,
    ) -> Result<Vec<Token>, SpeechError> {
        self.chunk_decoder(0)
            .recover(samples, span, original, stats)
            .map(|decoded| decoded.tokens)
    }

    /// Every span's tokens, in order: on the one backend in turn, or on
    /// every backend at once, each taking the next span as it finishes one.
    /// After an error or a panic no worker starts another span, and the
    /// error of the earliest span is returned (a panic is re-raised).
    fn decode_spans(
        &mut self,
        samples: &[f32],
        spans: &[Span],
        speech: &[Range<usize>],
        stats: &mut DecodeStats,
    ) -> Result<Vec<Decoded>, SpeechError> {
        if self.workers.len() == 1 || spans.len() <= 1 {
            let mut decoder = self.chunk_decoder(0);
            return spans
                .iter()
                .map(|span| decoder.decode_span(samples, span, speech, stats))
                .collect();
        }
        let (vocab, config) = (&self.vocab, &self.config);
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let mut slots: Vec<Option<Result<Decoded, SpeechError>>> =
            spans.iter().map(|_| None).collect();
        std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .workers
                .iter_mut()
                .take(spans.len())
                .map(|backend| {
                    let (next, failed) = (&next, &failed);
                    scope.spawn(move || {
                        // A panic stops the other workers as an error does.
                        let _stop = StopOnPanic(failed);
                        let mut decoder = ChunkDecoder {
                            backend,
                            vocab,
                            config,
                        };
                        let mut stats = DecodeStats::default();
                        let mut done = Vec::new();
                        while !failed.load(Ordering::Relaxed) {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(span) = spans.get(index) else {
                                break;
                            };
                            let tokens = decoder.decode_span(samples, span, speech, &mut stats);
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
        // Workers take spans in order and finish every span they take, so
        // the filled slots come first, and without an error every slot is
        // filled; the collect stops at the earliest error.
        debug_assert!(
            slots
                .iter()
                .skip_while(|slot| slot.is_some())
                .all(Option::is_none),
            "a decoded span follows one that was not decoded"
        );
        slots.into_iter().flatten().collect()
    }
}

/// Sets the flag when the worker that holds it unwinds.
struct StopOnPanic<'a>(&'a AtomicBool);

impl Drop for StopOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}

/// Two encoder frames: how close two copies of one piece either side of a
/// seam between runs start when `join` takes them for one word.
const SEAM_SAMPLES: usize = 2 * FRAME_SAMPLES;

/// The spans' tokens joined in order, by runs: a run is the consecutive
/// spans of one kind, the chunks between two gaps' windows or one gap's
/// windows, and its decodes merge with each other (`merge_all`), as they
/// did before the partition. Runs of the two kinds alternate, so every seam
/// between runs has a gap's window on one side, and both sides can keep one
/// word: an exact window hears the part of a word that sounds across its
/// edge as a word of its own, and a recovery keeps words past its chunk.
/// So when the last word before the seam and the first after it are the
/// same piece and start within [`SEAM_SAMPLES`] of each other, one copy
/// falls away: the one with fewer pieces, which heard less of the word,
/// and on a tie the gap window's. A word said again starts later than
/// that, unless it is two frames long or shorter: such a word said twice
/// back to back across a seam comes out once. A gap's run that begins
/// inside a word drops the pieces that continue it when any chunk, before
/// the run or after it, kept a token that starts before the run's first
/// window, as the merge before the partition did: the chunks heard the
/// word's start, or missed the word. An accepted recovery keeps words
/// inside the gap's window on either side of its chunk, so at either seam
/// of that chunk the tokens can step back in time, by up to the overlap
/// and the rest of a word that crosses the reach's edge; each keeps the
/// time its window gave it.
fn join(spans: &[Span], decoded: &[Vec<Token>], overlap_seconds: f64, vocab: &Vocab) -> Vec<Token> {
    let starts_word = |t: &Token| starts_word(vocab.piece(t.id));
    let mut at = 0;
    let runs: Vec<(&[Span], Vec<Token>)> = spans
        .chunk_by(|a, b| a.chunk == b.chunk)
        .map(|run| {
            let next = merge_all(&decoded[at..at + run.len()], overlap_seconds, vocab);
            at += run.len();
            (run, next)
        })
        .collect();
    // Where the earliest token any chunk kept starts: a run's first token
    // is its earliest.
    let chunks_from = runs
        .iter()
        .filter(|(run, _)| run[0].chunk)
        .filter_map(|(_, next)| next.first())
        .map(Token::sample)
        .min()
        .unwrap_or(usize::MAX);
    let mut out: Vec<Token> = Vec::with_capacity(decoded.iter().map(Vec::len).sum());
    let mut before = 0;
    for (run, mut next) in runs {
        if !run[0].chunk && chunks_from < run[0].window.start {
            let rest = next.iter().take_while(|t| !starts_word(t)).count();
            next.drain(..rest);
        }
        // The last word of the run before.
        if let Some(last) = out[before..]
            .iter()
            .rposition(starts_word)
            .map(|i| before + i)
            && let Some(first) = next.first()
            && first.id == out[last].id
            && first.sample().abs_diff(out[last].sample()) <= SEAM_SAMPLES
        {
            let earlier = out.len() - last;
            let later = 1 + next[1..].iter().take_while(|t| !starts_word(t)).count();
            if later > earlier || (later == earlier && run[0].chunk) {
                out.truncate(last);
            } else {
                next.drain(..later);
            }
        }
        before = out.len();
        out.extend(next);
    }
    out
}

/// The words of an accepted recovery's decode with any piece in `range`,
/// its chunk's reach, each from its start, wherever that lies, to the
/// punctuation after its last piece in `range` ([`Vocab::is_splice_safe`]),
/// where the trim before the partition cut. A decode that begins inside a
/// word gives the pieces that continue it as a word of their own.
fn own(tokens: &[Token], range: &Range<usize>, vocab: &Vocab) -> Vec<Token> {
    let mut kept = Vec::with_capacity(tokens.len());
    for word in tokens.chunk_by(|_, next| !starts_word(vocab.piece(next.id))) {
        let Some(last) = word.iter().rposition(|t| range.contains(&t.sample())) else {
            continue;
        };
        let rest = word[last + 1..].iter();
        let end = last + 1 + rest.take_while(|t| !vocab.is_splice_safe(t.id)).count();
        kept.extend_from_slice(&word[..end]);
    }
    kept
}

/// One span's decode: the tokens it keeps and the window they came from.
struct Decoded {
    tokens: Vec<Token>,
    window: Range<usize>,
}

/// One backend's view of a transcription: decodes a chunk and, when it
/// looks empty, retries it with more audio around it.
struct ChunkDecoder<'a, B: ?Sized> {
    backend: &'a mut B,
    vocab: &'a Vocab,
    config: &'a PipelineConfig,
}

impl<B: SpeechBackend + ?Sized> ChunkDecoder<'_, B> {
    /// The span's tokens: an extra window's whole decode; a chunk's,
    /// recovered when the first decode looks empty.
    fn decode_span(
        &mut self,
        samples: &[f32],
        span: &Span,
        speech: &[Range<usize>],
        stats: &mut DecodeStats,
    ) -> Result<Decoded, SpeechError> {
        let tokens = self.decode_range(samples, span.window.clone(), stats)?;
        let window = span.window.clone();
        if span.chunk && self.looks_empty(&tokens, speech, &window) {
            return self.recover(samples, span, tokens, stats);
        }
        Ok(Decoded { tokens, window })
    }

    /// Decodes one window of the recording; token frames are absolute and
    /// their phase places them where the window starts ([`Token::phase`]).
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
            range.start,
            &self.config.decoder,
            stats,
        )
    }

    /// Word starts among `tokens` whose frame lies inside `range`, so a
    /// recovery candidate decoded over a wider window is scored on the
    /// chunk's own audio and not on the neighbour's speech it also heard.
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

    /// Decodes the chunk again over wider windows, which only the window
    /// changes: a candidate is scored on the words that start in the
    /// chunk's window, as the chunk's own decode is (`looks_empty`), and an
    /// accepted one keeps the words with a piece in the span's reach
    /// ([`Span::reach`]).
    fn recover(
        &mut self,
        samples: &[f32],
        span: &Span,
        original: Vec<Token>,
        stats: &mut DecodeStats,
    ) -> Result<Decoded, SpeechError> {
        let config = self.config;
        let max = sample_count(config.chunker.max_seconds);
        let range = &span.window;
        let original_words = self.words_inside(&original, range);
        let mut best = (original, original_words, range.clone());
        for &(before, after) in &config.recovery.extensions_seconds {
            let start = range.start.saturating_sub(sample_count(before));
            let end = (range.end + sample_count(after)).min(samples.len());
            if end - start > max || (start == range.start && end == range.end) {
                continue;
            }
            stats.recoveries_tried += 1;
            let candidate = self.decode_range(samples, start..end, stats)?;
            let words = self.words_inside(&candidate, range);
            if words > best.1 {
                best = (candidate, words, start..end);
            }
        }
        let (mut tokens, words, window) = best;
        if words > original_words {
            stats.recoveries_accepted += 1;
            tokens = own(&tokens, &span.reach, self.vocab);
        }
        Ok(Decoded { tokens, window })
    }
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
    //! than `min_frames` encodes to silence (with `silent_needs` set, only
    //! one that holds that piece), which stands in for spike D's zero-token
    //! window so the recovery has something to recover. For the
    //! error paths a backend can fail or panic on a window that holds a
    //! given piece, take its time over every other window, and count the
    //! windows it is given.

    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::backend::{
        DecoderState, DecoderStep, EncoderOutput, Features, JointDecision, ModelShape,
    };

    pub struct FrameTokenBackend {
        shape: ModelShape,
        min_frames: usize,
        /// When set, a window shorter than `min_frames` encodes to silence
        /// only when it holds this piece.
        pub silent_needs: Option<u32>,
        /// Windows holding one of these pieces fail with `piece <id>`.
        pub fail_on: Vec<u32>,
        /// A window holding this piece panics.
        pub panic_on: Option<u32>,
        /// How long every other window takes.
        pub delay: Duration,
        /// Counts the windows, shared between backends.
        pub windows: Arc<AtomicUsize>,
        /// When not zero, a window hears every token this many frames
        /// early, on time or this many frames late, by its length in
        /// frames, as a model's timing depends on the audio around a word.
        pub jitter: usize,
        /// A window deaf at its edges, as a model with little context
        /// there can be: it hears nothing in its first `.0` frames and its
        /// last `.1`, on the windows whose length hashes to 0 modulo `.2`
        /// (1: every window).
        pub deaf: (usize, usize, usize),
    }

    impl FrameTokenBackend {
        pub fn new(vocab: &Vocab, min_frames: usize) -> Self {
            FrameTokenBackend {
                min_frames,
                silent_needs: None,
                fail_on: Vec::new(),
                panic_on: None,
                delay: Duration::ZERO,
                windows: Arc::default(),
                jitter: 0,
                deaf: (0, 0, 1),
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
                    // Most frames hold one piece or silence: the quick way.
                    let first = frame[0];
                    if frame.iter().all(|&x| x == first) {
                        return (first * 1000.0).round();
                    }
                    if frame.iter().all(|&x| x.abs() < 0.0005) {
                        return 0.0;
                    }
                    // A frame holds a few values; on a tie the larger wins.
                    let mut counts: Vec<(i64, usize)> = Vec::new();
                    for x in frame {
                        let k = (x * 1000.0).round() as i64;
                        match counts.iter_mut().find(|(c, _)| *c == k) {
                            Some((_, n)) => *n += 1,
                            None => counts.push((k, 1)),
                        }
                    }
                    counts
                        .into_iter()
                        .max_by_key(|&(k, n)| (n, k))
                        .map_or(0.0, |(k, _)| k as f32)
                })
                .collect::<Vec<f32>>();
            self.windows.fetch_add(1, Ordering::SeqCst);
            let holds = |piece: u32| data.contains(&(piece as f32));
            if let Some(&piece) = self.fail_on.iter().find(|&&piece| holds(piece)) {
                return Err(SpeechError::Backend(format!("piece {piece}").into()));
            }
            if let Some(piece) = self.panic_on.filter(|&piece| holds(piece)) {
                panic!("piece {piece}");
            }
            std::thread::sleep(self.delay);
            Ok(Features {
                mels: 1,
                frames,
                data,
            })
        }
        fn encode(&mut self, features: &Features) -> Result<EncoderOutput, SpeechError> {
            let holds = |piece: u32| features.data.contains(&(piece as f32));
            let mut data =
                if features.frames < self.min_frames && self.silent_needs.is_none_or(holds) {
                    vec![0.0; features.frames]
                } else {
                    features.data.clone()
                };
            let (head, tail, every) = self.deaf;
            let hash = (features.frames as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 33;
            if hash.is_multiple_of(every.max(1) as u64) {
                let n = data.len();
                data[..head.min(n)].fill(0.0);
                data[n.saturating_sub(tail)..].fill(0.0);
            }
            let by = self.jitter.min(data.len());
            if by > 0 {
                let rest = data.len() - by;
                match features.frames % 3 {
                    0 => {
                        data.rotate_right(by);
                        data[..by].fill(0.0);
                    }
                    1 => {
                        data.rotate_left(by);
                        data[rest..].fill(0.0);
                    }
                    _ => {}
                }
            }
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
    use std::sync::Arc;

    use super::fake::{FrameTokenBackend, audio};
    use super::*;
    use crate::chunker::{Cut, layout};
    use crate::language::StopwordRecognizer;
    use crate::vad::{EnergyVad, VadConfig};

    /// Pieces 1..=20 are word starts, 21..=40 continuations, 41 a full
    /// stop, 42 blank.
    fn vocab() -> Vocab {
        let mut pieces = vec!["<unk>".to_owned()];
        pieces.extend((1..=20).map(|i| format!("▁w{i}")));
        pieces.extend((21..=40).map(|i| format!("s{i}")));
        pieces.extend([".".to_owned(), "<blk>".to_owned()]);
        Vocab::from_pieces(pieces).unwrap()
    }

    fn transcriber(config: PipelineConfig, min_frames: usize) -> Transcriber<FrameTokenBackend> {
        transcriber_over(config, vec![FrameTokenBackend::new(&vocab(), min_frames)])
    }

    /// The fixtures' detector over `backends`, one worker each.
    fn transcriber_over(
        config: PipelineConfig,
        backends: Vec<FrameTokenBackend>,
    ) -> Transcriber<FrameTokenBackend> {
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
        let mut backends = backends.into_iter();
        let first = backends.next().expect("one backend at least");
        Transcriber::new(first, vocab, Box::new(vad), tagger, config).with_more_workers(backends)
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
        assert_eq!(transcript.spans.len(), 1);
        assert_eq!(transcript.text(), "w1s21s22 w2");
        assert_eq!(transcript.segments.len(), 2, "{:?}", transcript.segments);
        // Frame 10 starts at 0.72 s after the one-frame emission delay. The
        // chunk's frames start where the chunk does, so the fake's word lands
        // on the chunk frame nearest the recording's: within half a frame.
        assert!(
            (transcript.words[0].start - 0.72).abs() <= 0.04 + 1e-9,
            "{} from a chunk at {:?}",
            transcript.words[0].start,
            transcript.spans[0].window
        );
        assert_eq!(transcript.stats.windows, 1);
        assert_eq!(transcript.stats.recoveries_tried, 0);
    }

    #[test]
    fn a_chunk_off_the_frame_grid_times_its_words_from_its_own_start() {
        // A window starting 500 samples into the recording's frame 10, with
        // "w3" on the window's own frame 5 and "w4" on its frame 9.
        let mut t = transcriber(PipelineConfig::default(), 0);
        let start = 10 * FRAME_SAMPLES + 500;
        let mut samples = vec![0.0f32; 4 * SAMPLE_RATE];
        for (frame, piece) in [(5, 3.0), (9, 4.0)] {
            let at = start + frame * FRAME_SAMPLES;
            samples[at..at + FRAME_SAMPLES].fill(piece / 1000.0);
        }
        let mut stats = DecodeStats::default();
        let tokens = t
            .decode_range(&samples, start..start + 2 * SAMPLE_RATE, &mut stats)
            .unwrap();
        // The frames stay on the recording's grid, for the merge.
        assert_eq!(
            tokens
                .iter()
                .map(|t| (t.frame, t.phase))
                .collect::<Vec<_>>(),
            [(15, 500), (19, 500)]
        );
        let words = TokenAggregator.words(&timed_pieces(&tokens, t.vocab()));
        assert_eq!(join_words(&words), "w3 w4");
        // One frame of emission delay before the window's frames 5 and 9:
        // 1.12 s and 1.44 s plus the 500 samples, not rounded down to the
        // recording's frames 14 and 18.
        for (word, frame) in words.iter().zip([4, 8]) {
            let exact = (start + frame * FRAME_SAMPLES) as f64 / SAMPLE_RATE as f64;
            assert!(
                (word.start - exact).abs() < 1e-9,
                "{} != {exact}",
                word.start
            );
        }
        assert!((words[0].start - 1.151_25).abs() < 1e-9);
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
        assert!(transcript.spans.len() >= 3, "{:?}", transcript.spans);
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

    /// 20 words over 36 s in chunks of about 6 s: the parallel fixture.
    fn twenty_words() -> (PipelineConfig, Vec<f32>) {
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
        (config, samples)
    }

    /// `count` fake backends, each changed by `setup`.
    fn backends(count: usize, setup: impl Fn(&mut FrameTokenBackend)) -> Vec<FrameTokenBackend> {
        (0..count)
            .map(|_| {
                let mut backend = FrameTokenBackend::new(&vocab(), 0);
                setup(&mut backend);
                backend
            })
            .collect()
    }

    #[test]
    fn chunks_decoded_on_several_workers_merge_as_on_one() {
        let (config, samples) = twenty_words();
        let mut one = transcriber(config.clone(), 0);
        let alone = one.transcribe(&samples, None).unwrap();
        let mut three = transcriber_over(config, backends(3, |_| {}));
        assert_eq!(three.worker_count(), 3);
        let parallel = three.transcribe(&samples, None).unwrap();
        assert!(alone.spans.len() >= 4, "{:?}", alone.spans);
        assert_eq!(parallel, alone);
    }

    #[test]
    fn a_failing_chunk_fails_the_transcription_with_the_earliest_error() {
        // Words 5 and 15 fail the chunks that hold them; the earlier
        // chunk's error comes back, on one worker and on three.
        let (config, samples) = twenty_words();
        for workers in [1, 3] {
            let mut t = transcriber_over(
                config.clone(),
                backends(workers, |b| b.fail_on = vec![15, 5]),
            );
            let error = t.transcribe(&samples, None).unwrap_err().to_string();
            assert!(error.contains("piece 5"), "{workers} workers: {error}");
        }
    }

    #[test]
    fn a_panicking_worker_stops_the_others_and_the_panic_comes_back() {
        // Word 1 lies in the first chunk only, which panics at once; the
        // other windows take 100 ms each, so a worker that finishes one
        // sees the stop before it takes another.
        let (config, samples) = twenty_words();
        let windows = Arc::new(AtomicUsize::new(0));
        let mut t = transcriber_over(
            config.clone(),
            backends(3, |b| {
                b.panic_on = Some(1);
                b.delay = std::time::Duration::from_millis(100);
                b.windows = Arc::clone(&windows);
            }),
        );
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            t.transcribe(&samples, None)
        }));
        let panic = outcome.expect_err("the worker's panic is re-raised");
        assert_eq!(
            panic.downcast_ref::<String>().map(String::as_str),
            Some("piece 1")
        );
        let chunks = transcriber(config, 0)
            .transcribe(&samples, None)
            .unwrap()
            .spans;
        let decoded = windows.load(Ordering::SeqCst);
        assert!(
            decoded <= 3 && decoded < chunks.len(),
            "{decoded} of {} chunks decoded after the panic",
            chunks.len()
        );
    }

    #[test]
    fn the_merge_runs_at_the_two_second_default_overlap() {
        // Word 5 is a 1.6 s run of one piece that starts 0.88 s before the
        // second chunk does. The fake emits a run once per window, at its
        // first frame there: the first chunk at the run's start, the
        // second at its own. Within the 1 s tolerance of a 2 s overlap the
        // merge matches the two; at 1.5 s (0.75 s) it keeps both.
        let config = PipelineConfig {
            chunker: ChunkerConfig {
                target_seconds: 8.0,
                search_seconds: 1.0,
                ..ChunkerConfig::default()
            },
            recovery: RecoveryConfig {
                min_speech_seconds: 100.0,
                ..RecoveryConfig::default()
            },
            ..PipelineConfig::default()
        };
        assert_eq!(config.chunker.overlap_seconds, 2.0);
        let mut samples = audio(13.0, 15, &[1]);
        overlay(&mut samples, &audio(13.0, 130, &[3]));
        fill_noise(&mut samples, SAMPLE_RATE / 2..33 * SAMPLE_RATE / 4);
        fill_noise(&mut samples, 35 * SAMPLE_RATE / 4..12 * SAMPLE_RATE);
        let mut t = transcriber(config.clone(), 0);
        let speech = t.vad.speech_regions(&samples).unwrap();
        let chunks = layout(&samples, &speech, &config.chunker);
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert_eq!(chunks[0].cut, Cut::Pause);
        let right_frame = chunks[1].range.start / FRAME_SAMPLES;
        overlay(&mut samples, &audio(13.0, right_frame - 11, &[5; 20]));
        let transcript = t.transcribe(&samples, None).unwrap();
        let ranges: Vec<Range<usize>> = chunks.iter().map(|c| c.range.clone()).collect();
        assert_eq!(windows(&transcript, true), ranges);
        assert_eq!(transcript.text(), "w1 w5 w3");
        let mut stats = DecodeStats::default();
        let windows: Vec<Vec<Token>> = ranges
            .into_iter()
            .map(|range| t.decode_range(&samples, range, &mut stats).unwrap())
            .collect();
        assert_eq!(
            t.render(&merge_all(&windows, 1.5, t.vocab())),
            "w1 w5 w5 w3"
        );
    }

    /// Quiet words (pieces 1 to 3, under the detector's 0.01) in the
    /// lead-in, in a 13 s pause and in the tail, around loud words (11 to
    /// 16), and the quiet words of `more`, `(piece, seconds)`; the chunks
    /// skip the pause as a long one.
    fn quiet_stretches(
        decode_gaps: bool,
        more: &[(u32, f32)],
    ) -> (Transcriber<FrameTokenBackend>, Vec<f32>) {
        let config = PipelineConfig {
            chunker: ChunkerConfig {
                decode_gaps,
                ..ChunkerConfig::default()
            },
            ..PipelineConfig::default()
        };
        let mut samples = vec![0.0f32; 36 * SAMPLE_RATE];
        for (piece, at) in [
            (1, 0.5),
            (11, 3.0),
            (12, 5.0),
            (13, 7.0),
            (2, 14.0),
            (14, 21.0),
            (15, 23.0),
            (16, 25.0),
            (3, 33.0),
        ]
        .into_iter()
        .chain(more.iter().copied())
        {
            let frame = sample_count(at) / FRAME_SAMPLES;
            overlay(&mut samples, &audio(36.0, frame, &[piece; 6]));
        }
        (quiet_transcriber(config, 0), samples)
    }

    /// A transcriber whose detector takes pieces 1 to 10 (up to 0.01) for
    /// silence, over a fake with `min_frames` that decodes a short window
    /// to silence only when it holds the loud sound (0.05, piece 50).
    fn quiet_transcriber(
        config: PipelineConfig,
        min_frames: usize,
    ) -> Transcriber<FrameTokenBackend> {
        let mut t = transcriber(config, min_frames);
        t.workers[0].silent_needs = Some(50);
        t.vad = Box::new(EnergyVad {
            rms_threshold: 0.01,
            config: VadConfig {
                pad_seconds: 0.0,
                min_speech_seconds: 0.05,
                ..VadConfig::default()
            },
        });
        t
    }

    /// The defaults with the extra windows on, and the clamp at `max_seconds`.
    fn gaps_on(max_seconds: f32) -> PipelineConfig {
        PipelineConfig {
            chunker: ChunkerConfig {
                max_seconds,
                decode_gaps: true,
                ..ChunkerConfig::default()
            },
            ..PipelineConfig::default()
        }
    }

    /// The windows of the chunks (`true`) or of the extra windows.
    fn windows(transcript: &Transcript, chunks: bool) -> Vec<Range<usize>> {
        let spans = transcript.spans.iter().filter(|s| s.chunk == chunks);
        spans.map(|s| s.window.clone()).collect()
    }

    /// `frames` frames of `piece` from `seconds` on.
    fn put(samples: &mut [f32], piece: u32, seconds: f32, frames: usize) {
        let total = samples.len() as f32 / SAMPLE_RATE as f32;
        let frame = sample_count(seconds) / FRAME_SAMPLES;
        overlay(samples, &audio(total, frame, &vec![piece; frames]));
    }

    /// One chunk of loud sound the fake decodes to nothing (0.05 is past
    /// the blank), from 2 to 10 s, with word 11 at 4 s and quiet word 2 at
    /// 11 s. Windows under 130 frames decode to silence, so the 8.5 s
    /// chunk looks empty and its 6 s extension is accepted. The recovery
    /// keeps word 2, which lies in the chunk's reach, the overlap past the
    /// chunk; with the extra windows the tail's window keeps it too, and
    /// its copy falls away.
    fn recovered_last_chunk(seconds: usize) -> Vec<f32> {
        let mut samples = vec![0.0f32; seconds * SAMPLE_RATE];
        samples[2 * SAMPLE_RATE..10 * SAMPLE_RATE].fill(0.05);
        put(&mut samples, 11, 4.0, 6);
        put(&mut samples, 2, 11.0, 6);
        samples
    }

    #[test]
    fn a_recovered_chunk_reaching_into_the_tail_keeps_the_tails_later_words() {
        // Quiet word 3 lies 5 s past word 2, in the tail window only.
        let mut samples = recovered_last_chunk(30);
        put(&mut samples, 3, 16.0, 6);
        let chunked = quiet_transcriber(PipelineConfig::default(), 130)
            .transcribe(&samples, None)
            .unwrap();
        assert_eq!(chunked.stats.recoveries_accepted, 1);
        // The 6 s extension reaches to 16.25 s and hears word 3 too, past
        // the chunk's reach, so the recovery keeps only word 2, as before
        // the partition.
        assert_eq!(chunked.text(), "w11 w2");
        let t = quiet_transcriber(gaps_on(60.0), 130).transcribe(&samples, None);
        let transcript = t.unwrap();
        assert_eq!(windows(&transcript, true), windows(&chunked, true));
        assert_eq!(
            windows(&transcript, false).len(),
            1,
            "{:?}",
            transcript.spans
        );
        assert_eq!(transcript.text(), "w11 w2 w3");
    }

    #[test]
    fn a_recovered_chunk_next_to_a_split_gap_keeps_every_windows_words() {
        // The 39.75 s tail splits into three windows of the 15 s clamp,
        // each holding one quiet word past the recovery's.
        let mut samples = recovered_last_chunk(50);
        for (piece, seconds) in [(3, 20.0), (4, 32.0), (5, 44.0)] {
            put(&mut samples, piece, seconds, 6);
        }
        let mut t = quiet_transcriber(gaps_on(15.0), 130);
        let transcript = t.transcribe(&samples, None).unwrap();
        assert_eq!(transcript.stats.recoveries_accepted, 1);
        assert_eq!(
            windows(&transcript, true).len(),
            1,
            "{:?}",
            transcript.spans
        );
        assert_eq!(
            windows(&transcript, false).len(),
            3,
            "{:?}",
            transcript.spans
        );
        assert_eq!(transcript.text(), "w11 w2 w3 w4 w5");
    }

    #[test]
    fn recoveries_reaching_into_a_gap_from_both_sides_keep_the_words_between() {
        // Chunk A (1.75 to 10.25 s) looks empty, and its 6 s extension
        // after it hears quiet words 2 and 3 at 11 and 15 s. Chunk B, the
        // last (25.75 s to the end), has no audio after it, so its 6 s
        // extension either side is accepted and hears quiet words 5 and 4
        // at 20 and 24.5 s. Each recovery keeps the words in its chunk's
        // reach, the overlap either side, so words 2 and 4 and not 3 and 5,
        // as before the partition. With the extra windows the gap's window
        // hears all four, and its copies of words 2 and 4 fall away at the
        // seams.
        let mut samples = recovered_last_chunk(34);
        samples[26 * SAMPLE_RATE..].fill(0.05);
        for (piece, seconds) in [(3, 15.0), (5, 20.0), (4, 24.5), (12, 30.0)] {
            put(&mut samples, piece, seconds, 6);
        }
        let chunked = quiet_transcriber(PipelineConfig::default(), 130)
            .transcribe(&samples, None)
            .unwrap();
        assert_eq!(chunked.stats.recoveries_accepted, 2);
        assert_eq!(chunked.text(), "w11 w2 w4 w12");
        let transcript = quiet_transcriber(gaps_on(60.0), 130)
            .transcribe(&samples, None)
            .unwrap();
        assert_eq!(transcript.stats.recoveries_accepted, 2);
        assert_eq!(windows(&transcript, true), windows(&chunked, true));
        assert_eq!(
            windows(&transcript, false).len(),
            1,
            "{:?}",
            transcript.spans
        );
        assert_eq!(transcript.text(), "w11 w2 w3 w5 w4 w12");
    }

    #[test]
    fn a_word_where_a_gaps_window_meets_a_recovered_chunk_comes_out() {
        // Loud sound the fake decodes to nothing over 8 s, with word 11 in
        // it, and a gap's window beside it: after it, so the chunk's 6 s
        // extension after it is accepted, or before it, in the mirror, at
        // the recording's end, so the extension before it is. Quiet word 3,
        // three frames long, from five frames inside the chunk to four
        // frames into the gap, on and off the frame grid, over the exact
        // fake and the jittering one. The recovery keeps the word and the
        // gap's window may too: it comes out once, or twice where the two
        // copies start more than two frames apart, which the exact window
        // gives when it hears the end of a word that starts before it as a
        // word of its own, and the jitter when it moves the two windows'
        // words apart. It never goes missing.
        let mut twice = 0;
        for mirror in [false, true] {
            for jitter in [false, true] {
                let (total, loud) = if mirror {
                    (30.0, 21.75..29.75)
                } else {
                    (30.0, 2.0..10.0)
                };
                // Faint noise, so the gap's window is not digital silence.
                let mut samples: Vec<f32> = (0..sample_count(total))
                    .map(|i| if i % 2 == 0 { 3e-5 } else { -3e-5 })
                    .collect();
                samples[sample_count(loud.start)..sample_count(loud.end)].fill(0.05);
                put(&mut samples, 11, loud.start + 2.0, 6);
                let transcriber = || {
                    let mut t = quiet_transcriber(gaps_on(60.0), 130);
                    t.workers[0].jitter = usize::from(jitter);
                    t
                };
                let plain = transcriber().transcribe(&samples, None).unwrap();
                let chunk = plain.spans.iter().position(|s| s.chunk).unwrap();
                let gap = if mirror { chunk - 1 } else { chunk + 1 };
                assert!(!plain.spans[gap].chunk, "{:?}", plain.spans);
                let seam = plain.spans[gap]
                    .owned
                    .start
                    .max(plain.spans[chunk].owned.start);
                for offset in -5..=4isize {
                    for phase in [0, FRAME_SAMPLES / 2] {
                        let mut samples = samples.clone();
                        let at = (seam / FRAME_SAMPLES).checked_add_signed(offset).unwrap();
                        let from = at * FRAME_SAMPLES + phase;
                        samples[from..from + 3 * FRAME_SAMPLES].fill(0.003);
                        let transcript = transcriber().transcribe(&samples, None).unwrap();
                        assert_eq!(transcript.stats.recoveries_accepted, 1);
                        let copies: Vec<usize> = transcript
                            .tokens
                            .iter()
                            .filter(|t| t.id == 3)
                            .map(Token::sample)
                            .collect();
                        let case = format!("mirror {mirror} jitter {jitter} {offset} {phase}");
                        assert!(
                            copies.len() == 1
                                || (copies.len() == 2 && copies[1] - copies[0] > SEAM_SAMPLES),
                            "{case}: {copies:?} {}",
                            transcript.text()
                        );
                        twice += copies.len() - 1;
                    }
                }
            }
        }
        // 10 of the 80 with this layout.
        assert!(twice > 0 && twice < 20, "{twice}");
    }

    /// A token of `id` on the frame at `seconds`.
    fn piece(id: u32, seconds: f32) -> Token {
        Token {
            id,
            frame: sample_count(seconds) / FRAME_SAMPLES,
            phase: 0,
            confidence: 1.0,
            duration: 1,
        }
    }

    fn ids(tokens: &[Token]) -> Vec<u32> {
        tokens.iter().map(|t| t.id).collect()
    }

    /// The tokens as `(id, frame)`.
    fn timed(tokens: &[Token]) -> Vec<(u32, usize)> {
        tokens.iter().map(|t| (t.id, t.frame)).collect()
    }

    /// The span owning `owned` seconds and decoding `window`, which is
    /// also its reach (the join reads no reach).
    fn span(owned: (f32, f32), window: (f32, f32), chunk: bool) -> Span {
        let samples = |(a, b): (f32, f32)| sample_count(a)..sample_count(b);
        let (owned, window) = (samples(owned), samples(window));
        Span {
            owned,
            reach: window.clone(),
            window,
            chunk,
        }
    }

    #[test]
    fn a_decode_keeps_the_words_with_a_piece_in_its_span() {
        let vocab = vocab();
        let owned = sample_count(10.0)..sample_count(20.0);
        // "s21" continues a word heard before and "w1" lies before; "w4"
        // starts at the start; "w2 s22" starts inside and is kept whole
        // past the end; "w3" starts at the end and is not.
        let tokens = [
            piece(21, 9.92),
            piece(1, 9.6),
            piece(4, 10.0),
            piece(2, 19.9),
            piece(22, 20.2),
            piece(3, 20.0),
        ];
        assert_eq!(ids(&own(&tokens, &owned, &vocab)), [4, 2, 22]);
        // A word that starts before the span and goes on inside it is kept
        // whole.
        let across = [piece(1, 9.84), piece(21, 10.0), piece(4, 12.0)];
        assert_eq!(ids(&own(&across, &owned, &vocab)), [1, 21, 4]);
        // So is one with a full stop before the span: the word starts at
        // its first piece, not at the full stop.
        let across = [piece(1, 9.84), piece(41, 9.92), piece(21, 10.0)];
        assert_eq!(ids(&own(&across, &owned, &vocab)), [1, 41, 21]);
        // A full stop after the word's last piece in the span is cut, with
        // the rest of the word after it, as the trim before the partition
        // cut; inside the span it stays.
        let stop = [piece(2, 19.9), piece(41, 20.0), piece(22, 20.08)];
        assert_eq!(ids(&own(&stop, &owned, &vocab)), [2]);
        let stop = [piece(2, 19.84), piece(41, 19.92), piece(22, 20.0)];
        assert_eq!(ids(&own(&stop, &owned, &vocab)), [2, 41, 22]);
        // A decode that begins with a word's rest gives it as a word of its
        // own, kept when a piece of it lies in the span.
        let rest = [piece(21, 9.92), piece(31, 10.0), piece(5, 11.0)];
        assert_eq!(ids(&own(&rest, &owned, &vocab)), [21, 31, 5]);
        let later = sample_count(10.08)..sample_count(20.0);
        assert_eq!(ids(&own(&rest, &later, &vocab)), [5]);
        assert!(own(&[], &owned, &vocab).is_empty());
    }

    #[test]
    fn a_word_kept_either_side_of_a_seam_between_runs_comes_out_once() {
        // A chunk to 10 s, the gap's window from 10 to 20 s, a chunk after.
        let vocab = vocab();
        let spans = [
            span((0.0, 10.0), (0.0, 10.0), true),
            span((10.0, 20.0), (10.0, 20.0), false),
            span((20.0, 30.0), (20.0, 30.0), true),
        ];
        let joined = |chunk: &[Token], window: &[Token], next: &[Token]| {
            let decoded = [chunk.to_vec(), window.to_vec(), next.to_vec()];
            join(&spans, &decoded, 2.0, &vocab)
        };
        let at = |seconds: f32| sample_count(seconds) / FRAME_SAMPLES;
        let next = [piece(14, 21.0)];
        // The same piece within two frames of the chunk's last word, with
        // its rest, is that word heard again, and the window's falls away.
        let chunk = [piece(13, 9.84), piece(23, 9.92)];
        let again = [piece(13, 10.0), piece(23, 10.08), piece(2, 14.0)];
        assert_eq!(
            timed(&joined(&chunk, &again, &next)),
            [
                (13, at(9.84)),
                (23, at(9.92)),
                (2, at(14.0)),
                (14, at(21.0))
            ]
        );
        // The copy with more pieces heard more of the word and stays.
        let cut = [piece(13, 9.84)];
        assert_eq!(
            timed(&joined(&cut, &again, &next)),
            [
                (13, at(10.0)),
                (23, at(10.08)),
                (2, at(14.0)),
                (14, at(21.0))
            ]
        );
        // Three frames apart, or 0.4 s, it is a word said again; another
        // piece is another word.
        for apart in [0.24, 0.4] {
            let chunk = [piece(5, 10.0 - apart)];
            let said = [piece(5, 10.0), piece(2, 14.0)];
            assert_eq!(
                ids(&joined(&chunk, &said, &next)),
                [5, 5, 2, 14],
                "{apart} s apart"
            );
        }
        let chunk = [piece(6, 9.92)];
        let said = [piece(5, 10.0), piece(2, 14.0)];
        assert_eq!(ids(&joined(&chunk, &said, &next)), [6, 5, 2, 14]);
        // A word the window heard across its end and the chunk again at
        // its start: the window's copy falls away, the chunk's whole word
        // stays, and three frames apart both do.
        let late = [piece(2, 14.0), piece(7, 19.92)];
        let next = [piece(7, 20.0), piece(27, 20.08), piece(14, 21.0)];
        assert_eq!(
            timed(&joined(&chunk, &late, &next)),
            [
                (6, at(9.92)),
                (2, at(14.0)),
                (7, at(20.0)),
                (27, at(20.08)),
                (14, at(21.0))
            ]
        );
        let late = [piece(2, 14.0), piece(7, 19.76)];
        assert_eq!(ids(&joined(&chunk, &late, &next)), [6, 2, 7, 7, 27, 14]);
        // Two copies of one piece each: the window's falls away; and one
        // of two pieces against three, with every piece of it.
        let late = [piece(2, 14.0), piece(7, 19.92)];
        let alone = [piece(7, 20.0), piece(14, 21.0)];
        assert_eq!(
            timed(&joined(&chunk, &late, &alone)),
            [(6, at(9.92)), (2, at(14.0)), (7, at(20.0)), (14, at(21.0))]
        );
        let late = [piece(2, 14.0), piece(7, 19.84), piece(27, 19.92)];
        let longer = [piece(7, 20.0), piece(27, 20.08), piece(37, 20.16)];
        assert_eq!(ids(&joined(&chunk, &late, &longer)), [6, 2, 7, 27, 37]);
        // The known limit: a chunk that begins with the rest of a word the
        // window heard whole gives that piece twice.
        let whole = [piece(2, 14.0), piece(7, 19.84), piece(27, 19.92)];
        let rest = [piece(27, 20.0), piece(14, 21.0)];
        assert_eq!(ids(&joined(&chunk, &whole, &rest)), [6, 2, 7, 27, 27, 14]);
    }

    #[test]
    fn a_later_copy_three_frames_before_the_earlier_is_a_word_said_again() {
        // A recovered chunk to 10 s keeps word 5 at 10.24 s, past its
        // owned span, and the gap's window after it hears word 5 at 10 s:
        // three frames apart, whichever comes first, so both stay. Two
        // frames apart, the window's copy falls away.
        let vocab = vocab();
        let spans = [
            span((0.0, 10.0), (0.0, 10.0), true),
            span((10.0, 20.0), (10.0, 20.0), false),
        ];
        for (chunk, kept) in [(10.24, vec![5, 5, 2]), (10.16, vec![5, 2])] {
            let decoded = [vec![piece(5, chunk)], vec![piece(5, 10.0), piece(2, 14.0)]];
            let tokens = join(&spans, &decoded, 2.0, &vocab);
            assert_eq!(ids(&tokens), kept, "{chunk}");
        }
    }

    #[test]
    fn a_window_that_begins_inside_a_word_keeps_the_rest_only_when_no_chunk_kept_a_token_before_it()
    {
        // A chunk to 10 s, the gap's window from 10 to 20 s, a chunk after.
        // The window begins with the rest of a word: the chunk before heard
        // its start, and the rest falls away, unless the chunk kept
        // nothing.
        let vocab = vocab();
        let spans = [
            span((0.0, 10.0), (0.0, 10.0), true),
            span((10.0, 20.0), (10.0, 20.0), false),
            span((20.0, 30.0), (20.0, 30.0), true),
        ];
        let joined = |chunk: &[Token], window: &[Token]| {
            let decoded = [chunk.to_vec(), window.to_vec(), vec![piece(14, 21.0)]];
            ids(&join(&spans, &decoded, 2.0, &vocab))
        };
        let rest = [piece(25, 10.0), piece(2, 14.0)];
        assert_eq!(joined(&[piece(6, 9.92)], &rest), [6, 2, 14]);
        assert_eq!(joined(&[], &rest), [25, 2, 14]);
        // Only a chunk's token counts: the first window kept one, no chunk
        // did.
        let spans = [
            span((0.0, 10.0), (0.0, 10.0), false),
            span((10.0, 20.0), (10.0, 20.0), true),
            span((20.0, 30.0), (20.0, 30.0), false),
        ];
        let decoded = [
            vec![piece(2, 5.0)],
            Vec::new(),
            vec![piece(25, 20.0), piece(3, 24.0)],
        ];
        assert_eq!(ids(&join(&spans, &decoded, 2.0, &vocab)), [2, 25, 3]);
        // Any chunk before counts, not only the one right before: the
        // first chunk kept a token, the chunk before the second window
        // kept nothing, and the rest still falls away.
        let spans = [
            span((0.0, 5.0), (0.0, 5.0), true),
            span((5.0, 10.0), (5.0, 10.0), false),
            span((10.0, 20.0), (10.0, 20.0), true),
            span((20.0, 30.0), (20.0, 30.0), false),
        ];
        let decoded = [
            vec![piece(6, 2.0)],
            vec![piece(2, 7.0)],
            Vec::new(),
            vec![piece(25, 20.0), piece(3, 24.0)],
        ];
        assert_eq!(ids(&join(&spans, &decoded, 2.0, &vocab)), [6, 2, 3]);
        // Only a token that starts before the window counts, the earliest
        // any chunk kept: one at the window's start does not, nor one a
        // recovery kept inside the window, though an earlier chunk's does.
        let spans = [
            span((0.0, 10.0), (0.0, 10.0), true),
            span((10.0, 20.0), (10.0, 20.0), false),
        ];
        for at in [10.0, 10.5] {
            let decoded = [vec![piece(6, at)], vec![piece(25, 10.0), piece(2, 14.0)]];
            let kept = ids(&join(&spans, &decoded, 2.0, &vocab));
            assert_eq!(kept, [6, 25, 2], "{at}");
        }
        let spans = [
            span((0.0, 5.0), (0.0, 5.0), true),
            span((5.0, 10.0), (5.0, 10.0), false),
            span((10.0, 20.0), (10.0, 20.0), true),
            span((20.0, 30.0), (20.0, 30.0), false),
        ];
        let decoded = [
            vec![piece(6, 2.0)],
            vec![piece(2, 7.0)],
            vec![piece(7, 20.5)],
            vec![piece(25, 20.0), piece(3, 24.0)],
        ];
        assert_eq!(ids(&join(&spans, &decoded, 2.0, &vocab)), [6, 2, 7, 3]);
        // A chunk after the window counts too: the chunk before it kept
        // nothing, and a recovery after it kept a word from its start,
        // before the window.
        let spans = [
            span((0.0, 10.0), (0.0, 10.0), false),
            span((10.0, 20.0), (10.0, 20.0), true),
            span((20.0, 30.0), (20.0, 30.0), false),
            span((30.0, 40.0), (16.0, 40.0), true),
        ];
        let decoded = [
            vec![piece(2, 5.0)],
            Vec::new(),
            vec![piece(25, 20.0), piece(3, 24.0)],
            vec![piece(5, 19.0), piece(4, 34.0)],
        ];
        assert_eq!(ids(&join(&spans, &decoded, 2.0, &vocab)), [2, 3, 5, 4]);
    }

    #[test]
    fn chunks_between_two_windows_merge_with_each_other_as_before() {
        // Two chunks sharing 2 s hear word 4 in their overlap; the LCS
        // merge keeps it once, and the window's words follow.
        let vocab = vocab();
        let spans = [
            span((0.0, 9.0), (0.0, 10.0), true),
            span((9.0, 18.0), (8.0, 18.0), true),
            span((18.0, 30.0), (18.0, 30.0), false),
        ];
        let decoded = [
            vec![piece(11, 2.0), piece(4, 8.8)],
            vec![piece(4, 8.8), piece(12, 15.0)],
            vec![piece(2, 25.0)],
        ];
        assert_eq!(ids(&join(&spans, &decoded, 2.0, &vocab)), [11, 4, 12, 2]);
        let chunks = &decoded[..2];
        assert_eq!(
            join(&spans[..2], chunks, 2.0, &vocab),
            merge_all(chunks, 2.0, &vocab)
        );
    }

    #[test]
    fn a_punctuation_piece_from_a_later_recovery_loses_no_window_word() {
        // The loss #263 left to A16: a gap from 10 to 20 s, recoveries of
        // both neighbours reaching 2 s into it, the later one's kept
        // tokens starting with a full stop at 18 s, and a word only the
        // window heard, w2 at 14 s. The window keeps its whole decode, and
        // w2 stays.
        // The earlier recovery's copy of w13 is the window's first word and
        // the window's falls away; the later recovery's ". w5" and the
        // window's both come out, a duplicate.
        let vocab = vocab();
        let spans = [
            span((0.0, 10.0), (0.0, 10.0), true),
            span((10.0, 20.0), (10.0, 20.0), false),
            span((20.0, 30.0), (20.0, 30.0), true),
        ];
        let earlier = [piece(11, 4.0), piece(13, 10.4)];
        let later = [piece(41, 18.0), piece(5, 18.4), piece(12, 22.0)];
        let reach = |(a, b): (f32, f32)| sample_count(a)..sample_count(b);
        let earlier = own(&earlier, &reach((0.0, 12.0)), &vocab);
        let later = own(&later, &reach((18.0, 30.0)), &vocab);
        let window = vec![
            piece(13, 10.4),
            piece(2, 14.0),
            piece(41, 18.0),
            piece(5, 18.4),
        ];
        let decoded = [earlier, window, later];
        assert_eq!(
            ids(&join(&spans, &decoded, 2.0, &vocab)),
            [11, 13, 2, 41, 5, 41, 5, 12]
        );
    }

    /// A detector that finds speech in fixed `regions`.
    struct FixedVad(Vec<Range<usize>>);

    impl VoiceActivityDetector for FixedVad {
        fn speech_regions(&mut self, _: &[f32]) -> Result<Vec<Range<usize>>, SpeechError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn words_said_again_across_a_gaps_edge_come_out_end_to_end() {
        // Speech at 2.5 to 7.5 s and at 21 to 26 s, so the second chunk
        // starts at 20.75 s. It hears "w5 w6" (or "w5") said at 21.04 s,
        // and only the gap's window hears it said 0.88 s (0.8 s) before,
        // under the default settings and `CoreML`'s.
        let regions = [2.5..7.5, 21.0..26.0].map(|r| sample_count(r.start)..sample_count(r.end));
        for (phrase, apart) in [(&[5, 6][..], 0.88), (&[5][..], 0.8)] {
            let mut samples = vec![0.0f32; 30 * SAMPLE_RATE];
            let mut words = vec![(11, 3.0), (14, 24.0)];
            for (i, &id) in phrase.iter().enumerate() {
                let at = 21.04 + 0.32 * i as f32;
                words.extend([(id, at), (id, at - apart)]);
            }
            for (id, at) in words {
                put(&mut samples, id, at, 3);
            }
            let said = phrase.iter().map(|id| format!("w{id}")).collect::<Vec<_>>();
            let said = said.join(" ");
            for case in 0..2 {
                let transcribe = |gaps| {
                    let mut t = transcriber(random_config(case, gaps), 0);
                    t.vad = Box::new(FixedVad(regions.to_vec()));
                    t.transcribe(&samples, None).unwrap()
                };
                assert_eq!(transcribe(false).text(), format!("w11 {said} w14"));
                assert_eq!(transcribe(true).text(), format!("w11 {said} {said} w14"));
            }
        }
    }

    #[test]
    fn dither_with_one_quiet_word_in_an_otherwise_zero_window_is_decoded() {
        // Loud words around a 13 s gap of exact zeros, which is skipped.
        let mut samples = vec![0.0f32; 36 * SAMPLE_RATE];
        for (piece, seconds) in [
            (11, 3.0),
            (12, 5.0),
            (13, 7.0),
            (14, 21.0),
            (15, 23.0),
            (16, 25.0),
        ] {
            put(&mut samples, piece, seconds, 6);
        }
        let mut t = quiet_transcriber(gaps_on(60.0), 0);
        let zeros = t.transcribe(&samples, None).unwrap();
        assert_eq!(zeros.text(), "w11 w12 w13 w14 w15 w16");
        let chunks = windows(&zeros, true);
        let gap = chunks[0].end..chunks[1].start;
        let inside = |w: &Range<usize>| w.start >= gap.start && w.end <= gap.end;
        assert!(
            !windows(&zeros, false).iter().any(inside),
            "{:?}",
            zeros.spans
        );
        // TPDF dither at about -90 dBFS (3.16e-5 RMS, peaks under 1e-4):
        // the window is decoded and adds no word, and a quiet word in it
        // (piece 1, 1e-3) comes out.
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut uniform = || rng.below(1 << 20) as f32 / (1 << 20) as f32;
        let mut dither = samples.clone();
        for x in &mut dither[gap.clone()] {
            *x = (uniform() + uniform() - 1.0) * 3.16e-5 * 6.0f32.sqrt();
        }
        let dithered = t.transcribe(&dither, None).unwrap();
        assert!(
            windows(&dithered, false).iter().any(inside),
            "{:?}",
            dithered.spans
        );
        assert_eq!(dithered.text(), zeros.text());
        put(&mut dither, 1, 14.0, 6);
        let word = t.transcribe(&dither, None).unwrap();
        assert_eq!(word.text(), "w11 w12 w13 w1 w14 w15 w16");
    }

    #[test]
    fn extra_windows_decode_the_lead_in_the_skipped_pauses_and_the_tail() {
        let (mut t, samples) = quiet_stretches(false, &[]);
        let skipped = t.transcribe(&samples, None).unwrap();
        assert_eq!(skipped.text(), "w11 w12 w13 w14 w15 w16");
        assert!(windows(&skipped, false).is_empty());
        let (mut t, samples) = quiet_stretches(true, &[]);
        let transcript = t.transcribe(&samples, None).unwrap();
        assert_eq!(windows(&transcript, true), windows(&skipped, true));
        assert_eq!(transcript.text(), "w1 w11 w12 w13 w2 w14 w15 w16 w3");
        let extra = windows(&transcript, false);
        assert_eq!(extra.len(), 3, "{:?}", transcript.spans);
        assert_eq!(
            transcript.stats.windows,
            skipped.stats.windows + extra.len()
        );
        assert!(
            transcript
                .words
                .windows(2)
                .all(|w| w[0].start <= w[1].start)
        );
        // Silence in the extra windows adds nothing.
        let mut silent = samples;
        for (at, seconds) in [(0.3, 1.0), (13.8, 1.0), (32.8, 1.0)] {
            silent[sample_count(at)..sample_count(at + seconds)].fill(0.0);
        }
        let (mut t, _) = quiet_stretches(true, &[]);
        assert_eq!(t.transcribe(&silent, None).unwrap().text(), skipped.text());
    }

    #[test]
    fn an_extra_window_never_replaces_a_chunks_word() {
        // Quiet word 4 runs across the first chunk's end at 7.67 s and word
        // 5 across the second chunk's start at 20.77 s. The chunk and the
        // window each hear the word, the window at its own first frame or
        // the chunk at its. The chunk's stays. The window's word 4 starts
        // two frames from the chunk's and falls away; its word 5 starts
        // 0.22 s before the chunk's, so both copies stay.
        let more = [(4, 7.52), (5, 20.56)];
        let (mut t, samples) = quiet_stretches(false, &more);
        let chunked = t.transcribe(&samples, None).unwrap();
        assert_eq!(chunked.text(), "w11 w12 w13 w4 w5 w14 w15 w16");
        let (mut t, samples) = quiet_stretches(true, &more);
        let transcript = t.transcribe(&samples, None).unwrap();
        assert_eq!(windows(&transcript, true), windows(&chunked, true));
        assert_eq!(
            transcript.text(),
            "w1 w11 w12 w13 w4 w2 w5 w5 w14 w15 w16 w3"
        );
        // Every chunk token is there, unchanged and in order.
        let mut rest = transcript.tokens.iter();
        assert!(
            chunked.tokens.iter().all(|token| rest.any(|t| t == token)),
            "{:?}",
            transcript.tokens
        );
        // The windows heard the two words too.
        let mut stats = DecodeStats::default();
        for (gap, piece) in [(1, 4), (1, 5)] {
            let window = windows(&transcript, false)[gap].clone();
            let tokens = t.decode_range(&samples, window, &mut stats).unwrap();
            assert!(tokens.iter().any(|t| t.id == piece), "{tokens:?}");
        }
    }

    /// A word placed in a test recording: its pieces from `frame` on,
    /// `frames` frames each.
    struct Placed {
        pieces: Vec<u32>,
        frame: usize,
        frames: usize,
    }

    impl Placed {
        /// The sample where piece `i` starts, or the word ends.
        fn start(&self, i: usize) -> usize {
            (self.frame + i * self.frames) * FRAME_SAMPLES
        }

        fn span(&self) -> Range<usize> {
            self.start(0)..self.start(self.pieces.len())
        }

        /// Which of the word's pieces `token` is: the same piece, timed
        /// within `slack` frames of its own.
        fn piece_of(&self, token: &Token, slack: usize) -> Option<usize> {
            let slack = slack * FRAME_SAMPLES;
            (0..self.pieces.len()).find(|&i| {
                let around = self.start(i).saturating_sub(slack)..self.start(i + 1) + slack;
                self.pieces[i] == token.id && around.contains(&token.sample())
            })
        }

        fn put(&self, samples: &mut [f32]) {
            let piece_samples = self.frames * FRAME_SAMPLES;
            for (n, x) in samples[self.span()].iter_mut().enumerate() {
                *x = self.pieces[n / piece_samples] as f32 / 1000.0;
            }
        }
    }

    /// Words of one to three pieces at random over 20 to 80 s, loud (11 to
    /// 20) and quiet (1 to 10, which the detector takes for silence), a few
    /// frames apart and now and then after a pause of up to 15 s, over
    /// exact zeros or faint noise.
    fn placed_words(rng: &mut Rng, case: usize) -> (Vec<f32>, Vec<Placed>) {
        let total = (20 + rng.below(100)) * SAMPLE_RATE;
        let mut samples = vec![0.0f32; total];
        if case.is_multiple_of(2) {
            for (i, x) in samples.iter_mut().enumerate() {
                *x = if i % 2 == 0 { 3e-5 } else { -3e-5 };
            }
        }
        let mut words = Vec::new();
        let mut frame = rng.below(3 * SAMPLE_RATE) / FRAME_SAMPLES;
        let (mut id, mut quiet) = (0, false);
        loop {
            id = id % 10 + 1;
            // Runs of quiet words make gaps that hold words.
            quiet ^= rng.below(6) == 0;
            let base = id + if quiet { 0 } else { 10 };
            let pieces =
                [base, 20 + base, 30 + base][..=rng.below(if base > 10 { 2 } else { 3 })].to_vec();
            let word = Placed {
                frames: 2 + rng.below(6),
                frame,
                pieces,
            };
            if word.span().end + FRAME_SAMPLES >= total {
                return (samples, words);
            }
            word.put(&mut samples);
            frame = word.span().end / FRAME_SAMPLES + 4 + rng.below(6);
            if rng.below(5) == 0 {
                frame += rng.below(35 * SAMPLE_RATE) / FRAME_SAMPLES;
            }
            words.push(word);
        }
    }

    /// The default settings, `CoreML`'s and two between.
    fn random_config(case: usize, decode_gaps: bool) -> PipelineConfig {
        let (target_seconds, search_seconds, long_pause_seconds, max_seconds) = [
            (25.0, 4.0, 3.0, 60.0),
            (25.0, 4.0, f32::INFINITY, 15.0),
            (25.0, 4.0, 3.0, 15.0),
            (8.0, 1.0, f32::INFINITY, 60.0),
        ][case % 4];
        PipelineConfig {
            chunker: ChunkerConfig {
                target_seconds,
                search_seconds,
                long_pause_seconds,
                max_seconds,
                decode_gaps,
                ..ChunkerConfig::default()
            },
            ..PipelineConfig::default()
        }
    }

    /// The ownership oracle: random words under four settings, with the
    /// extra windows off and on, over the exact fake and over one whose
    /// windows hear every token a frame early, on time or a frame late,
    /// by the window's length. Every word clear of the partition's edges
    /// (the owned boundaries and the windows' ends) by two frames, and by
    /// the jitter, comes out exactly once with every piece if a window
    /// decoded it and not at all if none did. A word a window heard near an
    /// edge comes out once, or twice where it sounds across the end of a
    /// gap's window into a chunk (the known limit). No token comes out
    /// that no word holds.
    #[test]
    fn every_word_away_from_a_spans_edge_comes_out_once() {
        let mut rng = Rng(0x51_7cc1_b727_220a);
        let (mut far, mut near) = (0, [0usize; 2]);
        for case in 0..320 {
            let (samples, words) = placed_words(&mut rng, case);
            let jitter = case % 3 == 1;
            let mut config = random_config(case, (case / 4) % 2 == 1);
            // No recovery: its wider windows would hear what the spans'
            // windows did not.
            config.recovery.min_speech_seconds = f32::INFINITY;
            let mut t = quiet_transcriber(config, 0);
            t.workers[0].jitter = usize::from(jitter);
            let transcript = t.transcribe(&samples, None).unwrap();
            let slack = 2 + usize::from(jitter);
            let edges: Vec<usize> = transcript
                .spans
                .iter()
                .flat_map(|s| [s.owned.start, s.window.start, s.window.end])
                .filter(|&x| x > 0 && x < samples.len())
                .collect();
            let gap_ends: Vec<usize> = transcript
                .spans
                .windows(2)
                .filter(|p| !p[0].chunk && p[1].chunk && p[0].window.end == p[1].window.start)
                .map(|p| p[1].window.start)
                .collect();
            let context = || {
                let tokens = transcript
                    .tokens
                    .iter()
                    .map(|t| format!("{}@{}", t.id, t.sample()));
                format!(
                    "case {case}: {:?}\n{}",
                    transcript.spans,
                    tokens.collect::<Vec<_>>().join(" ")
                )
            };
            let mut strays = 0;
            let mut per = vec![(0, 0, 0u8); words.len()];
            for token in &transcript.tokens {
                let hit = words
                    .iter()
                    .enumerate()
                    .find_map(|(w, word)| word.piece_of(token, slack).map(|i| (w, i)));
                match hit {
                    Some((w, i)) => {
                        per[w].0 += usize::from(i == 0);
                        per[w].1 += 1;
                        per[w].2 |= 1 << i;
                    }
                    None => strays += 1,
                }
            }
            assert_eq!(strays, 0, "a token of no word; {}", context());
            for (word, &(firsts, count, pieces)) in words.iter().zip(&per) {
                let margin = slack * FRAME_SAMPLES;
                let around = word.start(0).saturating_sub(margin)..word.span().end + margin;
                let heard = transcript
                    .spans
                    .iter()
                    .any(|s| s.window.start < word.span().end && word.start(0) < s.window.end);
                if heard && edges.iter().any(|x| around.contains(x)) {
                    // Twice only where a gap's window ends at a chunk's
                    // start: the two exact windows each hear the word
                    // that sounds across the edge, and the window's copy
                    // starts first.
                    let gap_end = gap_ends.iter().any(|x| around.contains(x));
                    assert!(
                        firsts == 1 || (firsts == 2 && gap_end),
                        "word {:?} at {} came out {firsts} times; {}",
                        word.pieces,
                        word.start(0),
                        context()
                    );
                    near[firsts - 1] += 1;
                    continue;
                }
                far += 1;
                let decoded = transcript
                    .spans
                    .iter()
                    .any(|s| s.window.contains(&word.start(0)));
                let all = (1u8 << word.pieces.len()) - 1;
                let expected = if decoded {
                    (1, word.pieces.len(), all)
                } else {
                    (0, 0, 0)
                };
                assert_eq!(
                    (firsts, count, pieces),
                    expected,
                    "word {:?} at {}; {}",
                    word.pieces,
                    word.start(0),
                    context()
                );
            }
        }
        // Words checked away from an edge (4,683 with this seed), and near
        // one by how often they came out (649 once, 59 twice).
        assert!(
            far > 4_000 && near[0] > 500 && near[1] > 20,
            "{far} {near:?}"
        );
    }

    /// How often quiet word 3 came out.
    fn threes(transcript: &Transcript) -> usize {
        transcript.tokens.iter().filter(|t| t.id == 3).count()
    }

    #[test]
    fn a_word_where_two_windows_of_a_gap_meet_comes_out_once() {
        // Speech at 1 to 3 s and 40 to 42 s under the `CoreML` settings:
        // the gap after the first chunk is decoded in two windows that
        // share 2 s, and their owned spans meet at 25.75 s. The windows time
        // a word on their own frame grids, up to half a frame apart, and
        // merge as a run, so a quiet word up to three frames either side of
        // the seam, on and off the frame grid, comes out once. The same in
        // the mirror, the recording played backwards, and over the
        // jittering fake, which moves both windows' words alike here: a
        // gap's windows are of one length.
        let total = 45 * SAMPLE_RATE;
        for mirror in [false, true] {
            let flip = |r: Range<usize>| {
                if mirror {
                    total - r.end..total - r.start
                } else {
                    r
                }
            };
            let mut regions: Vec<Range<usize>> = [1.0..3.0, 40.0..42.0]
                .into_iter()
                .map(|r| flip(sample_count(r.start)..sample_count(r.end)))
                .collect();
            regions.sort_by_key(|r| r.start);
            // Faint noise, so no window is digital silence.
            let mut samples: Vec<f32> = (0..total)
                .map(|i| if i % 2 == 0 { 3e-5 } else { -3e-5 })
                .collect();
            for (piece, region) in [11, 12].into_iter().zip(&regions) {
                let at = region.start + SAMPLE_RATE / 2;
                samples[at..at + 6 * FRAME_SAMPLES].fill(piece as f32 / 1000.0);
            }
            for jitter in [false, true] {
                let transcriber = || {
                    let mut t = quiet_transcriber(random_config(1, true), 0);
                    t.vad = Box::new(FixedVad(regions.clone()));
                    t.workers[0].jitter = usize::from(jitter);
                    t
                };
                let spans = transcriber().transcribe(&samples, None).unwrap().spans;
                let gaps: Vec<&Span> = spans
                    .iter()
                    .filter(|s| !s.chunk && s.window.len() > 10 * SAMPLE_RATE)
                    .collect();
                assert_eq!(gaps.len(), 2, "{spans:?}");
                assert!(gaps[0].window.end > gaps[1].window.start, "{spans:?}");
                let seam = gaps[1].owned.start;
                if !mirror {
                    assert_eq!(seam, sample_count(25.75));
                }
                for offset in -3..=3 {
                    for phase in [0, FRAME_SAMPLES / 2] {
                        let mut samples = samples.clone();
                        let at = (seam / FRAME_SAMPLES).checked_add_signed(offset).unwrap();
                        let from = at * FRAME_SAMPLES + phase;
                        samples[from..from + 4 * FRAME_SAMPLES].fill(0.003);
                        let transcript = transcriber().transcribe(&samples, None).unwrap();
                        assert_eq!(transcript.spans, spans);
                        let text = transcript.text();
                        let case = format!("mirror {mirror} jitter {jitter} {offset} {phase}");
                        assert_eq!(threes(&transcript), 1, "{case}: {text}");
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_chunk_with_speech_is_retried_with_a_wider_window() {
        // The word sits at 4 s inside the one 9.25 s chunk (116 frames); the
        // fake encodes windows under 130 frames to silence, so only the
        // 6 s extension after the chunk decodes it.
        let (config, samples) = empty_chunk_fixture(50);
        let mut t = transcriber(config, 130);
        let transcript = t.transcribe(&samples, None).unwrap();
        assert_eq!(
            windows(&transcript, true).len(),
            1,
            "{:?}",
            transcript.spans
        );
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
        assert_eq!(
            windows(&transcript, true).len(),
            2,
            "{:?}",
            transcript.spans
        );
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

    /// A chunk from 1 to 9 s that owns the recording up to 9.75 s and
    /// reaches 1.5 s past its end.
    fn chunk_span() -> Span {
        Span {
            reach: 0..sample_count(10.5),
            ..span((0.0, 9.75), (1.0, 9.0), true)
        }
    }

    /// Recovers the 1 to 9 s chunk of the empty-chunk fixture, with a
    /// second word at 10.8 s, under `extensions`; windows under 160 frames
    /// (12.8 s) decode to silence.
    fn recover_with(extensions: &[(f32, f32)]) -> (Vec<Token>, Transcriber<FrameTokenBackend>) {
        let (mut config, mut samples) = empty_chunk_fixture(50);
        config.recovery.extensions_seconds = extensions.to_vec();
        overlay(&mut samples, &audio(14.0, 135, &[6]));
        let mut t = transcriber(config, 160);
        let tokens = t
            .recover(
                &samples,
                &chunk_span(),
                Vec::new(),
                &mut DecodeStats::default(),
            )
            .unwrap();
        (tokens, t)
    }

    #[test]
    fn an_accepted_recovery_keeps_only_the_words_in_its_reach() {
        // The 1 to 15 s window decodes both words; the one at 10.8 s lies
        // past the chunk's reach and belongs to the next chunk.
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
    fn a_recovery_must_hear_more_in_the_chunks_window_than_the_chunk_did() {
        // The chunk (1 to 9 s) owns up to 8 s and heard one word, at
        // 8.48 s: in its window, past its owned span. The 6 s extension
        // hears that word and no other, so it is not accepted.
        let (mut config, samples) = empty_chunk_fixture(106);
        config.recovery.extensions_seconds = vec![(0.0, 6.0)];
        let mut t = transcriber(config, 160);
        let mut stats = DecodeStats::default();
        let chunk = span((0.0, 8.0), (1.0, 9.0), true);
        let original = vec![piece(5, 8.48)];
        let kept = t.recover(&samples, &chunk, original.clone(), &mut stats);
        assert_eq!(kept.unwrap(), original);
        assert_eq!((stats.recoveries_tried, stats.recoveries_accepted), (1, 0));
    }

    #[test]
    fn a_recovery_counts_a_word_on_its_chunks_last_frame() {
        // Word 5 from 9 s, the end of the chunk (1 to 9 s), whose last
        // frame starts at 8.96 s. The 6 s extension starts on the chunk's
        // grid, half a frame off the recording's, and hears the word on
        // its frame from 9 s: the chunk's last frame, which counts.
        let mut samples = vec![0.0f32; 14 * SAMPLE_RATE];
        samples[9 * SAMPLE_RATE..9 * SAMPLE_RATE + 2 * FRAME_SAMPLES].fill(0.005);
        let config = PipelineConfig {
            recovery: RecoveryConfig {
                extensions_seconds: vec![(0.0, 6.0)],
                ..RecoveryConfig::default()
            },
            ..PipelineConfig::default()
        };
        let mut t = transcriber(config, 160);
        let mut stats = DecodeStats::default();
        let kept = t.recover(&samples, &chunk_span(), Vec::new(), &mut stats);
        let kept = kept.unwrap();
        assert_eq!(stats.recoveries_accepted, 1);
        assert_eq!(
            kept.iter().map(|t| (t.id, t.sample())).collect::<Vec<_>>(),
            [(5, 9 * SAMPLE_RATE)]
        );
        // A chunk that ends on the grid, at 8.96 s: the frame from its end
        // counts too, as before the partition.
        let mut stats = DecodeStats::default();
        let chunk = Span {
            window: 13 * FRAME_SAMPLES..112 * FRAME_SAMPLES,
            ..chunk_span()
        };
        let mut samples = vec![0.0f32; 14 * SAMPLE_RATE];
        let end = chunk.window.end;
        samples[end..end + 2 * FRAME_SAMPLES].fill(0.005);
        let kept = t.recover(&samples, &chunk, Vec::new(), &mut stats);
        assert_eq!(stats.recoveries_accepted, 1);
        assert_eq!(
            kept.unwrap()
                .iter()
                .map(|t| (t.id, t.sample()))
                .collect::<Vec<_>>(),
            [(5, end)]
        );
    }

    #[test]
    fn a_recovery_counts_a_word_on_its_chunks_first_frame() {
        // Word 5 from 7 s, the start of the chunk (7 to 15 s), whose first
        // frame starts at 6.96 s. The 6 s extension before it starts half a
        // frame off the recording's grid and hears the word on its frame
        // from 7 s: the chunk's first frame, which counts.
        let mut samples = vec![0.0f32; 15 * SAMPLE_RATE];
        samples[7 * SAMPLE_RATE..7 * SAMPLE_RATE + 2 * FRAME_SAMPLES].fill(0.005);
        let config = PipelineConfig {
            recovery: RecoveryConfig {
                extensions_seconds: vec![(6.0, 0.0)],
                ..RecoveryConfig::default()
            },
            ..PipelineConfig::default()
        };
        let mut t = transcriber(config, 160);
        let mut stats = DecodeStats::default();
        let chunk = Span {
            reach: sample_count(4.96)..15 * SAMPLE_RATE,
            ..span((6.25, 15.0), (7.0, 15.0), true)
        };
        let kept = t.recover(&samples, &chunk, Vec::new(), &mut stats);
        let kept = kept.unwrap();
        assert_eq!(stats.recoveries_accepted, 1);
        assert_eq!(
            kept.iter().map(|t| (t.id, t.sample())).collect::<Vec<_>>(),
            [(5, 7 * SAMPLE_RATE)]
        );
    }

    #[test]
    fn a_recovery_keeps_a_word_whole_past_its_reach() {
        // The chunk reaches to 10.6 s and "w7 s27" starts at 10.56 s. The
        // recovery keeps the whole word, so the merge with the next window,
        // which heard all of it, keeps it whole.
        let (mut config, mut samples) = empty_chunk_fixture(50);
        config.recovery.extensions_seconds = vec![(0.0, 6.0)];
        samples.resize(18 * SAMPLE_RATE, 0.0);
        overlay(&mut samples, &audio(18.0, 132, &[7, 27]));
        let mut t = transcriber(config, 110);
        let mut stats = DecodeStats::default();
        let chunk = Span {
            reach: 0..sample_count(10.6),
            ..span((0.0, 10.6), (1.0, 9.0), true)
        };
        let left = t.recover(&samples, &chunk, Vec::new(), &mut stats).unwrap();
        assert_eq!(stats.recoveries_accepted, 1);
        assert_eq!(t.render(&left), "w5s25 w7s27");
        let next = 15 * SAMPLE_RATE / 2..35 * SAMPLE_RATE / 2;
        let right = t.decode_range(&samples, next, &mut stats).unwrap();
        assert_eq!(t.render(&right), "w7s27");
        let merged = merge_all(&[left, right], 1.5, t.vocab());
        assert_eq!(t.render(&merged), "w5s25 w7s27");
    }

    #[test]
    fn silence_gives_an_empty_transcript() {
        let mut t = transcriber(PipelineConfig::default(), 0);
        let transcript = t.transcribe(&vec![0.0; 3 * SAMPLE_RATE], None).unwrap();
        assert!(transcript.segments.is_empty());
        assert!(transcript.spans.is_empty());
        assert_eq!(t.render(&[]), "");
    }

    /// A xorshift64 generator, so the random cases are the same every run.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            (x % n as u64) as usize
        }
    }

    /// Windows at random starts, on the grid, inside the first frame and
    /// anywhere: every token starts at its exact sample time, never before
    /// 0, and on the grid with the bits it had before tokens had a phase.
    #[test]
    fn a_window_at_any_sample_times_every_token_at_its_exact_sample() {
        let mut t = transcriber(PipelineConfig::default(), 0);
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let (mut checked, mut frame0_clamped, mut on_grid) = (0, 0, 0);
        for case in 0..3000 {
            let start = match case % 4 {
                0 => rng.below(FRAME_SAMPLES),
                1 => rng.below(200) * FRAME_SAMPLES,
                _ => rng.below(10 * SAMPLE_RATE),
            };
            let frames = 2 + rng.below(30);
            let len = frames * FRAME_SAMPLES + rng.below(FRAME_SAMPLES);
            let mut samples = vec![0.0f32; start + len];
            let mut expected = Vec::new();
            let (mut k, mut id) = (rng.below(3), 1u32);
            while k < frames {
                let at = start + k * FRAME_SAMPLES;
                samples[at..at + FRAME_SAMPLES].fill(id as f32 / 1000.0);
                expected.push((id, k));
                id = id % 20 + 1;
                k += 2 + rng.below(3);
            }
            let mut stats = DecodeStats::default();
            let tokens = t
                .decode_range(&samples, start..start + len, &mut stats)
                .unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .map(|t| (t.id, t.frame - start / FRAME_SAMPLES))
                    .collect::<Vec<_>>(),
                expected,
                "case {case} start {start}"
            );
            let pieces = timed_pieces(&tokens, t.vocab());
            for ((piece, token), &(_, k)) in pieces.iter().zip(&tokens).zip(&expected) {
                let exact = (start as i64 + (k as i64 - 1) * FRAME_SAMPLES as i64) as f64
                    / SAMPLE_RATE as f64;
                assert!(piece.start >= 0.0, "case {case}: {}", piece.start);
                assert!(
                    (piece.start - exact.max(0.0)).abs() < 1e-9,
                    "case {case} start {start} k {k}: {} != {exact}",
                    piece.start
                );
                assert_eq!(token.sample(), start + k * FRAME_SAMPLES);
                if exact < 0.0 {
                    frame0_clamped += 1;
                }
                if start % FRAME_SAMPLES == 0 {
                    on_grid += 1;
                    let old = token.frame.saturating_sub(1) as f64 * crate::backend::FRAME_SECONDS;
                    assert_eq!(piece.start.to_bits(), old.to_bits(), "case {case}");
                }
                checked += 1;
            }
        }
        assert!(
            checked > 10_000 && frame0_clamped > 50 && on_grid > 2_000,
            "{checked} {frame0_clamped} {on_grid}"
        );
    }

    mod differential;
}
