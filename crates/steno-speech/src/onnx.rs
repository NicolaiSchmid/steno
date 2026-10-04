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
//! opens after [`init_environment`], which configures the process-wide
//! environment with telemetry disabled before the first one: here through
//! `open_session`, in `steno-diarize` through its `session`. A
//! Microsoft-built ONNX Runtime library would otherwise report model and
//! usage details to Microsoft. That switch covers ONNX Runtime only: with
//! `DirectML` on, `DirectML.dll` and Direct3D 12 may log to Windows' own
//! diagnostic data, as for any program that uses them, under the system's
//! diagnostic data settings. Steno opens nothing for it, and no audio or
//! text is involved.
//!
//! # `DirectML`
//!
//! On Windows the encoder can run on `DirectML`, on any DirectX 12 GPU,
//! integrated ones included, when [`OnnxOptions::directml`] asks for it
//! (speech-stack decision 4). It is off by default
//! ([`SpeechSettings::directml_on_windows`]): gate G4 (at least three
//! times the CPU's speed on an integrated GPU) is open, because no
//! Windows machine with a GPU has run this.
//!
//! The probe is the session itself. `DirectML` must be in the ONNX Runtime
//! build, start on a hardware GPU (the default device filter leaves out
//! software adapters such as WARP), take the session, and run the encoder
//! once on a second of silence. If any step fails, the encoder opens on the
//! CPU. A later run that fails on `DirectML` moves the encoder to the CPU
//! for good and runs it again there, so a GPU that cannot do the work does
//! not fail the job. An abort inside the driver still ends the process,
//! which is why the app runs the engine in the sidecar; there, a child
//! that ends during a load or a request with `DirectML` in use switches it
//! off for the rest of the app's run
//! ([`crate::sidecar::directml_switched_off`]).
//! [`OnnxBackend::provider`] reports the provider in force; it is logged at
//! info level, without paths.
//!
//! The probe's second is about 100 feature frames, while the chunker's
//! windows are around 25 s and up to 60 s. A failure that shows only at
//! those lengths (out of GPU memory, a shape `DirectML` cannot take)
//! surfaces at the first real window, where the fallback above catches it.
//!
//! Only the encoder moves. The decoder and the joiner run once per token
//! on one frame, where a round trip to the GPU costs more than the step,
//! and Silero runs on 32 ms frames. The diarizer stays on the CPU in the
//! app's process (decision 5).
//!
//! [`SpeechSettings::directml_on_windows`]: crate::SpeechSettings::directml_on_windows

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorElementType, ValueType};

use crate::backend::{
    DecoderState, DecoderStep, EncoderOutput, Features, JointDecision, ModelShape, SAMPLE_RATE,
    SpeechBackend, split_logits,
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
    /// Windows only: run the encoder on `DirectML` when the probe passes
    /// (see [`DirectML`](self#directml)); ignored elsewhere. Off by default.
    pub directml: bool,
}

impl Default for OnnxOptions {
    fn default() -> Self {
        OnnxOptions {
            intra_threads: 4,
            inter_threads: 1,
            directml: false,
        }
    }
}

steno_core::string_enum! {
    /// Where ONNX Runtime runs the encoder. The sidecar protocol carries
    /// it by name, and a peer that does not know a name fails to read the
    /// whole reply, so a new variant bumps
    /// [`PROTOCOL_VERSION`](crate::sidecar::protocol::PROTOCOL_VERSION).
    #[derive(Default)]
    pub enum EncoderProvider {
        /// ONNX Runtime's CPU provider; every platform, and every model
        /// but the encoder.
        #[default]
        Cpu = "cpu",
        /// `DirectML` on a DirectX 12 GPU; Windows only.
        DirectMl = "directml",
    }
}

/// Configures ONNX Runtime's process-wide environment once, before the
/// first session: telemetry off (the privacy invariant in the module
/// docs). The first configuration committed wins, so every session in a
/// Steno process opens through here (`open_session`) or through
/// `steno-diarize`'s `session`, which both call this first. Returns
/// whether the environment in force is this one; `false` means a session
/// opened, or another configuration was committed, before the first call,
/// which is a bug (it is logged once).
pub fn init_environment() -> bool {
    static COMMITTED: OnceLock<bool> = OnceLock::new();
    *COMMITTED.get_or_init(|| {
        let committed = ort::init().with_telemetry(false).commit();
        if !committed {
            tracing::error!("ONNX Runtime was configured before Steno switched its telemetry off");
        }
        committed
    })
}

