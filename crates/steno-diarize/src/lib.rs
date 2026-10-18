//! Speaker diarization for one lane: who spoke when, as `SpeakerCluster`s
//! with ranges, a unit embedding, a confidence and a sample clip. The
//! pipeline is pyannote's community-1 shape ([`segmentation`],
//! [`extraction`], [`clustering`], [`timeline`], [`mapping`],
//! [`refinement`]) behind [`DiarizationBackend`], with ONNX and `CoreML` backends.
//!
//! Entry points: [`SidecarDiarizer`] runs the ONNX models in the speech
//! sidecar's child ([`sidecar`]), so an ONNX Runtime crash ends the child, not
//! the app; [`ModelDiarizer`] runs the same pipeline in this process; and
//! [`Pipeline`] exposes its stages one at a time for the calibration harness.
//!
//! Audio never leaves the device: this crate opens no connection, and
//! `SidecarDiarizer` hands a lane to the sidecar only over the child's stdin.

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
#[cfg(feature = "onnx")]
pub mod models;
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod pipeline;
pub mod refinement;
pub mod segmentation;
#[cfg(feature = "onnx")]
pub mod sidecar;
pub mod timeline;

pub use backend::{BackendError, DiarizationBackend, SegmentationGeometry};
pub use diarizer::{BackendLoader, ModelDiarizer};
pub use error::DiarizeError;
#[cfg(feature = "onnx")]
pub use models::Install;
pub use pipeline::{DEFAULT_CLUSTERING_THRESHOLD, DiarizerConfig, Pipeline};
#[cfg(feature = "onnx")]
pub use sidecar::SidecarDiarizer;

/// Exact for every count below 2^53, far beyond any sample count.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn to_f64(count: usize) -> f64 {
    count as f64
}

/// The first of the greatest items under `compare`, as Swift's `max(by:)`
/// picks it; `max_by` returns the last, which would decide ties differently.
pub(crate) fn first_max_by<T>(
    items: impl IntoIterator<Item = T>,
    mut compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
) -> Option<T> {
    // `min_by` keeps the first of equal items; comparing reversed makes the
    // greatest the minimum.
    items.into_iter().min_by(|lhs, rhs| compare(rhs, lhs))
}
