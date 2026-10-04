//! FluidAudio's `TdtDecoderV3.decodeWithTimings` (0.17.4) for the path
//! Steno takes (no language filter, since Steno passes `language: nil`; a
//! fresh decoder state per window; `initialTimeIndex` zero), as
//! `steno_speech`'s shared greedy loop under [`FLUID_AUDIO`] plus three
//! steps of its own, kept for parity with Swift: the early exit for a
//! window under two frames, the tail flush of the last window (three
//! boundary frames probed until five blanks in a row), and the emission
//! cutoff of the chunker's warm-up window.

use steno_speech::{
    DecodeStats, DecoderConfig, TdtModel, TokenBudget, TokenDuration, WindowEnd, confidence,
    decode_frames,
};

use crate::SpeechError;
use crate::Token;

/// `TdtConfig.durationBins`: the joint's bin index maps to this many frames.
pub const DURATION_BINS: [usize; 5] = [0, 1, 2, 3, 4];
/// `TdtConfig.maxSymbolsPerStep`: the flush's probe count.
pub const MAX_SYMBOLS_PER_STEP: usize = 10;
/// `TdtConfig.maxTokensPerChunk`.
pub const MAX_TOKENS_PER_CHUNK: usize = 150;
/// `TdtConfig.consecutiveBlankLimit`.
pub const CONSECUTIVE_BLANK_LIMIT: usize = 5;

/// `TdtDecoderV3`'s guards in the shared loop's terms.
///
/// Swift forces a zero duration to one when the frame already emitted a
/// token, and records the forced value: two symbols per frame,
/// [`TokenDuration::Advanced`]. Its own symbol limit
/// ([`MAX_SYMBOLS_PER_STEP`] emissions on one frame before a forced
/// advance) never fires behind that guard, since a frame's second
/// emission always moves on. A token is emitted only while the frame it
/// advances to is inside the window, and the token after
/// [`MAX_TOKENS_PER_CHUNK`] ends the window unemitted.
pub const FLUID_AUDIO: DecoderConfig = DecoderConfig {
    max_symbols_per_frame: 2,
    token_budget: TokenBudget::PerWindow(MAX_TOKENS_PER_CHUNK),
    window_end: WindowEnd::Drop,
    token_duration: TokenDuration::Advanced,
};

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

/// `TdtDurationMapping.mapDurationBin`.
pub fn duration_of(bin: usize) -> Result<usize, SpeechError> {
    DURATION_BINS
        .get(bin)
        .copied()
        .ok_or(SpeechError::DurationBin(bin))
}

