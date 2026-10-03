//! The four model calls of Parakeet TDT v3 on CoreML: preprocessor,
//! encoder, decoder step and joint step, over FluidAudio's `.mlmodelc`
//! bundles. The shared-decoder follow-up drives these through
//! `steno_speech::SpeechBackend`; until then [`crate::decoder`] does.
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
//! so no logits leave the model.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::SpeechError;
use crate::chunking::MAX_MODEL_SAMPLES;
use crate::coreml::{Array, ComputeUnits, EncoderView, Model, inputs};
use crate::vocab::Vocab;

/// Encoder hidden size (`ASRConstants.encoderHiddenSize`).
pub const ENCODER_HIDDEN: usize = 1024;
/// Decoder hidden size (`ASRConstants.decoderHiddenSize`).
pub const DECODER_HIDDEN: usize = 640;
/// LSTM layers of the prediction network (`AsrManager.decoderLayerCount`).
pub const DECODER_LAYERS: usize = 2;

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

/// The four loaded models and the vocabulary beside them.
#[derive(Debug)]
pub struct Backend {
    preprocessor: Model,
    encoder: Model,
    decoder: Model,
    joint: Model,
    vocab: Vocab,
    directory: PathBuf,
    load_seconds: f64,
}

/// What the joint decided for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointDecision {
    /// The argmax token id, `BLANK_ID` for blank.
    pub token: usize,
    /// The joint's probability for `token`.
    pub probability: f32,
    /// Index into the duration bins, not the duration itself.
    pub duration_bin: usize,
}

/// `value` as the `Int32` the models take, or [`SpeechError::Range`].
fn int32(name: &'static str, value: usize) -> Result<i32, SpeechError> {
    i32::try_from(value).map_err(|_| SpeechError::Range { name, value })
}

/// The preprocessor's mel spectrogram and its declared length.
pub struct Mel {
    mel: Array,
    mel_length: Array,
}

impl Mel {
    /// Frames along the mel time axis (1,501 for the full window).
    #[must_use]
    pub fn frames(&self) -> usize {
        self.mel.shape().get(2).copied().unwrap_or(0)
    }

    /// Declare every mel frame valid, padding included: the `encoderFull`
    /// recovery policy (`AsrManager.declaringFullMelLength`).
    pub fn declare_full_length(&mut self) -> Result<(), SpeechError> {
        let frames = i32::try_from(self.frames()).map_err(|_| SpeechError::Shape {
            name: "mel",
            shape: self.mel.shape().to_vec(),
        })?;
        self.mel_length.as_i32_mut()?[0] = frames;
        Ok(())
    }
}

/// Per-thread buffers: the padded audio input, the decoder state and the
/// joint inputs, allocated once per worker (`TdtDecoderV3` allocates its
/// reusable arrays per decode call; one set per thread is the same thing
/// with fewer allocations).
pub struct Scratch {
    audio: Array,
    audio_length: Array,
    targets: Array,
    target_length: Array,
    h: Array,
    c: Array,
    encoder_step: Array,
    decoder_step: Array,
    /// The decoder projection for the last fed token
    /// (`TdtDecoderState.predictorOutput`), ready for the joint.
    projection: Vec<f32>,
}

impl Scratch {
    fn new() -> Result<Scratch, SpeechError> {
        let mut target_length = Array::zeros_i32(&[1])?;
        target_length.as_i32_mut()?[0] = 1;
        Ok(Scratch {
            audio: Array::zeros_f32(&[1, MAX_MODEL_SAMPLES])?,
            audio_length: Array::zeros_i32(&[1])?,
            targets: Array::zeros_i32(&[1, 1])?,
            target_length,
            h: Array::zeros_f32(&[DECODER_LAYERS, 1, DECODER_HIDDEN])?,
            c: Array::zeros_f32(&[DECODER_LAYERS, 1, DECODER_HIDDEN])?,
            encoder_step: Array::zeros_f32(&[1, ENCODER_HIDDEN, 1])?,
            decoder_step: Array::zeros_f32(&[1, DECODER_HIDDEN, 1])?,
            projection: vec![0.0; DECODER_HIDDEN],
        })
    }

    /// A fresh decoder state: zero LSTM state and projection
    /// (`TdtDecoderState.reset`).
    pub fn reset_decoder(&mut self) -> Result<(), SpeechError> {
        self.h.as_f32_mut()?.fill(0.0);
        self.c.as_f32_mut()?.fill(0.0);
        self.projection.fill(0.0);
        Ok(())
    }
}

impl Backend {
    /// Load the four bundles and the vocabulary from `directory` with the
    /// default compute units.
    pub fn load(directory: &Path) -> Result<Backend, SpeechError> {
        Backend::load_with(directory, ComputeUnitsPlan::default())
    }

