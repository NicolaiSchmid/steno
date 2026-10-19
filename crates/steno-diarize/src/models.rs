//! The two model files the ONNX backend loads, one [`steno_speech::ModelAsset`]
//! ([`ASSET_ID`]) installed by `steno-speech`'s [`ModelStore`] like the speech
//! models. Who may download them is the caller's [`Install`]: [`ensure`]
//! installs what is missing, [`installed`] only finds an installed folder.
//! The store sends nothing but the request.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use steno_speech::model_store::sha256_of;
use steno_speech::{DownloadProgress, ModelAsset, ModelFile, ModelSource, ModelStore, SpeechError};

use crate::error::DiarizeError;

/// The asset's id, the name of its folder under the store root.
pub const ASSET_ID: &str = "diarization";

/// pyannote segmentation 3.0 as exported by sherpa-onnx.
pub const SEGMENTATION_FILE: &str = "pyannote-segmentation-3.0.onnx";

/// `WeSpeaker` ResNet34-LM trained on `VoxCeleb`.
pub const EMBEDDING_FILE: &str = "wespeaker-en-voxceleb-resnet34-lm.onnx";

/// The asset's name.
pub const DISPLAY_NAME: &str =
    "Speaker diarization (pyannote segmentation 3.0, WeSpeaker ResNet34-LM)";

/// The models' licences as SPDX ids: MIT for the pyannote segmentation,
/// CC-BY-4.0 for the `WeSpeaker` embeddings trained on `VoxCeleb`.
pub const LICENCE: &str = "MIT AND CC-BY-4.0";

/// The credit both licences ask for: creator, source, licence and change.
pub const ATTRIBUTION: &str = "pyannote segmentation 3.0 by pyannote.audio, Copyright (c) 2020 CNRS (https://github.com/pyannote/pyannote-audio), MIT, converted to ONNX by sherpa-onnx; WeSpeaker ResNet34-LM by WeSpeaker (https://github.com/wenet-e2e/wespeaker), trained on VoxCeleb, CC-BY-4.0 (https://creativecommons.org/licenses/by/4.0/), converted to ONNX by sherpa-onnx";

/// The sherpa-onnx export of the segmentation model on Hugging Face.
pub const SEGMENTATION_REPO: &str = "csukuangfj/sherpa-onnx-pyannote-segmentation-3-0";

/// The commit of [`SEGMENTATION_REPO`] the model is fetched at, so the bytes
/// never change under the manifest's checksum.
pub const SEGMENTATION_REVISION: &str = "9403a6902bb58e3d5ae8c7e77c3422de279db2e0";

/// The install policy the speech sidecar shares; see [`paths`].
pub use steno_speech::Install;

/// The asset: [`SEGMENTATION_FILE`] and [`EMBEDDING_FILE`] with their
/// sources, sizes and SHA-256.
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

/// Where the two model files are: plain paths a child process can be handed.
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
        Install::Allowed => ensure(store),
        Install::Never => installed(store),
    }
}

/// The paths of the two files when both are present with their manifest
/// sizes, else [`DiarizeError::NotInstalled`] naming the missing ones. Hashes
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

/// Installs the asset into `store` when a file is missing and returns the
/// paths of the two files; a failed download is [`DiarizeError::Model`].
///
/// ```no_run
/// use steno_speech::ModelStore;
///
/// let paths = steno_diarize::models::ensure(&ModelStore::from_environment())?;
/// assert!(paths.segmentation.is_file() && paths.embedding.is_file());
/// # Ok::<(), steno_diarize::DiarizeError>(())
/// ```
pub fn ensure(store: &ModelStore) -> Result<ModelPaths, DiarizeError> {
    remove_old_parts(store);
    let directory = store.ensure(&asset(), &mut log_download)?;
    Ok(ModelPaths::in_directory(&directory))
}

/// After a load over the installed files failed with `error`: hashes each
/// file and deletes those that fail the manifest, so a download replaces them,
/// and returns [`DiarizeError::NotInstalled`] naming them (or naming a file
/// that is already gone). When every file is intact, or cannot be checked, it
/// is `error`. The hash runs only after a failure.
pub(crate) fn after_failed_load(store: &ModelStore, error: DiarizeError) -> DiarizeError {
    after_failed_load_of(store, &asset(), error)
}

/// [`after_failed_load`] over `asset`'s manifest.
fn after_failed_load_of(
    store: &ModelStore,
    asset: &ModelAsset,
    error: DiarizeError,
) -> DiarizeError {
    let directory = match store.installed_directory(asset) {
        Ok(directory) => directory,
        Err(gone) => return gone.into(),
    };
    let missing: Vec<String> = asset
        .files
        .iter()
        .filter(|file| {
            let path = directory.join(&file.name);
            is_gone_after_check(file, &path, sha256_of(&path))
        })
        .map(|file| file.name.clone())
        .collect();
    if missing.is_empty() {
        return error;
    }
    // The error can hold the child's stderr, which may hold a path: debug only.
    tracing::debug!(%error, "the load that failed");
    DiarizeError::NotInstalled {
        asset: asset.id.clone(),
        directory,
        missing,
    }
}

