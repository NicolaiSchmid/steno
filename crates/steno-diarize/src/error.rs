//! The crate's error type: one variant per way the diarizer can fail to run.

use std::fmt;

use crate::backend::BackendError;

/// What the diarizer reports when it cannot run.
#[derive(Debug, thiserror::Error)]
pub enum DiarizeError {
    /// The ONNX models are not installed: a file is missing or has the wrong
    /// size and this diarizer may not download it, or a file failed its checksum
    /// after a failed load and was deleted. It survives boxing, so a caller finds
    /// it with `downcast_ref::<DiarizeError>()`.
    #[cfg(feature = "onnx")]
    #[error("model {asset} is not installed: {} missing in {}", missing.join(", "), directory.display())]
    NotInstalled {
        asset: String,
        /// `<root>/<asset id>`, where the files belong.
        directory: std::path::PathBuf,
        missing: Vec<String>,
    },
    /// The ONNX models could not be installed: a failed download, a bad
    /// checksum, a folder that could not be written.
    #[cfg(feature = "onnx")]
    #[error(transparent)]
    Model(steno_speech::SpeechError),
    /// The speech sidecar failed to diarize: its child could not load the
    /// models, died, hung or overran the memory ceiling.
    #[cfg(feature = "onnx")]
    #[error(transparent)]
    Sidecar(steno_speech::SpeechError),
    /// A model loaded but its metadata or declared shapes are not what the
    /// pipeline decodes.
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
