//! Steno's speech pipeline above the tensors: voice activity detection, the
//! pause-aligned chunker, the greedy TDT decoder, the overlap merge and the
//! mapping to segments, with the model calls behind [`SpeechBackend`]: ONNX
//! Runtime here, `CoreML` in `steno-speech-coreml`. The app runs ONNX speech
//! only in the sidecar ([`SidecarSpeechEngine`]), which gets the audio only
//! over its stdin; which platform runs which engine: [`runtime`].
//!
//! ```no_run
//! use steno_core::{AudioBuffer16k, SpeechEngine};
//! use steno_speech::{ModelStore, OnnxOptions, OnnxSpeechEngine};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
//! let engine = OnnxSpeechEngine::new(ModelStore::from_environment(), OnnxOptions::default());
//! engine.prepare().await?;
//! let segments = engine.transcribe(&AudioBuffer16k::silence(1.0), None).await?;
//! assert!(segments.is_empty());
//! # Ok(())
//! # }
//! ```

// Signal processing converts sample, frame and bin counts to floats on every
// other line; the values are tiny and per-site allowances would bury the
// arithmetic.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
// Tests compare the exact floats they constructed and assert on emptiness.
#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        clippy::assert_is_empty,
        clippy::single_range_in_vec_init,
        clippy::large_stack_arrays
    )
)]

pub mod backend;
pub mod chunker;
pub mod decoder;
pub mod engine;
pub mod error;
pub mod features;
pub mod language;
pub mod merge;
pub mod model_store;
pub mod onnx;
pub mod pipeline;
pub mod runtime;
pub mod segmentation;
pub mod sidecar;
pub mod vad;
pub mod vocab;
pub mod wav;
pub mod wer;

pub use backend::{
    DecoderState, DecoderStep, EncoderOutput, FRAME_SAMPLES, FRAME_SECONDS, Features,
    JointDecision, ModelShape, SAMPLE_RATE, SpeechBackend, sample_count, split_logits,
};
pub use chunker::{Chunk, ChunkerConfig, Cut, Gaps};
pub use decoder::{DecodeStats, DecoderConfig, Token, confidence};
pub use engine::{OnnxSpeechEngine, PARAKEET_V3_ID, PARAKEET_V3_LANGUAGES};
pub use error::{SidecarError, SpeechError};
pub use features::MelExtractor;
pub use language::{LanguageRecognizer, LanguageTagger, StopwordRecognizer, WhatlangRecognizer};
pub use model_store::{
    DownloadProgress, Install, ModelAsset, ModelFile, ModelSource, ModelStore,
    PARAKEET_V3_COREML_REPO, PARAKEET_V3_COREML_REVISION, PARAKEET_V3_FP32_REVISION,
    STENO_MODELS_REPO,
};
pub use onnx::{EncoderProvider, OnnxBackend, OnnxOptions};
pub use pipeline::{PipelineConfig, RecoveryConfig, Transcriber, Transcript};
pub use runtime::{SpeechRuntime, SpeechSettings};
pub use segmentation::{TokenAggregator, TranscriptSegmenter};
pub use sidecar::{SidecarConfig, SidecarHealth, SidecarSpeechEngine};
pub use vad::{AdaptiveEnergyVad, EnergyVad, SileroVad, VadConfig, VoiceActivityDetector};
pub use vocab::Vocab;
