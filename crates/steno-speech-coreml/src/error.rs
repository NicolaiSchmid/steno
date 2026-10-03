//! The crate's error: model loading, inference, shapes, files.

use std::path::PathBuf;

/// What can go wrong between the model directory and a transcript.
#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    /// A model bundle did not load; `path` is the `.mlmodelc` directory.
    #[error("loading {path}: {message}")]
    ModelLoad { path: PathBuf, message: String },

    /// CoreML refused a prediction or an array.
    #[error("CoreML: {0}")]
    CoreMl(String),

    /// An output had a shape the port does not expect.
    #[error("unexpected shape for {name}: {shape:?}")]
    Shape {
        name: &'static str,
        shape: Vec<usize>,
    },

    /// A model output is missing or not a multi-array.
    #[error("output {0} missing or not a multi-array")]
    MissingOutput(&'static str),

    /// Less than the 0.3 s FluidAudio accepts (`ASRError.invalidAudioData`).
    #[error("audio too short: {0} samples, need at least 4800")]
    AudioTooShort(usize),

    /// The joint reported a duration bin outside the configured bins.
    #[error("duration bin out of range: {0}")]
    DurationBin(usize),

    /// A length or token id did not fit the `Int32` the models take.
    #[error("{name} out of range: {value}")]
    Range { name: &'static str, value: usize },

    /// The vocabulary file did not parse.
    #[error("vocabulary {path}: {message}")]
    Vocabulary { path: PathBuf, message: String },

    /// A WAV file the harness could not read.
    #[error("{path}: {message}")]
    Wav { path: PathBuf, message: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