/// A session builder with the shared options, in the environment
/// [`init_environment`] configures.
fn builder(options: &OnnxOptions) -> Result<SessionBuilder, SpeechError> {
    init_environment();
    let options_error = |e: ort::Error<SessionBuilder>| SpeechError::SessionOptions(e.to_string());
    Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(options_error)?
        .with_intra_threads(options.intra_threads.max(1))
        .map_err(options_error)?
        .with_inter_threads(options.inter_threads.max(1))
        .map_err(options_error)
}

/// Opens one model file on the CPU with the shared options.
pub(crate) fn open_session(path: &Path, options: &OnnxOptions) -> Result<Session, SpeechError> {
    Ok(builder(options)?.commit_from_file(path)?)
}

/// Why the encoder did not open on `DirectML`, in words without paths or
/// ONNX Runtime's text, for the log.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    /// The ONNX Runtime build has no `DirectML` provider.
    NotInBuild,
    /// ONNX Runtime refused the session options `DirectML` needs.
    Options,
    /// `DirectML` did not start: no DirectX 12 GPU, or a `DirectML.dll`
    /// too old for ONNX Runtime.
    NoDevice,
    /// `DirectML` started and refused the model.
    Session,
    /// A run on `DirectML` failed (the probe run at load, or a later one).
    Run,
}

impl Fallback {
    fn describe(self) -> &'static str {
        match self {
            Fallback::NotInBuild => "this ONNX Runtime build has no DirectML",
            Fallback::Options => "ONNX Runtime refused the session options for DirectML",
            Fallback::NoDevice => {
                "DirectML did not start (no usable DirectX 12 GPU, or an old DirectML.dll)"
            }
            Fallback::Session => "DirectML refused the model",
            Fallback::Run => "a run on DirectML failed",
        }
    }
}

/// Opens `path` on `DirectML` (Windows only): the provider registered with
/// `error_on_failure`, so a missing GPU is an error here and not a silent
/// CPU session, and memory patterns off, which `DirectML` does not support.
/// The default device filter takes hardware GPUs only.
#[cfg(windows)]
fn open_directml(path: &Path, options: &OnnxOptions) -> Result<Session, (Fallback, String)> {
    use ort::ep::ExecutionProvider as _;
    init_environment();
    let directml = ort::ep::DirectML::default();
    if !directml.is_available().unwrap_or(false) {
        return Err((Fallback::NotInBuild, String::new()));
    }
    let refused = |fallback, e: &dyn std::fmt::Display| (fallback, e.to_string());
    builder(options)
        .map_err(|e| refused(Fallback::Options, &e))?
        .with_memory_pattern(false)
        .map_err(|e| refused(Fallback::Options, &e))?
        .with_execution_providers([directml.build().error_on_failure()])
        .map_err(|e| refused(Fallback::NoDevice, &e))?
        .commit_from_file(path)
        .map_err(|e| refused(Fallback::Session, &e))
}

/// A session that runs on `DirectML` when [`OnnxOptions::directml`] asks
/// for it and the probe passes, and on the CPU otherwise; the encoder's.
struct AcceleratedSession {
    /// `None` only between dropping a failed `DirectML` session and the CPU
    /// session that replaces it, or when that replacement failed to open.
    session: Option<Session>,
    provider: EncoderProvider,
    /// Why the session is not on `DirectML` though it was asked for.
    fallback: Option<Fallback>,
    /// Kept to reopen the model on the CPU after a failed run.
    path: PathBuf,
    options: OnnxOptions,
}

