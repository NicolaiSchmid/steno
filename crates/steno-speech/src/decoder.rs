//! The greedy TDT decode of one encoder window, the one loop both backends
//! run (plan invariant 4). Per frame the joint runs on the current
//! projection; a blank advances by its duration (at least one frame); a
//! token is emitted at the frame, fed to the prediction network, and
//! advances by its duration, where a zero duration keeps the frame for up
//! to `max_symbols_per_frame` symbols before a forced advance of one.
//!
//! [`DecoderConfig::default`] is `NeMo`'s reference semantics
//! (`GreedyTDTInfer`), which sherpa-onnx matched at 5.3 % WER in spike F;
//! the ONNX and the `CoreML` backends both run it. `FluidAudio`'s guards
//! (two symbols a frame, 150 tokens a window, a token past the window's
//! end dropped, the tail flush of the last window) changed nothing on
//! FLEURS German and are not ported (A2 in
//! `.plans/2026-10-07-stable-promotion.md`).
//! Swift: `FluidAudio`'s `TdtDecoderV3`, ported in
//! `spikes/coreml-rs/src/decoder.rs`.

use crate::backend::{DecoderState, EncoderOutput, ModelShape, SpeechBackend};
use crate::error::SpeechError;

/// One emitted token: a `SentencePiece` piece with its frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token {
    pub id: u32,
    /// The encoder frame (80 ms) the token was emitted at, counted from the
    /// start of the recording.
    pub frame: usize,
    /// The joint's probability for the piece ([`confidence`]).
    pub confidence: f32,
    /// The TDT duration in frames the model predicted, zero included.
    pub duration: usize,
}

/// Limits of the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderConfig {
    /// Symbols with duration zero allowed on one frame before the loop
    /// moves on (`NeMo`'s `max_symbols`).
    pub max_symbols_per_frame: usize,
    /// Tokens per second of window, plus 16, beyond which the window is
    /// abandoned as a runaway; the token that crosses the budget is kept.
    /// German speech needs about ten.
    pub tokens_per_second: usize,
}

/// `max_symbols_per_frame` is `NeMo`'s default `max_symbols` of 10; the
/// token budget is this crate's own guard, sized so a 60 s chunk of fast
/// German (about ten tokens a second) is never cut.
impl Default for DecoderConfig {
    fn default() -> Self {
        DecoderConfig {
            max_symbols_per_frame: 10,
            tokens_per_second: 40,
        }
    }
}

/// Counters a transcription reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeStats {
    pub windows: usize,
    pub decoder_calls: usize,
    pub joint_calls: usize,
    pub recoveries_tried: usize,
    pub recoveries_accepted: usize,
    /// Windows abandoned at the token budget; their tail is truncated.
    pub runaways: usize,
}

impl DecodeStats {
    /// Adds another worker's counts.
    pub(crate) fn add(&mut self, other: &DecodeStats) {
        self.windows += other.windows;
        self.decoder_calls += other.decoder_calls;
        self.joint_calls += other.joint_calls;
        self.recoveries_tried += other.recoveries_tried;
        self.recoveries_accepted += other.recoveries_accepted;
        self.runaways += other.runaways;
    }
}

