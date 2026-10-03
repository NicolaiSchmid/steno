//! The ONNX Runtime backend over our fp32 export of Parakeet TDT 0.6B v3
//! (`spikes/onnx-speech/export/`): `encoder.onnx` with its weights in
//! `encoder.weights` next to it, `decoder.onnx`, `joiner.onnx` and
//! `tokens.txt`. Sessions are opened from the file path, so ONNX Runtime
//! resolves the external weights against the model's directory and no
//! working-directory dance is needed. The model shape is read from the
//! graphs: the vocabulary from `tokens.txt`, the duration bins as the
//! joiner's output width minus the vocabulary (8198 - 8193 = 5), the
//! prediction network from the decoder's state inputs; the export's
//! `vocab_size` metadata (8192, without the blank) is checked against the
//! vocabulary and never used as the blank id, the mistake behind the 64 %
//! WER spike F measured for the spike E loop.
//! Swift: none on this path; the Mac runs `FluidAudio`'s `CoreML` models.
//!
//! Privacy invariant: ONNX Runtime's telemetry is off in every process
//! that opens a session, `steno-speech-sidecar` included. Every session
//! opens through one function here, which configures the process-wide
//! environment with telemetry disabled before the first one. A
//! Microsoft-built ONNX Runtime library would otherwise report model and
//! usage details to Microsoft.

use std::path::Path;
use std::sync::Once;

use ort::session::builder::GraphOptimizationLevel;
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorElementType, ValueType};

use crate::backend::{
    DecoderState, DecoderStep, EncoderOutput, Features, JointDecision, ModelShape, SpeechBackend,
    split_logits,
};
use crate::error::SpeechError;
use crate::features::MelExtractor;
use crate::vocab::Vocab;

/// Session options shared by every model. Each of the four sessions
/// (encoder, decoder, joiner, Silero) gets its own intra-op pool of this
/// size, and ONNX Runtime spins its threads briefly after each run; only
/// one session runs at a time, so the pools do not compete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnnxOptions {
    /// Threads inside one operator; four, as the speech-stack plan measured
    /// (decision 3).
    pub intra_threads: usize,
    /// Parallel operators; one, the graphs are sequential.
    pub inter_threads: usize,
}

impl Default for OnnxOptions {
    fn default() -> Self {
        OnnxOptions {
            intra_threads: 4,
            inter_threads: 1,
        }
    }
}

/// Configures ONNX Runtime's process-wide environment once, before the
/// first session: telemetry off (the privacy invariant in the module
/// docs). The first configuration committed wins, so nothing in a Steno
/// process opens a session any other way.
pub(crate) fn init_environment() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // `false` when `steno-diarize` committed the same settings first.
        let _ = ort::init().with_telemetry(false).commit();
    });
}

/// Opens one model file with the shared options, in the environment
/// [`init_environment`] configures.
pub(crate) fn open_session(path: &Path, options: &OnnxOptions) -> Result<Session, SpeechError> {
    init_environment();
    let options_error = |e: ort::Error<ort::session::builder::SessionBuilder>| {
        SpeechError::SessionOptions(e.to_string())
    };
    let mut builder = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(options_error)?
        .with_intra_threads(options.intra_threads.max(1))
        .map_err(options_error)?
        .with_inter_threads(options.inter_threads.max(1))
        .map_err(options_error)?;
    Ok(builder.commit_from_file(path)?)
}

/// The element type and declared shape of a tensor outlet (`-1` for a
/// dynamic axis); `None` for sequences and maps.
pub(crate) fn outlet_tensor(value: &ValueType) -> Option<(TensorElementType, Vec<i64>)> {
    match value {
        ValueType::Tensor { ty, shape, .. } => Some((*ty, shape.to_vec())),
        _ => None,
    }
}

/// An integer tensor in the element type the model declares.
fn int_tensor(
    ty: TensorElementType,
    shape: Vec<i64>,
    values: &[i64],
) -> Result<SessionInputValue<'static>, SpeechError> {
    match ty {
        TensorElementType::Int64 => Ok(Tensor::from_array((shape, values.to_vec()))?.into()),
        TensorElementType::Int32 => {
            let narrowed: Vec<i32> = values
                .iter()
                .map(|&v| {
                    i32::try_from(v).map_err(|_| {
                        SpeechError::Shape(format!("{v} does not fit the model's int32 input"))
                    })
                })
                .collect::<Result<_, _>>()?;
            Ok(Tensor::from_array((shape, narrowed))?.into())
        }
        other => Err(SpeechError::Shape(format!(
            "integer input declared as {other:?}"
        ))),
    }
}

fn f32_tensor(
    shape: Vec<i64>,
    values: Vec<f32>,
) -> Result<SessionInputValue<'static>, SpeechError> {
    Ok(Tensor::from_array((shape, values))?.into())
}