    /// Load with explicit compute units (the harness compares them).
    pub fn load_with(directory: &Path, units: ComputeUnitsPlan) -> Result<Backend, SpeechError> {
        let started = Instant::now();
        let vocab_path = [VOCABULARY_FILE, VOCABULARY_FILE_V3]
            .iter()
            .map(|name| directory.join(name))
            .find(|path| path.is_file())
            .unwrap_or_else(|| directory.join(VOCABULARY_FILE));
        let vocab = Vocab::load(&vocab_path)?;
        let preprocessor = Model::load(&directory.join(PREPROCESSOR_FILE), units.preprocessor)?;
        let encoder = Model::load(&directory.join(ENCODER_FILE), units.encoder)?;
        let decoder = Model::load(&directory.join(DECODER_FILE), units.decoder)?;
        let joint = Model::load(&directory.join(JOINT_FILE), units.joint)?;
        Ok(Backend {
            preprocessor,
            encoder,
            decoder,
            joint,
            vocab,
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

    /// Wall time of [`Backend::load`].
    #[must_use]
    pub fn load_seconds(&self) -> f64 {
        self.load_seconds
    }

    /// Buffers for one worker thread.
    pub fn scratch(&self) -> Result<Scratch, SpeechError> {
        Scratch::new()
    }

    /// Mel spectrogram of `samples` (at most 15 s, zero padded to the
    /// window) with `declared_length` as `audio_length`; `None` declares
    /// the full padded window (`preprocessorFull`).
    pub fn preprocess(
        &self,
        scratch: &mut Scratch,
        samples: &[f32],
        declared_length: Option<usize>,
    ) -> Result<Mel, SpeechError> {
        if samples.len() > MAX_MODEL_SAMPLES {
            return Err(SpeechError::Shape {
                name: "audio_signal",
                shape: vec![1, samples.len()],
            });
        }
        let audio = scratch.audio.as_f32_mut()?;
        audio[..samples.len()].copy_from_slice(samples);
        audio[samples.len()..].fill(0.0);
        let declared = declared_length
            .unwrap_or(MAX_MODEL_SAMPLES)
            .min(MAX_MODEL_SAMPLES);
        scratch.audio_length.as_i32_mut()?[0] = int32("audio_length", declared)?;
        let input = inputs(&[
            ("audio_signal", &scratch.audio),
            ("audio_length", &scratch.audio_length),
        ])?;
        let output = self.preprocessor.predict(&input)?;
        Ok(Mel {
            mel: output.array("mel")?,
            mel_length: output.array("mel_length")?,
        })
    }

    /// Encoder frames for `mel`.
    pub fn encode(&self, mel: &Mel) -> Result<EncoderView, SpeechError> {
        let input = inputs(&[("mel", &mel.mel), ("mel_length", &mel.mel_length)])?;
        let output = self.encoder.predict(&input)?;
        let encoder = output.array("encoder")?;
        let length = output.array("encoder_length")?.i32_scalar()?;
        EncoderView::new(
            encoder,
            ENCODER_HIDDEN,
            usize::try_from(length).unwrap_or(0),
        )
    }

    /// Feed `token` to the prediction network: updates the LSTM state and
    /// the cached projection in `scratch` (`TdtModelInference.runDecoder`
    /// followed by the `predictorOutput` cache).
    pub fn decoder_step(&self, scratch: &mut Scratch, token: usize) -> Result<(), SpeechError> {
        scratch.targets.as_i32_mut()?[0] = int32("token", token)?;
        let input = inputs(&[
            ("targets", &scratch.targets),
            ("target_length", &scratch.target_length),
            ("h_in", &scratch.h),
            ("c_in", &scratch.c),
        ])?;
        let output = self.decoder.predict(&input)?;
        output
            .array("decoder")?
            .copy_f32_into(&mut scratch.projection)?;
        output
            .array("h_out")?
            .copy_f32_into(scratch.h.as_f32_mut()?)?;
        output
            .array("c_out")?
            .copy_f32_into(scratch.c.as_f32_mut()?)?;
        Ok(())
    }

    /// The joint's decision for encoder frame `t` against the cached
    /// projection (`TdtModelInference.runJointPrepared`).
    pub fn joint_step(
        &self,
        scratch: &mut Scratch,
        encoder: &EncoderView,
        t: usize,
    ) -> Result<JointDecision, SpeechError> {
        encoder.copy_frame(t, scratch.encoder_step.as_f32_mut()?)?;
        scratch
            .decoder_step
            .as_f32_mut()?
            .copy_from_slice(&scratch.projection);
        let input = inputs(&[
            ("encoder_step", &scratch.encoder_step),
            ("decoder_step", &scratch.decoder_step),
        ])?;
        let output = self.joint.predict(&input)?;
        let token = output.array("token_id")?.i32_scalar()?;
        let probability = output.array("token_prob")?.f32_scalar()?;
        let duration_bin = output.array("duration")?.i32_scalar()?;
        Ok(JointDecision {
            token: usize::try_from(token).unwrap_or(0),
            probability,
            duration_bin: usize::try_from(duration_bin).unwrap_or(usize::MAX),
        })
    }
}
