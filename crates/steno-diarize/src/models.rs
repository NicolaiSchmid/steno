//! The two model files the ONNX backend loads, described as one
//! [`steno_speech::ModelAsset`] with the id [`ASSET_ID`], so they install
//! through `steno-speech`'s [`ModelStore`] like the speech models: into
//! `<store root>/diarization/` (in Steno,
//! `<models directory>/onnx/diarization/`), with the store's download
//! lock, resume, 64 MiB ranges, progress and mirror
//! (`<mirror>/diarization/<file name>`). Nothing is committed. How a
//! download runs and what may be deleted: `steno_speech::model_store`.
//! Rust-only: Swift's `ModelAsset.offlineDiarizer`
//! (`Sources/StenoSpeech/Models/ModelAsset.swift`) installs `FluidAudio`'s
//! `CoreML` models instead.
//!
//! Who may download them is the caller's [`Install`]: [`ensure`] installs
//! what is missing ([`Install::Allowed`]), [`installed`] only finds an
//! installed folder ([`Install::Never`]). A gate that asks whether a job
//! can diarize calls the same function, so the two never disagree.
//!
//! [`DISPLAY_NAME`], [`LICENCE`] and [`ATTRIBUTION`] are the asset's name
//! and the credit its licences require, for the notices the stable
//! promotion (`.plans/2026-10-07-stable-promotion.md`) has the app show;
//! Settings' Acknowledgements still show `steno-host`'s own string.
//!
//! This crate opens no connection of its own: the store fetches the
//! published files and sends nothing but the request.

use std::io::ErrorKind;
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

/// The credit both licences ask for: creator (with pyannote's copyright
/// notice, which MIT asks for), source, licence and change.
pub const ATTRIBUTION: &str = "pyannote segmentation 3.0 by pyannote.audio, Copyright (c) 2020 CNRS (https://github.com/pyannote/pyannote-audio), MIT, converted to ONNX by sherpa-onnx; WeSpeaker ResNet34-LM by WeSpeaker (https://github.com/wenet-e2e/wespeaker), trained on VoxCeleb, CC-BY-4.0 (https://creativecommons.org/licenses/by/4.0/), converted to ONNX by sherpa-onnx";

/// The sherpa-onnx export of the segmentation model on Hugging Face.
pub const SEGMENTATION_REPO: &str = "csukuangfj/sherpa-onnx-pyannote-segmentation-3-0";

/// The commit of [`SEGMENTATION_REPO`] the segmentation model is fetched
/// at, so the bytes never change under the manifest's checksum.
pub const SEGMENTATION_REVISION: &str = "9403a6902bb58e3d5ae8c7e77c3422de279db2e0";

/// The install policy the speech sidecar shares; see [`paths`].
pub use steno_speech::Install;

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
        Install::Allowed => ensure(store),
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
/// it is, without a request. The earlier store's partial files are
/// deleted first ([`remove_old_parts`]). A failed download is
/// [`DiarizeError::Model`].
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
/// file against the manifest and deletes those that fail, so the store,
/// Settings and a gate report the asset not installed and a download
/// replaces them; the result is then [`DiarizeError::NotInstalled`]
/// naming them. A file that is gone by then (a Settings Remove during the
/// load, or a concurrent check that deleted it first) is reported the same
/// way. When every file is intact, or cannot be hashed or deleted for
/// another reason, it is `error`. The hash runs only after a failure,
/// never on a load that works.
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
    // The load's error can be a crash report holding the child's stderr,
    // which may hold a path: debug only.
    tracing::debug!(%error, "the load that failed");
    DiarizeError::NotInstalled {
        asset: asset.id.clone(),
        directory,
        missing,
    }
}

/// Whether `file`, at `path`, is missing after the check, given `digest`,
/// its hash: deleted because the digest is not the manifest's, or already
/// gone (`NotFound`) when it was hashed or deleted. A file that cannot be
/// hashed or deleted for another reason (permissions, I/O) is kept and is
/// not missing.
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

