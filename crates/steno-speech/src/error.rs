//! The crate's one error type. Every boundary method of the engine returns
//! it through `?` as a `BoxError`, so the pipeline prints it as text.
//! Swift: `StenoSpeechError` in `Sources/StenoSpeech/StenoSpeech.swift`.

use std::path::PathBuf;

use thiserror::Error;

/// What the speech crate reports when it fails.
#[derive(Debug, Error)]
pub enum SpeechError {
    /// ONNX Runtime refused a model, an input or a run.
    #[error("ONNX Runtime: {0}")]
    Runtime(#[from] ort::Error),
    /// ONNX Runtime refused a session option (the builder error carries
    /// the builder for recovery, so it is reported as text).
    #[error("ONNX Runtime session options: {0}")]
    SessionOptions(String),
    /// A file could not be read, written, renamed or removed.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The asset's directory lacks files the manifest names.
    #[error("model {asset} is not installed: {} missing under {}", missing.join(", "), root.display())]
    NotInstalled {
        asset: String,
        root: PathBuf,
        missing: Vec<String>,
    },
    /// The manifest has no URL for a missing file (the fp32 export until it
    /// is hosted); the files have to be put in place by hand.
    #[error(
        "model {asset} has no download location yet; put its files under {} or point STENO_MODELS_DIR at a directory that holds them",
        root.display()
    )]
    NotHosted { asset: String, root: PathBuf },
    /// The HTTP download failed or returned a non-success status.
    #[error("download of {url} failed: {source}")]
    Download {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },
    /// A downloaded or installed file has the wrong length.
    #[error("{}: {actual} bytes, the manifest says {expected}", path.display())]
    Size {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    /// A downloaded or installed file has the wrong content.
    #[error("{}: sha256 {actual} does not match the manifest's {expected}", path.display())]
    Checksum {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    /// `tokens.txt` is not the sherpa-onnx `piece id` list in id order.
    #[error("{}: {detail}", path.display())]
    Vocabulary { path: PathBuf, detail: String },
    /// A model's inputs, outputs or metadata do not fit the export contract.
    #[error("model shape: {0}")]
    Shape(String),
    /// `transcribe` before `prepare` succeeded.
    #[error("the speech engine is not prepared; call prepare() first")]
    NotPrepared,
}

impl SpeechError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        SpeechError::Io {
            path: path.into(),
            source,
        }
    }
}
