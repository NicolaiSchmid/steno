//! Where the models live and how they get there. The manifest names every
//! file of an asset with its size and SHA-256; the store checks a directory
//! against it, downloads what is missing into a partial file of its own
//! while hashing, and renames only a verified, synced file into place. Models are
//! never committed (`.gitignore` covers `*.onnx`).
//!
//! A store's root holds one folder per asset id, `<root>/<asset id>/`. The
//! app's root comes from `steno-services`; [`ModelStore::from_environment`]
//! and [`ModelStore::default_root`] are conveniences for the `transcribe`
//! example and the FLEURS test (the crate docs say more).
//!
//! # Hosts
//!
//! A file's [`ModelSource`] is a plain URL or a file in a Hugging Face
//! model repository at a pinned commit,
//! `https://huggingface.co/<repo>/resolve/<revision>/<path>`. GitHub
//! release assets cap at 2 GB per file, so the small files stay there
//! (Silero VAD from the sherpa-onnx `asr-models` release; the diarization
//! models of `steno-diarize` likewise) and the fp32 Parakeet export (2.6
//! GB, of which `encoder.weights` is 2.4 GB) goes to Hugging Face, uploaded
//! by `tools/upload-models.sh` into `NicolaiSchmid/steno-models` (a
//! placeholder until the plan's parity list settles the account). Until
//! [`PARAKEET_V3_FP32_REVISION`] names a commit, the export has no source:
//! its files are produced by `spikes/onnx-speech/export/` and copied into
//! `<root>/parakeet-tdt-0.6b-v3-fp32/` by hand, or fetched from a mirror,
//! and [`ModelStore::ensure`] reports [`SpeechError::NotHosted`] when they
//! are missing. The checksums are those of the export
//! `spikes/onnx-speech/export/` produces with torch 2.14.1 and `NeMo`
//! 3.0.0; a hosted copy must match them or the manifest changes with it.
//!
//! A mirror ([`ModelStore::with_mirror`], the speech setting
//! `modelsMirror`) replaces every host: the file is fetched from
//! `<mirror>/<asset id>/<file name>`, the layout of a store root and of the
//! Hugging Face repository, so a copy of either served over HTTP is a
//! mirror.
//!
//! # Downloads
//!
//! A download writes `<name>.partial` beside the file while holding an
//! exclusive lock on it, hashes as it goes and renames only a verified,
//! synced file into place. A dropped connection resumes with a `Range`
//! request on the next attempt, and a partial left by a killed process is
//! picked up by the next run after its prefix is hashed again. A host
//! that ignores the range sends the whole file, which is then written from
//! the start; a resumed file whose checksum fails is downloaded once more
//! from zero. A second download of the same file while the first holds the
//! lock, or one on a file system without locks, writes a partial of its
//! own, `<name>.partial.<pid>.<call>`, which nothing resumes and which is
//! deleted when the call ends.
//! Swift: `Sources/StenoSpeech/Models/ModelAsset.swift`,
//! `ModelStore.swift` and `ModelDownloading.swift`, whose downloads go
//! through `FluidAudio` and `WhisperKit` instead.

use std::fs::{self, File, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use steno_core::StenoPaths;

use crate::error::SpeechError;

/// Where a file is downloaded from when no mirror is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// A plain HTTPS URL, such as a GitHub release asset (2 GB at most).
    Url(String),
    /// A file in a Hugging Face model repository at one commit, so the
    /// bytes behind the URL never change under the manifest's checksum.
    HuggingFace {
        /// `<owner>/<name>`.
        repo: String,
        /// A full commit hash, not a branch.
        revision: String,
        /// The file's path inside the repository.
        path: String,
    },
}

impl ModelSource {
    /// Hugging Face's download host.
    pub const HUGGING_FACE: &'static str = "https://huggingface.co";

    /// The URL a download fetches.
    #[must_use]
    pub fn url(&self) -> String {
        match self {
            ModelSource::Url(url) => url.clone(),
            ModelSource::HuggingFace {
                repo,
                revision,
                path,
            } => format!("{}/{repo}/resolve/{revision}/{path}", Self::HUGGING_FACE),
        }
    }
}

/// One file of an asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub name: String,
    /// `None` until the file is hosted; a mirror serves it regardless.
    pub source: Option<ModelSource>,
    /// Lower-case hex.
    pub sha256: String,
    pub size: u64,
}

/// The Hugging Face repository the fp32 export is uploaded to by
/// `tools/upload-models.sh`. A placeholder: which account hosts the models
/// is open in the plan's parity list.
pub const STENO_MODELS_REPO: &str = "NicolaiSchmid/steno-models";

/// The commit of [`STENO_MODELS_REPO`] that holds the export, printed by
/// `tools/upload-models.sh`; `None` until it is uploaded, which leaves the
/// export without a source.
pub const PARAKEET_V3_FP32_REVISION: Option<&str> = None;

/// The largest file a GitHub release takes.
pub const GITHUB_RELEASE_ASSET_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// One downloadable model bundle; the settings pane shows the display
/// name, the total size and the licence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAsset {
    /// The directory name under the root.
    pub id: String,
    pub display_name: String,
    pub licence: String,
    /// What the app and the download show next to the model.
    pub attribution: String,
    pub files: Vec<ModelFile>,
}

impl ModelAsset {
    /// Silero VAD v4 from the sherpa-onnx `asr-models` release.
    #[must_use]
    pub fn silero_vad() -> Self {
        ModelAsset {
            id: "silero-vad".to_owned(),
            display_name: "Silero VAD".to_owned(),
            licence: "MIT".to_owned(),
            attribution: "Silero VAD (Silero Team), MIT, as packaged by sherpa-onnx".to_owned(),
            files: vec![ModelFile {
                name: "silero_vad.onnx".to_owned(),
                source: Some(ModelSource::Url("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx".to_owned())),
                sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6".to_owned(),
                size: 643_854,
            }],
        }
    }

    /// Our fp32 ONNX export of Parakeet TDT 0.6B v3 with the 10000-frame
    /// position table (decision 3 of the speech-stack plan), from
    /// [`STENO_MODELS_REPO`] at [`PARAKEET_V3_FP32_REVISION`] once that is
    /// set, without a source until then.
    #[must_use]
    pub fn parakeet_v3_fp32() -> Self {
        match PARAKEET_V3_FP32_REVISION {
            Some(revision) => Self::parakeet_v3_fp32_from(STENO_MODELS_REPO, revision),
            None => Self::parakeet_v3_fp32_manifest(|_, _| None),
        }
    }