impl AcceleratedSession {
    /// Opens `path` on `DirectML` first when asked for on Windows, on the
    /// CPU after any failure there and everywhere else.
    fn open(path: &Path, options: &OnnxOptions) -> Result<Self, SpeechError> {
        let opened = |session, provider, fallback| AcceleratedSession {
            session: Some(session),
            provider,
            fallback,
            path: path.to_path_buf(),
            options: options.clone(),
        };
        #[cfg(windows)]
        let refused = if options.directml {
            match open_directml(path, options) {
                Ok(session) => return Ok(opened(session, EncoderProvider::DirectMl, None)),
                Err(refused) => Some(refused),
            }
        } else {
            None
        };
        #[cfg(not(windows))]
        let refused: Option<(Fallback, String)> = None;
        let session = open_session(path, options)?;
        // Logged once the CPU took the model: for a model that opens
        // nowhere, the CPU's error is the one that matters.
        let fallback = refused.map(|(reason, error)| {
            log_fallback(reason, &error);
            reason
        });
        Ok(opened(session, EncoderProvider::Cpu, fallback))
    }

    fn session(&self) -> Result<&Session, SpeechError> {
        self.session.as_ref().ok_or(SpeechError::NotPrepared)
    }

    /// Runs `work` on the session. On `DirectML` a failure reopens the model
    /// on the CPU, for this and every later run, and runs `work` there
    /// once more; on the CPU it is returned.
    fn run<T>(
        &mut self,
        mut work: impl FnMut(&mut Session) -> Result<T, SpeechError>,
    ) -> Result<T, SpeechError> {
        let session = self.session.as_mut().ok_or(SpeechError::NotPrepared)?;
        match work(session) {
            Err(error) if self.provider == EncoderProvider::DirectMl => {
                log_fallback(Fallback::Run, &error.to_string());
                // The DirectML session goes first, so the two never hold
                // the weights at the same time.
                self.session = None;
                self.provider = EncoderProvider::Cpu;
                self.fallback = Some(Fallback::Run);
                let session = self
                    .session
                    .insert(open_session(&self.path, &self.options)?);
                work(session)
            }
            result => result,
        }
    }
}

/// The fallback to the CPU, at warn level in fixed words; ONNX Runtime's
/// text, which can hold the model's path, at debug level only.
fn log_fallback(fallback: Fallback, error: &str) {
    tracing::warn!(
        reason = fallback.describe(),
        "DirectML is not usable; the speech encoder runs on the CPU"
    );
    tracing::debug!(error, "ONNX Runtime's DirectML error");
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
    encoder: AcceleratedSession,
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
        let mut encoder = AcceleratedSession::open(&directory.join("encoder.onnx"), options)?;
        let decoder = open_session(&directory.join("decoder.onnx"), options)?;
        let mut joiner = open_session(&directory.join("joiner.onnx"), options)?;
        let encoder_inputs = inputs(encoder.session()?, "encoder", 2)?;
        let decoder_inputs = inputs(&decoder, "decoder", 4)?;
        let joiner_inputs = inputs(&joiner, "joiner", 2)?;

        let (declared_vocab, declared_layers, declared_hidden) = {
            let metadata = encoder.session()?.metadata()?;
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
        let mut mel = MelExtractor::new();
        if encoder.provider == EncoderProvider::DirectMl {
            // The probe's last step: one run on a second of silence, so a
            // GPU that takes the session but cannot run it falls back here
            // rather than in the first meeting.
            let silence = mel.features(&vec![0.0; SAMPLE_RATE]);
            encoder.run(|session| encode_on(session, &encoder_inputs, &silence))?;
        }
        tracing::info!(
            provider = encoder.provider.as_str(),
            "speech encoder provider"
        );
        Ok((
            OnnxBackend {
                encoder,
                decoder,
                joiner,
                mel,
                shape,
                encoder_inputs,
                decoder_inputs,
                joiner_inputs,
            },
            vocab,
        ))
    }

    /// Where the encoder runs: [`EncoderProvider::DirectMl`] only when
    /// [`OnnxOptions::directml`] asked for it on Windows and the probe
    /// passed; a failed run on `DirectML` moves it to the CPU.
    ///
    /// `DirectMl` means the provider is registered for the encoder's
    /// session. ONNX Runtime still places any node `DirectML` does not
    /// support on the CPU, and does not tell the caller.
    #[must_use]
    pub fn provider(&self) -> EncoderProvider {
        self.encoder.provider
    }

    /// Why the encoder is not on `DirectML` though
    /// [`OnnxOptions::directml`] asked for it, in fixed words without
    /// paths: the probe failed, or a later run did. `None` while it runs
    /// there, and whenever it was not asked to (off Windows the request is
    /// ignored).
    #[must_use]
    pub fn fallback(&self) -> Option<&'static str> {
        self.encoder.fallback.map(Fallback::describe)
    }

    /// Whether the encoder still has a session: `false` only after a run
    /// failed on `DirectML` and the CPU could not reopen the model, after
    /// which every run fails.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.encoder.session.is_some()
    }
}

