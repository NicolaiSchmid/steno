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

    /// An output, or an input built for a model, had a shape the port
    /// does not expect.
    #[error("unexpected shape for {name}: {shape:?}")]
    Shape {
        name: &'static str,
        shape: Vec<usize>,
    },

    /// A model output is missing or not a multi-array.
    #[error("output {0} missing or not a multi-array")]
    MissingOutput(&'static str),

    /// A length or token id did not fit the `Int32` the models take.
    #[error("{name} out of range: {value}")]
    Range { name: &'static str, value: usize },

    /// A length, id or bin a model returned was negative.
    #[error("{name} is negative: {value}")]
    Negative { name: &'static str, value: i32 },

    /// The vocabulary file did not parse.
    #[error("vocabulary {path}: {message}")]
    Vocabulary { path: PathBuf, message: String },

    /// The shared pipeline failed above the models.
    #[error(transparent)]
    Pipeline(#[from] steno_speech::SpeechError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// A model call's failure inside the shared pipeline, which carries it as
/// a backend error and hands it back unchanged.
impl From<SpeechError> for steno_speech::SpeechError {
    fn from(error: SpeechError) -> Self {
        match error {
            SpeechError::Pipeline(inner) => inner,
            other => steno_speech::SpeechError::Backend(Box::new(other)),
        }
    }
}