    /// The export from a Hugging Face repository at `revision`, laid out
    /// `<asset id>/<file name>` the way `tools/upload-models.sh` uploads it.
    #[must_use]
    pub fn parakeet_v3_fp32_from(repo: &str, revision: &str) -> Self {
        Self::parakeet_v3_fp32_manifest(|id, name| {
            Some(ModelSource::HuggingFace {
                repo: repo.to_owned(),
                revision: revision.to_owned(),
                path: format!("{id}/{name}"),
            })
        })
    }

    fn parakeet_v3_fp32_manifest(source: impl Fn(&str, &str) -> Option<ModelSource>) -> Self {
        const ID: &str = "parakeet-tdt-0.6b-v3-fp32";
        let file = |name: &str, sha256: &str, size: u64| ModelFile {
            name: name.to_owned(),
            source: source(ID, name),
            sha256: sha256.to_owned(),
            size,
        };
        ModelAsset {
            id: ID.to_owned(),
            display_name: "Parakeet TDT 0.6B v3 (fp32, ONNX)".to_owned(),
            licence: "CC-BY-4.0".to_owned(),
            attribution: "Parakeet TDT 0.6B v3 by NVIDIA (https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3), CC-BY-4.0, converted to ONNX".to_owned(),
            files: vec![
                file("encoder.onnx", "9dc761845d0c7774d222203afca9ec3e3a6372da06999817d1eeb353efd59ca8", 82_798_411),
                file("encoder.weights", "9a22d372c51455c34f13405da2520baefb7125bd16981397561423ed32d24f36", 2_435_420_160),
                file("decoder.onnx", "4340de52b5fed87d7441650cedbb2648af93bcca4836e7b062b245590119fe79", 47_234_123),
                file("joiner.onnx", "468a21c3656cabbefff979a2c1f5cd7d2b1e353b13ba016cbcf2d8ff1a6fc3b1", 25_286_710),
                file("tokens.txt", "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d", 93_939),
            ],
        }
    }

    /// Every asset the crate knows.
    #[must_use]
    pub fn all() -> Vec<Self> {
        vec![Self::silero_vad(), Self::parakeet_v3_fp32()]
    }

