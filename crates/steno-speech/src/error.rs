//! [`SpeechError`], the crate's one error type, and [`SidecarError`], its
//! cause when the speech sidecar fails. Every boundary method of the
//! engines returns it through `?` as a `BoxError`, so the pipeline prints
//! it as text.
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
    /// A missing file has no source in the manifest and no mirror is set
    /// (no asset Steno ships); it has to be put in place by hand.
    #[error(
        "model {asset} has no download location; put its files in {}",
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
    /// An asset id in a manifest is not one plain path component, or a
    /// file name is not plain components joined by `/`, so joining it to
    /// the root could escape it.
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
    /// The speech sidecar failed: unless the child reported the error
    /// itself ([`SidecarError::Remote`], [`SidecarError::DiarizerLoad`]), no
    /// child is left running, and the next call starts a fresh one.
    #[error("speech sidecar: {0}")]
    Sidecar(#[from] SidecarError),
}

/// How the speech sidecar failed. Every variant but
/// [`SidecarError::Remote`] and [`SidecarError::DiarizerLoad`] leaves the
/// parent without a child: the next `prepare`, `transcribe` or `diarize`
/// spawns and loads again.
#[derive(Debug, Error)]
pub enum SidecarError {
    /// The binary could not be started.
    #[error("could not start {}: {source}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A pipe to or from the child failed, or a thread that supervises it
    /// could not start.
    #[error("a pipe to or from the child: {0}")]
    Pipe(#[source] std::io::Error),
    /// The child sent bytes the protocol does not define, answered with
    /// the wrong message or spoke another protocol version; it was killed.
    #[error("protocol violation, the child was killed: {0}")]
    Protocol(String),
    /// The child died before answering, at start or mid-request: an abort
    /// out of ONNX Runtime, a panic, a signal, an exit, a library it could
    /// not load. `stderr` is the last lines it wrote.
    #[error("the child died before answering ({status}){}", if stderr.is_empty() { String::new() } else { format!(": {stderr}") })]
    Crashed { status: String, stderr: String },
    /// The child did not answer within the request's limit and was killed.
    #[error("the child did not answer within {:.1} s and was killed", after.as_secs_f64())]
    Timeout { after: Duration },
    /// The child's resident set passed the ceiling and it was killed.
    #[error(
        "the child used {rss_bytes} bytes, over the {ceiling_bytes} byte ceiling, and was killed"
    )]
    MemoryCeiling { rss_bytes: u64, ceiling_bytes: u64 },
    /// The child reported an error of its own (models that failed to load,
    /// a run ONNX Runtime refused) and keeps running.
    #[error("{0}")]
    Remote(String),
    /// The child could not load the diarizer's models (a file ONNX Runtime
    /// refuses) and keeps running; reported by the child, like
    /// [`SidecarError::Remote`].
    #[error("the diarizer's models did not load: {0}")]
    DiarizerLoad(String),
    /// A path the protocol's JSON carries (the models root, a diarizer
    /// model file) is not valid UTF-8; nothing was sent to a child.
    #[error("the model path {} is not valid UTF-8, which the protocol cannot carry", path.display())]
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
