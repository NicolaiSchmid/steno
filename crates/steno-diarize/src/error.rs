//! The crate's error type: one variant per way the diarizer can fail to
//! run, so a log line tells a model file that could not be fetched from a
//! model that loaded but is not the one expected from a backend that
//! failed on a tensor call. `ModelDiarizer` boxes it into core's
//! `BoundaryResult`.

use std::fmt;

use crate::backend::BackendError;
use crate::models::ModelError;

/// What the diarizer reports when it cannot run.
#[derive(Debug, thiserror::Error)]
pub enum DiarizeError {
    /// A model file could not be fetched or verified.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// A model loaded but is not the one the pipeline expects: its
    /// metadata or its declared shapes disagree with what the pipeline
    /// decodes.
    #[error("model metadata: {0}")]
    Metadata(String),
    /// The tensor backend failed to load or to run.
    #[error("tensor backend: {0}")]
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
    /// Wraps any backend error, a message included.
    pub fn backend(error: impl Into<BackendError>) -> Self {
        DiarizeError::Backend(error.into())
    }

    /// A model that is not the one expected, with what differs.
    pub fn metadata(message: impl fmt::Display) -> Self {
        DiarizeError::Metadata(message.to_string())
    }
}
