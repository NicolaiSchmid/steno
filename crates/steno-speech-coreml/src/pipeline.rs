//! A recording to merged tokens: FluidAudio's `AsrManager.transcribe` path
//! for Steno's call (`transcribeWithState`, `ChunkProcessor.process`,
//! `executeMLInferenceWithTimings`) over the CoreML backend.
//!
//! Long audio: silence-aligned windows decoded by `concurrency` workers
//! (FluidAudio's `parallelChunkConcurrency = 4`), each window retried
//! through the empty-decode recovery ladder, then merged in order,
//! clamped monotonic, seam duplicates collapsed and seam gaps re-decoded.
//! Short audio (one window) skips the merge.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use steno_speech::DecodeStats;

use crate::SpeechError;
use crate::Token;
use crate::backend::{Backend, Scratch};
use crate::chunking::{
    FRAME_SAMPLES, FRAME_SECONDS, Layout, MAX_MODEL_SAMPLES, SAMPLE_RATE, Window,
    adaptive_speech_rms_threshold, encoder_frames, plan_windows, silence_aligned_chunk_starts,
    speech_end_samples, speech_like_seconds,
};
use crate::decoder::{Hypothesis, WindowSpec, decode_window};
use crate::merge::{
    collapse_seam_word_duplicates, enforce_monotonic, merge_chunks, splice_candidate, word_neighbor,
};
use crate::vocab::Vocab;

/// The knobs Steno leaves at FluidAudio's defaults (`ASRConfig`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Windows decoded at once (`parallelChunkConcurrency`).
    pub concurrency: usize,
    /// The post-merge repair pass (`seamGapRepair`).
    pub seam_gap_repair: bool,
    /// Smallest inter-token gap the repair probes
    /// (`seamGapRepairMinGapSeconds`, floored at 0.5 by FluidAudio).
    pub seam_gap_min_gap_seconds: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            concurrency: 4,
            seam_gap_repair: true,
            seam_gap_min_gap_seconds: 1.5,
        }
    }
}

impl Config {
    /// `ASRConfig.init`: `max(1, parallelChunkConcurrency)`,
    /// `max(0.5, seamGapRepairMinGapSeconds)`.
    #[must_use]
    pub fn clamped(self) -> Config {
        Config {
            concurrency: self.concurrency.max(1),
            seam_gap_min_gap_seconds: self.seam_gap_min_gap_seconds.max(0.5),
            ..self
        }
    }
}

/// Counters and timings of one transcription, for the harness table.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    /// Windows decoded, recovery passes and repair probes excluded.
    pub windows: usize,
    /// Prediction-network calls.
    pub decoder_calls: usize,
    /// Joint calls.
    pub joint_calls: usize,
    /// Recovery ladder passes run (each a preprocessor, encoder and decode).
    pub recoveries_tried: usize,
    /// Ladder passes whose result replaced an empty decode.
    pub recoveries_accepted: usize,
    /// Seam gaps probed by the repair pass.
    pub repair_probes: usize,
    /// Tokens the repair pass spliced in.
    pub repaired_tokens: usize,
    /// Summed over all workers, so with four of them these exceed wall time.
    pub preprocessor_seconds: f64,
    /// Encoder time, summed over workers.
    pub encoder_seconds: f64,
    /// Decode-loop time, summed over workers.
    pub decoder_seconds: f64,
}

impl Stats {
    fn add(&mut self, other: &Stats) {
        self.windows += other.windows;
        self.decoder_calls += other.decoder_calls;
        self.joint_calls += other.joint_calls;
        self.recoveries_tried += other.recoveries_tried;
        self.recoveries_accepted += other.recoveries_accepted;
        self.repair_probes += other.repair_probes;
        self.repaired_tokens += other.repaired_tokens;
        self.preprocessor_seconds += other.preprocessor_seconds;
        self.encoder_seconds += other.encoder_seconds;
        self.decoder_seconds += other.decoder_seconds;
    }
}

/// The merged tokens of a recording and how they were produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transcript {
    /// Merged tokens in text order.
    pub tokens: Vec<Token>,
    /// Counters and timings of the run.
    pub stats: Stats,
}

/// How the model is told where the audio ends inside the padded window
/// (`AsrManager.InferenceLengthPolicy`). The default is the normal path;
/// the flags are the recovery perturbations for a window the model
/// decodes to nothing although it carries speech.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LengthPolicy {
    /// Declare the zero padding valid to the encoder.
    encoder_full: bool,
    /// Declare the zero padding valid to the preprocessor.
    preprocessor_full: bool,
    /// Declare the audio 0.2 s shorter and silence that tail.
    trimmed_tail: bool,
}