/// One encoder run over a window of at least one frame, its output
/// frame-major.
fn encode_on(
    session: &mut Session,
    inputs: &Inputs,
    features: &Features,
) -> Result<EncoderOutput, SpeechError> {
    let frames =
        i64::try_from(features.frames).map_err(|_| SpeechError::Shape("window too long".into()))?;
    let audio = f32_tensor(vec![1, features.mels as i64, frames], features.data.clone())?;
    let length = int_tensor(inputs.types[1], vec![1], &[frames])?;
    let outputs = session.run(vec![
        (inputs.names[0].as_str(), audio),
        (inputs.names[1].as_str(), length),
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
        self.encoder
            .run(|session| encode_on(session, &self.encoder_inputs, features))
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
    use std::path::{Path, PathBuf};

    use super::*;

    /// A model small enough to write by hand, `y = x · W + b` with `x` of
    /// shape `[n, 3]`, as ONNX protobuf bytes (opset 13): enough for
    /// ONNX Runtime, and `DirectML` where there is one, to open and run.
    fn affine_model() -> Vec<u8> {
        fn varint(mut value: u64, out: &mut Vec<u8>) {
            while value >= 0x80 {
                out.push((value as u8) | 0x80);
                value >>= 7;
            }
            out.push(value as u8);
        }
        fn number(field: u64, value: u64, out: &mut Vec<u8>) {
            varint(field << 3, out);
            varint(value, out);
        }
        fn bytes(field: u64, value: &[u8], out: &mut Vec<u8>) {
            varint((field << 3) | 2, out);
            varint(value.len() as u64, out);
            out.extend_from_slice(value);
        }
        fn message(build: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
            let mut out = Vec::new();
            build(&mut out);
            out
        }
        // Field numbers from onnx.proto.
        const FLOAT: u64 = 1;
        // TensorProto: dims 1, data_type 2, name 8, raw_data 9.
        let tensor = |name: &str, dims: &[u64], values: &[f32]| {
            message(|t| {
                for &d in dims {
                    number(1, d, t);
                }
                number(2, FLOAT, t);
                bytes(8, name.as_bytes(), t);
                let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
                bytes(9, &raw, t);
            })
        };
        // ValueInfoProto: name 1, type 2 (TypeProto.tensor_type 1:
        // elem_type 1, shape 2 of dims 1, each dim_value 1 or dim_param 2).
        let value_info = |name: &str, width: u64| {
            message(|v| {
                bytes(1, name.as_bytes(), v);
                let shape = message(|s| {
                    bytes(1, &message(|d| bytes(2, b"n", d)), s);
                    bytes(1, &message(|d| number(1, width, d)), s);
                });
                let tensor_type = message(|t| {
                    number(1, FLOAT, t);
                    bytes(2, &shape, t);
                });
                bytes(2, &message(|t| bytes(1, &tensor_type, t)), v);
            })
        };
        // NodeProto: input 1, output 2, op_type 4.
        let node = |inputs: &[&str], output: &str, op: &str| {
            message(|n| {
                for input in inputs {
                    bytes(1, input.as_bytes(), n);
                }
                bytes(2, output.as_bytes(), n);
                bytes(4, op.as_bytes(), n);
            })
        };
        // GraphProto: node 1, name 2, initializer 5, input 11, output 12.
        let graph = message(|g| {
            bytes(1, &node(&["x", "w"], "xw", "MatMul"), g);
            bytes(1, &node(&["xw", "b"], "y", "Add"), g);
            bytes(2, b"affine", g);
            bytes(5, &tensor("w", &[3, 2], &[1.0, 0.0, 0.0, 1.0, 1.0, 1.0]), g);
            bytes(5, &tensor("b", &[2], &[0.5, -0.5]), g);
            bytes(11, &value_info("x", 3), g);
            bytes(12, &value_info("y", 2), g);
        });
        // ModelProto: ir_version 1 (8 here), graph 7, opset_import 8 (its
        // version 2, 13 here).
        message(|m| {
            number(1, 8, m);
            bytes(7, &graph, m);
            bytes(8, &message(|o| number(2, 13, o)), m);
        })
    }

    /// The affine model in a temporary directory.
    fn affine_model_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("affine.onnx");
        std::fs::write(&path, affine_model()).unwrap();
        (dir, path)
    }

    /// `y` for `x = [[1, 2, 3], [0, 0, 0]]`: `[[4.5, 4.5], [0.5, -0.5]]`.
    fn run_affine(session: &mut Session) -> Result<Vec<f32>, SpeechError> {
        let x = f32_tensor(vec![2, 3], vec![1.0, 2.0, 3.0, 0.0, 0.0, 0.0])?;
        let outputs = session.run(vec![("x", x)])?;
        Ok(outputs[0].try_extract_tensor::<f32>()?.1.to_vec())
    }

    const AFFINE_Y: [f32; 4] = [4.5, 4.5, 0.5, -0.5];

    fn directml_options() -> OnnxOptions {
        OnnxOptions {
            directml: true,
            ..OnnxOptions::default()
        }
    }

    /// The probe on this machine, with its findings printed: CI runs it
    /// with `--nocapture` on Windows, where the runner has no GPU, so its
    /// log records what `DirectML` makes of that. Whatever it decides, the
    /// session's answer is correct; off Windows the request is ignored.
    #[test]
    fn directml_probe_opens_a_working_session_on_whatever_this_machine_has() {
        let (_dir, path) = affine_model_file();
        #[cfg(windows)]
        {
            use ort::ep::ExecutionProvider as _;
            init_environment();
            println!(
                "DirectML in this ONNX Runtime build: {:?}",
                ort::ep::DirectML::default().is_available()
            );
            match open_directml(&path, &directml_options()) {
                Ok(_) => println!("DirectML session: opened"),
                Err((fallback, error)) => {
                    println!(
                        "DirectML session: {} ({fallback:?}): {error}",
                        fallback.describe()
                    );
                }
            }
        }
        let mut accelerated = AcceleratedSession::open(&path, &directml_options()).unwrap();
        println!("provider chosen: {}", accelerated.provider);
        assert_eq!(
            accelerated.fallback.is_some(),
            cfg!(windows) && accelerated.provider == EncoderProvider::Cpu,
            "a reason exactly when DirectML was asked for and not used"
        );
        if !cfg!(windows) {
            assert_eq!(accelerated.provider, EncoderProvider::Cpu);
        }
        assert_eq!(accelerated.run(run_affine).unwrap(), AFFINE_Y);
        assert_eq!(
            AcceleratedSession::open(&path, &OnnxOptions::default())
                .unwrap()
                .provider,
            EncoderProvider::Cpu,
            "without the request the CPU is the provider everywhere"
        );
        assert!(
            !OnnxOptions::default().directml,
            "DirectML is opt-in until gate G4 passes"
        );
    }

    #[test]
    fn a_model_that_opens_nowhere_is_an_error_with_or_without_directml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.onnx");
        std::fs::write(&path, b"not a model").unwrap();
        for options in [OnnxOptions::default(), directml_options()] {
            assert!(AcceleratedSession::open(&path, &options).is_err());
        }
    }

    /// A session as `open` leaves it after the probe passed, without a GPU:
    /// a CPU session labelled `DirectML`, so the fallback can be driven.
    fn as_if_on_directml(path: &Path) -> AcceleratedSession {
        let mut session = AcceleratedSession::open(path, &directml_options()).unwrap();
        session.provider = EncoderProvider::DirectMl;
        session.fallback = None;
        session
    }

    #[test]
    fn a_failed_run_on_directml_moves_to_the_cpu_for_good_and_runs_again() {
        let (_dir, path) = affine_model_file();
        let mut session = as_if_on_directml(&path);
        let mut calls = 0;
        let y = session
            .run(|s| {
                calls += 1;
                if calls == 1 {
                    Err(SpeechError::Shape("device removed".into()))
                } else {
                    run_affine(s)
                }
            })
            .unwrap();
        assert_eq!((y.as_slice(), calls), (AFFINE_Y.as_slice(), 2));
        assert_eq!(session.provider, EncoderProvider::Cpu);
        assert_eq!(session.fallback, Some(Fallback::Run));

        // On the CPU a failure is the caller's, with no second try.
        let mut calls = 0;
        let error = session
            .run(|_| -> Result<(), SpeechError> {
                calls += 1;
                Err(SpeechError::Shape("bad input".into()))
            })
            .unwrap_err();
        assert!(matches!(error, SpeechError::Shape(_)));
        assert_eq!(calls, 1);
        assert_eq!(session.run(run_affine).unwrap(), AFFINE_Y);
    }

    #[test]
    fn when_the_cpu_cannot_reopen_the_model_the_run_fails_and_stays_failed() {
        let (dir, path) = affine_model_file();
        let mut session = as_if_on_directml(&path);
        drop(dir);
        let fail = |_: &mut Session| -> Result<(), SpeechError> {
            Err(SpeechError::Shape("device removed".into()))
        };
        assert!(matches!(session.run(fail), Err(SpeechError::Runtime(_))));
        assert_eq!(session.provider, EncoderProvider::Cpu);
        assert!(
            session.session.is_none(),
            "what `OnnxBackend::usable` reads"
        );
        assert!(matches!(
            session.run(run_affine),
            Err(SpeechError::NotPrepared)
        ));
    }

    #[test]
    fn telemetry_is_switched_off_before_any_session() {
        // A missing model still reaches ONNX Runtime, which would set up
        // its default environment (telemetry on) had nothing come first.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.onnx");
        assert!(open_session(&missing, &OnnxOptions::default()).is_err());
        assert!(
            init_environment(),
            "a session opened before telemetry was switched off"
        );
        assert!(!ort::init().commit(), "nothing can switch it on again");
    }

    /// The `.rs` files under `directory`, recursively.
    fn rust_files(directory: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }

    #[test]
    fn the_workspace_configures_onnx_runtime_in_one_place_with_telemetry_off() {
        // `ort` keeps the committed settings to itself and ONNX Runtime
        // has no call that reads telemetry back, so the source is checked:
        // one environment in the whole workspace, this one, with telemetry
        // off, and sessions only where `init_environment` comes first.
        // Unit test modules are left out.
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut files = Vec::new();
        for entry in std::fs::read_dir(crates).unwrap() {
            let path = entry.unwrap().path();
            for part in ["src", "tests", "examples", "benches"] {
                if path.join(part).is_dir() {
                    rust_files(&path.join(part), &mut files);
                }
            }
        }
        rust_files(&crates.join("../apps/desktop/src-tauri/src"), &mut files);
        let name = |path: &Path| {
            let relative = path.strip_prefix(crates).unwrap_or(path);
            relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        };
        // Split, so this test's own text does not count.
        let environments = [concat!("ort::", "init("), concat!("ort::", "init_from(")];
        let sessions = [
            concat!("Session::", "builder("),
            concat!("SessionBuilder::", "new("),
        ];
        let mut configured = Vec::new();
        let mut opened = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
            // A unit test module may poke at `ort` itself, as this one does.
            let text = text
                .split("#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or_default();
            let file = name(path);
            for needle in environments {
                configured.extend(text.matches(needle).map(|_| file.clone()));
            }
            if sessions.iter().any(|needle| text.contains(needle)) {
                assert!(
                    text.contains("init_environment()"),
                    "{file} opens a session without `init_environment`"
                );
                opened.push(file);
            }
        }
        opened.sort();
        assert_eq!(configured, ["steno-speech/src/onnx.rs"]);
        assert_eq!(
            opened,
            ["steno-diarize/src/onnx.rs", "steno-speech/src/onnx.rs"]
        );
        let source = std::fs::read_to_string(crates.join("steno-speech/src/onnx.rs")).unwrap();
        assert!(source.contains(concat!("ort::", "init().with_telemetry(false).commit()")));
    }
}
