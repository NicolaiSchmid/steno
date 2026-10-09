//! The four model calls of Parakeet TDT v3 on CoreML behind
//! `steno_speech`'s [`SpeechBackend`]: [`Models`] holds the loaded
//! `.mlmodelc` bundles and the vocabulary, shared by every worker;
//! [`Backend`] is one worker's view of them, which the shared pipeline
//! drives (chunker, decode loop, merge, segmentation).
//! Swift: `FluidAudio`'s `AsrModels` and `TdtModelInference`.
//!
//! Model contract (FluidAudio 0.17.4, `parakeet-tdt-0.6b-v3`):
//!
//! | Model | Inputs | Outputs |
//! |---|---|---|
//! | `Preprocessor` | `audio_signal [1, 240000] f32`, `audio_length [1] i32` | `mel [1, 128, 1501] f32`, `mel_length [1] i32` |
//! | `Encoder` | `mel`, `mel_length` | `encoder [1, 1024, 188] f32` (strided), `encoder_length [1] i32` |
//! | `Decoder` | `targets [1, 1] i32`, `target_length [1] i32`, `h_in`, `c_in [2, 1, 640] f32` | `decoder [1, 640, 1] f32`, `h_out`, `c_out` |
//! | `JointDecisionv3` | `encoder_step [1, 1024, 1]`, `decoder_step [1, 640, 1]` | `token_id [1,1,1] i32`, `token_prob [1,1,1] f32`, `duration [1,1,1] i32` |
//!
//! The joint already holds the argmax over tokens and over duration bins,
//! so no logits leave the model. The models take one fixed window of
//! 15 s ([`MAX_WINDOW_SAMPLES`]); the engine clamps the chunker to it.
//! Every call allocates its own input arrays, so a [`Backend`] holds no
//! CoreML object of its own and moves to a worker thread as it is.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use steno_speech::{
    DecoderState, DecoderStep, EncoderOutput, Features, JointDecision, ModelShape, SpeechBackend,
    Vocab,
};

use crate::SpeechError;
use crate::coreml::{Array, ComputeUnits, EncoderView, Model, inputs};
use crate::vocab::{self, BLANK_TOKEN};

/// The models' window: 15 s at 16 kHz (`ASRConstants.maxModelSamples`).
pub const MAX_WINDOW_SAMPLES: usize = 240_000;
/// Mel frames of the full window: one per 10 ms hop, plus one.
const MEL_FRAMES: usize = MAX_WINDOW_SAMPLES / 160 + 1;
/// Mel bins of the preprocessor's output.
const MEL_BINS: usize = 128;
/// Encoder hidden size (`ASRConstants.encoderHiddenSize`).
pub const ENCODER_HIDDEN: usize = 1024;
/// Decoder hidden size (`ASRConstants.decoderHiddenSize`).
pub const DECODER_HIDDEN: usize = 640;
/// LSTM layers of the prediction network (`AsrManager.decoderLayerCount`).
pub const DECODER_LAYERS: usize = 2;
/// `TdtConfig.durationBins`: the joint's bin index maps to this many frames.
pub const DURATION_BINS: [usize; 5] = [0, 1, 2, 3, 4];

/// File names inside the model directory (`AsrModels.Names`, v3 with the
/// `int8` encoder precision Steno's `ParakeetEngine` requests).
pub const PREPROCESSOR_FILE: &str = "Preprocessor.mlmodelc";
pub const ENCODER_FILE: &str = "Encoder.mlmodelc";
pub const DECODER_FILE: &str = "Decoder.mlmodelc";
pub const JOINT_FILE: &str = "JointDecisionv3.mlmodelc";
/// Vocabulary file (`ModelNames.ASR.vocabularyFile`, the name Steno's
/// `ModelAsset` requires).
pub const VOCABULARY_FILE: &str = "parakeet_vocab.json";
/// The model repository's name for the same bytes; read when
/// [`VOCABULARY_FILE`] is absent.
pub const VOCABULARY_FILE_V3: &str = "parakeet_v3_vocab.json";