/// `AsrManager.emptyDecodeRecoveryPolicies`, in order.
const RECOVERY_LADDER: [LengthPolicy; 5] = [
    LengthPolicy {
        encoder_full: true,
        preprocessor_full: false,
        trimmed_tail: false,
    },
    LengthPolicy {
        encoder_full: false,
        preprocessor_full: true,
        trimmed_tail: false,
    },
    LengthPolicy {
        encoder_full: false,
        preprocessor_full: false,
        trimmed_tail: true,
    },
    LengthPolicy {
        encoder_full: true,
        preprocessor_full: false,
        trimmed_tail: true,
    },
    LengthPolicy {
        encoder_full: false,
        preprocessor_full: true,
        trimmed_tail: true,
    },
];
/// `AsrManager.trimmedTailSamples`: 0.2 s.
const TRIMMED_TAIL_SAMPLES: usize = SAMPLE_RATE / 5;
/// `AsrManager.emptyDecodeRecoveryMinimumSamples`: 2 s.
const RECOVERY_MIN_SAMPLES: usize = 2 * SAMPLE_RATE;
/// `AsrManager.emptyDecodeRecoveryMinimumRMS`: about -50 dBFS.
const RECOVERY_MIN_RMS: f64 = 0.003;
/// `AsrManager.emptyDecodeRecoveryMinimumConfidence`.
const RECOVERY_MIN_CONFIDENCE: f32 = 0.7;
/// `AsrManager.emptyDecodeRecoveryMinimumTokens`.
const RECOVERY_MIN_TOKENS: usize = 2;
/// `ASRConstants.minimumRequiredSamples`: 0.3 s.
const MINIMUM_SAMPLES: usize = SAMPLE_RATE * 3 / 10;
/// `ChunkProcessor.maxSeamGapRepairs`: probes per file.
const MAX_SEAM_GAP_REPAIRS: usize = 32;
/// `ChunkProcessor.seamGapMinSpeechSeconds`.
const SEAM_GAP_MIN_SPEECH_SECONDS: f64 = 0.5;

/// Where a window sits in the recording, for the decoder.
#[derive(Debug, Clone, Copy)]
struct Placement {
    frame_offset: usize,
    emit_after_frame: Option<usize>,
    is_last: bool,
}

/// The loaded backend and the configuration; one per engine.
#[derive(Debug)]
pub struct Transcriber {
    backend: Backend,
    config: Config,
}

impl Transcriber {
    /// Over a loaded backend with the default configuration.
    #[must_use]
    pub fn new(backend: Backend) -> Transcriber {
        Transcriber::with_config(backend, Config::default())
    }

    /// With `config`, clamped as `ASRConfig.init` clamps it: at least one
    /// worker, at least half a second of gap.
    #[must_use]
    pub fn with_config(backend: Backend, config: Config) -> Transcriber {
        Transcriber {
            backend,
            config: config.clamped(),
        }
    }

    #[must_use]
    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    #[must_use]
    pub fn vocab(&self) -> &Vocab {
        self.backend.vocab()
    }

    #[must_use]
    pub fn config(&self) -> Config {
        self.config
    }

    /// Transcribe `audio` (16 kHz mono) to merged tokens.
    pub fn transcribe(&self, audio: &[f32]) -> Result<Transcript, SpeechError> {
        if audio.len() < MINIMUM_SAMPLES {
            return Err(SpeechError::AudioTooShort(audio.len()));
        }
        if audio.len() <= MAX_MODEL_SAMPLES {
            return self.transcribe_short(audio);
        }
        self.transcribe_long(audio)
    }

    /// One window: frame aligned by zero padding when that stays within
    /// the model window, decoded as both first and last window
    /// (`isLastChunk`); `transcribeWithState`, the short branch.
    fn transcribe_short(&self, audio: &[f32]) -> Result<Transcript, SpeechError> {
        let aligned_len = audio.len().div_ceil(FRAME_SAMPLES) * FRAME_SAMPLES;
        let mut padded: Vec<f32>;
        let samples: &[f32] = if aligned_len > audio.len() && aligned_len <= MAX_MODEL_SAMPLES {
            padded = audio.to_vec();
            padded.resize(aligned_len, 0.0);
            &padded
        } else {
            audio
        };
        let mut scratch = self.backend.scratch()?;
        let mut stats = Stats::default();
        let hypothesis = self.transcribe_window(
            &mut scratch,
            samples,
            Placement {
                frame_offset: 0,
                emit_after_frame: None,
                is_last: true,
            },
            &mut stats,
        )?;
        stats.windows = 1;
        Ok(Transcript {
            tokens: hypothesis.tokens,
            stats,
        })
    }

