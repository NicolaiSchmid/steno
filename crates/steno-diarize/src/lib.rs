//! Speaker diarization for one lane: who spoke when, as `SpeakerCluster`s
//! with ranges, a unit embedding, a confidence and a sample clip. Plan:
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md` (`WP4d`) executing
//! decision 6 and gate G3 of `.plans/2026-10-01-cross-platform-speech-stack.md`;
//! the refinement constants come from `.plans/2026-09-29-speaker-calibration.md`.
//!
//! The pipeline is the pyannote community-1 shape `FluidAudio` runs on the
//! Mac, with Steno's own clustering and refinement above the tensors:
//!
//! 1. [`segmentation`]: ten-second windows every two seconds through the
//!    pyannote segmentation 3.0 model; its powerset classes become
//!    per-frame speaker activity for up to three local speakers.
//! 2. [`extraction`]: one embedding per window and local speaker, over
//!    the frames where only that speaker is active (a fifth of the
//!    window at least, about two seconds), through the `WeSpeaker`
//!    embedding model.
//! 3. [`clustering`]: agglomerative clustering of the unit embeddings,
//!    average linkage on cosine distance, cut at
//!    [`DiarizerConfig::clustering_threshold`].
//! 4. [`timeline`]: windows vote per frame for their clusters; the frame's
//!    speaker count is the rounded mean over the windows covering it;
//!    runs become exclusive turns.
//! 5. [`mapping`]: turns and chunks become "Speaker n" clusters with merged
//!    ranges, the duration-weighted unit mean embedding and the sample
//!    clip.
//! 6. [`refinement`]: each cluster with thirty seconds of speech is
//!    re-embedded over its own concatenated speech, clusters merge at
//!    cosine 0.60, fragments join at 0.30 or are dropped.
//!
//! The two models sit behind [`DiarizationBackend`]:
//! [`onnx::OnnxBackend`] runs the sherpa-onnx exports through ONNX Runtime
//! on every platform; `coreml::CoreMlBackend` runs `FluidAudio`'s compiled
//! models on the Mac, so the Mac keeps the embeddings the Swift app
//! stored. Everything above the trait is shared and tested without models.
//!
//! Where this crate knowingly differs from `FluidAudio` above the tensors:
//! the windows follow sherpa-onnx's layout, every full window and then one
//! padded tail whose padded frames do not vote, where `FluidAudio` strides
//! window offsets up to the end of the audio and counts every padded
//! frame ([`segmentation::window_offsets`]); average linkage on cosine
//! distance where it cuts centroid linkage and then runs `VBx`
//! ([`clustering`]); the speaker count rounds half up and a cluster
//! nobody voted for is active only where no cluster has a vote
//! ([`timeline`]). It also omits `FluidAudio`'s fallback to overlapped
//! frames, which the clean-frame ratio makes unreachable
//! ([`extraction::ExtractionRules::min_active_ratio`]). Everything else is
//! one for one with `Sources/StenoSpeech/Diarization` and the `FluidAudio`
//! code it ran.
//!
//! Entry points: [`ModelDiarizer`] is the `steno_core::Diarizer` the
//! meeting pipeline (WP6) holds, built by [`ModelDiarizer::onnx`] over
//! [`ModelStore::for_paths`] or by `ModelDiarizer::coreml` over
//! `coreml::model_directory`, where the Swift app installs `FluidAudio`'s
//! models; [`Pipeline`] exposes `analyze`, `map` and `refine` one at a
//! time for the calibration harness, which analyses a lane once and
//! sweeps the cut; [`fbank`] is the feature front end the ONNX backend
//! puts in front of the embedding model. Features: `onnx` builds the
//! ONNX Runtime backend, `coreml` the `CoreML` one (a no-op off macOS);
//! both are on by default.
//!
//! Audio never leaves the device: the only network access in this crate is
//! [`ModelStore`] fetching the published model files.

#![deny(unsafe_code)]

pub mod backend;
pub mod clustering;
#[cfg(all(feature = "coreml", target_os = "macos"))]
pub mod coreml;
mod diarizer;
mod error;
pub mod extraction;
pub mod fbank;
pub mod mapping;
pub mod models;
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod pipeline;
pub mod refinement;
pub mod segmentation;
pub mod timeline;

pub use backend::{BackendError, DiarizationBackend, SegmentationGeometry};
pub use diarizer::{BackendLoader, ModelDiarizer};
pub use error::DiarizeError;
pub use models::ModelStore;
pub use pipeline::{DEFAULT_CLUSTERING_THRESHOLD, DiarizerConfig, Pipeline};

/// Exact for every count below 2^53, far beyond any sample count.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn to_f64(count: usize) -> f64 {
    count as f64
}

/// The first of the greatest items under `compare`, as Swift's `max(by:)`
/// and `argmax` pick it; the standard library's `max_by` returns the last,
/// which would clip, absorb or decode differently from the Swift code on
/// a tie.
pub(crate) fn first_max_by<T>(
    items: impl IntoIterator<Item = T>,
    mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
) -> Option<T> {
    // `min_by` keeps the first of equal items; comparing reversed makes
    // the greatest the minimum.
    items.into_iter().min_by(|lhs, rhs| compare(rhs, lhs))
}
