//! Steno's speech pipeline above the tensors: voice activity detection, the
//! pause-aligned chunker, the greedy TDT decoder, the overlap merge and the
//! mapping from pieces to segments, with the model calls behind one trait
//! so the `CoreML` backend on the Mac and every other platform (ONNX
//! Runtime, here) share one loop. Plan:
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md` (WP4, invariant 4) and
//! decisions 1 to 5 of `.plans/2026-10-01-cross-platform-speech-stack.md`.
//!
//! - [`backend`]: [`SpeechBackend`], the four model calls (features,
//!   encoder, decoder step, joint step), the tensors they exchange and the
//!   logits split.
//! - [`features`]: the `NeMo` mel preprocessor in Rust.
//! - [`vocab`]: `tokens.txt`, word boundaries, the splice-safe set.
//! - [`decoder`]: the greedy TDT loop over one encoder window.
//! - [`vad`]: [`VoiceActivityDetector`], Silero through ONNX Runtime and an
//!   energy detector for tests.
//! - [`chunker`]: the longest-pause layout with the 60 s memory clamp.
//! - [`merge`]: the time-tolerant LCS merge of overlapping windows.
//! - [`segmentation`]: pieces to words to [`RawSegment`](steno_core::RawSegment)s.
//! - [`language`]: the per-segment language tagger.
//! - [`pipeline`]: [`Transcriber`], which runs the above in order and
//!   retries empty chunks with a wider window.
//! - [`onnx`]: the ONNX Runtime backend over our fp32 export.
//! - [`model_store`]: the manifest and the checksummed download.
//! - [`engine`]: [`OnnxSpeechEngine`], the `SpeechEngine` implementation.
//! - [`error`]: [`SpeechError`], the one error type.
//!
//! # Models
//!
//! Nothing is committed. [`ModelStore`] keeps every asset under
//! `<support directory>/Models/<asset id>/` or under the directory
//! `STENO_MODELS_DIR` names. Silero VAD downloads from the sherpa-onnx
//! release with its checksum verified. The fp32 Parakeet export is not
//! hosted yet: produce it with `spikes/onnx-speech/export/` and place
//! `encoder.onnx`, `encoder.weights`, `decoder.onnx`, `joiner.onnx` and
//! `tokens.txt` in `<root>/parakeet-tdt-0.6b-v3-fp32/`; `prepare` says so
//! when they are missing.
//!
//! # Threads and process boundaries
//!
//! Inference is synchronous and CPU-bound. [`OnnxSpeechEngine`] runs it on
//! a blocking thread when a tokio runtime is present. Every type here is
//! `Send` and holds its own sessions, so the sidecar of speech-stack
//! decision 5 can host a [`Transcriber`] in another process behind the
//! same `SpeechEngine`.
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

// Signal processing counts samples, frames and bins in `usize` and turns
// them into seconds and weights in `f32`/`f64` on every other line; the
// values are tiny (a recording is under 2^32 samples) and the per-site
// allowances the other crates use would bury the arithmetic. The remaining
// pedantic lints stay on.
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
pub mod segmentation;
pub mod vad;
pub mod vocab;

pub use backend::{
    DecoderState, DecoderStep, EncoderOutput, FRAME_SAMPLES, FRAME_SECONDS, Features,
    JointDecision, ModelShape, SAMPLE_RATE, SpeechBackend, sample_count, split_logits,
};
pub use chunker::{Chunk, ChunkerConfig, Cut};
pub use decoder::{DecodeStats, DecoderConfig, Token};
pub use engine::OnnxSpeechEngine;
pub use error::SpeechError;
pub use features::MelExtractor;
pub use language::{LanguageRecognizer, LanguageTagger, StopwordRecognizer, WhatlangRecognizer};
pub use model_store::{DownloadProgress, ModelAsset, ModelFile, ModelStore};
pub use onnx::{OnnxBackend, OnnxOptions};
pub use pipeline::{PipelineConfig, RecoveryConfig, Transcriber, Transcript};
pub use segmentation::{TokenAggregator, TranscriptSegmenter};
pub use vad::{EnergyVad, SileroVad, VadConfig, VoiceActivityDetector};
pub use vocab::Vocab;