    /// `ChunkProcessor.process` for v3 without mel context.
    fn transcribe_long(&self, audio: &[f32]) -> Result<Transcript, SpeechError> {
        let layout = Layout::v3();
        let total = audio.len();
        let speech_end = speech_end_samples(audio);
        let chunk_starts = silence_aligned_chunk_starts(audio, layout, false);
        let windows = plan_windows(total, speech_end, &chunk_starts, layout, 0);
        let (outputs, mut stats) = self.decode_windows(audio, &windows)?;
        stats.windows = outputs.len();
        let several = outputs.len() > 1;

        let mut outputs = outputs.into_iter();
        let Some(first) = outputs.next() else {
            return Ok(Transcript {
                tokens: Vec::new(),
                stats,
            });
        };
        let vocab = self.vocab();
        // Text order is the merge order; frames are clamped, never sorted
        // (issue #825).
        let mut merged = enforce_monotonic(outputs.fold(first, |merged, window| {
            merge_chunks(&merged, &window, vocab)
        }));
        if several {
            merged = collapse_seam_word_duplicates(&merged, vocab);
        }

        if several && merged.len() > 1 && self.config.seam_gap_repair {
            let threshold = adaptive_speech_rms_threshold(audio);
            let mut scratch = self.backend.scratch()?;
            merged = self.repair_seam_gaps(&mut scratch, audio, merged, threshold, &mut stats)?;
        }
        Ok(Transcript {
            tokens: merged,
            stats,
        })
    }