/// The joint's probability as a confidence: non-finite values are zero,
/// the rest clamped to `0..=1` (`TdtDurationMapping.clampProbability`).
#[must_use]
pub fn confidence(probability: f32) -> f32 {
    // NaN passes through `clamp`.
    if probability.is_finite() {
        probability.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Frames for the joint's duration bin `bin`; a bin outside the model's
/// table is a shape error.
fn duration(shape: &ModelShape, bin: usize) -> Result<usize, SpeechError> {
    shape.durations.get(bin).copied().ok_or_else(|| {
        SpeechError::Shape(format!(
            "duration bin {bin} outside the {} bins",
            shape.durations.len()
        ))
    })
}

/// Decodes `encoder` with a fresh prediction-network state, primed with
/// the blank as start of sequence; token frames are offset by
/// `frame_offset`, the window's first frame in the recording. A window
/// abandoned at the token budget is logged as a warning.
pub fn decode_window<B: SpeechBackend + ?Sized>(
    backend: &mut B,
    encoder: &EncoderOutput,
    frame_offset: usize,
    config: &DecoderConfig,
    stats: &mut DecodeStats,
) -> Result<Vec<Token>, SpeechError> {
    let encoder_hidden = backend.shape().encoder_hidden;
    if encoder.hidden != encoder_hidden {
        return Err(SpeechError::Shape(format!(
            "encoder frames are {} wide, the joint takes {encoder_hidden}",
            encoder.hidden
        )));
    }
    let frames = encoder.len.min(encoder.data.len() / encoder.hidden.max(1));
    stats.windows += 1;
    if frames == 0 {
        return Ok(Vec::new());
    }
    let shape = backend.shape();
    let blank = shape.blank_id;
    let zeros = DecoderState::zeros(shape.decoder_layers, shape.decoder_hidden);
    let mut step = backend.decoder_step(blank, &zeros)?;
    stats.decoder_calls += 1;
    let mut tokens = Vec::new();
    let mut t = 0;
    let mut symbols_here = 0;
    while t < frames {
        let decision = backend.joint_step(encoder.frame(t), &step.projection)?;
        stats.joint_calls += 1;
        let duration = duration(backend.shape(), decision.duration_bin)?;
        if decision.token == blank {
            t += duration.max(1);
            symbols_here = 0;
            continue;
        }
        let frame = t;
        // A zero duration holds the frame until the symbol limit forces
        // an advance of one.
        let advance = if duration == 0 && symbols_here + 1 < config.max_symbols_per_frame {
            0
        } else {
            duration.max(1)
        };
        t += advance;
        tokens.push(Token {
            id: decision.token,
            frame: frame_offset + frame,
            confidence: confidence(decision.probability),
            duration,
        });
        // 80 ms frames are 12.5 a second; dividing by 12 errs high.
        if tokens.len() > config.tokens_per_second * frames / 12 + 16 {
            stats.runaways += 1;
            tracing::warn!(
                frames,
                tokens = tokens.len(),
                frame_offset,
                "runaway decode: window abandoned at the token budget, its tail is lost"
            );
            break;
        }
        step = backend.decoder_step(decision.token, &step.state)?;
        stats.decoder_calls += 1;
        symbols_here = if advance > 0 { 0 } else { symbols_here + 1 };
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{DecoderStep, Features, JointDecision};

    /// A backend driven by a script of joint decisions, in call order; the
    /// decoder step records what it was fed and the state it was fed with,
    /// and returns that state with every `h` one higher.
    struct ScriptedBackend {
        shape: ModelShape,
        script: std::collections::VecDeque<JointDecision>,
        fed: Vec<u32>,
        states: Vec<DecoderState>,
    }

    impl ScriptedBackend {
        fn new(decisions: &[(u32, usize)]) -> Self {
            ScriptedBackend {
                shape: ModelShape {
                    vocab_size: 10,
                    blank_id: 9,
                    durations: vec![0, 1, 2, 3, 4],
                    decoder_layers: 1,
                    decoder_hidden: 2,
                    encoder_hidden: 1,
                },
                script: decisions
                    .iter()
                    .map(|&(token, duration_bin)| JointDecision {
                        token,
                        probability: 0.9,
                        duration_bin,
                    })
                    .collect(),
                fed: Vec::new(),
                states: Vec::new(),
            }
        }
    }

    impl SpeechBackend for ScriptedBackend {
        fn shape(&self) -> &ModelShape {
            &self.shape
        }
        fn features(&mut self, samples: &[f32]) -> Result<Features, SpeechError> {
            Ok(Features {
                mels: 1,
                frames: samples.len(),
                data: samples.to_vec(),
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
            state: &DecoderState,
        ) -> Result<DecoderStep, SpeechError> {
            self.fed.push(token);
            self.states.push(state.clone());
            Ok(DecoderStep {
                projection: vec![token as f32],
                state: DecoderState {
                    h: state.h.iter().map(|x| x + 1.0).collect(),
                    c: state.c.clone(),
                },
            })
        }
        fn joint_step(
            &mut self,
            _frame: &[f32],
            _projection: &[f32],
        ) -> Result<JointDecision, SpeechError> {
            Ok(self.script.pop_front().unwrap_or(JointDecision {
                token: 9,
                probability: 1.0,
                duration_bin: 1,
            }))
        }
    }

    fn encoder(frames: usize) -> EncoderOutput {
        EncoderOutput {
            hidden: 1,
            len: frames,
            data: vec![0.0; frames],
        }
    }

    /// `encoder` decoded under `config` from frame 0, with its counters.
    fn decode_with(
        backend: &mut ScriptedBackend,
        encoder: &EncoderOutput,
        config: &DecoderConfig,
    ) -> Result<(Vec<Token>, DecodeStats), SpeechError> {
        let mut stats = DecodeStats::default();
        let tokens = decode_window(backend, encoder, 0, config, &mut stats)?;
        Ok((tokens, stats))
    }

    /// `encoder` decoded with the default limits from frame 0.
    fn decode(
        backend: &mut ScriptedBackend,
        encoder: &EncoderOutput,
    ) -> Result<(Vec<Token>, DecodeStats), SpeechError> {
        decode_with(backend, encoder, &DecoderConfig::default())
    }

    #[test]
    fn blanks_advance_by_their_duration_and_tokens_stay_on_zero_durations() {
        // Frame 0: blank, skip 2. Frame 2: token 1 dur 0, token 2 dur 1.
        // Frame 3: blank dur 0 (forced to 1). Frame 4: token 3 dur 3 -> 7 > len.
        let mut backend = ScriptedBackend::new(&[(9, 2), (1, 0), (2, 1), (9, 0), (3, 3)]);
        let mut stats = DecodeStats::default();
        let tokens = decode_window(
            &mut backend,
            &encoder(6),
            100,
            &DecoderConfig::default(),
            &mut stats,
        )
        .unwrap();
        assert_eq!(
            tokens
                .iter()
                .map(|t| (t.id, t.frame, t.duration))
                .collect::<Vec<_>>(),
            vec![(1, 102, 0), (2, 102, 1), (3, 104, 3)]
        );
        // SOS priming, then one feed per token.
        assert_eq!(backend.fed, vec![9, 1, 2, 3]);
        // Priming starts from zeros of `layers * hidden`; each feed takes
        // the state the step before it returned.
        assert_eq!(
            backend
                .states
                .iter()
                .map(|s| s.h.clone())
                .collect::<Vec<_>>(),
            vec![vec![0.0; 2], vec![1.0; 2], vec![2.0; 2], vec![3.0; 2]]
        );
        assert_eq!(stats.joint_calls, 5);
        assert_eq!(stats.decoder_calls, 4);
        assert_eq!(stats.windows, 1);
    }

    #[test]
    fn a_frame_gives_up_after_the_symbol_limit() {
        let script: Vec<(u32, usize)> = (0..12).map(|_| (1, 0)).collect();
        let mut backend = ScriptedBackend::new(&script);
        let config = DecoderConfig {
            max_symbols_per_frame: 3,
            ..DecoderConfig::default()
        };
        let (tokens, _) = decode_with(&mut backend, &encoder(2), &config).unwrap();
        // Three on frame 0, three on frame 1, then the window ends; each
        // records the predicted duration zero, the forced advance included.
        assert_eq!(
            tokens
                .iter()
                .map(|t| (t.frame, t.duration))
                .collect::<Vec<_>>(),
            vec![(0, 0), (0, 0), (0, 0), (1, 0), (1, 0), (1, 0)]
        );
    }

    #[test]
    fn the_default_limits_are_nemos_with_a_per_second_budget() {
        assert_eq!(
            DecoderConfig::default(),
            DecoderConfig {
                max_symbols_per_frame: 10,
                tokens_per_second: 40,
            }
        );
    }

    #[test]
    fn frames_past_the_encoder_data_are_not_decoded() {
        // `len` claims five frames, the data holds two.
        let mut backend = ScriptedBackend::new(&[(1, 1), (2, 1), (3, 1)]);
        let short = EncoderOutput {
            hidden: 1,
            len: 5,
            data: vec![0.0; 2],
        };
        let (tokens, stats) = decode(&mut backend, &short).unwrap();
        assert_eq!(
            tokens.iter().map(|t| (t.id, t.frame)).collect::<Vec<_>>(),
            vec![(1, 0), (2, 1)]
        );
        assert_eq!(stats.joint_calls, 2);
    }

    #[test]
    fn an_empty_window_decodes_nothing() {
        let mut backend = ScriptedBackend::new(&[(1, 7)]);
        let (tokens, stats) = decode(&mut backend, &encoder(0)).unwrap();
        assert!(tokens.is_empty());
        // Counted as a window, with no model call at all.
        assert_eq!(
            (stats.windows, stats.decoder_calls, stats.joint_calls),
            (1, 0, 0)
        );
        assert!(backend.fed.is_empty());
    }

    #[test]
    fn a_duration_bin_outside_the_table_is_a_shape_error() {
        let mut backend = ScriptedBackend::new(&[(1, 7)]);
        assert!(matches!(
            decode(&mut backend, &encoder(1)),
            Err(SpeechError::Shape(_))
        ));
    }

    #[test]
    fn a_probability_is_clamped_to_a_confidence_and_a_non_finite_one_is_zero() {
        assert_eq!(confidence(1.5), 1.0);
        assert_eq!(confidence(-0.5), 0.0);
        assert_eq!(confidence(f32::INFINITY), 0.0);
        assert_eq!(confidence(f32::NEG_INFINITY), 0.0);
        assert_eq!(confidence(f32::NAN), 0.0);
        assert_eq!(confidence(0.3), 0.3);
    }

    #[test]
    fn a_frame_width_the_joint_does_not_take_is_a_shape_error() {
        // Two-wide frames for a joint that takes one: slicing by the model's
        // width would read past the data, by the frames' width the joint
        // would get the wrong input.
        let mut backend = ScriptedBackend::new(&[]);
        let wide = EncoderOutput {
            hidden: 2,
            len: 3,
            data: vec![0.0; 6],
        };
        assert!(matches!(
            decode(&mut backend, &wide),
            Err(SpeechError::Shape(_))
        ));
    }

    #[test]
    fn a_probability_that_is_not_a_number_is_zero_confidence() {
        let mut backend = ScriptedBackend::new(&[(1, 1)]);
        backend.script[0].probability = f32::NAN;
        let (tokens, _) = decode(&mut backend, &encoder(1)).unwrap();
        assert_eq!(tokens[0].confidence, 0.0);
    }

    #[test]
    fn a_runaway_window_stops_at_the_budget() {
        let script: Vec<(u32, usize)> = (0..1000).map(|_| (1, 0)).collect();
        let mut backend = ScriptedBackend::new(&script);
        let config = DecoderConfig {
            max_symbols_per_frame: 1000,
            tokens_per_second: 12,
        };
        let (tokens, stats) = decode_with(&mut backend, &encoder(12), &config).unwrap();
        // The budget is 12 * 12 / 12 + 16; the token that crosses it is kept.
        assert_eq!(tokens.len(), 12 * 12 / 12 + 16 + 1);
        assert_eq!(stats.runaways, 1);
    }
}
