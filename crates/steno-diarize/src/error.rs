//! The crate's error type: one variant per way the diarizer can fail to
//! run, so a log line tells a model file that could not be fetched from a
//! model that loaded but is not the one expected from a backend that
//! failed on a tensor call. `ModelDiarizer` boxes it into core's
//! `BoundaryResult`.

use std::fmt;

use crate::backend::BackendError;

/// What the diarizer reports when it cannot run.
#[derive(Debug, thiserror::Error)]
pub enum DiarizeError {
    /// The ONNX models could not be installed: a download that failed or
    /// was cut off, a file that failed its checksum, a folder that could
    /// not be written ([`crate::models::ensure`]).
    #[cfg(feature = "onnx")]
    #[error(transparent)]
    Model(#[from] steno_speech::SpeechError),
    /// A model loaded but is not the one the pipeline expects: its
    /// metadata or its declared shapes disagree with what the pipeline
    /// decodes.
    #[error("model metadata: {0}")]
    Metadata(String),
    /// The diarization backend failed to load or to run.
    #[error("diarization backend: {0}")]
    Backend(#[source] BackendError),
    /// The backend returned a tensor of the wrong size.
    #[error("{what}: expected {expected} values, got {got}")]
    Shape {
        what: &'static str,
        expected: usize,
        got: usize,
    },
}

impl DiarizeError {
    /// Wraps a backend error or a plain message.
    pub fn backend(error: impl Into<BackendError>) -> Self {
        DiarizeError::Backend(error.into())
    }

    /// A model that is not the one expected, with what differs.
    pub fn metadata(message: impl fmt::Display) -> Self {
        DiarizeError::Metadata(message.to_string())
    }
}