/// Reads a length output of either integer width.
fn extract_length(value: &ort::value::DynValue) -> Result<usize, SpeechError> {
    let first = match value.try_extract_tensor::<i64>() {
        Ok((_, data)) => data.first().copied().unwrap_or(0),
        Err(_) => i64::from(
            value
                .try_extract_tensor::<i32>()?
                .1
                .first()
                .copied()
                .unwrap_or(0),
        ),
    };
    Ok(usize::try_from(first).unwrap_or(0))
}

fn dimension(shape: &[i64], axis: usize) -> Option<usize> {
    shape
        .get(axis)
        .and_then(|&d| usize::try_from(d).ok())
        .filter(|&d| d > 0)
}

struct Inputs {
    names: Vec<String>,
    types: Vec<TensorElementType>,
    shapes: Vec<Vec<i64>>,
}

fn inputs(session: &Session, model: &str, expected: usize) -> Result<Inputs, SpeechError> {
    let outlets = session.inputs();
    if outlets.len() != expected {
        return Err(SpeechError::Shape(format!(
            "{model} has {} inputs, expected {expected}",
            outlets.len()
        )));
    }
    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut shapes = Vec::new();
    for outlet in outlets {
        let (ty, shape) = outlet_tensor(outlet.dtype()).ok_or_else(|| {
            SpeechError::Shape(format!("{model} input {} is not a tensor", outlet.name()))
        })?;
        names.push(outlet.name().to_owned());
        types.push(ty);
        shapes.push(shape);
    }
    Ok(Inputs {
        names,
        types,
        shapes,
    })
}

/// The three sessions and the preprocessor.
pub struct OnnxBackend {
    encoder: Session,
    decoder: Session,
    joiner: Session,
    mel: MelExtractor,
    shape: ModelShape,
    encoder_inputs: Inputs,
    decoder_inputs: Inputs,
    joiner_inputs: Inputs,
}

impl OnnxBackend {
    /// The files an export directory holds.
    pub const FILES: [&'static str; 5] = [
        "encoder.onnx",
        "encoder.weights",
        "decoder.onnx",
        "joiner.onnx",
        "tokens.txt",
    ];

    /// Loads the export in `directory` and returns the backend with its
    /// vocabulary.
    pub fn load(directory: &Path, options: &OnnxOptions) -> Result<(Self, Vocab), SpeechError> {
        let vocab = Vocab::load(&directory.join("tokens.txt"))?;
        let encoder = open_session(&directory.join("encoder.onnx"), options)?;
        let decoder = open_session(&directory.join("decoder.onnx"), options)?;
        let mut joiner = open_session(&directory.join("joiner.onnx"), options)?;
        let encoder_inputs = inputs(&encoder, "encoder", 2)?;
        let decoder_inputs = inputs(&decoder, "decoder", 4)?;
        let joiner_inputs = inputs(&joiner, "joiner", 2)?;

        let (declared_vocab, declared_layers, declared_hidden) = {
            let metadata = encoder.metadata()?;
            let custom = |key: &str| {
                metadata
                    .custom(key)
                    .and_then(|v| v.trim().parse::<usize>().ok())
            };
            (
                custom("vocab_size"),
                custom("pred_rnn_layers"),
                custom("pred_hidden"),
            )
        };
        if let Some(declared) = declared_vocab
            && declared != vocab.len()
            && declared + 1 != vocab.len()
        {
            return Err(SpeechError::Shape(format!(
                "metadata vocab_size {declared} does not fit tokens.txt with {} pieces",
                vocab.len()
            )));
        }
        let state_shape = &decoder_inputs.shapes[2];
        let decoder_layers = declared_layers
            .or_else(|| dimension(state_shape, 0))
            .ok_or_else(|| SpeechError::Shape("decoder layers unknown".into()))?;
        let decoder_hidden = declared_hidden
            .or_else(|| dimension(state_shape, 2))
            .ok_or_else(|| SpeechError::Shape("decoder hidden size unknown".into()))?;
        let encoder_hidden = dimension(&joiner_inputs.shapes[0], 1)
            .ok_or_else(|| SpeechError::Shape("encoder output width unknown".into()))?;
        let declared_width = joiner
            .outputs()
            .first()
            .and_then(|o| outlet_tensor(o.dtype()))
            .and_then(|(_, shape)| dimension(&shape, shape.len().checked_sub(1)?));
        let joint_width = if let Some(width) = declared_width {
            width
        } else {
            // Dynamic last axis: one probe run tells.
            let outputs = joiner.run(vec![
                (
                    joiner_inputs.names[0].as_str(),
                    f32_tensor(vec![1, encoder_hidden as i64, 1], vec![0.0; encoder_hidden])?,
                ),
                (
                    joiner_inputs.names[1].as_str(),
                    f32_tensor(vec![1, decoder_hidden as i64, 1], vec![0.0; decoder_hidden])?,
                ),
            ])?;
            outputs[0].try_extract_tensor::<f32>()?.1.len()
        };
        if joint_width <= vocab.len() || joint_width - vocab.len() > 8 {
            return Err(SpeechError::Shape(format!(
                "joiner emits {joint_width} logits for {} pieces; expected the pieces plus 1 to 8 duration bins",
                vocab.len()
            )));
        }
        let shape = ModelShape {
            vocab_size: vocab.len(),
            blank_id: vocab.blank_id(),
            durations: (0..joint_width - vocab.len()).collect(),
            decoder_layers,
            decoder_hidden,
            encoder_hidden,
        };
        Ok((
            OnnxBackend {
                encoder,
                decoder,
                joiner,
                mel: MelExtractor::new(),
                shape,
                encoder_inputs,
                decoder_inputs,
                joiner_inputs,
            },
            vocab,
        ))
    }
}

