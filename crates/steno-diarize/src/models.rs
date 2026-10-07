//! The two model files the ONNX backend loads, described as one
//! [`steno_speech::ModelAsset`] with the id [`ASSET_ID`], so they install
//! through `steno-speech`'s [`ModelStore`] like the speech models: into
//! `<store root>/diarization/`, which in Steno's store root (the models
//! directory's `onnx/` folder) is `<models directory>/onnx/diarization/`,
//! with the store's download lock, resume, 64 MiB ranges, progress and
//! mirror (`<mirror>/diarization/<file name>`). Nothing is committed; a
//! build without the files downloads them on first use. How a download
//! runs and what may be deleted: `steno_speech::model_store`.
//!
//! This crate opens no connection of its own: the store fetches the
//! published files, and only receives.

use std::path::{Path, PathBuf};

use steno_speech::{DownloadProgress, ModelAsset, ModelFile, ModelSource, ModelStore, SpeechError};

/// The asset's id, the name of its folder under the store root.
pub const ASSET_ID: &str = "diarization";

/// pyannote segmentation 3.0 as exported by sherpa-onnx (MIT; the
/// Hugging Face original is gated, the export is public).
pub const SEGMENTATION_FILE: &str = "pyannote-segmentation-3.0.onnx";

/// `WeSpeaker` ResNet34-LM trained on `VoxCeleb`, from the sherpa-onnx
/// speaker recognition models (Apache-2.0).
pub const EMBEDDING_FILE: &str = "wespeaker-en-voxceleb-resnet34-lm.onnx";

/// The asset: [`SEGMENTATION_FILE`] and [`EMBEDDING_FILE`] with their
/// sources, sizes and SHA-256, and the licences the app shows.
#[must_use]
pub fn asset() -> ModelAsset {
    let file = |name: &str, url: &str, sha256: &str, size: u64| ModelFile {
        name: name.to_owned(),
        source: Some(ModelSource::Url(url.to_owned())),
        sha256: sha256.to_owned(),
        size,
    };
    ModelAsset {
        id: ASSET_ID.to_owned(),
        display_name: "Speaker diarization (pyannote segmentation 3.0, WeSpeaker ResNet34-LM)"
            .to_owned(),
        licence: "MIT and Apache-2.0".to_owned(),
        attribution: "pyannote segmentation 3.0 (pyannote.audio, CNRS), MIT, as exported by sherpa-onnx; WeSpeaker ResNet34-LM trained on VoxCeleb (WeSpeaker), Apache-2.0, from the sherpa-onnx speaker recognition models".to_owned(),
        files: vec![
            file(
                SEGMENTATION_FILE,
                "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx",
                "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079",
                5_992_913,
            ),
            file(
                EMBEDDING_FILE,
                "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx",
                "e9848563da86f263117134dfd7ad63c92355b37de492b55e325400c9d9c39012",
                26_530_550,
            ),
        ],
    }
}

/// Where the two model files are: plain paths, which
/// [`crate::onnx::OnnxBackend::load`] opens and a child process can be
/// handed as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPaths {
    pub segmentation: PathBuf,
    pub embedding: PathBuf,
}

impl ModelPaths {
    /// The files in `directory`, the asset's folder
    /// ([`ModelStore::directory`]).
    #[must_use]
    pub fn in_directory(directory: &Path) -> Self {
        ModelPaths {
            segmentation: directory.join(SEGMENTATION_FILE),
            embedding: directory.join(EMBEDDING_FILE),
        }
    }
}

/// Installs the asset into `store` when a file is missing
/// ([`ModelStore::ensure`], which logs nothing itself; the progress goes
/// to the debug log) and returns the paths of the two files. An installed
/// folder is used as it is, without a request.
///
/// ```no_run
/// use steno_speech::ModelStore;
///
/// let paths = steno_diarize::models::ensure(&ModelStore::from_environment())?;
/// assert!(paths.segmentation.is_file() && paths.embedding.is_file());
/// # Ok::<(), steno_speech::SpeechError>(())
/// ```
pub fn ensure(store: &ModelStore) -> Result<ModelPaths, SpeechError> {
    let directory = store.ensure(&asset(), &mut log_download)?;
    Ok(ModelPaths::in_directory(&directory))
}

fn log_download(progress: DownloadProgress<'_>) {
    tracing::debug!(
        file = progress.file,
        received = progress.received,
        total = progress.total,
        "diarization model download"
    );
}