    /// Bytes on disk once installed.
    #[must_use]
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Refuses an id or a file name that is not exactly one normal path
    /// component (`..`, an absolute path, an empty string, a slash), so
    /// the manifest can never name a path outside the root.
    pub fn validate(&self) -> Result<(), SpeechError> {
        for name in
            std::iter::once(self.id.as_str()).chain(self.files.iter().map(|f| f.name.as_str()))
        {
            if !is_plain_name(name) {
                return Err(SpeechError::InvalidName {
                    asset: self.id.clone(),
                    name: name.to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// True when `name` is one `Component::Normal` and nothing else.
fn is_plain_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// One progress report of a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress<'a> {
    pub file: &'a str,
    pub received: u64,
    pub total: u64,
}

/// How long a download waits for its connection, the TLS handshake
/// included; without a limit a stalled host would hang `prepare`, and every
/// `transcribe` waiting behind it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a download then waits for the response headers.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// The shortest body timeout, for a small file (200 ms under test).
const MIN_BODY_TIMEOUT: Duration = Duration::from_millis(if cfg!(test) { 200 } else { 60_000 });

/// The slowest average rate, in bytes per second, a body may arrive at.
const MIN_BODY_RATE: u64 = 64 * 1024;

/// How long a download may take to receive its body once the headers are
/// in: `max(MIN_BODY_TIMEOUT, size / MIN_BODY_RATE)`, about 10 h for 2.4 GB.
/// It bounds a stalled body without cutting a download that keeps moving at
/// 64 KiB/s or more.
fn body_timeout(size: u64) -> Duration {
    MIN_BODY_TIMEOUT.max(Duration::from_secs(size / MIN_BODY_RATE))
}

/// The models root, the mirror and the HTTP client.
#[derive(Debug, Clone)]
pub struct ModelStore {
    root: PathBuf,
    mirror: Option<String>,
    agent: ureq::Agent,
}

impl ModelStore {
    /// Names the store root [`ModelStore::from_environment`] uses in place
    /// of the default one.
    pub const ENVIRONMENT_VARIABLE: &'static str = "STENO_MODELS_DIR";

    /// Names the mirror [`ModelStore::from_environment`] sets.
    pub const MIRROR_ENVIRONMENT_VARIABLE: &'static str = "STENO_MODELS_MIRROR";

    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ModelStore {
            root: root.into(),
            mirror: None,
            agent: ureq::Agent::config_builder()
                .timeout_connect(Some(CONNECT_TIMEOUT))
                .timeout_recv_response(Some(RESPONSE_TIMEOUT))
                .build()
                .new_agent(),
        }
    }

    /// Fetches every file from `<mirror>/<asset id>/<file name>` instead of
    /// its source, unhosted files included; `None` or an empty string
    /// restores the sources. Checksums and sizes still come from the
    /// manifest, so a mirror cannot change what is installed.
    #[must_use]
    pub fn with_mirror(mut self, mirror: Option<String>) -> Self {
        self.mirror = mirror
            .map(|m| m.trim().trim_end_matches('/').to_owned())
            .filter(|m| !m.is_empty());
        self
    }

    #[must_use]
    pub fn mirror(&self) -> Option<&str> {
        self.mirror.as_deref()
    }

    /// Where `file` of `asset` is downloaded from: the mirror when one is
    /// set, else the file's source; `None` when neither exists.
    #[must_use]
    pub fn url_for(&self, asset: &ModelAsset, file: &ModelFile) -> Option<String> {
        match &self.mirror {
            Some(mirror) => Some(format!("{mirror}/{}/{}", asset.id, file.name)),
            None => file.source.as_ref().map(ModelSource::url),
        }
    }

    /// A convenience for the `transcribe` example and the FLEURS test; the
    /// app's root comes from `steno-services`. The root `STENO_MODELS_DIR`
    /// names, else the default root, with the mirror `STENO_MODELS_MIRROR`
    /// names.
    #[must_use]
    pub fn from_environment() -> Self {
        Self::new(Self::environment_root().unwrap_or_else(Self::default_root)).with_mirror(
            std::env::var(Self::MIRROR_ENVIRONMENT_VARIABLE)
                .ok()
                .filter(|v| !v.is_empty()),
        )
    }

    /// The directory `STENO_MODELS_DIR` names, made absolute against the
    /// current directory when it is relative; `None` when unset or empty.
    /// The model-gated tests use the same reading.
    #[must_use]
    pub fn environment_root() -> Option<PathBuf> {
        let value = std::env::var_os(Self::ENVIRONMENT_VARIABLE).filter(|v| !v.is_empty())?;
        let path = PathBuf::from(value);
        Some(match std::env::current_dir() {
            Ok(cwd) if path.is_relative() => cwd.join(path),
            _ => path,
        })
    }

    /// A convenience for the `transcribe` example and the FLEURS test; the
    /// app's root comes from `steno-services`. `<support directory>/Models`
    /// ([`steno_core::StenoPaths`]).
    #[must_use]
    pub fn default_root() -> PathBuf {
        StenoPaths::default_support_directory().join("Models")
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<asset id>`.
    #[must_use]
    pub fn directory(&self, asset: &ModelAsset) -> PathBuf {
        self.root.join(&asset.id)
    }

    /// Files absent or of the wrong size, by name; a file whose name
    /// fails [`ModelAsset::validate`] is never looked up and counts as
    /// missing.
    #[must_use]
    pub fn missing_files(&self, asset: &ModelAsset) -> Vec<String> {
        let directory = self.directory(asset);
        let invalid = asset.validate().is_err();
        asset
            .files
            .iter()
            .filter(|file| invalid || !is_complete(&directory.join(&file.name), file))
            .map(|file| file.name.clone())
            .collect()
    }

    /// True when every file is present with its manifest size. Content is
    /// checked at download time and by [`ModelStore::verify`].
    #[must_use]
    pub fn is_installed(&self, asset: &ModelAsset) -> bool {
        self.missing_files(asset).is_empty()
    }

    /// The asset's directory when [`ModelStore::is_installed`], else
    /// [`SpeechError::NotInstalled`] naming the missing files.
    pub(crate) fn installed_directory(&self, asset: &ModelAsset) -> Result<PathBuf, SpeechError> {
        asset.validate()?;
        let directory = self.directory(asset);
        let missing = self.missing_files(asset);
        if missing.is_empty() {
            Ok(directory)
        } else {
            Err(SpeechError::NotInstalled {
                asset: asset.id.clone(),
                directory,
                missing,
            })
        }
    }

    /// Hashes every installed file against the manifest.
    pub fn verify(&self, asset: &ModelAsset) -> Result<(), SpeechError> {
        let directory = self.installed_directory(asset)?;
        for file in &asset.files {
            let path = directory.join(&file.name);
            check_digest(file, &path, sha256_of(&path)?)?;
        }
        Ok(())
    }

    /// Installs the asset if needed and returns its directory. Missing
    /// files with a URL ([`ModelStore::url_for`]) are downloaded and
    /// verified, a file up to [`DOWNLOAD_ATTEMPTS`] times when the host
    /// answers 5xx, the connection drops or the body stalls, each attempt
    /// resuming where the last one stopped; a missing file without a URL is
    /// [`SpeechError::NotHosted`]. Another process's per-call partials that
    /// have gone stale are removed first; `<name>.partial` stays and is
    /// resumed.
    ///
    /// ```no_run
    /// use steno_speech::{ModelAsset, ModelStore};
    ///
    /// let store = ModelStore::from_environment();
    /// let directory = store.ensure(&ModelAsset::silero_vad(), &mut |p| {
    ///     eprintln!("{}: {} of {} bytes", p.file, p.received, p.total);
    /// })?;
    /// assert!(directory.join("silero_vad.onnx").is_file());
    /// # Ok::<(), steno_speech::SpeechError>(())
    /// ```
    pub fn ensure(
        &self,
        asset: &ModelAsset,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<PathBuf, SpeechError> {
        asset.validate()?;
        let directory = self.directory(asset);
        for file in &asset.files {
            remove_stale_partials(&directory, &file.name);
        }
        let missing = self.missing_files(asset);
        if missing.is_empty() {
            return Ok(directory);
        }
        fs::create_dir_all(&directory).map_err(|e| SpeechError::io(&directory, e))?;
        for file in asset.files.iter().filter(|f| missing.contains(&f.name)) {
            let Some(url) = self.url_for(asset, file) else {
                return Err(SpeechError::NotHosted {
                    asset: asset.id.clone(),
                    directory,
                });
            };
            self.download_with_retries(&url, file, &directory.join(&file.name), progress)?;
        }
        Ok(directory)
    }

    /// Deletes the asset's directory.
    pub fn remove(&self, asset: &ModelAsset) -> Result<(), SpeechError> {
        asset.validate()?;
        let directory = self.directory(asset);
        if directory.exists() {
            fs::remove_dir_all(&directory).map_err(|e| SpeechError::io(&directory, e))?;
        }
        Ok(())
    }

    /// [`ModelStore::download`], tried again after a transient failure.
    /// Every attempt continues the one partial file; a resumed file whose
    /// checksum fails is truncated and fetched once more from zero.
    fn download_with_retries(
        &self,
        url: &str,
        file: &ModelFile,
        destination: &Path,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<(), SpeechError> {
        let mut partial = Partial::open(destination, file)?;
        if partial.resumable && is_complete(destination, file) {
            // Installed by another download between `missing_files` and
            // the lock; the handle may even be that file, renamed.
            partial.done = true;
            return Ok(());
        }
        let mut attempt = 1;
        loop {
            match self.download(url, file, destination, &mut partial, progress) {
                Err(error) if attempt < DOWNLOAD_ATTEMPTS && is_transient(&error) => {
                    tracing::warn!(%error, attempt, received = partial.len, "model download failed, resuming");
                    std::thread::sleep(RETRY_DELAY * attempt);
                    attempt += 1;
                }
                Err(SpeechError::Checksum { .. })
                    if partial.resumed && attempt < DOWNLOAD_ATTEMPTS =>
                {
                    tracing::warn!(file = %file.name, "resumed model download failed its checksum, starting over");
                    partial.restart()?;
                    attempt += 1;
                }
                Err(error) => {
                    if matches!(
                        error,
                        SpeechError::Size { .. } | SpeechError::Checksum { .. }
                    ) {
                        partial.discard();
                    }
                    return Err(error);
                }
                Ok(()) => return Ok(()),
            }
        }
    }

    /// Streams the rest of `url` into `partial`, verifies and syncs it and
    /// renames it into place while its lock is still held, so no other
    /// download can pick the file up between the rename and the unlock.
    fn download(
        &self,
        url: &str,
        file: &ModelFile,
        destination: &Path,
        partial: &mut Partial,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<(), SpeechError> {
        self.stream_to(url, file, partial, progress)?;
        if partial.len != file.size {
            return Err(SpeechError::Size {
                path: destination.to_path_buf(),
                expected: file.size,
                actual: partial.len,
            });
        }
        check_digest(file, destination, hex(&partial.hasher.clone().finalize()))?;
        fs::rename(&partial.path, destination).map_err(|e| SpeechError::io(destination, e))?;
        partial.done = true;
        sync_parent(destination);
        Ok(())
    }

    /// Requests the bytes `partial` lacks: a `Range` request when it holds
    /// some, honoured only by a `206` that starts at its length; a `200`
    /// (the host ignored the range) restarts the file. A `206` elsewhere or
    /// a `416` restarts it and asks again for the whole body.
    fn request(
        &self,
        url: &str,
        file: &ModelFile,
        partial: &mut Partial,
    ) -> Result<ureq::http::Response<ureq::Body>, SpeechError> {
        let get = |range: Option<u64>| {
            let remaining = file.size.saturating_sub(range.unwrap_or(0));
            let request = self.agent.get(url);
            let request = match range {
                Some(offset) => request.header("Range", format!("bytes={offset}-")),
                None => request,
            };
            request
                .config()
                .timeout_recv_body(Some(body_timeout(remaining)))
                .build()
                .call()
                .map_err(|source| SpeechError::Download {
                    url: url.to_owned(),
                    source: Box::new(source),
                })
        };
        partial.resumed = false;
        if partial.len > 0 {
            match get(Some(partial.len)) {
                Ok(response) if response.status().as_u16() == 206 => {
                    if content_range_start(&response) == Some(partial.len) {
                        partial.resumed = true;
                        return Ok(response);
                    }
                }
                Ok(response) => {
                    partial.restart()?;
                    return Ok(response);
                }
                Err(SpeechError::Download { source, .. })
                    if matches!(*source, ureq::Error::StatusCode(416)) => {}
                Err(error) => return Err(error),
            }
            partial.restart()?;
        }
        get(None)
    }

    /// Streams the rest of the file into `partial` while hashing and syncs
    /// it. Stops at the first byte over the manifest size, so a misbehaving
    /// host cannot fill the disk.
    fn stream_to(
        &self,
        url: &str,
        file: &ModelFile,
        partial: &mut Partial,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<(), SpeechError> {
        let response = self.request(url, file, partial)?;
        let offset = partial.len;
        let total = response
            .body()
            .content_length()
            .map_or(file.size, |rest| offset + rest);
        let mut body = response.into_body();
        // One byte over the manifest size is enough to tell a long body.
        let mut reader = body
            .with_config()
            .limit(file.size.saturating_sub(offset) + 1)
            .reader();
        let mut buffer = vec![0u8; 1 << 16];
        progress(DownloadProgress {
            file: &file.name,
            received: partial.len,
            total,
        });
        loop {
            let n = reader
                .read(&mut buffer)
                .map_err(|e| SpeechError::io(&partial.path, e))?;
            if n == 0 {
                break;
            }
            if partial.len + n as u64 > file.size {
                // Counted, not written: the size check names the overrun.
                partial.len += n as u64;
                break;
            }
            partial.append(&buffer[..n])?;
            progress(DownloadProgress {
                file: &file.name,
                received: partial.len,
                total,
            });
        }
        // Without the sync a power loss could leave a file of the right
        // length but lost contents, which `is_installed` (sizes only) takes.
        partial
            .file
            .sync_all()
            .map_err(|e| SpeechError::io(&partial.path, e))
    }
}

/// True when `path` is a file of the manifest size.
fn is_complete(path: &Path, file: &ModelFile) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == file.size)
}

/// The first byte of a `Content-Range: bytes <first>-<last>/<total>`.
fn content_range_start(response: &ureq::http::Response<ureq::Body>) -> Option<u64> {
    let value = response.headers().get("content-range")?.to_str().ok()?;
    let range = value.trim().strip_prefix("bytes")?.trim_start();
    range.split_once('-')?.0.trim().parse().ok()
}

/// Tries per file before a download fails: a 5xx answer or a dropped
/// connection is often gone a few seconds later.
pub const DOWNLOAD_ATTEMPTS: u32 = 3;

/// The wait before the second attempt; each later one waits this much
/// longer (10 ms under test).
const RETRY_DELAY: Duration = Duration::from_millis(if cfg!(test) { 10 } else { 1000 });

/// Whether a failed download may succeed when tried again: a 5xx answer, a
/// connection that could not be made, dropped or timed out, or a body that
/// stalled past [`body_timeout`]. A wrong size or checksum, a 4xx answer and
/// a disk error fail at once.
fn is_transient(error: &SpeechError) -> bool {
    let dropped = |error: &std::io::Error| {
        use std::io::ErrorKind::{
            ConnectionAborted, ConnectionRefused, ConnectionReset, TimedOut, UnexpectedEof,
        };
        matches!(
            error.kind(),
            ConnectionAborted | ConnectionRefused | ConnectionReset | TimedOut | UnexpectedEof
        )
    };
    match error {
        SpeechError::Download { source, .. } => match source.as_ref() {
            ureq::Error::StatusCode(status) => (500..600).contains(status),
            ureq::Error::ConnectionFailed | ureq::Error::Timeout(_) => true,
            ureq::Error::Io(error) => dropped(error),
            _ => false,
        },
        // The body is read through `std::io`; its timeout arrives as
        // `io::Error::other(ureq::Error::Timeout(_))`, of kind `Other`.
        SpeechError::Io { source, .. } => {
            dropped(source)
                || source
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<ureq::Error>())
                    .is_some_and(|inner| matches!(inner, ureq::Error::Timeout(_)))
        }
        _ => false,
    }
}

/// Another process's partial download nothing has written to for this long
/// is stale; a live download writes it many times a second.
const STALE_PARTIAL: Duration = Duration::from_secs(10 * 60);

/// Numbers this process's downloads, so each has a partial file of its own.
static NEXT_CALL: AtomicU64 = AtomicU64::new(0);

/// `<name>.partial.`, followed by `<pid>.<call>`.
fn partial_prefix(name: &str) -> String {
    format!("{name}.partial.")
}

/// The file a download writes, with the bytes it holds and their hash.
/// `<name>.partial`, held under an exclusive lock, survives a failed call
/// for the next one to resume; a per-call partial is deleted with it. A
/// partial that ends up wrong is deleted either way.
struct Partial {
    path: PathBuf,
    file: File,
    /// Bytes in the file, all of them hashed into `hasher`.
    len: u64,
    hasher: Sha256,
    /// `<name>.partial` under this call's lock, rather than a per-call file.
    resumable: bool,
    /// The current stream continues bytes written before it.
    resumed: bool,
    /// Renamed into place, or left to the download that did.
    done: bool,
}

impl Partial {
    /// `<name>.partial` beside `destination` with its bytes hashed, when
    /// this call can take its lock; else a per-call partial of its own.
    fn open(destination: &Path, file: &ModelFile) -> Result<Self, SpeechError> {
        let path = destination.with_file_name(format!("{}.partial", file.name));
        let handle = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| SpeechError::io(&path, e))?;
        match handle.try_lock() {
            Ok(()) => {
                let mut partial = Partial::new(path, handle, true);
                partial.adopt(file.size)?;
                Ok(partial)
            }
            Err(TryLockError::WouldBlock) => Self::per_call(destination, &file.name),
            Err(TryLockError::Error(error)) => {
                tracing::debug!(%error, path = %path.display(), "no file locks here, the download will not resume");
                Self::per_call(destination, &file.name)
            }
        }
    }

    fn new(path: PathBuf, file: File, resumable: bool) -> Self {
        Partial {
            path,
            file,
            len: 0,
            hasher: Sha256::new(),
            resumable,
            resumed: false,
            done: false,
        }
    }

    /// Hashes what an earlier call left; a file longer than the manifest
    /// size cannot be a prefix and is emptied.
    fn adopt(&mut self, size: u64) -> Result<(), SpeechError> {
        let len = self
            .file
            .metadata()
            .map_err(|e| SpeechError::io(&self.path, e))?
            .len();
        if len > size {
            return self.restart();
        }
        if len > 0 {
            tracing::info!(path = %self.path.display(), len, "resuming a model download");
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| SpeechError::io(&self.path, e))?;
        let mut buffer = vec![0u8; 1 << 16];
        let mut remaining = len;
        while remaining > 0 {
            let want = buffer
                .len()
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let n = self
                .file
                .read(&mut buffer[..want])
                .map_err(|e| SpeechError::io(&self.path, e))?;
            if n == 0 {
                break;
            }
            self.hasher.update(&buffer[..n]);
            remaining -= n as u64;
        }
        self.len = len - remaining;
        self.file
            .seek(SeekFrom::Start(self.len))
            .map_err(|e| SpeechError::io(&self.path, e))?;
        Ok(())
    }

    /// Creates this call's partial download beside `destination`,
    /// `<name>.partial.<pid>.<call>`. A name that exists already, left by
    /// a dead process that had the same pid, is passed over for the next
    /// call number.
    fn per_call(destination: &Path, name: &str) -> Result<Self, SpeechError> {
        loop {
            let path = destination.with_file_name(format!(
                "{}{}.{}",
                partial_prefix(name),
                std::process::id(),
                NEXT_CALL.fetch_add(1, Ordering::Relaxed)
            ));
            match File::options().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok(Partial::new(path, file, false)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(SpeechError::io(&path, e)),
            }
        }
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), SpeechError> {
        self.file
            .write_all(bytes)
            .map_err(|e| SpeechError::io(&self.path, e))?;
        self.hasher.update(bytes);
        self.len += bytes.len() as u64;
        Ok(())
    }

    /// Empties the file for a download from zero.
    fn restart(&mut self) -> Result<(), SpeechError> {
        self.file
            .set_len(0)
            .and_then(|()| self.file.seek(SeekFrom::Start(0)).map(drop))
            .map_err(|e| SpeechError::io(&self.path, e))?;
        self.len = 0;
        self.hasher = Sha256::new();
        self.resumed = false;
        Ok(())
    }

    /// Marks the bytes as wrong, so the file is deleted even if resumable.
    fn discard(&mut self) {
        self.resumable = false;
    }
}

impl Drop for Partial {
    /// Deletes a per-call or discarded partial, and an empty one. Runs
    /// before the handle closes, so `<name>.partial` goes while still
    /// locked.
    fn drop(&mut self) {
        if !self.done && (!self.resumable || self.len == 0) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// The pid in `file_name` when it is a partial download of the file
/// `prefix` belongs to, `<prefix><pid>.<call>`; `None` for any other name.
fn partial_pid(file_name: &str, prefix: &str) -> Option<u32> {
    let (pid, call) = file_name.strip_prefix(prefix)?.split_once('.')?;
    call.parse::<u64>().ok()?;
    pid.parse().ok()
}

/// Removes the stale partial downloads of `name` in `directory`, which a
/// killed process leaves behind (2.4 GB for `encoder.weights`). This
/// process's own partials belong to downloads still running and are never
/// removed. Another process's is removed after [`STALE_PARTIAL`] without a
/// write; should that process still be alive, its rename then fails and
/// nothing is installed. Best effort: a file that cannot be read or
/// removed stays.
fn remove_stale_partials(directory: &Path, name: &str) {
    let prefix = partial_prefix(name);
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = partial_pid(&entry.file_name().to_string_lossy(), &prefix) else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        // Not `DirEntry::metadata`: on Windows that is the cached directory
        // entry, whose write time can lag for a file open for writing.
        let stale = fs::symlink_metadata(entry.path())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= STALE_PARTIAL);
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Syncs the directory holding `path`, so the rename survives a power loss.
/// Best effort: some FUSE and CIFS mounts refuse to sync a directory, and
/// the file itself is synced already, so a failure is logged and the
/// install stands. Not on Windows, which cannot open a directory as a file;
/// NTFS journals the rename.
fn sync_parent(path: &Path) {
    if cfg!(unix)
        && let Some(parent) = path.parent()
        && let Err(error) = File::open(parent).and_then(|directory| directory.sync_all())
    {
        tracing::warn!(
            directory = %parent.display(),
            %error,
            "could not sync the models directory after installing a file"
        );
    }
}

/// Compares `actual` with `file`'s manifest digest; `path` names the file
/// in the error.
fn check_digest(file: &ModelFile, path: &Path, actual: String) -> Result<(), SpeechError> {
    if actual.eq_ignore_ascii_case(&file.sha256) {
        Ok(())
    } else {
        Err(SpeechError::Checksum {
            path: path.to_path_buf(),
            expected: file.sha256.clone(),
            actual,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// The lower-case hex SHA-256 of a file, streamed.
pub fn sha256_of(path: &Path) -> Result<String, SpeechError> {
    let mut file = File::open(path).map_err(|e| SpeechError::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| SpeechError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;

    use super::*;

    /// A whole HTTP response carrying `body`.
    fn response(status: &str, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    /// Answers one connection per entry of `responses`, in order, with
    /// those bytes (none: the connection is closed unanswered), and returns
    /// the URL.
    fn serve(responses: Vec<Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for bytes in responses {
                let (stream, _) = listener.accept().unwrap();
                read_request_head(&stream);
                let mut stream = &stream;
                stream.write_all(&bytes).unwrap();
                stream.flush().unwrap();
            }
        });
        format!("http://{address}/model.onnx")
    }

    /// Serves `body` once and returns the URL.
    fn serve_once(body: &[u8]) -> String {
        serve(vec![response("200 OK", body)])
    }

    /// Serves `body` to one connection per entry of `heads`, in order of
    /// arrival: the first `heads[k]` bytes at once, the rest once the k-th
    /// sender returned fires.
    fn serve_held(body: &[u8], heads: Vec<usize>) -> (String, Vec<mpsc::Sender<()>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (senders, receivers): (Vec<_>, Vec<_>) = heads.iter().map(|_| mpsc::channel()).unzip();
        let bytes = response("200 OK", body);
        let header = bytes.len() - body.len();
        std::thread::spawn(move || {
            for (head, release) in heads.into_iter().zip(receivers) {
                let (stream, _) = listener.accept().unwrap();
                let bytes = bytes.clone();
                std::thread::spawn(move || {
                    read_request_head(&stream);
                    let (now, later) = bytes.split_at(header + head);
                    let mut stream = &stream;
                    stream.write_all(now).unwrap();
                    stream.flush().unwrap();
                    let _ = release.recv();
                    let _ = stream.write_all(later);
                });
            }
        });
        (format!("http://{address}/model.onnx"), senders)
    }

    /// Reads a request up to the blank line that ends its head.
    fn read_request_head(stream: &TcpStream) {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
            line.clear();
        }
    }

    fn asset(url: Option<String>, body: &[u8], sha256: &str) -> ModelAsset {
        ModelAsset {
            id: "test-asset".to_owned(),
            display_name: "Test".to_owned(),
            licence: "MIT".to_owned(),
            attribution: String::new(),
            files: vec![ModelFile {
                name: "model.onnx".to_owned(),
                source: url.map(ModelSource::Url),
                sha256: sha256.to_owned(),
                size: body.len() as u64,
            }],
        }
    }

    fn digest(body: &[u8]) -> String {
        hex(&Sha256::digest(body))
    }

    #[test]
    fn a_download_is_verified_and_moved_into_place() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"not really a model".to_vec();
        let asset = asset(Some(serve_once(&body)), &body, &digest(&body));
        assert!(!store.is_installed(&asset));
        assert_eq!(store.missing_files(&asset), ["model.onnx"]);
        let mut reports = Vec::new();
        let mut partials = Vec::new();
        let directory = store
            .ensure(&asset, &mut |p| {
                reports.push((p.received, p.total));
                partials.extend(
                    fs::read_dir(dir.path().join("test-asset"))
                        .unwrap()
                        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned()),
                );
            })
            .unwrap();
        // The download writes the resumable partial, not the target.
        assert!(
            !partials.is_empty() && partials.iter().all(|name| name == "model.onnx.partial"),
            "{partials:?}"
        );
        assert_eq!(directory, dir.path().join("test-asset"));
        assert_eq!(fs::read(directory.join("model.onnx")).unwrap(), body);
        assert!(store.is_installed(&asset));
        assert_eq!(
            reports.last(),
            Some(&(body.len() as u64, body.len() as u64))
        );
        store.verify(&asset).unwrap();
        // A second ensure is a no-op (the server is gone).
        store.ensure(&asset, &mut |_| {}).unwrap();
        store.remove(&asset).unwrap();
        assert!(!directory.exists());
    }

    #[test]
    fn two_downloads_of_one_file_in_one_process_write_their_own_partials() {
        // The second download starts while the first is half done; the
        // first then finishes and must install what it received.
        const WAIT: Duration = Duration::from_secs(30);
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        // 2 MB, so the body timeout (30 s) is far longer than the hold.
        let body: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8 + 1).collect();
        let (url, release) = serve_held(&body, vec![50_000, 10_000]);
        let asset = asset(Some(url), &body, &digest(&body));
        let (first_started, first_waits) = mpsc::channel();
        let (second_started, second_waits) = mpsc::channel();
        std::thread::scope(|scope| {
            // Owned here, so a failed assertion drops the senders and frees
            // the held downloads instead of leaving the scope waiting.
            let release = release;
            let first = scope.spawn(|| {
                store.ensure(&asset, &mut |p| {
                    if p.received >= 50_000 {
                        let _ = first_started.send(());
                    }
                })
            });
            first_waits.recv_timeout(WAIT).unwrap();
            let second = scope.spawn(|| {
                store.ensure(&asset, &mut |p| {
                    if p.received >= 10_000 {
                        let _ = second_started.send(());
                    }
                })
            });
            second_waits.recv_timeout(WAIT).unwrap();
            release[0].send(()).unwrap();
            let first = first.join().unwrap();
            let installed = store.verify(&asset);
            release[1].send(()).unwrap();
            let second = second.join().unwrap();
            first.unwrap();
            installed.unwrap();
            second.unwrap();
        });
        store.verify(&asset).unwrap();
        let names: Vec<_> = fs::read_dir(store.directory(&asset))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["model.onnx"]);
    }

    #[test]
    fn a_5xx_answer_or_a_dropped_connection_is_tried_again_a_few_times() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"not really a model".to_vec();
        let ok = response("200 OK", &body);
        let unavailable = response("503 Service Unavailable", b"");
        let url = serve(vec![unavailable.clone(), Vec::new(), ok.clone()]);
        let flaky = asset(Some(url), &body, &digest(&body));
        store.ensure(&flaky, &mut |_| {}).unwrap();
        store.verify(&flaky).unwrap();
        store.remove(&flaky).unwrap();
        // The attempts are bounded, and a 4xx answer is final.
        let attempts = DOWNLOAD_ATTEMPTS as usize;
        let mut responses = vec![unavailable; attempts];
        responses.push(ok.clone());
        let down = asset(Some(serve(responses)), &body, &digest(&body));
        let error = store.ensure(&down, &mut |_| {}).unwrap_err();
        assert!(matches!(&error, SpeechError::Download { .. }), "{error}");
        let url = serve(vec![response("404 Not Found", b""), ok]);
        let missing = asset(Some(url), &body, &digest(&body));
        let error = store.ensure(&missing, &mut |_| {}).unwrap_err();
        assert!(matches!(&error, SpeechError::Download { .. }), "{error}");
        assert!(!store.is_installed(&missing));
    }

    #[test]
    fn a_body_that_stalls_times_out_and_is_tried_again() {
        // The first answer sends half the body and then nothing; the second
        // sends it all. Without the body timeout the first read never ends.
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"not really a model".to_vec();
        let (url, release) = serve_held(&body, vec![body.len() / 2, body.len()]);
        let asset = asset(Some(url), &body, &digest(&body));
        let (done, finished) = mpsc::channel();
        let installer = store.clone();
        let wanted = asset.clone();
        std::thread::spawn(move || {
            let _ = done.send(installer.ensure(&wanted, &mut |_| {}));
        });
        let result = finished
            .recv_timeout(Duration::from_secs(10))
            .expect("the stalled body was never given up");
        drop(release);
        result.unwrap();
        store.verify(&asset).unwrap();
    }

    #[test]
    fn a_partial_name_a_dead_process_with_this_pid_left_is_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"not really a model".to_vec();
        let asset = asset(Some(serve_once(&body)), &body, &digest(&body));
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        // Another download holds the resumable partial, so this one takes
        // a per-call name.
        let held = File::create(directory.join("model.onnx.partial")).unwrap();
        held.lock().unwrap();
        // The next call numbers, with room for other tests drawing some.
        let next = NEXT_CALL.load(Ordering::Relaxed);
        let own = std::process::id();
        for call in next..next + 64 {
            fs::write(
                directory.join(format!("model.onnx.partial.{own}.{call}")),
                b"old",
            )
            .unwrap();
        }
        store.ensure(&asset, &mut |_| {}).unwrap();
        store.verify(&asset).unwrap();
        assert_eq!(
            fs::read(directory.join(format!("model.onnx.partial.{own}.{next}"))).unwrap(),
            b"old"
        );
        drop(held);
    }

    #[test]
    fn a_wrong_checksum_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"tampered".to_vec();
        let asset = asset(Some(serve_once(&body)), &body, &digest(b"original"));
        let error = store.ensure(&asset, &mut |_| {}).unwrap_err();
        assert!(matches!(error, SpeechError::Checksum { .. }), "{error}");
        let directory = dir.path().join("test-asset");
        assert!(fs::read_dir(&directory).unwrap().next().is_none());
        assert!(!store.is_installed(&asset));
    }

    #[test]
    fn a_wrong_size_on_the_wire_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"short".to_vec();
        let mut short = asset(Some(serve_once(&body)), &body, &digest(&body));
        short.files[0].size += 1;
        assert!(matches!(
            store.ensure(&short, &mut |_| {}),
            Err(SpeechError::Size { .. })
        ));
        // A body longer than the manifest says is cut off at the first
        // byte over, before the checksum is even looked at.
        let long = vec![0u8; 1 << 20];
        let mut long_asset = asset(Some(serve_once(&long)), &long, &digest(&long));
        long_asset.files[0].size = 1000;
        let mut last = 0;
        let error = store
            .ensure(&long_asset, &mut |p| last = p.received)
            .unwrap_err();
        let destination = dir.path().join("test-asset").join("model.onnx");
        assert!(
            matches!(&error, SpeechError::Size { actual, path, .. } if *actual > 1000 && *path == destination),
            "{error}"
        );
        assert!(last <= 1000);
        let directory = dir.path().join("test-asset");
        assert!(fs::read_dir(&directory).unwrap().next().is_none());
    }

    #[test]
    fn files_without_a_url_report_not_hosted_and_verify_catches_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"local only".to_vec();
        let asset = asset(None, &body, &digest(&body));
        assert!(matches!(
            store.ensure(&asset, &mut |_| {}),
            Err(SpeechError::NotHosted { .. })
        ));
        assert!(matches!(
            store.verify(&asset),
            Err(SpeechError::NotInstalled { .. })
        ));
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("model.onnx"), &body).unwrap();
        assert_eq!(store.ensure(&asset, &mut |_| {}).unwrap(), directory);
        store.verify(&asset).unwrap();
        fs::write(directory.join("model.onnx"), b"tampered!!").unwrap();
        assert!(matches!(
            store.verify(&asset),
            Err(SpeechError::Checksum { .. })
        ));
    }

