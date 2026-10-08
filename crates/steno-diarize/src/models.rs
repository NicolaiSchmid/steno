//! The two model files the ONNX backend loads, described as one
//! [`steno_speech::ModelAsset`] with the id [`ASSET_ID`], so they install
//! through `steno-speech`'s [`ModelStore`] like the speech models: into
//! `<store root>/diarization/`, which in Steno's store root (the models
//! directory's `onnx/` folder) is `<models directory>/onnx/diarization/`,
//! with the store's download lock, resume, 64 MiB ranges, progress and
//! mirror (`<mirror>/diarization/<file name>`). Nothing is committed. How
//! a download runs and what may be deleted: `steno_speech::model_store`.
//! Rust-only: Swift's `ModelAsset.offlineDiarizer`
//! (`Sources/StenoSpeech/Models/ModelAsset.swift`) installs `FluidAudio`'s
//! `CoreML` models instead.
//!
//! Who may download them is the caller's [`Install`]: [`ensure`] installs
//! what is missing ([`Install::Allowed`], the CLI's model commands and
//! Settings' Download), [`installed`] only finds an installed folder
//! ([`Install::Never`], the app's pipeline) and is the check a gate that
//! asks whether a job can diarize calls too, so the two never disagree.
//!
//! [`DISPLAY_NAME`], [`LICENCE`] and [`ATTRIBUTION`] are the asset's name
//! and the credit its licences require, for the notices the stable
//! promotion (`.plans/2026-10-07-stable-promotion.md`) has the app show;
//! Settings' Acknowledgements still show `steno-host`'s own string.
//!
//! This crate opens no connection of its own: the store fetches the
//! published files and sends nothing but the request.

use std::path::{Path, PathBuf};

use steno_speech::model_store::sha256_of;
use steno_speech::{DownloadProgress, ModelAsset, ModelFile, ModelSource, ModelStore, SpeechError};

use crate::error::DiarizeError;

/// The asset's id, the name of its folder under the store root.
pub const ASSET_ID: &str = "diarization";

/// pyannote segmentation 3.0 as exported by sherpa-onnx (the Hugging Face
/// original is gated, the export is public).
pub const SEGMENTATION_FILE: &str = "pyannote-segmentation-3.0.onnx";

/// `WeSpeaker` ResNet34-LM trained on `VoxCeleb`, from the sherpa-onnx
/// speaker recognition models.
pub const EMBEDDING_FILE: &str = "wespeaker-en-voxceleb-resnet34-lm.onnx";

/// The asset's name.
pub const DISPLAY_NAME: &str =
    "Speaker diarization (pyannote segmentation 3.0, WeSpeaker ResNet34-LM)";

/// The models' licences as SPDX ids: MIT for the pyannote segmentation,
/// CC-BY-4.0 for the `WeSpeaker` embeddings trained on `VoxCeleb`.
pub const LICENCE: &str = "MIT AND CC-BY-4.0";

/// The credit both licences ask for: creator, source, licence and change.
pub const ATTRIBUTION: &str = "pyannote segmentation 3.0 by pyannote.audio (https://github.com/pyannote/pyannote-audio), MIT, converted to ONNX by sherpa-onnx; WeSpeaker ResNet34-LM by WeSpeaker (https://github.com/wenet-e2e/wespeaker), trained on VoxCeleb, CC-BY-4.0 (https://creativecommons.org/licenses/by/4.0/), converted to ONNX by sherpa-onnx";

/// The sherpa-onnx export of the segmentation model on Hugging Face.
pub const SEGMENTATION_REPO: &str = "csukuangfj/sherpa-onnx-pyannote-segmentation-3-0";

/// The commit of [`SEGMENTATION_REPO`] the segmentation model is fetched
/// at, so the bytes never change under the manifest's checksum.
pub const SEGMENTATION_REVISION: &str = "9403a6902bb58e3d5ae8c7e77c3422de279db2e0";

/// Whether building the diarizer's backend may download its models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// A missing file is downloaded first ([`ensure`]): the CLI's explicit
    /// commands and Settings' Download.
    Allowed,
    /// A missing file is [`DiarizeError::NotInstalled`] and no request is
    /// made ([`installed`]): the app's pipeline, which must not download
    /// during a job.
    Never,
}