/// Deletes `<file name>*.part` in the asset's folder in `store`: the
/// temporary files of the diarizer's earlier store, which nothing resumes
/// and the installed size would otherwise count. [`ensure`] and Settings'
/// Download call it. Best effort: a file that cannot be deleted is logged
/// and left.
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

    /// A file removed between the check and the load (a Settings Remove)
    /// makes the failed load [`DiarizeError::NotInstalled`] naming it, not
    /// the backend's error, and the file still there is kept.
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
                // The SHA-256 of the 12 bytes `segmentation`.
                file(
                    "a.onnx",
                    "fba586be3b6f140b30389654d548a660d3a746cf8344ab6f39248caf65e2da4d",
                    12,
                ),
                // The SHA-256 of the 9 bytes `embedding`.
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

    /// A load failed over the synthetic asset in `store`; the result.
    fn fail_load(store: &ModelStore) -> DiarizeError {
        after_failed_load_of(
            store,
            &synthetic_asset(),
            DiarizeError::metadata("the load failed"),
        )
    }

    /// Intact files survive a failed load, which keeps its own error: a
    /// crash, a hang or a ceiling hit over good models deletes nothing.
    #[test]
    fn intact_files_are_kept_and_the_load_keeps_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let (store, folder) = install_synthetic(dir.path(), b"embedding");
        let error = fail_load(&store);
        assert!(
            matches!(&error, DiarizeError::Metadata(detail) if detail == "the load failed"),
            "{error:?}"
        );
        assert_eq!(
            std::fs::read(folder.join("a.onnx")).unwrap(),
            b"segmentation"
        );
        assert_eq!(std::fs::read(folder.join("b.onnx")).unwrap(), b"embedding");
    }

    /// A file of the right size that fails its checksum is deleted and is
    /// the one file the result names; the intact one is kept.
    #[test]
    fn only_the_file_that_fails_its_checksum_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (store, folder) = install_synthetic(dir.path(), b"embeddinG");
        let error = fail_load(&store);
        let DiarizeError::NotInstalled { asset, missing, .. } = &error else {
            panic!("not installed: {error:?}");
        };
        assert_eq!(asset, "synthetic");
        assert_eq!(missing, &["b.onnx".to_owned()]);
        assert!(!folder.join("b.onnx").exists());
        assert_eq!(
            std::fs::read(folder.join("a.onnx")).unwrap(),
            b"segmentation"
        );
    }

    /// The checksum warning names the file and its digests, never the
    /// load's error, whose crash report can hold the child's stderr: that
    /// is logged once, at debug.
    #[test]
    fn the_load_error_is_logged_at_debug_only() {
        #[derive(Clone, Default)]
        struct Log(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Log {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let (store, _folder) = install_synthetic(dir.path(), b"embeddinG");
        let log = Log::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer({
                let log = log.clone();
                move || log.clone()
            })
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let error = tracing::subscriber::with_default(subscriber, || {
            after_failed_load_of(
                &store,
                &synthetic_asset(),
                DiarizeError::metadata("the child died: /home/someone/stderr-line"),
            )
        });
        assert!(
            matches!(error, DiarizeError::NotInstalled { .. }),
            "{error:?}"
        );
        let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let warning: Vec<_> = lines.iter().filter(|l| l.contains("WARN")).collect();
        assert_eq!(warning.len(), 1, "{text}");
        assert!(warning[0].contains("failed its checksum"), "{text}");
        assert!(warning[0].contains("b.onnx"), "{text}");
        let leaks: Vec<_> = lines.iter().filter(|l| l.contains("stderr-line")).collect();
        assert_eq!(leaks.len(), 1, "{text}");
        assert!(leaks[0].contains("DEBUG"), "{text}");
    }

    /// A file deleted since the folder was found, by a concurrent check or
    /// a Settings Remove, is missing whether it is gone by its hash or by
    /// its removal; any other error when hashing keeps it and does not
    /// name it.
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
        // Intact.
        std::fs::write(&kept, b"embedding").unwrap();
        assert!(!is_gone_after_check(file, &kept, sha256_of(&kept)));
        assert!(kept.exists());
    }

    /// A file that cannot be read is not hashed, so it is neither deleted
    /// nor named, and the load keeps its error. Unix only, where a file
    /// can be made unreadable; skipped where the test runs as root.
    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_read_is_skipped() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let (store, folder) = install_synthetic(dir.path(), b"embeddinG");
        let unreadable = folder.join("b.onnx");
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&unreadable).is_ok() {
            eprintln!("skipped: the file is still readable (root?)");
            return;
        }
        let error = fail_load(&store);
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(&error, DiarizeError::Metadata(_)), "{error:?}");
        assert_eq!(std::fs::read(&unreadable).unwrap(), b"embeddinG");
    }
}
