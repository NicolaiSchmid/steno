//! The greedy TDT loop of FluidAudio's `TdtDecoderV3.decodeWithTimings`
//! (0.17.4) over the backend's decoder and joint steps, for the path
//! Steno takes: no language filter (Steno passes `language: nil`), a fresh
//! decoder state per window, `initialTimeIndex` zero.
//!
//! Each window: prime the LSTM with blank as start of sequence, then walk
//! the encoder frames; a blank skips `duration` frames without touching
//! the LSTM (the inner loop), a token is emitted, fed to the LSTM and the
//! frame advances by its duration. Guards: `duration == 0` is forced to 1
//! for blanks and for repeated emissions on one frame, at most
//! [`MAX_SYMBOLS_PER_STEP`] emissions per frame before a forced advance,
//! at most [`MAX_TOKENS_PER_CHUNK`] tokens per window. The last window
//! keeps probing three boundary frames until five blanks in a row.

use crate::SpeechError;
use crate::Token;
use crate::backend::{Backend, JointDecision, Scratch};
use crate::coreml::EncoderView;
use crate::vocab::BLANK_ID;

/// `TdtConfig.durationBins`: the joint's bin index maps to this many frames.
pub const DURATION_BINS: [usize; 5] = [0, 1, 2, 3, 4];
/// `TdtConfig.maxSymbolsPerStep`.
pub const MAX_SYMBOLS_PER_STEP: usize = 10;
/// `TdtConfig.maxTokensPerChunk`.
pub const MAX_TOKENS_PER_CHUNK: usize = 150;
/// `TdtConfig.consecutiveBlankLimit`.
pub const CONSECUTIVE_BLANK_LIMIT: usize = 5;

/// What one window decoded (`TdtHypothesis`): the emitted tokens and the
/// count of tokens decoded before the emission cutoff. The suppressed
/// count matters to the empty-decode recovery gate: a window whose only
/// tokens were suppressed decoded fine and is not retried
/// (`AsrManager.isWholeWindowBlank`, issue #909).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hypothesis {
    /// Emitted tokens, global frames.
    pub tokens: Vec<Token>,
    /// Tokens decoded before the emission cutoff and dropped.
    pub suppressed: usize,
}

impl Hypothesis {
    /// Nothing at all came out, not even suppressed tokens.
    #[must_use]
    pub fn is_whole_window_blank(&self) -> bool {
        self.tokens.is_empty() && self.suppressed == 0
    }

    /// Mean token confidence, `0` for no tokens.
    #[must_use]
    pub fn mean_confidence(&self) -> f32 {
        if self.tokens.is_empty() {
            return 0.0;
        }
        // Token counts per window are at most 150.
        #[allow(clippy::cast_precision_loss)]
        let count = self.tokens.len() as f32;
        self.tokens
            .iter()
            .map(|token| token.confidence)
            .sum::<f32>()
            / count
    }
}

/// Counters the harness reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeCounts {
    /// Prediction-network calls, one per emitted token plus the priming.
    pub decoder_calls: usize,
    /// Joint calls, one per frame step.
    pub joint_calls: usize,
}

/// What the decoder needs to know about the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSpec {
    /// Frames of real audio in the window (`actualAudioFrames`); the
    /// decoder stops at `min(encoder_length, actual_frames)`.
    pub actual_frames: usize,
    /// Added to every frame index (`globalFrameOffset`).
    pub frame_offset: usize,
    /// Tokens at global frames before this are decoded but not emitted
    /// (`emitTokensAfterGlobalFrame`).
    pub emit_after_frame: Option<usize>,
    /// Run the end-of-audio flush (`isLastChunk`).
    pub is_last: bool,
}