    /// Decode every window, `concurrency` at a time, in index order. Each
    /// worker owns its scratch arrays; the models are shared. A worker
    /// panic propagates out of `std::thread::scope` as a panic, not as an
    /// `Err`; the only panic sites are the mutex `expect`s, which fire
    /// only after an earlier panic poisoned the lock.
    fn decode_windows(
        &self,
        audio: &[f32],
        windows: &[Window],
    ) -> Result<(Vec<Vec<Token>>, Stats), SpeechError> {
        let workers = self.config.concurrency.min(windows.len()).max(1);
        let next = AtomicUsize::new(0);
        let outputs: Vec<Mutex<Vec<Token>>> = windows.iter().map(|_| Mutex::default()).collect();
        let failure: Mutex<Option<SpeechError>> = Mutex::new(None);
        let totals: Mutex<Stats> = Mutex::new(Stats::default());

        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    let mut stats = Stats::default();
                    let result = (|| -> Result<(), SpeechError> {
                        let mut scratch = self.backend.scratch()?;
                        loop {
                            let index = next.fetch_add(1, Ordering::SeqCst);
                            let Some(window) = windows.get(index) else {
                                return Ok(());
                            };
                            if failure.lock().expect("failure lock").is_some() {
                                return Ok(());
                            }
                            let samples = &audio[window.context_start..window.audio_end];
                            let hypothesis = self.transcribe_window(
                                &mut scratch,
                                samples,
                                Placement {
                                    frame_offset: window.frame_origin / FRAME_SAMPLES,
                                    emit_after_frame: window.emit_after_frame,
                                    is_last: window.is_last,
                                },
                                &mut stats,
                            )?;
                            *outputs[index].lock().expect("output lock") = hypothesis.tokens;
                        }
                    })();
                    totals.lock().expect("stats lock").add(&stats);
                    if let Err(error) = result {
                        let mut slot = failure.lock().expect("failure lock");
                        if slot.is_none() {
                            *slot = Some(error);
                        }
                    }
                });
            }
        });

        if let Some(error) = failure.into_inner().expect("failure lock") {
            return Err(error);
        }
        let outputs = outputs
            .into_iter()
            .map(|slot| slot.into_inner().expect("output lock"))
            .collect();
        Ok((outputs, totals.into_inner().expect("stats lock")))
    }

    /// One window through the normal path, then the recovery ladder when
    /// it decoded to nothing although it carries speech
    /// (`AsrManager.executeMLInferenceWithTimings`).
    fn transcribe_window(
        &self,
        scratch: &mut Scratch,
        samples: &[f32],
        placement: Placement,
        stats: &mut Stats,
    ) -> Result<Hypothesis, SpeechError> {
        let hypothesis = self.infer(scratch, samples, placement, LengthPolicy::default(), stats)?;
        if !hypothesis.is_whole_window_blank() || !should_recover(samples) {
            return Ok(hypothesis);
        }
        for policy in RECOVERY_LADDER {
            let retry = self.infer(scratch, samples, placement, policy, stats)?;
            stats.recoveries_tried += 1;
            if is_credible(&retry) {
                stats.recoveries_accepted += 1;
                return Ok(retry);
            }
        }
        Ok(hypothesis)
    }

    /// Preprocessor, encoder and decoder for one window under `policy`
    /// (`AsrManager.runInference`).
    fn infer(
        &self,
        scratch: &mut Scratch,
        samples: &[f32],
        placement: Placement,
        policy: LengthPolicy,
        stats: &mut Stats,
    ) -> Result<Hypothesis, SpeechError> {
        let full_len = samples.len();
        // `.trimmedTail`: 0.2 s shorter on a frame boundary, and the trimmed
        // samples silenced too, which `preprocess` does by zero padding
        // past the slice it is given.
        let trimmed_len = RECOVERY_MIN_SAMPLES.max(full_len.saturating_sub(TRIMMED_TAIL_SAMPLES))
            / FRAME_SAMPLES
            * FRAME_SAMPLES;
        let effective_len = if policy.trimmed_tail {
            trimmed_len.min(full_len)
        } else {
            full_len
        };
        let declared = (!policy.preprocessor_full).then_some(effective_len);

        let started = Instant::now();
        let mut mel = self
            .backend
            .preprocess(scratch, &samples[..effective_len], declared)?;
        if policy.encoder_full {
            mel.declare_full_length()?;
        }
        let after_preprocessor = Instant::now();
        let encoder = self.backend.encode(&mel)?;
        let after_encoder = Instant::now();
        let actual_frames = encoder_frames(effective_len);
        let mut counts = DecodeStats::default();
        let hypothesis = decode_window(
            &mut self.backend.window_model(scratch, &encoder),
            encoder.valid,
            WindowSpec {
                actual_frames,
                frame_offset: placement.frame_offset,
                emit_after_frame: placement.emit_after_frame,
                is_last: placement.is_last,
            },
            &mut counts,
        )?;
        stats.preprocessor_seconds += (after_preprocessor - started).as_secs_f64();
        stats.encoder_seconds += (after_encoder - after_preprocessor).as_secs_f64();
        stats.decoder_seconds += after_encoder.elapsed().as_secs_f64();
        stats.decoder_calls += counts.decoder_calls;
        stats.joint_calls += counts.joint_calls;
        Ok(hypothesis)
    }

    /// Re-decode inter-token gaps that plausibly hold dropped speech with a
    /// fresh seam-free window and splice in only in-gap tokens
    /// (`ChunkProcessor.repairSeamGaps`, issue #758). Up to three passes so
    /// a partial recovery's residual gap gets its own probe; gaps that
    /// yielded nothing are remembered and skipped.
    fn repair_seam_gaps(
        &self,
        scratch: &mut Scratch,
        audio: &[f32],
        tokens: Vec<Token>,
        speech_rms_threshold: f32,
        stats: &mut Stats,
    ) -> Result<Vec<Token>, SpeechError> {
        let total = audio.len();
        let vocab = self.vocab();
        // 1.5 s at 80 ms is 18.75 frames; Swift's `Int()` truncates to 18
        // and the cast does the same.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let min_gap_frames =
            ((self.config.seam_gap_min_gap_seconds / FRAME_SECONDS) as usize).max(2);
        // The probe window is one chunk of samples (`chunk_samples`), no mel
        // context.
        let window_samples = Layout::v3().chunk_samples;

        let mut working = tokens;
        let mut probes = 0usize;
        let mut probed_gap_starts: HashSet<usize> = HashSet::new();
        for _ in 0..3 {
            let mut inserts: Vec<Token> = Vec::new();
            for index in 0..working.len().saturating_sub(1) {
                if probes >= MAX_SEAM_GAP_REPAIRS {
                    break;
                }
                let current = working[index];
                let next = working[index + 1];
                // Conservative end of the current token: its decoded
                // duration when present, else one frame.
                let gap_start_frame = current.frame + current.duration.max(1);
                let gap_end_frame = next.frame;
                if gap_end_frame < gap_start_frame + min_gap_frames
                    || probed_gap_starts.contains(&gap_start_frame)
                {
                    continue;
                }
                let gap_start_sample = gap_start_frame * FRAME_SAMPLES;
                let gap_end_sample = (gap_end_frame * FRAME_SAMPLES).min(total);
                if gap_end_sample <= gap_start_sample {
                    continue;
                }
                let speech_seconds = speech_like_seconds(
                    audio,
                    gap_start_sample,
                    gap_end_sample,
                    speech_rms_threshold,
                );
                if speech_seconds < SEAM_GAP_MIN_SPEECH_SECONDS {
                    continue;
                }
                probed_gap_starts.insert(gap_start_frame);
                probes += 1;
                stats.repair_probes += 1;

                // Cold start at the gap first (replaying the pre-gap noise
                // can re-blank), gap-centred fallback.
                let gap_center = usize::midpoint(gap_start_sample, gap_end_sample);
                let placements = [
                    gap_start_sample,
                    gap_center.saturating_sub(window_samples / 2),
                ];
                let lead = word_neighbor(&working, index, -1, vocab);
                let tail = word_neighbor(&working, index + 1, 1, vocab);
                for placement in placements {
                    let window_start = placement.min(total.saturating_sub(window_samples))
                        / FRAME_SAMPLES
                        * FRAME_SAMPLES;
                    let window_end = (window_start + window_samples).min(total);
                    if window_end <= window_start {
                        continue;
                    }
                    let hypothesis = self.transcribe_window(
                        scratch,
                        &audio[window_start..window_end],
                        Placement {
                            frame_offset: window_start / FRAME_SAMPLES,
                            emit_after_frame: None,
                            is_last: window_end >= total,
                        },
                        stats,
                    )?;
                    let candidate = splice_candidate(
                        &hypothesis.tokens,
                        gap_start_frame,
                        gap_end_frame,
                        lead,
                        tail,
                        vocab,
                    );
                    if candidate.is_empty() {
                        continue;
                    }
                    stats.repaired_tokens += candidate.len();
                    inserts.extend(candidate);
                    break;
                }
            }
            if inserts.is_empty() {
                break;
            }
            working.extend(inserts);
            working.sort_by_key(|token| token.frame);
        }
        Ok(working)
    }
}

