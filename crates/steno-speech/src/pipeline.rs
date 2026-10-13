//! The pipeline above the tensors, in order: VAD regions, the pause-aligned
//! layout, one decode per chunk with the empty-decode recovery (and, when
//! [`ChunkerConfig::decode_gaps`] is on, one per extra window over the audio
//! no chunk covers), the LCS merge of the chunks, the extra windows put in
//! between them (`splice_gaps`), pieces to words to segments, and the
//! language tag. A chunk is a range the layout chose; a window is the range
//! actually decoded: the chunk itself or, after a recovery, the chunk with
//! more audio around it, or an extra window.
//! Generic over [`SpeechBackend`]: the ONNX and the `CoreML` engines run
//! it, and a fake backend drives it in tests.
//! Swift: `Sources/StenoSpeech/Engines/ParakeetEngine.swift` and
//! `ParakeetMapping.swift`, with `FluidAudio`'s `ChunkProcessor` in between.

use std::iter;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use steno_core::{LanguageTag, RawSegment, TimedWord};

use crate::backend::{FRAME_SAMPLES, SAMPLE_RATE, SpeechBackend, sample_count};
use crate::chunker::{Chunk, ChunkerConfig, gap_windows, layout};
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
    /// The extra windows over the gaps ([`gap_windows`]); empty when
    /// [`ChunkerConfig::decode_gaps`] is off.
    pub gap_windows: Vec<Range<usize>>,
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
        let chunks = layout(samples, &speech, &self.config.chunker);
        let gap_windows = gap_windows(samples, &chunks, &self.config.chunker);
        let mut stats = DecodeStats::default();
        let jobs = jobs(&chunks, &gap_windows);
        let mut decoded = self.decode_windows(samples, &jobs, &speech, &mut stats)?;
        let overlap = self.config.chunker.overlap_seconds;
        let extra = decoded.split_off(chunks.len());
        let tokens = splice_gaps(
            &merge_all(&decoded, f64::from(overlap), &self.vocab),
            &gap_windows,
            &extra,
            overlap,
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
            gap_windows,
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
        range: &Range<usize>,
        original: Vec<Token>,
        stats: &mut DecodeStats,
    ) -> Result<Vec<Token>, SpeechError> {
        self.chunk_decoder(0)
            .recover(samples, range, original, stats)
    }

    /// Every job's tokens, in job order: on the one backend in turn, or on
    /// every backend at once, each taking the next job as it finishes one.
    /// After an error or a panic no worker starts another job, and the
    /// error of the earliest job is returned (a panic is re-raised).
    fn decode_windows(
        &mut self,
        samples: &[f32],
        jobs: &[Job],
        speech: &[Range<usize>],
        stats: &mut DecodeStats,
    ) -> Result<Vec<Vec<Token>>, SpeechError> {
        if self.workers.len() == 1 || jobs.len() <= 1 {
            let mut decoder = self.chunk_decoder(0);
            return jobs
                .iter()
                .map(|job| decoder.decode_job(samples, job, speech, stats))
                .collect();
        }
        let (vocab, config) = (&self.vocab, &self.config);
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let mut slots: Vec<Option<Result<Vec<Token>, SpeechError>>> =
            jobs.iter().map(|_| None).collect();
        std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .workers
                .iter_mut()
                .take(jobs.len())
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
                            let Some(job) = jobs.get(index) else {
                                break;
                            };
                            let tokens = decoder.decode_job(samples, job, speech, &mut stats);
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
        // Workers take jobs in order and finish every job they take, so
        // the filled slots come first, and without an error every slot is
        // filled; the collect stops at the earliest error.
        debug_assert!(
            slots
                .iter()
                .skip_while(|slot| slot.is_some())
                .all(Option::is_none),
            "a decoded job follows one that was not decoded"
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

/// One decode: a chunk of the layout, retried with more audio when it
/// looks empty, or an extra window over audio no chunk covers, decoded as
/// it is (the detector found no speech there to judge it by).
struct Job {
    range: Range<usize>,
    recover: bool,
}

/// The chunks in order, then the extra windows in order.
fn jobs(chunks: &[Chunk], gap_windows: &[Range<usize>]) -> Vec<Job> {
    let chunks = chunks.iter().map(|chunk| Job {
        range: chunk.range.clone(),
        recover: true,
    });
    let gaps = gap_windows.iter().map(|range| Job {
        range: range.clone(),
        recover: false,
    });
    chunks.chain(gaps).collect()
}

/// Puts the tokens of the extra windows (`windows`, decoded as `decoded`)
/// into the merged chunk tokens by time; the chunk tokens are never changed
/// or dropped. The windows of one gap merge with each other first. Chunk
/// tokens inside the gap come from an accepted recovery, which keeps its
/// chunk's overlap and so can reach into the gap from the chunk before it
/// or the one after it, or from both. They split into two sides where the
/// earlier chunk's recovery stops keeping words, at the first word that
/// starts on a frame past `overlap_seconds` after the gap's start
/// ([`keep_chunk_and_overlap`]), and each side's tokens cover the gap from
/// the first of them to the last: the window tokens before the first side,
/// between the two and after the second go in, and those inside a side's
/// cover are dropped. Where window tokens meet chunk tokens, [`Seam::fit`]
/// drops the window's copy of the chunk's adjacent word; every such check
/// is against chunk tokens, never against another window's.
fn splice_gaps(
    tokens: &[Token],
    windows: &[Range<usize>],
    decoded: &[Vec<Token>],
    overlap_seconds: f32,
    vocab: &Vocab,
) -> Vec<Token> {
    let seam = Seam { vocab };
    // Window tokens to put in before `tokens[at]`, in order.
    let mut inserts: Vec<(usize, Vec<Token>)> = Vec::new();
    let mut first = 0;
    while first < windows.len() {
        // The windows of one gap overlap each other; those of two gaps
        // never do.
        let mut end = first + 1;
        while end < windows.len() && windows[end].start < windows[end - 1].end {
            end += 1;
        }
        let gap = merge_all(&decoded[first..end], f64::from(overlap_seconds), vocab);
        let span = windows[first].start..windows[end - 1].end;
        let at = tokens.partition_point(|t| t.sample() < span.start);
        let past = tokens.partition_point(|t| t.sample() < span.end);
        let kept_to = (span.start + sample_count(overlap_seconds)) / FRAME_SAMPLES;
        let later = tokens[at..past]
            .iter()
            .position(|t| t.frame > kept_to && seam.starts_word(t));
        let mid = later.map_or(past, |n| at + n);
        // Before each side that holds tokens, and after the last, go the
        // window tokens past the previous side's cover.
        let sides = [at..mid, mid..past]
            .into_iter()
            .filter(|side| !side.is_empty());
        let mut covered_to = None;
        for side in sides.chain(iter::once(past..past)) {
            let uncovered = gap
                .iter()
                .filter(|t| covered_to.is_none_or(|to| t.sample() > to));
            inserts.push((
                side.start,
                seam.fit(uncovered.copied().collect(), tokens, side.start),
            ));
            covered_to = tokens[side].last().map(Token::sample);
        }
        first = end;
    }
    let mut out =
        Vec::with_capacity(tokens.len() + inserts.iter().map(|(_, w)| w.len()).sum::<usize>());
    let mut done = 0;
    for (at, window_tokens) in inserts {
        out.extend_from_slice(&tokens[done..at]);
        out.extend(window_tokens);
        done = at;
    }
    out.extend_from_slice(&tokens[done..]);
    out
}

/// Where window tokens meet the chunk tokens: a window word is the chunk's
/// adjacent word heard again only when it is the same piece and starts
/// within two encoder frames of it. A word said again starts later than
/// that. A copy timed further off stays in, since the seam cannot tell it
/// from a word said again, and a repeated word is the lesser error than a
/// lost one.
struct Seam<'a> {
    vocab: &'a Vocab,
}

impl Seam<'_> {
    fn starts_word(&self, token: &Token) -> bool {
        starts_word(self.vocab.piece(token.id))
    }

    fn repeats(earlier: &Token, later: &Token) -> bool {
        earlier.id == later.id && later.sample().abs_diff(earlier.sample()) <= 2 * FRAME_SAMPLES
    }

    /// The window tokens `part` that go in between `tokens[at - 1]` and
    /// `tokens[at]`. Dropped, and nothing else: a token at or after
    /// `tokens[at]`; when the chunk starts inside a word, the window's last
    /// piece if it is the chunk's first; otherwise the window's last word
    /// if it repeats the chunk's next word; the rest of a word at the
    /// window's start, which belongs to the chunk's last word; and the
    /// window's first word if it repeats the chunk's last word.
    fn fit(&self, mut part: Vec<Token>, tokens: &[Token], at: usize) -> Vec<Token> {
        if let Some(next) = tokens.get(at) {
            part.retain(|t| t.sample() < next.sample());
            let last = if self.starts_word(next) {
                part.iter()
                    .rposition(|t| self.starts_word(t))
                    .filter(|&i| Self::repeats(&part[i], next))
            } else {
                part.len().checked_sub(1).filter(|&i| part[i].id == next.id)
            };
            if let Some(last) = last {
                part.truncate(last);
            }
        }
        if at > 0 {
            let rest = part.iter().take_while(|t| !self.starts_word(t)).count();
            part.drain(..rest);
            let chunk_last = tokens[..at].iter().rev().find(|t| self.starts_word(t));
            if let (Some(first), Some(last)) = (part.first(), chunk_last)
                && Self::repeats(last, first)
            {
                let word = 1 + part[1..]
                    .iter()
                    .take_while(|t| !self.starts_word(t))
                    .count();
                part.drain(..word);
            }
        }
        part
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
    fn decode_job(
        &mut self,
        samples: &[f32],
        job: &Job,
        speech: &[Range<usize>],
        stats: &mut DecodeStats,
    ) -> Result<Vec<Token>, SpeechError> {
        if job.recover {
            self.decode_chunk(samples, &job.range, speech, stats)
        } else {
            self.decode_range(samples, job.range.clone(), stats)
        }
    }

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
            let data = if features.frames < self.min_frames && self.silent_needs.is_none_or(holds) {
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
    use std::sync::Arc;

    use super::fake::{FrameTokenBackend, audio};
    use super::*;
    use crate::chunker::Cut;
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
        assert_eq!(transcript.chunks.len(), 1);
        assert_eq!(transcript.text(), "w1s21s22 w2");
        assert_eq!(transcript.segments.len(), 2, "{:?}", transcript.segments);
        // Frame 10 starts at 0.72 s after the one-frame emission delay. The
        // chunk's frames start where the chunk does, so the fake's word lands
        // on the chunk frame nearest the recording's: within half a frame.
        assert!(
            (transcript.words[0].start - 0.72).abs() <= 0.04 + 1e-9,
            "{} from a chunk at {:?}",
            transcript.words[0].start,
            transcript.chunks[0].range
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
        assert!(alone.chunks.len() >= 4, "{:?}", alone.chunks);
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
            .chunks;
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
        assert_eq!(transcript.chunks, chunks);
        assert_eq!(transcript.text(), "w1 w5 w3");
        let mut stats = DecodeStats::default();
        let windows: Vec<Vec<Token>> = chunks
            .iter()
            .map(|c| {
                t.decode_range(&samples, c.range.clone(), &mut stats)
                    .unwrap()
            })
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

    /// `frames` frames of `piece` from `seconds` on.
    fn put(samples: &mut [f32], piece: u32, seconds: f32, frames: usize) {
        let total = samples.len() as f32 / SAMPLE_RATE as f32;
        let frame = sample_count(seconds) / FRAME_SAMPLES;
        overlay(samples, &audio(total, frame, &vec![piece; frames]));
    }

    /// One chunk of loud sound the fake decodes to nothing (0.05 is past
    /// the blank), from 2 to 10 s, with word 11 at 4 s and quiet word 2 at
    /// 11 s. Windows under 130 frames decode to silence, so the 8.5 s
    /// chunk looks empty and its 6 s extension is accepted; the recovery
    /// keeps word 2, which lies in the tail past the chunk, inside its
    /// overlap.
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
        assert_eq!(chunked.text(), "w11 w2");
        let t = quiet_transcriber(gaps_on(60.0), 130).transcribe(&samples, None);
        let transcript = t.unwrap();
        assert_eq!(transcript.chunks, chunked.chunks);
        assert_eq!(
            transcript.gap_windows.len(),
            1,
            "{:?}",
            transcript.gap_windows
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
        assert_eq!(transcript.chunks.len(), 1, "{:?}", transcript.chunks);
        assert_eq!(
            transcript.gap_windows.len(),
            3,
            "{:?}",
            transcript.gap_windows
        );
        assert_eq!(transcript.text(), "w11 w2 w3 w4 w5");
    }

    #[test]
    fn recoveries_reaching_into_a_gap_from_both_sides_keep_the_words_between() {
        // Chunk A (1.75 to 10.25 s) looks empty, and its 6 s extension
        // after it keeps quiet word 2 at 11 s. Chunk B, the last (25.75 s
        // to the end), has no audio after it, so its 6 s extension either
        // side is accepted and keeps quiet word 4 at 24.5 s. Quiet words 3
        // and 5 at 15 and 20 s lie in the gap between them, where only the
        // window hears them.
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
        assert_eq!(transcript.chunks, chunked.chunks);
        assert_eq!(
            transcript.gap_windows.len(),
            1,
            "{:?}",
            transcript.gap_windows
        );
        assert_eq!(transcript.text(), "w11 w2 w3 w5 w4 w12");
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

    /// `splice_gaps` at the 2 s overlap (a 1 s tolerance), as ids.
    fn spliced(chunk: &[Token], windows: &[(f32, f32)], decoded: &[&[Token]]) -> Vec<u32> {
        let windows: Vec<Range<usize>> = windows
            .iter()
            .map(|&(a, b)| sample_count(a)..sample_count(b))
            .collect();
        let decoded: Vec<Vec<Token>> = decoded.iter().map(|d| d.to_vec()).collect();
        splice_gaps(chunk, &windows, &decoded, 2.0, &vocab())
            .iter()
            .map(|t| t.id)
            .collect()
    }

    #[test]
    fn a_window_token_falls_away_only_where_it_repeats_a_chunk_token() {
        let gap = [(10.0, 20.0)];
        // The same piece within two frames of the chunk's word on either
        // side is that word; 0.4 s away it is a word said again.
        let chunk = [piece(13, 9.84), piece(14, 20.0)];
        let repeats = [piece(13, 10.0), piece(2, 14.0), piece(14, 19.84)];
        assert_eq!(spliced(&chunk, &gap, &[&repeats]), [13, 2, 14]);
        let chunk = [piece(5, 9.6), piece(6, 20.0)];
        let apart = [piece(5, 10.0), piece(2, 14.0), piece(6, 19.6)];
        assert_eq!(spliced(&chunk, &gap, &[&apart]), [5, 5, 2, 6, 6]);
        // A word's rest at the window's start is the chunk's last word's.
        let chunk = [piece(4, 9.6), piece(14, 20.8)];
        let rest = [piece(24, 10.0), piece(2, 14.0)];
        assert_eq!(spliced(&chunk, &gap, &[&rest]), [4, 2, 14]);
        // Nothing before the first chunk token: the window keeps it all.
        let lead_in = [piece(24, 1.0), piece(2, 5.0)];
        assert_eq!(spliced(&chunk, &[(0.0, 9.0)], &[&lead_in]), [24, 2, 4, 14]);
    }

    #[test]
    fn a_two_piece_word_heard_on_both_sides_of_a_seam_comes_out_once() {
        let gap = [(7.75, 20.75)];
        // The chunk heard "w4 s24" late, the window again at its start.
        let chunk = [
            piece(13, 6.0),
            piece(4, 7.6),
            piece(24, 7.68),
            piece(14, 22.0),
        ];
        let early = [piece(4, 7.76), piece(24, 7.84), piece(2, 14.0)];
        assert_eq!(spliced(&chunk, &gap, &[&early]), [13, 4, 24, 2, 14]);
        // The window heard "w5 s25" at its end, the chunk at its start.
        let chunk = [
            piece(13, 6.0),
            piece(5, 20.8),
            piece(25, 20.88),
            piece(14, 22.0),
        ];
        let late = [piece(2, 14.0), piece(5, 20.64), piece(25, 20.72)];
        assert_eq!(spliced(&chunk, &gap, &[&late]), [13, 2, 5, 25, 14]);
        // A chunk that starts inside a word: its first piece, not a word
        // start, is what the window's last piece repeats, however timed.
        let chunk = [piece(13, 6.0), piece(25, 20.88), piece(14, 22.0)];
        let late = [piece(2, 14.0), piece(5, 20.4), piece(25, 20.56)];
        assert_eq!(spliced(&chunk, &gap, &[&late]), [13, 2, 5, 25, 14]);
    }

    #[test]
    fn words_a_window_hears_again_beside_a_recovery_come_out_once() {
        // A recovery kept "w13 w4" inside the gap after its chunk, and the
        // window heard the two again, the second a frame later.
        let gap = [(7.75, 20.75)];
        let (before, after) = ([piece(11, 4.0)], [piece(12, 22.0)]);
        let side = |words: &[Token]| [&before[..], words, &after[..]].concat();
        let kept = side(&[piece(13, 8.4), piece(4, 8.72)]);
        let again = [piece(13, 8.48), piece(4, 8.8), piece(2, 14.0)];
        assert_eq!(spliced(&kept, &gap, &[&again]), [11, 13, 4, 2, 12]);
        // A recovery before the next chunk kept "w5 w6", and the window
        // heard the two a frame early.
        let kept = side(&[piece(5, 19.6), piece(6, 19.92)]);
        let again = [piece(2, 14.0), piece(5, 19.52), piece(6, 19.84)];
        assert_eq!(spliced(&kept, &gap, &[&again]), [11, 2, 5, 6, 12]);
        // Further off than two frames, the copy stays in.
        let kept = side(&[piece(13, 8.4), piece(4, 8.72)]);
        let late = [piece(4, 9.04), piece(2, 14.0)];
        assert_eq!(spliced(&kept, &gap, &[&late]), [11, 13, 4, 4, 2, 12]);
    }

    #[test]
    fn words_said_again_across_a_gaps_edge_all_come_out() {
        // The chunks heard nothing in the gap, so a window word with a
        // chunk word's piece is a word said again unless the two start
        // within two frames; "A B | A B", "the | the" and "go go | go".
        let gap = [(7.76, 20.8)];
        let (first, last) = (piece(13, 6.0), piece(14, 22.4));
        let around = |words: &[Token]| [&[first][..], words, &[last][..]].concat();
        let chunk = around(&[piece(5, 20.8), piece(6, 21.12)]);
        let window = [piece(2, 14.0), piece(5, 20.0), piece(6, 20.32)];
        assert_eq!(spliced(&chunk, &gap, &[&window]), [13, 2, 5, 6, 5, 6, 14]);
        let chunk = around(&[piece(5, 20.8)]);
        let window = [piece(2, 14.0), piece(5, 20.16)];
        assert_eq!(spliced(&chunk, &gap, &[&window]), [13, 2, 5, 5, 14]);
        let chunk = around(&[piece(7, 20.8)]);
        let window = [piece(2, 14.0), piece(7, 20.0), piece(7, 20.4)];
        assert_eq!(spliced(&chunk, &gap, &[&window]), [13, 2, 7, 7, 7, 14]);
        let chunk = [piece(13, 6.96), piece(4, 7.28), last];
        let window = [piece(13, 7.76), piece(4, 8.08), piece(2, 14.0)];
        assert_eq!(spliced(&chunk, &gap, &[&window]), [13, 4, 13, 4, 2, 14]);
        let chunk = [first, piece(4, 7.2), last];
        let window = [piece(4, 7.84), piece(2, 14.0)];
        assert_eq!(spliced(&chunk, &gap, &[&window]), [13, 4, 4, 2, 14]);
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
    fn chunk_tokens_inside_a_gap_cover_it_from_each_sides_first_to_its_last() {
        // Recoveries of both neighbours reach into the gap: the window's
        // words between the two sides stay, without their repeats.
        let chunk = [
            piece(11, 4.0),
            piece(2, 11.0),
            piece(4, 28.0),
            piece(12, 31.0),
        ];
        let window = [
            piece(2, 11.04),
            piece(3, 15.0),
            piece(5, 20.0),
            piece(4, 28.04),
        ];
        assert_eq!(
            spliced(&chunk, &[(10.25, 29.75)], &[&window]),
            [11, 2, 3, 5, 4, 12]
        );
        // A recovery kept words 2 and 6 past its chunk, inside the gap the
        // window decodes: the window's words between them fall away, and
        // its words either side stay, without their repeats.
        let chunk = [piece(11, 4.0), piece(2, 11.0), piece(6, 12.0)];
        let window = [
            piece(3, 10.5),
            piece(2, 11.04),
            piece(7, 11.5),
            piece(6, 12.04),
            piece(8, 16.0),
        ];
        assert_eq!(
            spliced(&chunk, &[(10.25, 30.0)], &[&window]),
            [11, 3, 2, 6, 8]
        );
        // Two gaps (a window of digital silence left out between them)
        // are each checked against the chunk tokens, not each other: word
        // 4 at 10 s repeats the chunk's at 9.92 s, and word 3 at 10.56 s
        // stays beside the first gap's at 10.48 s.
        let chunk = [piece(4, 9.92), piece(14, 30.0)];
        let gaps = [(10.0, 10.5), (10.5, 20.0)];
        let first = [piece(4, 10.0), piece(3, 10.48)];
        let second = [piece(3, 10.56), piece(2, 14.0)];
        assert_eq!(spliced(&chunk, &gaps, &[&first, &second]), [4, 3, 3, 2, 14]);
    }

    #[test]
    fn the_sides_split_where_the_earlier_recovery_stops_keeping_words() {
        // The earlier chunk ends at 9.25 s and its recovery keeps the words
        // that start up to 2 s later. The later chunk's recovery kept "w8
        // s28 s38" from 11.52 s, before the gap's middle: the window's word
        // 9 at 11.36 s, between the two sides, stays.
        let gap = [(9.25, 14.39)];
        let chunk = [
            piece(11, 4.0),
            piece(2, 9.6),
            piece(8, 11.52),
            piece(28, 12.0),
            piece(38, 12.48),
            piece(12, 16.0),
        ];
        let window = [piece(2, 9.6), piece(9, 11.36), piece(8, 11.52)];
        assert_eq!(spliced(&chunk, &gap, &[&window]), [11, 2, 9, 8, 28, 38, 12]);
        // The earlier recovery kept "w2 s22 s32" whole across 11.25 s: the
        // word stays on its side, and the window's word 7 at 12.48 s, past
        // it, stays.
        let chunk = [
            piece(11, 4.0),
            piece(2, 10.96),
            piece(22, 11.44),
            piece(32, 11.92),
            piece(4, 13.6),
            piece(12, 16.0),
        ];
        let window = [piece(2, 10.96), piece(7, 12.48), piece(4, 13.6)];
        assert_eq!(spliced(&chunk, &gap, &[&window]), [11, 2, 22, 32, 7, 4, 12]);
        // A word on the very frame the recovery keeps to (11.2 s) is the
        // earlier side's, so the window's word 5 past it stays.
        let chunk = [
            piece(11, 4.0),
            piece(2, 9.6),
            piece(3, 11.2),
            piece(4, 13.6),
            piece(12, 16.0),
        ];
        let window = [piece(3, 11.2), piece(5, 12.4), piece(4, 13.6)];
        assert_eq!(spliced(&chunk, &gap, &[&window]), [11, 2, 3, 5, 4, 12]);
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
        let gap = zeros.chunks[0].range.end..zeros.chunks[1].range.start;
        let inside = |w: &Range<usize>| w.start >= gap.start && w.end <= gap.end;
        assert!(
            !zeros.gap_windows.iter().any(inside),
            "{:?}",
            zeros.gap_windows
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
            dithered.gap_windows.iter().any(inside),
            "{:?}",
            dithered.gap_windows
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
        assert!(skipped.gap_windows.is_empty());
        let (mut t, samples) = quiet_stretches(true, &[]);
        let transcript = t.transcribe(&samples, None).unwrap();
        assert_eq!(transcript.chunks, skipped.chunks);
        assert_eq!(transcript.text(), "w1 w11 w12 w13 w2 w14 w15 w16 w3");
        assert_eq!(
            transcript.gap_windows.len(),
            3,
            "{:?}",
            transcript.gap_windows
        );
        assert_eq!(
            transcript.stats.windows,
            skipped.stats.windows + transcript.gap_windows.len()
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
        // 0.22 s before the chunk's, where the seam cannot tell it from the
        // word said again, so it stays.
        let more = [(4, 7.52), (5, 20.56)];
        let (mut t, samples) = quiet_stretches(false, &more);
        let chunked = t.transcribe(&samples, None).unwrap();
        assert_eq!(chunked.text(), "w11 w12 w13 w4 w5 w14 w15 w16");
        let (mut t, samples) = quiet_stretches(true, &more);
        let transcript = t.transcribe(&samples, None).unwrap();
        assert_eq!(transcript.chunks, chunked.chunks);
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
            let window = transcript.gap_windows[gap].clone();
            let tokens = t.decode_range(&samples, window, &mut stats).unwrap();
            assert!(tokens.iter().any(|t| t.id == piece), "{tokens:?}");
        }
    }

    /// A word a layout placed: its pieces from `frame` on, `frames` frames
    /// each.
    struct Placed {
        pieces: Vec<u32>,
        frame: usize,
        frames: usize,
    }

    impl Placed {
        fn new(pieces: Vec<u32>, seconds: f32, frames: usize) -> Self {
            let frame = sample_count(seconds) / FRAME_SAMPLES;
            Placed {
                pieces,
                frame,
                frames,
            }
        }

        /// The sample where piece `i` starts, or the word ends.
        fn start(&self, i: usize) -> usize {
            (self.frame + i * self.frames) * FRAME_SAMPLES
        }

        fn span(&self) -> Range<usize> {
            self.start(0)..self.start(self.pieces.len())
        }

        /// Which of the word's pieces `token` is: the same piece, timed
        /// from a frame before it starts (a window off the recording's
        /// grid times it up to a frame early) to the next piece's start.
        fn piece_of(&self, token: &Token) -> Option<usize> {
            (0..self.pieces.len()).find(|&i| {
                let from = self.start(i).saturating_sub(FRAME_SAMPLES);
                self.pieces[i] == token.id && (from..self.start(i + 1)).contains(&token.sample())
            })
        }

        fn put(&self, samples: &mut [f32]) {
            let piece_samples = self.frames * FRAME_SAMPLES;
            for (n, x) in samples[self.span()].iter_mut().enumerate() {
                *x = self.pieces[n / piece_samples] as f32 / 1000.0;
            }
        }

        /// Whether the word would sit two frames or more from the others.
        fn clear_of(&self, others: &[Placed]) -> bool {
            let margin = 2 * FRAME_SAMPLES;
            others.iter().all(|o| {
                o.start(0) >= self.span().end + margin || self.start(0) >= o.span().end + margin
            })
        }
    }

    /// A property case: the audio, the words in it, and the speech regions
    /// when they are fixed rather than the detector's.
    struct Layout {
        samples: Vec<f32>,
        words: Vec<Placed>,
        regions: Option<Vec<Range<usize>>>,
    }

    /// A random layout of the 20 words, loud (11 to 20) and quiet (1 to
    /// 10), one or two pieces each, a quarter of them said twice in a row,
    /// some after loud sound the fake decodes to nothing, over exact zeros
    /// or faint noise.
    fn random_layout(rng: &mut Rng, case: usize) -> Layout {
        let total = (20 + rng.below(50)) * SAMPLE_RATE;
        let mut samples = vec![0.0f32; total];
        if case.is_multiple_of(2) {
            for (i, x) in samples.iter_mut().enumerate() {
                *x = if i % 2 == 0 { 3e-5 } else { -3e-5 };
            }
        }
        let mut ids: Vec<u32> = (1..=20).collect();
        for i in (1..ids.len()).rev() {
            ids.swap(i, rng.below(i + 1));
        }
        let mut words: Vec<Placed> = Vec::new();
        let mut frame = rng.below(4 * SAMPLE_RATE) / FRAME_SAMPLES;
        for id in ids {
            let frames = 3 + rng.below(6);
            let pieces = if case % 3 == 1 {
                vec![id, 20 + id]
            } else {
                vec![id]
            };
            for _ in 0..=usize::from(rng.below(4) == 0) {
                let word = Placed {
                    pieces: pieces.clone(),
                    frame,
                    frames,
                };
                if word.span().end + FRAME_SAMPLES >= total {
                    return Layout {
                        samples,
                        words,
                        regions: None,
                    };
                }
                if id > 10 && rng.below(3) == 0 {
                    let from = frame.saturating_sub(10 + rng.below(60)) * FRAME_SAMPLES;
                    let from = from.max(words.last().map_or(0, |w| w.span().end));
                    samples[from..word.start(0)].fill(0.05);
                }
                word.put(&mut samples);
                frame += pieces.len() * frames + 2 + rng.below(4);
                words.push(word);
            }
            let pause = if rng.below(3) == 0 { 12 } else { 3 };
            frame += rng.below(pause * SAMPLE_RATE) / FRAME_SAMPLES;
        }
        Layout {
            samples,
            words,
            regions: None,
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

    /// The words only the windows heard that a property run checked: all
    /// of them, those in a window holding chunk tokens, and those between
    /// chunk tokens inside their window.
    #[derive(Default)]
    struct Checked {
        words: usize,
        beside: usize,
        between: usize,
    }

    /// Checks `on` against the words the layout placed, told apart by
    /// where they lie: a word the chunks heard comes out as they gave it,
    /// or once more where it sounds across a window's edge (the seam
    /// cannot tell the window's copy from the word said again); a word
    /// only the windows heard comes out once, with every piece they heard;
    /// a word nobody heard, and a token of no word, do not come out.
    fn placed_words_come_out(
        t: &mut Transcriber<FrameTokenBackend>,
        samples: &[f32],
        words: &[Placed],
        off: &Transcript,
        on: &Transcript,
        checked: &mut Checked,
    ) {
        // What each gap's windows give, merged with each other as the
        // chunks are (the LCS merge may take a word said twice within its
        // tolerance for one, there as between two chunks).
        let mut stats = DecodeStats::default();
        let mut heard = Vec::new();
        for gap in on.gap_windows.chunk_by(|a, b| b.start < a.end) {
            let decoded: Vec<Vec<Token>> = gap
                .iter()
                .map(|w| t.decode_range(samples, w.clone(), &mut stats).unwrap())
                .collect();
            heard.extend(merge_all(&decoded, 2.0, t.vocab()));
        }
        // Per word: its starts, its tokens and which pieces; then strays.
        let tally = |tokens: &[Token]| {
            let mut per = vec![(0, 0, 0u8); words.len()];
            let mut strays = 0;
            for token in tokens {
                let hit = words
                    .iter()
                    .enumerate()
                    .find_map(|(w, word)| word.piece_of(token).map(|i| (w, i)));
                if let Some((w, i)) = hit {
                    per[w].0 += usize::from(i == 0);
                    per[w].1 += 1;
                    per[w].2 |= 1 << i;
                } else {
                    strays += 1;
                }
            }
            (per, strays)
        };
        let ((chunked, off_strays), (windowed, _), (spliced, on_strays)) =
            (tally(&off.tokens), tally(&heard), tally(&on.tokens));
        let show = |tokens: &[Token]| {
            let pieces = tokens.iter().map(|t| format!("{}@{}", t.id, t.sample()));
            pieces.collect::<Vec<_>>().join(" ")
        };
        let context = || {
            format!(
                "chunks {:?} windows {:?}\n off {}\n heard {}\n on {}",
                on.chunks,
                on.gap_windows,
                show(&off.tokens),
                show(&heard),
                show(&on.tokens)
            )
        };
        assert!(on_strays <= off_strays, "a stray token; {}", context());
        for (w, word) in words.iter().enumerate() {
            let (heard_by, (firsts, count, pieces)) = (windowed[w], spliced[w]);
            if chunked[w].1 > 0 {
                let edge = on
                    .gap_windows
                    .iter()
                    .any(|g| word.span().contains(&g.start) || word.span().contains(&g.end));
                let once_more = edge && firsts <= chunked[w].0 + 1;
                assert!(
                    count == chunked[w].1 || once_more,
                    "chunk word {:?} at {} came out {count} times; {}",
                    word.pieces,
                    word.start(0),
                    context()
                );
            } else if heard_by.0 > 0 {
                let all = heard_by.2 | chunked[w].2;
                assert!(
                    firsts == 1 && pieces == all && count == all.count_ones() as usize,
                    "window word {:?} at {} came out {count} times; {}",
                    word.pieces,
                    word.start(0),
                    context()
                );
                let inside =
                    |range: Range<usize>| off.tokens.iter().any(|c| range.contains(&c.sample()));
                let window = on.gap_windows.iter().find(|g| g.contains(&word.start(0)));
                let (before, after) = window.map_or((false, false), |g| {
                    (
                        inside(g.start..word.start(0)),
                        inside(word.span().end..g.end),
                    )
                });
                checked.words += 1;
                checked.beside += usize::from(before || after);
                checked.between += usize::from(before && after);
            } else {
                assert_eq!(firsts, 0, "unheard word {:?}; {}", word.pieces, context());
            }
        }
    }

    /// Chunk A, before a gap of 2.6 to 7 or 7 to 20 s, and chunk B after
    /// it, the speech regions, each of loud sound the fake decodes to
    /// nothing: 6 to 8.5 s with one loud word, which under 130 frames looks
    /// empty and is recovered into the gap, or 10.8 to 12.8 s with five,
    /// decoded plainly, or absent (a lead-in or a tail). Quiet words of one
    /// to three pieces lie in A's recovered overlap (word 2) or B's (word
    /// 4), long ones across the reach of A's (2, then 7 past it) or B's (8,
    /// then 9 before it), and anywhere in the gap (3, 5, 6 and 10).
    fn two_sided_layout(rng: &mut Rng) -> Layout {
        let (mode_a, mut mode_b) = (rng.below(3), rng.below(3));
        if mode_a == 2 && mode_b == 2 {
            mode_b = 0;
        }
        let gap = if rng.below(2) == 0 {
            rng.tenths(2.6, 45)
        } else {
            rng.tenths(7.0, 130)
        };
        let lengths = |rng: &mut Rng, mode| match mode {
            0 => rng.tenths(6.0, 25),
            1 => rng.tenths(10.8, 20),
            _ => 0.0,
        };
        let a = if mode_a == 2 {
            0.0
        } else {
            rng.tenths(1.0, 20)
        };
        let a_end = a + lengths(rng, mode_a);
        let b = a_end + gap;
        let total = b + lengths(rng, mode_b);
        let mut samples = vec![0.0f32; sample_count(total)];
        if rng.below(2) == 0 {
            for (i, x) in samples.iter_mut().enumerate() {
                *x = if i % 2 == 0 { 3e-5 } else { -3e-5 };
            }
        }
        let (mut words, mut regions) = (Vec::new(), Vec::new());
        for (mode, from, to, ids) in [
            (mode_a, a, a_end, [11, 13, 14, 15, 16]),
            (mode_b, b, total, [12, 17, 18, 19, 20]),
        ] {
            let region = sample_count(from)..sample_count(to);
            samples[region.clone()].fill(0.05);
            regions.extend((!region.is_empty()).then_some(region));
            let count = [1, 5, 0][mode];
            for (i, id) in ids.into_iter().take(count).enumerate() {
                words.push(Placed::new(vec![id], from + 0.5 + 2.0 * i as f32, 5));
            }
        }
        let quiet = |rng: &mut Rng, id: u32, seconds: f32| {
            let pieces = [id, 20 + id, 30 + id][..=rng.below(3)].to_vec();
            let frames = 2 + rng.below(6);
            let length = (pieces.len() * frames) as f32 * 0.08;
            (Placed::new(pieces, seconds, frames), length)
        };
        let mut gap_words = Vec::new();
        let straddle = rng.below(3);
        if mode_a != 2 && straddle == 1 {
            let long = Placed::new(
                vec![2, 22, 32],
                rng.tenths(a_end + 1.6, 4),
                6 + rng.below(2),
            );
            let past = long.span().end as f32 / SAMPLE_RATE as f32 + rng.tenths(0.08, 3);
            gap_words.push(long);
            gap_words.push(Placed::new(vec![7], past, 2 + rng.below(2)));
        } else if mode_a != 2 {
            let at = rng.tenths(a_end + 0.3, 17);
            gap_words.push(quiet(rng, 2, at).0);
        }
        if mode_b != 2 && straddle == 2 {
            let frames = 6 + rng.below(2);
            let start = rng.tenths(b - 1.9, 4) - 0.24 * frames as f32;
            gap_words.push(Placed::new(vec![8, 28, 38], start, frames));
            let before = start - rng.tenths(0.32, 3);
            gap_words.push(Placed::new(vec![9], before, 3));
        } else if mode_b != 2 {
            let (mut word, length) = quiet(rng, 4, 0.0);
            word.frame = sample_count(b - rng.tenths(0.3, 16) - length) / FRAME_SAMPLES;
            gap_words.push(word);
        }
        for id in [3, 5, 6, 10] {
            if rng.below(3) > 0 {
                let at = rng.tenths(a_end + 0.1, ((gap - 0.2) * 10.0) as usize);
                gap_words.push(quiet(rng, id, at).0);
            }
        }
        let (lo, hi) = (sample_count(a_end + 0.1), sample_count(b - 0.1));
        for word in gap_words {
            let inside = lo <= word.start(0) && word.span().end <= hi;
            if inside && word.clear_of(&words) {
                words.push(word);
            }
        }
        for word in &words {
            word.put(&mut samples);
        }
        Layout {
            samples,
            words,
            regions: Some(regions),
        }
    }

    /// One property case: with the extra windows on, the chunks stay;
    /// every chunk token stays, in order; and the placed words come out as
    /// [`placed_words_come_out`] says.
    fn check_layout(
        layout: &Layout,
        config: impl Fn(bool) -> PipelineConfig,
        min_frames: usize,
        case: usize,
        checked: &mut Checked,
    ) {
        let transcriber = |gaps| {
            let mut t = quiet_transcriber(config(gaps), min_frames);
            if let Some(regions) = &layout.regions {
                t.vad = Box::new(FixedVad(regions.clone()));
            }
            t
        };
        let samples = &layout.samples;
        let off = transcriber(false).transcribe(samples, None).unwrap();
        let mut t = transcriber(true);
        let on = t.transcribe(samples, None).unwrap();
        assert_eq!(on.chunks, off.chunks, "case {case}");
        let mut rest = on.tokens.iter();
        assert!(
            off.tokens.iter().all(|token| rest.any(|t| t == token)),
            "case {case}: a chunk token changed"
        );
        placed_words_come_out(&mut t, samples, &layout.words, &off, &on, checked);
    }

    /// Random layouts under four settings, a third with windows under 130
    /// frames that hold loud sound decoded to silence, so that recoveries
    /// are accepted, and [`two_sided_layout`]s under the same settings, all
    /// checked by [`check_layout`].
    #[test]
    fn extra_windows_add_their_words_and_never_replace_or_repeat_a_chunks() {
        let mut rng = Rng(0x51_7cc1_b727_220a);
        let mut checked = Checked::default();
        for case in 0..240 {
            let layout = random_layout(&mut rng, case);
            let min_frames = if case % 3 == 2 { 130 } else { 0 };
            let config = |gaps| random_config(case, gaps);
            check_layout(&layout, config, min_frames, case, &mut checked);
        }
        // Window words checked (538 with this seed), and those beside chunk
        // tokens inside their window (10).
        let Checked { words, beside, .. } = checked;
        assert!(words > 500 && beside > 5, "{words} {beside}");
        let mut two_sided = Checked::default();
        for case in 0..400 {
            let layout = two_sided_layout(&mut rng);
            let config = |gaps| random_config(case, gaps);
            check_layout(&layout, config, 130, case, &mut two_sided);
        }
        // Window words checked (640), and those between chunk tokens on
        // both sides inside their window (14).
        let Checked { words, between, .. } = two_sided;
        assert!(words > 600 && between > 10, "{words} {between}");
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
            phase: 0,
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

        /// `from` plus up to `count - 1` tenths.
        fn tenths(&mut self, from: f32, count: usize) -> f32 {
            from + self.below(count.max(1)) as f32 / 10.0
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
}
