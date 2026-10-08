//! The crate's error type: one variant per way the diarizer can fail to
//! run, so a log line tells a model file that could not be fetched from a
//! model that loaded but is not the one expected from a backend that
//! failed on a tensor call. `ModelDiarizer` boxes it into core's
//! `BoundaryResult`.

use std::fmt;
use std::path::PathBuf;

use crate::backend::BackendError;

/// What the diarizer reports when it cannot run.
#[derive(Debug, thiserror::Error)]
pub enum DiarizeError {
    /// The ONNX models are not installed and this diarizer may not
    /// download them ([`crate::Install::Never`]): a file is missing, has
    /// the wrong size, or failed its checksum after a failed load and was
    /// deleted. The fields and message are `steno_speech`'s
    /// `SpeechError::NotInstalled`, which converts into this variant. The
    /// variant survives boxing into core's `BoxError`, so a caller tells
    /// it from a failed load with `downcast_ref::<DiarizeError>()`.
    #[error("model {asset} is not installed: {} missing in {}", missing.join(", "), directory.display())]
    NotInstalled {
        asset: String,
        /// `<root>/<asset id>`, where the files belong.
        directory: PathBuf,
        missing: Vec<String>,
    },
    /// The ONNX models could not be installed: a download that failed or
    /// was cut off, a file that failed its checksum, a folder that could
    /// not be written ([`crate::models::ensure`]).
    #[cfg(feature = "onnx")]
    #[error(transparent)]
    Model(steno_speech::SpeechError),
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

/// `SpeechError::NotInstalled` becomes [`DiarizeError::NotInstalled`] with
/// its fields; every other store error is [`DiarizeError::Model`].
#[cfg(feature = "onnx")]
impl From<steno_speech::SpeechError> for DiarizeError {
    fn from(error: steno_speech::SpeechError) -> Self {
        match error {
            steno_speech::SpeechError::NotInstalled {
                asset,
                directory,
                missing,
            } => DiarizeError::NotInstalled {
                asset,
                directory,
                missing,
            },
            other => DiarizeError::Model(other),
        }
    }
}