    #[test]
    fn only_another_process_s_partial_untouched_for_the_stale_age_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"local only".to_vec();
        let mut asset = asset(None, &body, &digest(&body));
        asset.files.push(ModelFile {
            name: "notes.txt".to_owned(),
            source: None,
            sha256: digest(b"notes"),
            size: 5,
        });
        let directory = store.directory(&asset);
        fs::create_dir_all(&directory).unwrap();
        let minute = Duration::from_secs(60);
        let now = std::time::SystemTime::now();
        let own = std::process::id();
        let other = if own == 1 { 2 } else { 1 };
        // (name, contents, last write)
        let files = [
            ("model.onnx", &body[..], now - 2 * STALE_PARTIAL),
            ("notes.txt", b"notes", now - 2 * STALE_PARTIAL),
            (
                &*format!("model.onnx.partial.{other}.0"),
                b"half",
                now - 11 * minute,
            ),
            (
                &*format!("model.onnx.partial.{other}.1"),
                b"half",
                now - 9 * minute,
            ),
            (
                &*format!("model.onnx.partial.{other}.2"),
                b"half",
                now + 60 * minute,
            ),
            (
                &*format!("model.onnx.partial.{own}.0"),
                b"half",
                now - 2 * STALE_PARTIAL,
            ),
            (
                &*format!("model.onnx.partial.{other}"),
                b"half",
                now - 2 * STALE_PARTIAL,
            ),
            ("model.onnx.partial.x.0", b"half", now - 2 * STALE_PARTIAL),
            ("other.onnx.partial.1.0", b"half", now - 2 * STALE_PARTIAL),
        ];
        for (name, contents, modified) in files {
            let path = directory.join(name);
            fs::write(&path, contents).unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        store.ensure(&asset, &mut |_| {}).unwrap();
        let mut left: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected: Vec<_> = files[..]
            .iter()
            .map(|(name, _, _)| (*name).to_owned())
            .filter(|name| *name != format!("model.onnx.partial.{other}.0"))
            .collect();
        expected.sort();
        assert_eq!(left, expected);
    }