/// Whether an empty decode of `samples` deserves a retry: at least 2 s of
/// audio and an RMS at or above -50 dBFS
/// (`AsrManager.shouldRecoverEmptyDecode`).
fn should_recover(samples: &[f32]) -> bool {
    if samples.len() < RECOVERY_MIN_SAMPLES {
        return false;
    }
    // Squared in f32 and accumulated in f64, as the Swift does.
    let energy: f64 = samples.iter().map(|s| f64::from(*s * *s)).sum();
    // Window lengths are at most 240,000.
    #[allow(clippy::cast_precision_loss)]
    let count = samples.len() as f64;
    (energy / count).sqrt() >= RECOVERY_MIN_RMS
}

/// Whether a recovered hypothesis may replace the empty decode: enough
/// tokens and a mean confidence noise does not reach
/// (`AsrManager.recoveryIsCredible`).
fn is_credible(hypothesis: &Hypothesis) -> bool {
    hypothesis.tokens.len() >= RECOVERY_MIN_TOKENS
        && hypothesis.mean_confidence() >= RECOVERY_MIN_CONFIDENCE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_gate_needs_two_seconds_above_the_floor() {
        assert!(!should_recover(&vec![0.5; RECOVERY_MIN_SAMPLES - 1]));
        assert!(!should_recover(&vec![0.001; RECOVERY_MIN_SAMPLES]));
        assert!(should_recover(&vec![0.01; RECOVERY_MIN_SAMPLES]));
    }

    #[test]
    fn credibility_needs_two_confident_tokens() {
        let token = |confidence: f32| Token {
            id: 1,
            frame: 0,
            confidence,
            duration: 1,
        };
        let one = Hypothesis {
            tokens: vec![token(0.95)],
            suppressed: 0,
        };
        assert!(!is_credible(&one));
        let weak = Hypothesis {
            tokens: vec![token(0.6), token(0.75)],
            suppressed: 0,
        };
        assert!(!is_credible(&weak));
        let strong = Hypothesis {
            tokens: vec![token(0.65), token(0.8)],
            suppressed: 0,
        };
        assert!(is_credible(&strong));
        assert!(
            !Hypothesis {
                tokens: vec![],
                suppressed: 1
            }
            .is_whole_window_blank()
        );
        assert!(Hypothesis::default().is_whole_window_blank());
    }

    #[test]
    fn config_is_clamped_like_asrconfig() {
        let config = Config {
            concurrency: 0,
            seam_gap_repair: true,
            seam_gap_min_gap_seconds: 0.1,
        };
        let clamped = config.clamped();
        assert_eq!(clamped.concurrency, 1);
        assert_eq!(clamped.seam_gap_min_gap_seconds, 0.5);
    }
}