/// `TdtDurationMapping.clampProbability`.
fn clamp_probability(p: f32) -> f32 {
    if p.is_finite() {
        p.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// `TdtDurationMapping.mapDurationBin`.
fn duration_of(decision: JointDecision) -> Result<usize, SpeechError> {
    DURATION_BINS
        .get(decision.duration_bin)
        .copied()
        .ok_or(SpeechError::DurationBin(decision.duration_bin))
}

/// Decode one window with a fresh decoder state.
#[allow(clippy::too_many_lines)]
pub fn decode_window(
    backend: &Backend,
    scratch: &mut Scratch,
    encoder: &EncoderView,
    spec: WindowSpec,
    counts: &mut DecodeCounts,
) -> Result<Hypothesis, SpeechError> {
    let mut hypothesis = Hypothesis::default();
    // Early exit for very short audio (under two frames).
    if encoder.valid <= 1 {
        return Ok(hypothesis);
    }
    let effective_len = encoder.valid.min(spec.actual_frames);
    let mut t: usize = 0;
    if t >= effective_len {
        return Ok(hypothesis);
    }
    let last_timestep = effective_len - 1;
    let mut safe_t = t.min(last_timestep);
    let mut active = t < effective_len;

    scratch.reset_decoder()?;
    backend.decoder_step(scratch, BLANK_ID)?;
    counts.decoder_calls += 1;

    let emit =
        |hypothesis: &mut Hypothesis, id: usize, frame: usize, confidence: f32, duration: usize| {
            let global = frame + spec.frame_offset;
            if spec.emit_after_frame.is_none_or(|after| global >= after) {
                hypothesis.tokens.push(Token {
                    id,
                    frame: global,
                    confidence,
                    duration,
                });
            } else {
                hypothesis.suppressed += 1;
            }
        };

    let mut last_emission_frame: Option<usize> = None;
    let mut emissions_at_frame = 0usize;
    let mut processed = 0usize;
    let mut label_frame;

    while active {
        let decision = backend.joint_step(scratch, encoder, safe_t)?;
        counts.joint_calls += 1;
        let mut label = decision.token;
        let mut score = clamp_probability(decision.probability);
        let mut duration = duration_of(decision)?;
        let mut blank = label == BLANK_ID;
        // Prevent repeated non-blank emissions at the same frame when
        // duration is zero.
        if !blank && duration == 0 && last_emission_frame == Some(t) && emissions_at_frame >= 1 {
            duration = 1;
        }
        // Prevent an infinite loop when a blank has duration zero.
        if blank && duration == 0 {
            duration = 1;
        }
        label_frame = t;
        t += duration;
        safe_t = t.min(last_timestep);
        active = t < effective_len;
        let mut advance = active && blank;

        // Inner loop: consecutive blanks reuse the cached projection, the
        // LSTM does not see silence.
        while advance {
            label_frame = t;
            let inner = backend.joint_step(scratch, encoder, safe_t)?;
            counts.joint_calls += 1;
            label = inner.token;
            score = clamp_probability(inner.probability);
            duration = duration_of(inner)?;
            blank = label == BLANK_ID;
            if blank && duration == 0 {
                duration = 1;
            }
            t += duration;
            safe_t = t.min(last_timestep);
            active = t < effective_len;
            advance = active && blank;
        }

        if active && label != BLANK_ID {
            processed += 1;
            if processed > MAX_TOKENS_PER_CHUNK {
                break;
            }
            emit(&mut hypothesis, label, label_frame, score, duration);
            backend.decoder_step(scratch, label)?;
            counts.decoder_calls += 1;
            if last_emission_frame == Some(label_frame) {
                emissions_at_frame += 1;
            } else {
                last_emission_frame = Some(label_frame);
                emissions_at_frame = 1;
            }
            // Force-blank: too many emissions without advancing.
            if emissions_at_frame >= MAX_SYMBOLS_PER_STEP {
                t = (t + 1).min(last_timestep);
                safe_t = t.min(last_timestep);
                emissions_at_frame = 0;
                last_emission_frame = None;
            }
        }
        active = t < effective_len;
    }

    if spec.is_last {
        // Swift's `EncoderFrameView.count` is the valid frame count, so the
        // flush never probes a padding frame; `valid > 1` was checked above.
        let count = encoder.valid;
        let mut additional = 0usize;
        let mut consecutive_blanks = 0usize;
        let mut final_t = t;
        while additional < MAX_SYMBOLS_PER_STEP && consecutive_blanks < CONSECUTIVE_BLANK_LIMIT {
            let variations = [
                final_t.min(count - 1),
                (effective_len - 1).min(count - 1),
                effective_len.saturating_sub(2).min(count - 1),
            ];
            let frame_index = variations[additional % variations.len()];
            let decision = backend.joint_step(scratch, encoder, frame_index)?;
            counts.joint_calls += 1;
            let score = clamp_probability(decision.probability);
            let duration = duration_of(decision)?;
            if decision.token == BLANK_ID {
                consecutive_blanks += 1;
            } else {
                consecutive_blanks = 0;
                emit(
                    &mut hypothesis,
                    decision.token,
                    final_t.min(effective_len - 1),
                    score,
                    duration,
                );
                backend.decoder_step(scratch, decision.token)?;
                counts.decoder_calls += 1;
            }
            final_t = (final_t + duration.max(1)).min(effective_len);
            additional += 1;
        }
    }
    Ok(hypothesis)
}