impl SpeechBackend for OnnxBackend {
    fn shape(&self) -> &ModelShape {
        &self.shape
    }

    fn features(&mut self, samples: &[f32]) -> Result<Features, SpeechError> {
        Ok(self.mel.features(samples))
    }

    fn encode(&mut self, features: &Features) -> Result<EncoderOutput, SpeechError> {
        if features.frames == 0 {
            return Ok(EncoderOutput {
                hidden: self.shape.encoder_hidden,
                len: 0,
                data: Vec::new(),
            });
        }
        let frames = i64::try_from(features.frames)
            .map_err(|_| SpeechError::Shape("window too long".into()))?;
        let audio = f32_tensor(vec![1, features.mels as i64, frames], features.data.clone())?;
        let length = int_tensor(self.encoder_inputs.types[1], vec![1], &[frames])?;
        let outputs = self.encoder.run(vec![
            (self.encoder_inputs.names[0].as_str(), audio),
            (self.encoder_inputs.names[1].as_str(), length),
        ])?;
        if outputs.len() < 2 {
            return Err(SpeechError::Shape(format!(
                "encoder has {} outputs, expected 2",
                outputs.len()
            )));
        }
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        let (Some(hidden), Some(frames_out)) = (dimension(shape, 1), dimension(shape, 2)) else {
            return Err(SpeechError::Shape(format!(
                "encoder output shape {shape:?}"
            )));
        };
        let len = extract_length(&outputs[1])?.min(frames_out);
        // [1, hidden, frames] to frame-major.
        let mut frame_major = vec![0.0f32; len * hidden];
        for (h, row) in data.chunks_exact(frames_out).enumerate().take(hidden) {
            for (t, &x) in row.iter().enumerate().take(len) {
                frame_major[t * hidden + h] = x;
            }
        }
        Ok(EncoderOutput {
            hidden,
            len,
            data: frame_major,
        })
    }

    fn decoder_step(
        &mut self,
        token: u32,
        state: &DecoderState,
    ) -> Result<DecoderStep, SpeechError> {
        let layers = self.shape.decoder_layers as i64;
        let hidden = self.shape.decoder_hidden as i64;
        let outputs = self.decoder.run(vec![
            (
                self.decoder_inputs.names[0].as_str(),
                int_tensor(
                    self.decoder_inputs.types[0],
                    vec![1, 1],
                    &[i64::from(token)],
                )?,
            ),
            (
                self.decoder_inputs.names[1].as_str(),
                int_tensor(self.decoder_inputs.types[1], vec![1], &[1])?,
            ),
            (
                self.decoder_inputs.names[2].as_str(),
                f32_tensor(vec![layers, 1, hidden], state.h.clone())?,
            ),
            (
                self.decoder_inputs.names[3].as_str(),
                f32_tensor(vec![layers, 1, hidden], state.c.clone())?,
            ),
        ])?;
        if outputs.len() < 4 {
            return Err(SpeechError::Shape(format!(
                "decoder has {} outputs, expected 4",
                outputs.len()
            )));
        }
        let projection = outputs[0].try_extract_tensor::<f32>()?.1.to_vec();
        let h = outputs[2].try_extract_tensor::<f32>()?.1.to_vec();
        let c = outputs[3].try_extract_tensor::<f32>()?.1.to_vec();
        Ok(DecoderStep {
            projection,
            state: DecoderState { h, c },
        })
    }

    fn joint_step(
        &mut self,
        encoder_frame: &[f32],
        projection: &[f32],
    ) -> Result<JointDecision, SpeechError> {
        let outputs = self.joiner.run(vec![
            (
                self.joiner_inputs.names[0].as_str(),
                f32_tensor(
                    vec![1, encoder_frame.len() as i64, 1],
                    encoder_frame.to_vec(),
                )?,
            ),
            (
                self.joiner_inputs.names[1].as_str(),
                f32_tensor(vec![1, projection.len() as i64, 1], projection.to_vec())?,
            ),
        ])?;
        if outputs.len() < 1 {
            return Err(SpeechError::Shape("joiner returned no output".into()));
        }
        let (_, logits) = outputs[0].try_extract_tensor::<f32>()?;
        split_logits(logits, self.shape.vocab_size)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn telemetry_is_switched_off_before_any_session() {
        super::init_environment();
        // Committed already: a later configuration cannot switch it on.
        assert!(!ort::init().with_telemetry(true).commit());
    }
}