    #[test]
    fn hosts_fit_their_limits_and_hugging_face_urls_pin_a_commit() {
        // GitHub release assets cap at 2 GB a file, so a file that large
        // can only come from Hugging Face.
        let hosted = ModelAsset::parakeet_v3_fp32_from(STENO_MODELS_REPO, "0123abcd");
        for asset in ModelAsset::all().into_iter().chain([hosted.clone()]) {
            for file in &asset.files {
                if let Some(ModelSource::Url(url)) = &file.source {
                    assert!(url.starts_with("https://"), "{url}");
                    assert!(file.size < GITHUB_RELEASE_ASSET_LIMIT, "{}", file.name);
                }
            }
        }
        let weights = hosted
            .files
            .iter()
            .find(|f| f.name == "encoder.weights")
            .unwrap();
        assert!(weights.size > GITHUB_RELEASE_ASSET_LIMIT);
        assert_eq!(
            weights.source.as_ref().unwrap().url(),
            "https://huggingface.co/NicolaiSchmid/steno-models/resolve/0123abcd/parakeet-tdt-0.6b-v3-fp32/encoder.weights"
        );
        // Same files, sizes and checksums whether hosted or not.
        let unhosted = ModelAsset::parakeet_v3_fp32();
        assert_eq!(PARAKEET_V3_FP32_REVISION, None);
        assert!(unhosted.files.iter().all(|f| f.source.is_none()));
        let strip = |asset: &ModelAsset| {
            asset
                .files
                .iter()
                .map(|f| (f.name.clone(), f.sha256.clone(), f.size))
                .collect::<Vec<_>>()
        };
        assert_eq!(strip(&hosted), strip(&unhosted));
        // A mirror replaces every host, unhosted files included.
        let store = ModelStore::new("/models");
        let weights = &unhosted.files[1];
        assert_eq!(weights.name, "encoder.weights");
        assert_eq!(store.url_for(&unhosted, weights), None);
        let silero = ModelAsset::silero_vad();
        assert_eq!(
            store.url_for(&silero, &silero.files[0]).unwrap(),
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx"
        );
        let mirrored = store.with_mirror(Some(" http://mirror.local/models/ ".to_owned()));
        assert_eq!(mirrored.mirror(), Some("http://mirror.local/models"));
        assert_eq!(
            mirrored.url_for(&unhosted, weights).unwrap(),
            "http://mirror.local/models/parakeet-tdt-0.6b-v3-fp32/encoder.weights"
        );
        assert_eq!(mirrored.with_mirror(Some(String::new())).mirror(), None);
    }

