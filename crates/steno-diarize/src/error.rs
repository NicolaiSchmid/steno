//! The crate's error type.

use std::fmt;

use crate::backend::BackendError;
use crate::models::ModelError;

/// What the diarizer reports when it cannot run.
#[derive(Debug, thiserror::Error)]
pub enum DiarizeError {
    /// A model file could not be fetched or verified.
    #[error(transparent)]
    Model(#[from] ModelError),
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
    /// The diarizer's lock was poisoned by a panic in an earlier call.
    #[error("the diarizer is unusable after a panic in an earlier call")]
    Poisoned,
}

impl DiarizeError {
    /// Wraps any backend error.
    pub fn backend(error: impl Into<BackendError>) -> Self {
        DiarizeError::Backend(error.into())
    }

    /// A backend error from a message.
    pub fn message(message: impl fmt::Display) -> Self {
        DiarizeError::Backend(message.to_string().into())
    }
}
