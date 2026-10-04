//! The greedy TDT decode of one encoder window, the one loop both backends
//! run (plan invariant 4). Per frame the joint runs on the current
//! projection; a blank advances by its duration (at least one frame); a
//! token is emitted at the frame, fed to the prediction network, and
//! advances by its duration, where a zero duration keeps the frame for up
//! to `max_symbols_per_frame` symbols before a forced advance of one.
//!
//! [`DecoderConfig::default`] is `NeMo`'s reference semantics
//! (`GreedyTDTInfer`), which sherpa-onnx matched at 5.3 % WER in spike F,
//! and what the ONNX pipeline runs ([`decode_window`]). The other variants
//! of [`TokenBudget`], [`WindowEnd`] and [`TokenDuration`] are `FluidAudio`'s
//! guards, which `steno-speech-coreml` sets (`FLUID_AUDIO`) for parity with
//! the Swift app; the steps it keeps around the loop are listed in its
//! decoder module. The WP4 notes in the plan list where the two
//! configurations differ.
//!
//! The model side is [`TdtModel`]: the prediction network and the joint
//! over one window's encoder frames, which [`decode_window`] builds over a
//! [`SpeechBackend`] and `steno-speech-coreml` over its models
//! (`WindowModel`).
//! Swift: `FluidAudio`'s `TdtDecoderV3`, ported in
//! `spikes/coreml-rs/src/decoder.rs`.

use crate::backend::{DecoderState, DecoderStep, EncoderOutput, JointDecision, SpeechBackend};
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
    /// The TDT duration in frames: the model's prediction, or the frames
    /// the loop advanced under [`TokenDuration::Advanced`].
    pub duration: usize,
}

/// When the loop stops a window for emitting too many tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenBudget {
    /// This many tokens per second of window, plus 16, beyond which the
    /// window is abandoned as a runaway; the token that crosses the
    /// budget is kept. German speech needs about ten.
    PerSecond(usize),
    /// At most this many tokens per window; the next one ends the window
    /// and is not emitted (`FluidAudio`'s `maxTokensPerChunk`).
    PerWindow(usize),
}

/// What happens to a token whose duration reaches the end of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEnd {
    /// It is emitted, as `NeMo` does; the merge owns the overlap.
    Emit,
    /// It is dropped and the window ends, as `FluidAudio` does.
    Drop,
}

/// Which duration a token records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenDuration {
    /// The model's prediction, zero included.
    Predicted,
    /// The frames the loop advanced after it: the prediction, or one when
    /// the symbol limit forced the advance (`FluidAudio` rewrites the
    /// duration it acts on).
    Advanced,
}

/// Limits of the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderConfig {
    /// Symbols with duration zero allowed on one frame before the loop
    /// moves on (`NeMo`'s `max_symbols`).
    pub max_symbols_per_frame: usize,
    /// When a window that emits too many tokens is abandoned.
    pub token_budget: TokenBudget,
    /// What happens to a token that advances past the window's end.
    pub window_end: WindowEnd,
    /// Which duration each [`Token`] records.
    pub token_duration: TokenDuration,
}