    #[test]
    fn names_that_could_leave_the_root_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"x".to_vec();
        for (id, name) in [
            ("../escape", "model.onnx"),
            ("test-asset", "../model.onnx"),
            ("test-asset", "/abs/model.onnx"),
            ("test-asset", ""),
            ("", "model.onnx"),
            ("a/b", "model.onnx"),
            (".", "model.onnx"),
        ] {
            let mut bad = asset(None, &body, &digest(&body));
            bad.id = id.to_owned();
            bad.files[0].name = name.to_owned();
            let error = bad.validate().unwrap_err();
            assert!(
                matches!(error, SpeechError::InvalidName { .. }),
                "{id} {name}: {error}"
            );
            assert!(matches!(
                store.ensure(&bad, &mut |_| {}),
                Err(SpeechError::InvalidName { .. })
            ));
            assert!(matches!(
                store.verify(&bad),
                Err(SpeechError::InvalidName { .. })
            ));
            assert!(matches!(
                store.remove(&bad),
                Err(SpeechError::InvalidName { .. })
            ));
            assert!(!store.is_installed(&bad));
        }
        for asset in ModelAsset::all() {
            asset.validate().unwrap();
        }
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
        // An invalid asset lists every file as missing, even one a join of
        // its id would find: "." resolves to the root itself.
        let mut dot = asset(None, &body, &digest(&body));
        dot.id = ".".to_owned();
        fs::write(dir.path().join("model.onnx"), &body).unwrap();
        assert_eq!(store.missing_files(&dot), ["model.onnx"]);
    }

    #[test]
    fn the_manifest_is_consistent_and_the_root_follows_the_support_directory() {
        for asset in ModelAsset::all() {
            assert!(!asset.files.is_empty());
            for file in &asset.files {
                assert_eq!(file.sha256.len(), 64, "{}", file.name);
                assert!(file.sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(file.size > 0);
            }
        }
        assert!(ModelAsset::parakeet_v3_fp32().total_size() > 2_500_000_000);
        assert!(ModelAsset::silero_vad().files[0].source.is_some());
        assert_eq!(
            ModelStore::default_root(),
            StenoPaths::default_support_directory().join("Models")
        );
        let store = ModelStore::new("/models");
        assert_eq!(
            store.directory(&ModelAsset::silero_vad()),
            PathBuf::from("/models/silero-vad")
        );
    }
}