/// Decode one window of `valid` encoder frames with a fresh decoder state.
pub fn decode_window<M: TdtModel<Error = SpeechError> + ?Sized>(
    model: &mut M,
    valid: usize,
    spec: WindowSpec,
    stats: &mut DecodeStats,
) -> Result<Hypothesis, SpeechError> {
    let effective_len = valid.min(spec.actual_frames);
    // Early exit for very short audio (under two frames).
    if valid <= 1 || effective_len == 0 {
        return Ok(Hypothesis::default());
    }
    let decoded = decode_frames(model, effective_len, spec.frame_offset, &FLUID_AUDIO, stats)?;
    let mut tokens = decoded.tokens;

    if spec.is_last {
        // Swift's `EncoderFrameView.count` is the valid frame count, so the
        // flush never probes a padding frame; `valid > 1` was checked above.
        let blank = model.blank_id();
        let mut additional = 0usize;
        let mut consecutive_blanks = 0usize;
        let mut final_t = decoded.stop_frame;
        while additional < MAX_SYMBOLS_PER_STEP && consecutive_blanks < CONSECUTIVE_BLANK_LIMIT {
            let variations = [
                final_t.min(valid - 1),
                (effective_len - 1).min(valid - 1),
                effective_len.saturating_sub(2).min(valid - 1),
            ];
            let decision = model.joint(variations[additional % variations.len()])?;
            stats.joint_calls += 1;
            let duration = model.duration(decision.duration_bin)?;
            if decision.token == blank {
                consecutive_blanks += 1;
            } else {
                consecutive_blanks = 0;
                tokens.push(steno_speech::Token {
                    id: decision.token,
                    frame: spec.frame_offset + final_t.min(effective_len - 1),
                    confidence: confidence(decision.probability),
                    duration,
                });
                model.feed(decision.token)?;
                stats.decoder_calls += 1;
            }
            final_t = (final_t + duration.max(1)).min(effective_len);
            additional += 1;
        }
    }

    let mut hypothesis = Hypothesis::default();
    for token in tokens {
        if spec
            .emit_after_frame
            .is_none_or(|after| token.frame >= after)
        {
            hypothesis.tokens.push(Token::from(token));
        } else {
            hypothesis.suppressed += 1;
        }
    }
    Ok(hypothesis)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use steno_speech::JointDecision;

    use super::*;
    use crate::vocab::BLANK_ID;

    const BLANK: u32 = 8192;

    /// A model driven by a script of joint decisions in call order; it
    /// records every call, so two loops over the same script can be
    /// compared call for call.
    #[derive(Default)]
    struct Scripted {
        script: VecDeque<JointDecision>,
        calls: Vec<(char, usize)>,
    }

    impl Scripted {
        fn new(decisions: &[(u32, usize)]) -> Scripted {
            Scripted {
                script: decisions
                    .iter()
                    .map(|&(token, duration_bin)| JointDecision {
                        token,
                        probability: 0.5,
                        duration_bin,
                    })
                    .collect(),
                calls: Vec::new(),
            }
        }
    }

    impl TdtModel for Scripted {
        type Error = SpeechError;
        fn blank_id(&self) -> u32 {
            BLANK
        }
        fn duration(&self, bin: usize) -> Result<usize, SpeechError> {
            duration_of(bin)
        }
        fn start(&mut self) -> Result<(), SpeechError> {
            self.calls.push(('s', 0));
            Ok(())
        }
        fn feed(&mut self, token: u32) -> Result<(), SpeechError> {
            self.calls.push(('f', token as usize));
            Ok(())
        }
        fn joint(&mut self, t: usize) -> Result<JointDecision, SpeechError> {
            self.calls.push(('j', t));
            Ok(self.script.pop_front().unwrap_or(JointDecision {
                token: BLANK,
                probability: 1.0,
                duration_bin: 1,
            }))
        }
    }

    fn spec(actual_frames: usize, is_last: bool) -> WindowSpec {
        WindowSpec {
            actual_frames,
            frame_offset: 100,
            emit_after_frame: None,
            is_last,
        }
    }

    fn frames(hypothesis: &Hypothesis) -> Vec<(usize, usize, usize)> {
        hypothesis
            .tokens
            .iter()
            .map(|t| (t.id, t.frame, t.duration))
            .collect()
    }

    #[test]
    fn a_second_zero_duration_emission_advances_and_records_one_frame() {
        // Frame 0: token 1 dur 0, token 2 dur 0 (forced to 1). Frame 1:
        // token 3 dur 0, blank. Frame 2: blank dur 2 past the end.
        let mut model = Scripted::new(&[(1, 0), (2, 0), (3, 0), (BLANK, 1), (BLANK, 2)]);
        let mut stats = DecodeStats::default();
        let hypothesis = decode_window(&mut model, 4, spec(3, false), &mut stats).unwrap();
        assert_eq!(
            frames(&hypothesis),
            vec![(1, 100, 0), (2, 100, 1), (3, 101, 0)]
        );
        assert_eq!((stats.decoder_calls, stats.joint_calls), (4, 5));
    }

    #[test]
    fn a_token_reaching_the_window_end_is_dropped_unfed() {
        let mut model = Scripted::new(&[(1, 1), (2, 2)]);
        let hypothesis =
            decode_window(&mut model, 3, spec(3, false), &mut DecodeStats::default()).unwrap();
        assert_eq!(frames(&hypothesis), vec![(1, 100, 1)]);
        assert_eq!(model.calls, vec![('s', 0), ('j', 0), ('f', 1), ('j', 1)]);
    }

    #[test]
    fn the_token_after_the_budget_ends_the_window() {
        let script: Vec<(u32, usize)> = (0..200).map(|_| (1, 1)).collect();
        let mut model = Scripted::new(&script);
        let hypothesis = decode_window(
            &mut model,
            300,
            spec(300, false),
            &mut DecodeStats::default(),
        )
        .unwrap();
        assert_eq!(hypothesis.tokens.len(), MAX_TOKENS_PER_CHUNK);
    }

    #[test]
    fn a_window_under_two_frames_decodes_nothing() {
        let mut model = Scripted::new(&[(1, 1)]);
        let hypothesis =
            decode_window(&mut model, 1, spec(1, true), &mut DecodeStats::default()).unwrap();
        assert!(hypothesis.is_whole_window_blank());
        assert_eq!(model.calls, Vec::new());
    }

    #[test]
    fn the_last_window_flushes_until_five_blanks() {
        // The loop: blank past the end of two frames. The flush: a token at
        // the clamped stop frame, then five blanks.
        let mut model = Scripted::new(&[(BLANK, 2), (7, 0)]);
        let hypothesis =
            decode_window(&mut model, 2, spec(2, true), &mut DecodeStats::default()).unwrap();
        assert_eq!(frames(&hypothesis), vec![(7, 101, 0)]);
        assert_eq!(
            model.calls.iter().filter(|(kind, _)| *kind == 'j').count(),
            7
        );
    }

    #[test]
    fn tokens_before_the_cutoff_are_suppressed_but_fed() {
        let mut model = Scripted::new(&[(1, 1), (2, 1), (3, 1)]);
        let window = WindowSpec {
            emit_after_frame: Some(102),
            ..spec(4, false)
        };
        let hypothesis = decode_window(&mut model, 4, window, &mut DecodeStats::default()).unwrap();
        assert_eq!(frames(&hypothesis), vec![(3, 102, 1)]);
        assert_eq!(hypothesis.suppressed, 2);
        assert!(model.calls.contains(&('f', 1)));
    }

    /// FluidAudio's loop as #163 ported it, before the shared loop: the
    /// oracle for [`the_shared_loop_decodes_as_fluid_audio_does`].
    #[allow(clippy::too_many_lines)]
    fn fluid_audio_reference(
        model: &mut Scripted,
        valid: usize,
        spec: WindowSpec,
        counts: &mut (usize, usize),
    ) -> Result<Hypothesis, SpeechError> {
        let mut hypothesis = Hypothesis::default();
        if valid <= 1 {
            return Ok(hypothesis);
        }
        let effective_len = valid.min(spec.actual_frames);
        let mut t: usize = 0;
        if t >= effective_len {
            return Ok(hypothesis);
        }
        let last_timestep = effective_len - 1;
        let mut safe_t = t.min(last_timestep);
        let mut active = t < effective_len;
        model.start()?;
        counts.0 += 1;
        let emit = |hypothesis: &mut Hypothesis,
                    id: u32,
                    frame: usize,
                    confidence: f32,
                    duration: usize| {
            let global = frame + spec.frame_offset;
            if spec.emit_after_frame.is_none_or(|after| global >= after) {
                hypothesis.tokens.push(Token {
                    id: id as usize,
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
            let decision = model.joint(safe_t)?;
            counts.1 += 1;
            let mut label = decision.token;
            let mut score = confidence(decision.probability);
            let mut duration = duration_of(decision.duration_bin)?;
            let mut blank = label == BLANK;
            if !blank && duration == 0 && last_emission_frame == Some(t) && emissions_at_frame >= 1
            {
                duration = 1;
            }
            if blank && duration == 0 {
                duration = 1;
            }
            label_frame = t;
            t += duration;
            safe_t = t.min(last_timestep);
            active = t < effective_len;
            let mut advance = active && blank;
            while advance {
                label_frame = t;
                let inner = model.joint(safe_t)?;
                counts.1 += 1;
                label = inner.token;
                score = confidence(inner.probability);
                duration = duration_of(inner.duration_bin)?;
                blank = label == BLANK;
                if blank && duration == 0 {
                    duration = 1;
                }
                t += duration;
                safe_t = t.min(last_timestep);
                active = t < effective_len;
                advance = active && blank;
            }
            if active && label != BLANK {
                processed += 1;
                if processed > MAX_TOKENS_PER_CHUNK {
                    break;
                }
                emit(&mut hypothesis, label, label_frame, score, duration);
                model.feed(label)?;
                counts.0 += 1;
                if last_emission_frame == Some(label_frame) {
                    emissions_at_frame += 1;
                } else {
                    last_emission_frame = Some(label_frame);
                    emissions_at_frame = 1;
                }
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
            let count = valid;
            let mut additional = 0usize;
            let mut consecutive_blanks = 0usize;
            let mut final_t = t;
            while additional < MAX_SYMBOLS_PER_STEP && consecutive_blanks < CONSECUTIVE_BLANK_LIMIT
            {
                let variations = [
                    final_t.min(count - 1),
                    (effective_len - 1).min(count - 1),
                    effective_len.saturating_sub(2).min(count - 1),
                ];
                let frame_index = variations[additional % variations.len()];
                let decision = model.joint(frame_index)?;
                counts.1 += 1;
                let score = confidence(decision.probability);
                let duration = duration_of(decision.duration_bin)?;
                if decision.token == BLANK {
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
                    model.feed(decision.token)?;
                    counts.0 += 1;
                }
                final_t = (final_t + duration.max(1)).min(effective_len);
                additional += 1;
            }
        }
        Ok(hypothesis)
    }

    /// xorshift64, so the check needs no dependency and replays exactly.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            usize::try_from(self.0 % u64::try_from(n).unwrap()).unwrap()
        }
    }

    #[test]
    fn the_shared_loop_decodes_as_fluid_audio_does() {
        let mut rng = Rng(0x5eed_07d7);
        for _ in 0..20_000 {
            // Long token-heavy windows reach the budget; a rare bin 5 is
            // the duration error.
            let long = rng.below(8) == 0;
            let valid = rng.below(if long { 400 } else { 24 });
            let blank_share = if long { 2 } else { 2 + rng.below(6) };
            let script: Vec<JointDecision> = (0..rng.below(if long { 1200 } else { 80 }))
                .map(|_| JointDecision {
                    token: if rng.below(10) < blank_share {
                        BLANK
                    } else {
                        u32::try_from(rng.below(6)).unwrap()
                    },
                    probability: [0.25, 1.5, f32::NAN][rng.below(3)],
                    duration_bin: if rng.below(500) == 0 {
                        5
                    } else {
                        [0, 0, 1, 2, 3, 4][rng.below(6)]
                    },
                })
                .collect();
            let spec = WindowSpec {
                actual_frames: rng.below(valid + 4),
                frame_offset: rng.below(50),
                emit_after_frame: [None, Some(rng.below(80))][rng.below(2)],
                is_last: rng.below(2) == 0,
            };
            let mut ours = Scripted {
                script: script.clone().into(),
                calls: Vec::new(),
            };
            let mut theirs = Scripted {
                script: script.into(),
                calls: Vec::new(),
            };
            let mut stats = DecodeStats::default();
            let mut counts = (0, 0);
            let got = decode_window(&mut ours, valid, spec, &mut stats);
            let want = fluid_audio_reference(&mut theirs, valid, spec, &mut counts);
            assert_eq!(
                format!("{got:?}"),
                format!("{want:?}"),
                "{spec:?} valid {valid}"
            );
            assert_eq!(ours.calls, theirs.calls, "{spec:?} valid {valid}");
            assert_eq!((stats.decoder_calls, stats.joint_calls), counts);
        }
        assert_eq!(usize::try_from(BLANK).unwrap(), BLANK_ID);
    }
}
