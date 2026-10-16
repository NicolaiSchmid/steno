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

    /// CoreML raised an Objective-C exception, which the `coreml` module
    /// caught at the boundary (`coreml::contained`): its `name` and
    /// `reason`, empty when it had none (and then left out of the text).
    /// The engine drops its models after one, so the next call loads them
    /// again.
    #[error("CoreML raised {name}{}", reason_suffix(.reason))]
    Exception { name: String, reason: String },

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

impl SpeechError {
    /// Whether this is, or carries through the shared pipeline, an
    /// Objective-C exception CoreML raised ([`SpeechError::Exception`]).
    #[must_use]
    pub fn is_exception(&self) -> bool {
        match self {
            SpeechError::Exception { .. } => true,
            SpeechError::Pipeline(steno_speech::SpeechError::Backend(source)) => source
                .downcast_ref::<SpeechError>()
                .is_some_and(SpeechError::is_exception),
            _ => false,
        }
    }
}

/// `": reason"`, or nothing for an exception without one.
fn reason_suffix(reason: &str) -> String {
    if reason.is_empty() {
        String::new()
    } else {
        format!(": {reason}")
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipeline_error_goes_back_unchanged_and_a_model_error_as_a_backend_error() {
        let inner = steno_speech::SpeechError::Shape("frames".to_owned());
        let back = steno_speech::SpeechError::from(SpeechError::Pipeline(inner));
        assert!(
            matches!(&back, steno_speech::SpeechError::Shape(detail) if detail == "frames"),
            "{back:?}"
        );
        let model = steno_speech::SpeechError::from(SpeechError::MissingOutput("logits"));
        let steno_speech::SpeechError::Backend(source) = model else {
            panic!("{model:?}");
        };
        assert!(matches!(
            source.downcast_ref::<SpeechError>(),
            Some(SpeechError::MissingOutput("logits"))
        ));
    }

    /// The engine tells an exception from any other failure also once the
    /// shared pipeline has carried it back, and the text a meeting fails
    /// with names the exception and its reason.
    #[test]
    fn an_exception_is_known_bare_and_through_the_pipeline() {
        let exception = || SpeechError::Exception {
            name: "NSGenericException".to_owned(),
            reason: "no IOSurface".to_owned(),
        };
        assert_eq!(
            exception().to_string(),
            "CoreML raised NSGenericException: no IOSurface"
        );
        assert!(exception().is_exception());
        let carried = SpeechError::Pipeline(steno_speech::SpeechError::from(exception()));
        assert!(carried.is_exception(), "{carried:?}");
        assert_eq!(
            carried.to_string(),
            "CoreML raised NSGenericException: no IOSurface"
        );
        assert!(!SpeechError::CoreMl("refused".to_owned()).is_exception());
        let bare = SpeechError::Exception {
            name: "nil".to_owned(),
            reason: String::new(),
        };
        assert_eq!(bare.to_string(), "CoreML raised nil", "no trailing colon");
        let other = SpeechError::Pipeline(steno_speech::SpeechError::from(
            SpeechError::MissingOutput("mel"),
        ));
        assert!(!other.is_exception());
    }
}