/// The asset: [`SEGMENTATION_FILE`] and [`EMBEDDING_FILE`] with their
/// sources, sizes and SHA-256, [`DISPLAY_NAME`], [`LICENCE`] and
/// [`ATTRIBUTION`].
#[must_use]
pub fn asset() -> ModelAsset {
    let file = |name: &str, source: ModelSource, sha256: &str, size: u64| ModelFile {
        name: name.to_owned(),
        source: Some(source),
        sha256: sha256.to_owned(),
        size,
    };
    ModelAsset {
        id: ASSET_ID.to_owned(),
        display_name: DISPLAY_NAME.to_owned(),
        licence: LICENCE.to_owned(),
        attribution: ATTRIBUTION.to_owned(),
        files: vec![
            file(
                SEGMENTATION_FILE,
                ModelSource::HuggingFace {
                    repo: SEGMENTATION_REPO.to_owned(),
                    revision: SEGMENTATION_REVISION.to_owned(),
                    path: "model.onnx".to_owned(),
                },
                "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079",
                5_992_913,
            ),
            file(
                EMBEDDING_FILE,
                ModelSource::Url(
                    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx"
                        .to_owned(),
                ),
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

/// The paths of the two files, by `install`: [`ensure`] when it is
/// [`Install::Allowed`], [`installed`] when it is [`Install::Never`].
pub fn paths(store: &ModelStore, install: Install) -> Result<ModelPaths, DiarizeError> {
    match install {
        Install::Allowed => Ok(ensure(store)?),
        Install::Never => installed(store),
    }
}

/// The paths of the two files when both are present with their manifest
/// sizes ([`ModelStore::installed_directory`]), else
/// [`DiarizeError::NotInstalled`] naming the missing ones. It hashes
/// nothing and opens no connection.
///
/// ```no_run
/// use steno_speech::ModelStore;
///
/// let store = ModelStore::from_environment();
/// let ready = steno_diarize::models::installed(&store).is_ok();
/// ```
pub fn installed(store: &ModelStore) -> Result<ModelPaths, DiarizeError> {
    Ok(ModelPaths::in_directory(
        &store.installed_directory(&asset())?,
    ))
}

/// Installs the asset into `store` when a file is missing
/// ([`ModelStore::ensure`]; the download's progress goes to the debug log)
/// and returns the paths of the two files. An installed folder is used as
/// it is, without a request. The `<file name>*.part` files the diarizer's
/// earlier store left, which nothing resumes, are deleted first.
///
/// ```no_run
/// use steno_speech::ModelStore;
///
/// let paths = steno_diarize::models::ensure(&ModelStore::from_environment())?;
/// assert!(paths.segmentation.is_file() && paths.embedding.is_file());
/// # Ok::<(), steno_speech::SpeechError>(())
/// ```
pub fn ensure(store: &ModelStore) -> Result<ModelPaths, SpeechError> {
    let asset = asset();
    remove_old_parts(&store.directory(&asset), &asset);
    let directory = store.ensure(&asset, &mut log_download)?;
    Ok(ModelPaths::in_directory(&directory))
}

/// After a load over the installed files failed with `error`: hashes each
/// file against the manifest and deletes those that fail, so the store,
/// Settings and a gate report the asset not installed and a download
/// replaces them; the result is then [`DiarizeError::NotInstalled`]
/// naming them. When every file is intact, or cannot be hashed, it is
/// `error`. The hash runs only after a failure, never on a load that
/// works.
pub(crate) fn after_failed_load(store: &ModelStore, error: DiarizeError) -> DiarizeError {
    let asset = asset();
    let directory = store.directory(&asset);
    let mut missing = Vec::new();
    for file in &asset.files {
        let path = directory.join(&file.name);
        let Ok(actual) = sha256_of(&path) else {
            continue;
        };
        if actual == file.sha256 {
            continue;
        }
        tracing::warn!(path = %path.display(), %actual, expected = %file.sha256, %error, "diarization model failed its checksum, deleting it");
        match std::fs::remove_file(&path) {
            Ok(()) => missing.push(file.name.clone()),
            Err(remove) => {
                tracing::warn!(path = %path.display(), error = %remove, "corrupt diarization model left in place");
            }
        }
    }
    if missing.is_empty() {
        return error;
    }
    DiarizeError::NotInstalled {
        asset: asset.id,
        directory,
        missing,
    }
}

/// Deletes `<file name>*.part` in `directory` for each file of `asset`:
/// the temporary files of the diarizer's earlier store, which the
/// installed size would otherwise count. Best effort.
fn remove_old_parts(directory: &Path, asset: &ModelAsset) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let old_part = Path::new(name).extension().is_some_and(|e| e == "part")
            && asset.files.iter().any(|file| name.starts_with(&file.name));
        if old_part && let Err(error) = std::fs::remove_file(entry.path()) {
            tracing::warn!(path = %entry.path().display(), %error, "old partial download left in place");
        }
    }
}

fn log_download(progress: DownloadProgress<'_>) {
    tracing::debug!(
        file = progress.file,
        received = progress.received,
        total = progress.total,
        "diarization model download"
    );
}