/// Compute units per model, as `AsrModels.loadLocal` assigns them from
/// `defaultConfiguration()` (`.cpuAndNeuralEngine`; the preprocessor is
/// pinned to the CPU because all of its ops map there anyway).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComputeUnitsPlan {
    /// `Preprocessor.mlmodelc`.
    pub preprocessor: ComputeUnits,
    /// `Encoder.mlmodelc`.
    pub encoder: ComputeUnits,
    /// `Decoder.mlmodelc`.
    pub decoder: ComputeUnits,
    /// `JointDecisionv3.mlmodelc`.
    pub joint: ComputeUnits,
}

impl Default for ComputeUnitsPlan {
    fn default() -> Self {
        ComputeUnitsPlan {
            preprocessor: ComputeUnits::CpuOnly,
            encoder: ComputeUnits::CpuAndNeuralEngine,
            decoder: ComputeUnits::CpuAndNeuralEngine,
            joint: ComputeUnits::CpuAndNeuralEngine,
        }
    }
}

/// The four loaded models and the vocabulary beside them; `Sync`, so
/// every worker's [`Backend`] shares one set, as `FluidAudio`'s chunk
/// workers share one `AsrModels` (`AsrManager.makeWorkerClone`).
#[derive(Debug)]
pub struct Models {
    preprocessor: Model,
    encoder: Model,
    decoder: Model,
    joint: Model,
    vocab: Vocab,
    shape: ModelShape,
    directory: PathBuf,
    load_seconds: f64,
}

/// `value` as the `Int32` the models take, or [`SpeechError::Range`].
fn int32(name: &'static str, value: usize) -> Result<i32, SpeechError> {
    i32::try_from(value).map_err(|_| SpeechError::Range { name, value })
}

/// A non-negative `Int32` output as `usize`, or [`SpeechError::Range`].
fn index(name: &'static str, value: i32) -> Result<usize, SpeechError> {
    usize::try_from(value).map_err(|_| SpeechError::Negative { name, value })
}

/// An `f32` array of `shape` holding `values`.
fn filled(shape: &[usize], values: &[f32]) -> Result<Array, SpeechError> {
    let mut array = Array::zeros_f32(shape)?;
    let slots = array.as_f32_mut()?;
    if slots.len() != values.len() {
        return Err(SpeechError::Shape {
            name: "input",
            shape: vec![values.len()],
        });
    }
    slots.copy_from_slice(values);
    Ok(array)
}

/// An `i32` array of shape `[1]` or `[1, 1]` holding `value`.
fn scalar_i32(shape: &[usize], value: i32) -> Result<Array, SpeechError> {
    let mut array = Array::zeros_i32(shape)?;
    array.as_i32_mut()?[0] = value;
    Ok(array)
}

impl Models {
    /// Loads the four bundles and the vocabulary from `directory` with the
    /// default compute units.
    pub fn load(directory: &Path) -> Result<Models, SpeechError> {
        Models::load_with(directory, ComputeUnitsPlan::default())
    }

