//! The crate's one error type. Every boundary method of the engine returns
//! it through `?` as a `BoxError`, so the pipeline prints it as text.
//! Swift: `StenoSpeechError` in `Sources/StenoSpeech/StenoSpeech.swift`.

use std::path::PathBuf;
use std::time::Duration;

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
    #[error("model {asset} is not installed: {} missing in {}", missing.join(", "), directory.display())]
    NotInstalled {
        asset: String,
        /// `<root>/<asset id>`, where the files belong.
        directory: PathBuf,
        missing: Vec<String>,
    },
    /// The manifest has no URL for a missing file (the fp32 export until it
    /// is hosted); the files have to be put in place by hand.
    #[error(
        "model {asset} has no download location yet; put its files in {}",
        directory.display()
    )]
    NotHosted {
        asset: String,
        /// `<root>/<asset id>`, where the files belong.
        directory: PathBuf,
    },
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
    /// An asset id or file name in a manifest is not one plain path
    /// component, so joining it to the root could escape it.
    #[error("model asset {asset}: {name:?} is not a plain file or directory name")]
    InvalidName { asset: String, name: String },
    /// `tokens.txt` is not the sherpa-onnx `piece id` list in id order.
    #[error("{}: {detail}", path.display())]
    Vocabulary { path: PathBuf, detail: String },
    /// A model's inputs, outputs or metadata do not fit the export contract.
    #[error("model shape: {0}")]
    Shape(String),
    /// The blocking worker thread that runs inference ended without a
    /// result (a panic or a runtime shutdown).
    #[error("the speech worker thread stopped: {0}")]
    Worker(String),
    /// `transcribe` before `prepare` succeeded.
    #[error("the speech engine is not prepared; call prepare() first")]
    NotPrepared,
    /// A WAV file is not 16 kHz PCM-16.
    #[error("{}: {detail}", path.display())]
    Wav { path: PathBuf, detail: String },
    /// The sidecar process failed; unless the child itself reported the
    /// error ([`SidecarError::Remote`]) the client has stopped it, and the
    /// next call starts a fresh one.
    #[error("speech sidecar: {0}")]
    Sidecar(#[from] SidecarError),
}

/// How the sidecar process failed. Every variant but
/// [`SidecarError::Remote`] leaves the client without a child: the next
/// `prepare` or `transcribe` spawns and loads again.
#[derive(Debug, Error)]
pub enum SidecarError {
    /// The binary could not be started.
    #[error("could not start {}: {source}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Writing to the child's stdin failed while it was still running.
    #[error("pipe to the sidecar: {0}")]
    Pipe(#[source] std::io::Error),
    /// The child sent bytes the protocol does not define, answered with
    /// the wrong message or spoke another protocol version; it was killed.
    #[error("protocol violation, the sidecar was stopped: {0}")]
    Protocol(String),
    /// The child died before answering: an abort out of ONNX Runtime, a
    /// panic, a signal, an exit. `stderr` is the last lines it wrote.
    #[error("the sidecar died mid-request ({status}){}", if stderr.is_empty() { String::new() } else { format!(": {stderr}") })]
    Crashed { status: String, stderr: String },
    /// The child did not answer within the request's limit and was killed.
    #[error("the sidecar did not answer within {:.1} s and was stopped", after.as_secs_f64())]
    Timeout { after: Duration },
    /// The child's resident set passed the ceiling and it was killed.
    #[error(
        "the sidecar used {rss_bytes} bytes, over the {ceiling_bytes} byte ceiling, and was stopped"
    )]
    MemoryCeiling { rss_bytes: u64, ceiling_bytes: u64 },
    /// The child reported an error of its own (models that failed to load,
    /// a run ONNX Runtime refused) and keeps running.
    #[error("{0}")]
    Remote(String),
    /// The models root is not valid UTF-8, which the protocol's JSON cannot
    /// carry; no child was started.
    #[error("the models root {} is not valid UTF-8, which the sidecar protocol cannot carry", path.display())]
    NotUtf8 { path: PathBuf },
}

impl SpeechError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        SpeechError::Io {
            path: path.into(),
            source,
        }
    }
}