/// Whether `file` is missing after the check: deleted for a wrong digest,
/// or already gone. A file that cannot be hashed or deleted for another
/// reason is kept and is not missing.
fn is_gone_after_check(file: &ModelFile, path: &Path, digest: Result<String, SpeechError>) -> bool {
    let actual = match digest {
        Ok(actual) if actual == file.sha256 => return false,
        Ok(actual) => actual,
        Err(SpeechError::Io { source, .. }) => {
            return source.kind() == ErrorKind::NotFound;
        }
        Err(_) => return false,
    };
    tracing::warn!(path = %path.display(), %actual, expected = %file.sha256, "diarization model failed its checksum after a failed load, deleting it");
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(remove) if remove.kind() == ErrorKind::NotFound => true,
        Err(remove) => {
            tracing::warn!(path = %path.display(), error = %remove, "corrupt diarization model left in place");
            false
        }
    }
}

/// Deletes `<file name>*.part` in the asset's folder: leftovers of the
/// earlier store, which nothing resumes. Best effort.
pub fn remove_old_parts(store: &ModelStore) {
    let asset = asset();
    let Ok(entries) = std::fs::read_dir(store.directory(&asset)) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A file removed between the check and the load makes the failed load
    /// [`DiarizeError::NotInstalled`] naming it; the file still there is kept.
    #[test]
    fn a_file_gone_by_the_failed_load_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::in_models_directory(dir.path());
        let asset = asset();
        let folder = store.directory(&asset);
        std::fs::create_dir_all(&folder).unwrap();
        let segmentation = &asset.files[0];
        std::fs::File::create(folder.join(&segmentation.name))
            .unwrap()
            .set_len(segmentation.size)
            .unwrap();

        let error = after_failed_load(&store, DiarizeError::metadata("the load failed"));
        let DiarizeError::NotInstalled { missing, .. } = &error else {
            panic!("not installed: {error:?}");
        };
        assert_eq!(missing, &[EMBEDDING_FILE.to_owned()]);
        assert!(folder.join(SEGMENTATION_FILE).exists(), "not hashed, kept");
    }

    /// Two files whose manifest holds the SHA-256 of their known contents.
    fn synthetic_asset() -> ModelAsset {
        let file = |name: &str, sha256: &str, size: u64| ModelFile {
            name: name.to_owned(),
            source: None,
            sha256: sha256.to_owned(),
            size,
        };
        ModelAsset {
            id: "synthetic".to_owned(),
            display_name: "Synthetic".to_owned(),
            licence: "MIT".to_owned(),
            attribution: String::new(),
            files: vec![
                file(
                    "a.onnx",
                    "fba586be3b6f140b30389654d548a660d3a746cf8344ab6f39248caf65e2da4d",
                    12,
                ),
                file(
                    "b.onnx",
                    "aa580156f36e357b5bfb0dcd869a026c7b0a244e7b01cba17d5da1dc1e7039cd",
                    9,
                ),
            ],
        }
    }

    /// The synthetic asset installed in a store in `dir`, with `b.onnx`
    /// holding `embedding`; its folder.
    fn install_synthetic(dir: &Path, embedding: &[u8]) -> (ModelStore, PathBuf) {
        let store = ModelStore::in_models_directory(dir);
        let folder = store.directory(&synthetic_asset());
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("a.onnx"), b"segmentation").unwrap();
        std::fs::write(folder.join("b.onnx"), embedding).unwrap();
        assert!(store.is_installed(&synthetic_asset()));
        (store, folder)
    }

    fn fail_load(store: &ModelStore) -> DiarizeError {
        after_failed_load_of(
            store,
            &synthetic_asset(),
            DiarizeError::metadata("the load failed"),
        )
    }

    /// Intact files survive a failed load, which keeps its own error; a file
    /// failing its checksum is deleted and named, and the intact one is kept.
    #[test]
    fn only_the_file_that_fails_its_checksum_is_deleted() {
        for (embedding, deleted) in [(&b"embedding"[..], false), (&b"embeddinG"[..], true)] {
            let dir = tempfile::tempdir().unwrap();
            let (store, folder) = install_synthetic(dir.path(), embedding);
            let error = fail_load(&store);
            if deleted {
                assert!(
                    matches!(&error, DiarizeError::NotInstalled { asset, missing, .. }
                        if asset == "synthetic" && missing == &["b.onnx".to_owned()]),
                    "{error:?}"
                );
                assert!(!folder.join("b.onnx").exists());
            } else {
                assert!(
                    matches!(&error, DiarizeError::Metadata(detail) if detail == "the load failed"),
                    "{error:?}"
                );
                assert_eq!(std::fs::read(folder.join("b.onnx")).unwrap(), embedding);
            }
            assert_eq!(
                std::fs::read(folder.join("a.onnx")).unwrap(),
                b"segmentation"
            );
        }
    }

    /// A file deleted since the folder was found is missing whether gone by its
    /// hash or its removal; any other hashing error keeps it unnamed.
    #[test]
    fn a_file_that_vanishes_during_the_check_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = &synthetic_asset().files[1];
        let gone = dir.path().join("gone.onnx");
        // Absent when it is hashed.
        assert!(is_gone_after_check(file, &gone, sha256_of(&gone)));
        // Hashed as corrupt, then deleted by someone else before the remove.
        assert!(is_gone_after_check(file, &gone, Ok("0".repeat(64))));
        // Kept when the hash fails for another reason.
        let kept = dir.path().join("kept.onnx");
        std::fs::write(&kept, b"embeddinG").unwrap();
        let denied = SpeechError::Io {
            path: kept.clone(),
            source: std::io::Error::from(ErrorKind::PermissionDenied),
        };
        assert!(!is_gone_after_check(file, &kept, Err(denied)));
        assert!(kept.exists());
        std::fs::write(&kept, b"embedding").unwrap();
        assert!(!is_gone_after_check(file, &kept, sha256_of(&kept)));
        assert!(kept.exists());
    }
}