    /// Loads with explicit compute units (the harness compares them).
    pub fn load_with(directory: &Path, units: ComputeUnitsPlan) -> Result<Models, SpeechError> {
        let started = Instant::now();
        let vocab_path = [VOCABULARY_FILE, VOCABULARY_FILE_V3]
            .iter()
            .map(|name| directory.join(name))
            .find(|path| path.is_file())
            .unwrap_or_else(|| directory.join(VOCABULARY_FILE));
        let vocab = vocab::load(&vocab_path)?;
        if vocab.blank_id() != BLANK_TOKEN {
            return Err(SpeechError::Vocabulary {
                path: vocab_path,
                message: format!(
                    "{} pieces, the joint's blank is {BLANK_TOKEN}",
                    vocab.len() - 1
                ),
            });
        }
        let preprocessor = Model::load(&directory.join(PREPROCESSOR_FILE), units.preprocessor)?;
        let encoder = Model::load(&directory.join(ENCODER_FILE), units.encoder)?;
        let decoder = Model::load(&directory.join(DECODER_FILE), units.decoder)?;
        let joint = Model::load(&directory.join(JOINT_FILE), units.joint)?;
        let shape = ModelShape {
            vocab_size: vocab.len(),
            blank_id: BLANK_TOKEN,
            durations: DURATION_BINS.to_vec(),
            decoder_layers: DECODER_LAYERS,
            decoder_hidden: DECODER_HIDDEN,
            encoder_hidden: ENCODER_HIDDEN,
        };
        Ok(Models {
            preprocessor,
            encoder,
            decoder,
            joint,
            vocab,
            shape,
            directory: directory.to_path_buf(),
            load_seconds: started.elapsed().as_secs_f64(),
        })
    }

