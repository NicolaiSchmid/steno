//! The greedy TDT decode of one encoder window: `NeMo`'s reference semantics
//! (`GreedyTDTInfer`), which sherpa-onnx matched at 5.3 % WER in spike F.
//! Per frame the joint runs on the current projection; a blank advances by
//! its duration (at least one frame); a token is emitted at the frame,
//! fed to the prediction network, and advances by its duration, where a
//! zero duration keeps the frame for up to `max_symbols_per_frame` symbols
//! before a forced advance. `FluidAudio`'s extra guards (one symbol per frame
//! before forcing an advance, the tail pass over the last window) live in
//! `crates/steno-speech-coreml/src/decoder.rs`, not here; the WP4 notes in
//! the plan list where the two loops differ.
//! Swift: `FluidAudio`'s `TdtDecoderV3`, ported in
//! `spikes/coreml-rs/src/decoder.rs`.

use crate::backend::{DecoderState, EncoderOutput, SpeechBackend};
use crate::error::SpeechError;

/// One emitted token: a `SentencePiece` piece with its frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Token {
    pub id: u32,
    /// The encoder frame (80 ms) the token was emitted at, counted from the
    /// start of the recording.
    pub frame: usize,
    /// The joint's probability for the piece.
    pub confidence: f32,
    /// The TDT duration in frames the model attached to it.
    pub duration: usize,
}

/// Limits of the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderConfig {
    /// Symbols with duration zero allowed on one frame before the loop
    /// moves on (`NeMo`'s `max_symbols`).
    pub max_symbols_per_frame: usize,
    /// Tokens per second of window beyond which the window is abandoned
    /// as a runaway, plus 16; the token that crosses the budget is kept.
    /// German speech needs about ten.
    pub max_tokens_per_second: usize,
}

/// `max_symbols_per_frame` is `NeMo`'s default `max_symbols` of 10; the
/// token budget is this crate's own guard, sized so a 60 s chunk of fast
/// German (about ten tokens a second) is never cut.
impl Default for DecoderConfig {
    fn default() -> Self {
        DecoderConfig {
            max_symbols_per_frame: 10,
            max_tokens_per_second: 40,
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

/// Decodes `encoder` with a fresh prediction-network state; token frames
/// are offset by `frame_offset`, the window's first frame in the recording.
pub fn decode_window<B: SpeechBackend + ?Sized>(
    backend: &mut B,
    encoder: &EncoderOutput,
    frame_offset: usize,
    config: &DecoderConfig,
    stats: &mut DecodeStats,
) -> Result<Vec<Token>, SpeechError> {
    let shape = backend.shape().clone();
    if encoder.hidden != shape.encoder_hidden {
        return Err(SpeechError::Shape(format!(
            "encoder frames are {} wide, the joint takes {}",
            encoder.hidden, shape.encoder_hidden
        )));
    }
    let len = encoder.len.min(encoder.data.len() / encoder.hidden.max(1));
    let mut tokens = Vec::new();
    stats.windows += 1;
    if len == 0 {
        return Ok(tokens);
    }
    // 80 ms frames are 12.5 a second; dividing by 12 errs high.
    let budget = config.max_tokens_per_second * len / 12 + 16;
    let mut decoder_state = DecoderState::zeros(shape.decoder_layers, shape.decoder_hidden);
    let mut step = backend.decoder_step(shape.blank_id, &decoder_state)?;
    stats.decoder_calls += 1;
    let mut t = 0;
    let mut symbols_here = 0;
    while t < len {
        let decision = backend.joint_step(encoder.frame(t), &step.projection)?;
        stats.joint_calls += 1;
        let duration = *shape.durations.get(decision.duration_bin).ok_or_else(|| {
            SpeechError::Shape(format!(
                "duration bin {} outside the {} bins",
                decision.duration_bin,
                shape.durations.len()
            ))
        })?;
        if decision.token == shape.blank_id {
            t += duration.max(1);
            symbols_here = 0;
            continue;
        }
        tokens.push(Token {
            id: decision.token,
            frame: frame_offset + t,
            // NaN passes through `clamp`; a non-finite probability is zero.
            confidence: if decision.probability.is_finite() {
                decision.probability.clamp(0.0, 1.0)
            } else {
                0.0
            },
            duration,
        });
        if tokens.len() > budget {
            stats.runaways += 1;
            tracing::warn!(
                frames = len,
                tokens = tokens.len(),
                frame_offset,
                "runaway decode: window abandoned at the token budget, its tail is lost"
            );
            break;
        }
        decoder_state = step.state;
        step = backend.decoder_step(decision.token, &decoder_state)?;
        stats.decoder_calls += 1;
        symbols_here += 1;
        if duration > 0 {
            t += duration;
            symbols_here = 0;
        } else if symbols_here >= config.max_symbols_per_frame {
            t += 1;
            symbols_here = 0;
        }
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{DecoderStep, Features, JointDecision, ModelShape};

    /// A backend driven by a script of joint decisions, in call order; the
    /// decoder step records what it was fed.
    struct ScriptedBackend {
        shape: ModelShape,
        script: std::collections::VecDeque<JointDecision>,
        fed: Vec<u32>,
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
        // Three on frame 0, three on frame 1, then the window ends.
        assert_eq!(
            tokens.iter().map(|t| t.frame).collect::<Vec<_>>(),
            vec![0, 0, 0, 1, 1, 1]
        );
    }

    #[test]
    fn an_empty_window_decodes_nothing_and_a_bad_duration_bin_is_a_shape_error() {
        let mut backend = ScriptedBackend::new(&[(1, 7)]);
        assert!(decode(&mut backend, &encoder(0)).unwrap().is_empty());
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
            max_tokens_per_second: 12,
        };
        let mut stats = DecodeStats::default();
        let tokens = decode_window(&mut backend, &encoder(12), 0, &config, &mut stats).unwrap();
        // The budget is 12 * 12 / 12 + 16; the token that crosses it is kept.
        assert_eq!(tokens.len(), 12 * 12 / 12 + 16 + 1);
        assert_eq!(stats.runaways, 1);
    }
}