/// `max_symbols_per_frame` is `NeMo`'s default `max_symbols` of 10; the
/// token budget is this crate's own guard, sized so a 60 s chunk of fast
/// German (about ten tokens a second) is never cut.
impl Default for DecoderConfig {
    fn default() -> Self {
        DecoderConfig {
            max_symbols_per_frame: 10,
            token_budget: TokenBudget::PerSecond(40),
            window_end: WindowEnd::Emit,
            token_duration: TokenDuration::Predicted,
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

/// The prediction network and the joint over one window's encoder frames:
/// the calls the loop makes, with the backend's own error.
pub trait TdtModel {
    type Error;

    /// The blank's id.
    fn blank_id(&self) -> u32;

    /// Frames for the joint's duration bin `bin`; a bin outside the
    /// model's table is the backend's error.
    fn duration(&self, bin: usize) -> Result<usize, Self::Error>;

    /// Resets the prediction network and primes it with the blank as start
    /// of sequence.
    fn start(&mut self) -> Result<(), Self::Error>;

    /// Feeds an emitted token to the prediction network.
    fn feed(&mut self, token: u32) -> Result<(), Self::Error>;

    /// Runs the joint over encoder frame `t` of the window and the
    /// projection of the last fed token.
    fn joint(&mut self, t: usize) -> Result<JointDecision, Self::Error>;
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

/// What [`decode_frames`] decoded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decoded {
    /// The emitted tokens, in order, with frames counted from the start of
    /// the recording.
    pub tokens: Vec<Token>,
    /// The window frame the loop reached: at or past the decoded frames,
    /// or, when the token budget ended the window, the frame that token's
    /// advance reaches. `FluidAudio`'s tail flush starts here.
    pub stop_frame: usize,
}

/// Decodes the first `frames` encoder frames of `model`'s window with a
/// fresh prediction-network state; token frames are offset by
/// `frame_offset`, the window's first frame in the recording.
pub fn decode_frames<M: TdtModel + ?Sized>(
    model: &mut M,
    frames: usize,
    frame_offset: usize,
    config: &DecoderConfig,
    stats: &mut DecodeStats,
) -> Result<Decoded, M::Error> {
    stats.windows += 1;
    if frames == 0 {
        return Ok(Decoded::default());
    }
    let mut tokens = Vec::new();
    let blank = model.blank_id();
    model.start()?;
    stats.decoder_calls += 1;
    let mut t = 0;
    let mut symbols_here = 0;
    while t < frames {
        let decision = model.joint(t)?;
        stats.joint_calls += 1;
        let duration = model.duration(decision.duration_bin)?;
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
        if config.window_end == WindowEnd::Drop && t >= frames {
            break;
        }
        if let TokenBudget::PerWindow(max) = config.token_budget
            && tokens.len() >= max
        {
            stats.runaways += 1;
            break;
        }
        tokens.push(Token {
            id: decision.token,
            frame: frame_offset + frame,
            confidence: confidence(decision.probability),
            duration: match config.token_duration {
                TokenDuration::Predicted => duration,
                TokenDuration::Advanced => advance,
            },
        });
        // 80 ms frames are 12.5 a second; dividing by 12 errs high.
        if let TokenBudget::PerSecond(per_second) = config.token_budget
            && tokens.len() > per_second * frames / 12 + 16
        {
            stats.runaways += 1;
            break;
        }
        model.feed(decision.token)?;
        stats.decoder_calls += 1;
        symbols_here = if advance > 0 { 0 } else { symbols_here + 1 };
    }
    Ok(Decoded {
        tokens,
        stop_frame: t,
    })
}

/// [`TdtModel`] over a [`SpeechBackend`] and one window's encoder output.
struct BackendWindow<'a, B: ?Sized> {
    backend: &'a mut B,
    encoder: &'a EncoderOutput,
    /// The projection of the last fed token and the state after it.
    step: DecoderStep,
}

impl<B: SpeechBackend + ?Sized> TdtModel for BackendWindow<'_, B> {
    type Error = SpeechError;

    fn blank_id(&self) -> u32 {
        self.backend.shape().blank_id
    }

    fn duration(&self, bin: usize) -> Result<usize, SpeechError> {
        let durations = &self.backend.shape().durations;
        durations.get(bin).copied().ok_or_else(|| {
            SpeechError::Shape(format!(
                "duration bin {bin} outside the {} bins",
                durations.len()
            ))
        })
    }

    fn start(&mut self) -> Result<(), SpeechError> {
        let shape = self.backend.shape();
        let zeros = DecoderState::zeros(shape.decoder_layers, shape.decoder_hidden);
        let blank = shape.blank_id;
        self.step = self.backend.decoder_step(blank, &zeros)?;
        Ok(())
    }

    fn feed(&mut self, token: u32) -> Result<(), SpeechError> {
        self.step = self.backend.decoder_step(token, &self.step.state)?;
        Ok(())
    }

    fn joint(&mut self, t: usize) -> Result<JointDecision, SpeechError> {
        self.backend
            .joint_step(self.encoder.frame(t), &self.step.projection)
    }
}

/// Decodes `encoder` with a fresh prediction-network state; token frames
/// are offset by `frame_offset`, the window's first frame in the recording.
/// A window abandoned at the token budget is logged as a warning.
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
    let mut window = BackendWindow {
        backend,
        encoder,
        // Replaced by `start` before the first joint.
        step: DecoderStep {
            projection: Vec::new(),
            state: DecoderState::zeros(0, 0),
        },
    };
    let runaways = stats.runaways;
    let tokens = decode_frames(&mut window, frames, frame_offset, config, stats)?.tokens;
    if stats.runaways > runaways {
        tracing::warn!(
            frames,
            tokens = tokens.len(),
            frame_offset,
            "runaway decode: window abandoned at the token budget, its tail is lost"
        );
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{DecoderStep, Features, JointDecision, ModelShape};

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

    /// `encoder` decoded with the default limits from frame 0.
    fn decode(
        backend: &mut ScriptedBackend,
        encoder: &EncoderOutput,
    ) -> Result<Vec<Token>, SpeechError> {
        decode_window(
            backend,
            encoder,
            0,
            &DecoderConfig::default(),
            &mut DecodeStats::default(),
        )
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
        let tokens = decode_window(
            &mut backend,
            &encoder(2),
            0,
            &config,
            &mut DecodeStats::default(),
        )
        .unwrap();
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
    fn the_default_limits_are_nemos() {
        assert_eq!(
            DecoderConfig::default(),
            DecoderConfig {
                max_symbols_per_frame: 10,
                token_budget: TokenBudget::PerSecond(40),
                window_end: WindowEnd::Emit,
                token_duration: TokenDuration::Predicted,
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
        let mut stats = DecodeStats::default();
        let tokens = decode_window(
            &mut backend,
            &short,
            0,
            &DecoderConfig::default(),
            &mut stats,
        )
        .unwrap();
        assert_eq!(
            tokens.iter().map(|t| (t.id, t.frame)).collect::<Vec<_>>(),
            vec![(1, 0), (2, 1)]
        );
        assert_eq!(stats.joint_calls, 2);
    }

    #[test]
    fn an_empty_window_decodes_nothing_and_a_bad_duration_bin_is_a_shape_error() {
        let mut backend = ScriptedBackend::new(&[(1, 7)]);
        let mut stats = DecodeStats::default();
        let tokens = decode_window(
            &mut backend,
            &encoder(0),
            0,
            &DecoderConfig::default(),
            &mut stats,
        )
        .unwrap();
        assert!(tokens.is_empty());
        // Counted as a window, with no model call at all.
        assert_eq!(
            (stats.windows, stats.decoder_calls, stats.joint_calls),
            (1, 0, 0)
        );
        assert!(backend.fed.is_empty());
        assert!(matches!(
            decode(&mut backend, &encoder(1)),
            Err(SpeechError::Shape(_))
        ));
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
        let tokens = decode(&mut backend, &encoder(1)).unwrap();
        assert_eq!(tokens[0].confidence, 0.0);
    }

    #[test]
    fn a_runaway_window_stops_at_the_budget() {
        let script: Vec<(u32, usize)> = (0..1000).map(|_| (1, 0)).collect();
        let mut backend = ScriptedBackend::new(&script);
        let config = DecoderConfig {
            max_symbols_per_frame: 1000,
            token_budget: TokenBudget::PerSecond(12),
            ..DecoderConfig::default()
        };
        let mut stats = DecodeStats::default();
        let tokens = decode_window(&mut backend, &encoder(12), 0, &config, &mut stats).unwrap();
        // The budget is 12 * 12 / 12 + 16; the token that crosses it is kept.
        assert_eq!(tokens.len(), 12 * 12 / 12 + 16 + 1);
        assert_eq!(stats.runaways, 1);
    }
}