    #[must_use]
    pub fn vocab(&self) -> &Vocab {
        &self.vocab
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Wall time of [`Models::load`].
    #[must_use]
    pub fn load_seconds(&self) -> f64 {
        self.load_seconds
    }

    /// The mel spectrogram of `samples` (at most one window), zero padded
    /// to the window with the real length declared; only the valid frames
    /// are kept (`AsrManager.runInference`, the default length policy).
    fn preprocess(&self, samples: &[f32]) -> Result<Features, SpeechError> {
        if samples.len() > MAX_WINDOW_SAMPLES {
            return Err(SpeechError::Shape {
                name: "audio_signal",
                shape: vec![1, samples.len()],
            });
        }
        let mut audio = Array::zeros_f32(&[1, MAX_WINDOW_SAMPLES])?;
        audio.as_f32_mut()?[..samples.len()].copy_from_slice(samples);
        let audio_length = scalar_i32(&[1], int32("audio_length", samples.len())?)?;
        let output = self.preprocessor.predict(&inputs(&[
            ("audio_signal", &audio),
            ("audio_length", &audio_length),
        ])?)?;
        let mel = output.array("mel")?;
        let &[1, mels, frames] = mel.shape() else {
            return Err(SpeechError::Shape {
                name: "mel",
                shape: mel.shape().to_vec(),
            });
        };
        let valid = index("mel_length", output.array("mel_length")?.i32_scalar()?)?.min(frames);
        let all = mel.to_vec_f32()?;
        let data = all
            .chunks_exact(frames)
            .flat_map(|row| &row[..valid])
            .copied()
            .collect();
        Ok(Features {
            mels,
            frames: valid,
            data,
        })
    }

    /// The encoder frames of `features`, padded back to the window with
    /// zeros, which is what the preprocessor writes past the declared
    /// length; frame-major and only the frames the encoder marks valid.
    fn encode(&self, features: &Features) -> Result<EncoderOutput, SpeechError> {
        if features.mels != MEL_BINS
            || features.frames > MEL_FRAMES
            || features.data.len() != MEL_BINS * features.frames
        {
            return Err(SpeechError::Shape {
                name: "mel",
                shape: vec![1, features.mels, features.frames],
            });
        }
        let mut mel = Array::zeros_f32(&[1, MEL_BINS, MEL_FRAMES])?;
        let padded = mel.as_f32_mut()?;
        for (row, values) in padded
            .as_chunks_mut::<MEL_FRAMES>()
            .0
            .iter_mut()
            .zip(features.data.chunks_exact(features.frames.max(1)))
        {
            row[..features.frames].copy_from_slice(&values[..features.frames]);
        }
        let mel_length = scalar_i32(&[1], int32("mel_length", features.frames)?)?;
        let output = self
            .encoder
            .predict(&inputs(&[("mel", &mel), ("mel_length", &mel_length)])?)?;
        let length = index(
            "encoder_length",
            output.array("encoder_length")?.i32_scalar()?,
        )?;
        let view = EncoderView::new(output.array("encoder")?, ENCODER_HIDDEN, length)?;
        let mut data = vec![0.0; view.valid * ENCODER_HIDDEN];
        for (t, frame) in data
            .as_chunks_mut::<ENCODER_HIDDEN>()
            .0
            .iter_mut()
            .enumerate()
        {
            view.copy_frame(t, frame)?;
        }
        Ok(EncoderOutput {
            hidden: ENCODER_HIDDEN,
            len: view.valid,
            data,
        })
    }

    /// Feeds `token` to the prediction network from `state`
    /// (`TdtModelInference.runDecoder`).
    fn decoder_step(&self, token: u32, state: &DecoderState) -> Result<DecoderStep, SpeechError> {
        let lstm = [DECODER_LAYERS, 1, DECODER_HIDDEN];
        let targets = scalar_i32(&[1, 1], int32("token", token as usize)?)?;
        let target_length = scalar_i32(&[1], 1)?;
        let h = filled(&lstm, &state.h)?;
        let c = filled(&lstm, &state.c)?;
        let output = self.decoder.predict(&inputs(&[
            ("targets", &targets),
            ("target_length", &target_length),
            ("h_in", &h),
            ("c_in", &c),
        ])?)?;
        Ok(DecoderStep {
            projection: output.array("decoder")?.to_vec_f32()?,
            state: DecoderState {
                h: output.array("h_out")?.to_vec_f32()?,
                c: output.array("c_out")?.to_vec_f32()?,
            },
        })
    }

    /// The joint's decision for one encoder frame and one projection
    /// (`TdtModelInference.runJointPrepared`).
    fn joint_step(
        &self,
        encoder_frame: &[f32],
        projection: &[f32],
    ) -> Result<JointDecision, SpeechError> {
        let encoder_step = filled(&[1, ENCODER_HIDDEN, 1], encoder_frame)?;
        let decoder_step = filled(&[1, DECODER_HIDDEN, 1], projection)?;
        let output = self.joint.predict(&inputs(&[
            ("encoder_step", &encoder_step),
            ("decoder_step", &decoder_step),
        ])?)?;
        let token = index("token_id", output.array("token_id")?.i32_scalar()?)?;
        Ok(JointDecision {
            token: u32::try_from(token).map_err(|_| SpeechError::Range {
                name: "token_id",
                value: token,
            })?,
            probability: output.array("token_prob")?.f32_scalar()?,
            duration_bin: index("duration", output.array("duration")?.i32_scalar()?)?,
        })
    }
}

/// One worker's [`SpeechBackend`] over the shared [`Models`]; cloning it
/// gives another worker over the same models.
#[derive(Debug, Clone)]
pub struct Backend {
    models: Arc<Models>,
}

impl Backend {
    #[must_use]
    pub fn new(models: Arc<Models>) -> Backend {
        Backend { models }
    }

    #[must_use]
    pub fn models(&self) -> &Models {
        &self.models
    }
}

impl SpeechBackend for Backend {
    fn shape(&self) -> &ModelShape {
        &self.models.shape
    }

    fn features(&mut self, samples: &[f32]) -> Result<Features, steno_speech::SpeechError> {
        Ok(self.models.preprocess(samples)?)
    }

    fn encode(&mut self, features: &Features) -> Result<EncoderOutput, steno_speech::SpeechError> {
        Ok(self.models.encode(features)?)
    }

    fn decoder_step(
        &mut self,
        token: u32,
        state: &DecoderState,
    ) -> Result<DecoderStep, steno_speech::SpeechError> {
        Ok(self.models.decoder_step(token, state)?)
    }

    fn joint_step(
        &mut self,
        encoder_frame: &[f32],
        projection: &[f32],
    ) -> Result<JointDecision, steno_speech::SpeechError> {
        Ok(self.models.joint_step(encoder_frame, projection)?)
    }
}
