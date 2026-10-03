//! Where the models live and how they get there. The manifest names every
//! file of an asset with its size and SHA-256; the store checks a directory
//! against it, downloads what is missing into `<file>.partial` while
//! hashing, and renames only a verified file into place. Models are never
//! committed (`.gitignore` covers `*.onnx`).
//!
//! The root is `<support directory>/Models` ([`steno_core::StenoPaths`]),
//! or the directory `STENO_MODELS_DIR` names, with one sub-directory per
//! asset id. Silero VAD downloads from the sherpa-onnx `asr-models`
//! release. The fp32 Parakeet export (2.6 GB) is not hosted yet: until the
//! release plan names a host, its files are produced by
//! `spikes/onnx-speech/export/` and copied into
//! `<root>/parakeet-tdt-0.6b-v3-fp32/` by hand; [`ModelStore::ensure`]
//! reports [`SpeechError::NotHosted`] when they are missing. The checksums
//! are those of the 2026-10-02 export on atlas (torch 2.14.1, `NeMo` 3.0.0);
//! a hosted copy must match them or the manifest changes with it.
//! Swift: `Sources/StenoSpeech/Models/ModelAsset.swift`,
//! `ModelStore.swift` and `ModelDownloading.swift`, whose downloads go
//! through `FluidAudio` and `WhisperKit` instead.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use steno_core::StenoPaths;

use crate::error::SpeechError;

/// One file of an asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub name: String,
    /// `None` until the file is hosted.
    pub url: Option<String>,
    /// Lower-case hex.
    pub sha256: String,
    pub size: u64,
}

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
                url: Some("https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx".to_owned()),
                sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6".to_owned(),
                size: 643_854,
            }],
        }
    }

    /// Our fp32 ONNX export of Parakeet TDT 0.6B v3 with the 10000-frame
    /// position table (decision 3 of the speech-stack plan).
    #[must_use]
    pub fn parakeet_v3_fp32() -> Self {
        let file = |name: &str, sha256: &str, size: u64| ModelFile {
            name: name.to_owned(),
            url: None,
            sha256: sha256.to_owned(),
            size,
        };
        ModelAsset {
            id: "parakeet-tdt-0.6b-v3-fp32".to_owned(),
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

/// The models root and the HTTP client.
#[derive(Debug, Clone)]
pub struct ModelStore {
    root: PathBuf,
    agent: ureq::Agent,
}

impl ModelStore {
    /// Names a directory that replaces the default root.
    pub const ENVIRONMENT_VARIABLE: &'static str = "STENO_MODELS_DIR";

    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ModelStore {
            root: root.into(),
            agent: ureq::agent(),
        }
    }

    /// The root `STENO_MODELS_DIR` names, else the default root.
    #[must_use]
    pub fn from_environment() -> Self {
        Self::new(Self::environment_root().unwrap_or_else(Self::default_root))
    }

    /// The directory `STENO_MODELS_DIR` names, made absolute against the
    /// current directory when it is relative; `None` when unset or empty.
    /// The model-gated tests use the same reading.
    #[must_use]
    pub fn environment_root() -> Option<PathBuf> {
        let value = std::env::var_os(Self::ENVIRONMENT_VARIABLE).filter(|v| !v.is_empty())?;
        let path = PathBuf::from(value);
        if path.is_absolute() {
            Some(path)
        } else {
            Some(std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path)))
        }
    }

    /// `<support directory>/Models`, the Swift app's models root.
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
        asset
            .files
            .iter()
            .filter(|file| {
                asset.validate().is_err()
                    || !fs::metadata(directory.join(&file.name))
                        .is_ok_and(|m| m.is_file() && m.len() == file.size)
            })
            .map(|file| file.name.clone())
            .collect()
    }

    /// True when every file is present with its manifest size. Content is
    /// checked at download time and by [`ModelStore::verify`].
    #[must_use]
    pub fn is_installed(&self, asset: &ModelAsset) -> bool {
        self.missing_files(asset).is_empty()
    }

    /// Hashes every installed file against the manifest.
    pub fn verify(&self, asset: &ModelAsset) -> Result<(), SpeechError> {
        asset.validate()?;
        let missing = self.missing_files(asset);
        if !missing.is_empty() {
            return Err(SpeechError::NotInstalled {
                asset: asset.id.clone(),
                root: self.root.clone(),
                missing,
            });
        }
        let directory = self.directory(asset);
        for file in &asset.files {
            let path = directory.join(&file.name);
            let actual = sha256_of(&path)?;
            if !actual.eq_ignore_ascii_case(&file.sha256) {
                return Err(SpeechError::Checksum {
                    path,
                    expected: file.sha256.clone(),
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Installs the asset if needed and returns its directory. Missing
    /// files with a URL are downloaded and verified; a missing file
    /// without one is [`SpeechError::NotHosted`].
    pub fn ensure(
        &self,
        asset: &ModelAsset,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<PathBuf, SpeechError> {
        asset.validate()?;
        let directory = self.directory(asset);
        let missing = self.missing_files(asset);
        if missing.is_empty() {
            return Ok(directory);
        }
        fs::create_dir_all(&directory).map_err(|e| SpeechError::io(&directory, e))?;
        for file in asset.files.iter().filter(|f| missing.contains(&f.name)) {
            let Some(url) = &file.url else {
                return Err(SpeechError::NotHosted {
                    asset: asset.id.clone(),
                    root: self.root.clone(),
                });
            };
            self.download(url, file, &directory.join(&file.name), progress)?;
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

    /// Downloads into a sibling `.partial.<pid>` file (two processes, the
    /// app and a sidecar, can then install the same asset without writing
    /// one inode), verifies it and renames it into place; nothing is left
    /// behind on failure.
    fn download(
        &self,
        url: &str,
        file: &ModelFile,
        destination: &Path,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<(), SpeechError> {
        let suffix = format!("partial.{}", std::process::id());
        let partial = destination.with_extension(match destination.extension() {
            Some(extension) => format!("{}.{suffix}", extension.to_string_lossy()),
            None => suffix,
        });
        let result = self
            .stream_to(url, file, &partial, progress)
            .and_then(|()| {
                fs::rename(&partial, destination).map_err(|e| SpeechError::io(destination, e))
            });
        if result.is_err() {
            let _ = fs::remove_file(&partial);
        }
        result
    }

    /// Streams `url` into `partial` while hashing; refuses the body as soon
    /// as it exceeds the manifest size, so a misbehaving host cannot fill
    /// the disk.
    fn stream_to(
        &self,
        url: &str,
        file: &ModelFile,
        partial: &Path,
        progress: &mut dyn FnMut(DownloadProgress<'_>),
    ) -> Result<(), SpeechError> {
        let response = self
            .agent
            .get(url)
            .call()
            .map_err(|source| SpeechError::Download {
                url: url.to_owned(),
                source: Box::new(source),
            })?;
        let total = response.body().content_length().unwrap_or(file.size);
        let mut body = response.into_body();
        // One byte over the manifest size is enough to tell a long body.
        let mut reader = body.with_config().limit(file.size + 1).reader();
        let mut out = File::create(partial).map_err(|e| SpeechError::io(partial, e))?;
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        let mut buffer = vec![0u8; 1 << 16];
        let size_error = |received| SpeechError::Size {
            path: partial.to_path_buf(),
            expected: file.size,
            actual: received,
        };
        progress(DownloadProgress {
            file: &file.name,
            received,
            total,
        });
        loop {
            let n = reader
                .read(&mut buffer)
                .map_err(|e| SpeechError::io(partial, e))?;
            if n == 0 {
                break;
            }
            received += n as u64;
            if received > file.size {
                return Err(size_error(received));
            }
            out.write_all(&buffer[..n])
                .map_err(|e| SpeechError::io(partial, e))?;
            hasher.update(&buffer[..n]);
            progress(DownloadProgress {
                file: &file.name,
                received,
                total,
            });
        }
        out.flush().map_err(|e| SpeechError::io(partial, e))?;
        drop(out);
        if received != file.size {
            return Err(size_error(received));
        }
        let actual = hex(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(&file.sha256) {
            return Err(SpeechError::Checksum {
                path: partial.to_path_buf(),
                expected: file.sha256.clone(),
                actual,
            });
        }
        Ok(())
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
    use std::net::TcpListener;

    use super::*;

    /// Serves `body` once over HTTP on a local port and returns the URL.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                line.clear();
            }
            let mut stream = &stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        format!("http://{address}/model.onnx")
    }

    fn asset(url: Option<String>, body: &[u8], sha256: &str) -> ModelAsset {
        ModelAsset {
            id: "test-asset".to_owned(),
            display_name: "Test".to_owned(),
            licence: "MIT".to_owned(),
            attribution: String::new(),
            files: vec![ModelFile {
                name: "model.onnx".to_owned(),
                url,
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
        let asset = asset(Some(serve_once(body.clone())), &body, &digest(&body));
        assert!(!store.is_installed(&asset));
        assert_eq!(store.missing_files(&asset), ["model.onnx"]);
        let mut reports = Vec::new();
        let directory = store
            .ensure(&asset, &mut |p| reports.push((p.received, p.total)))
            .unwrap();
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
    fn a_wrong_checksum_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path());
        let body = b"tampered".to_vec();
        let asset = asset(Some(serve_once(body.clone())), &body, &digest(b"original"));
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
        let mut short = asset(Some(serve_once(body.clone())), &body, &digest(&body));
        short.files[0].size += 1;
        assert!(matches!(
            store.ensure(&short, &mut |_| {}),
            Err(SpeechError::Size { .. })
        ));
        // A body longer than the manifest says is cut off at the first
        // byte over, before the checksum is even looked at.
        let long = vec![0u8; 1 << 20];
        let mut long_asset = asset(Some(serve_once(long.clone())), &long, &digest(&long));
        long_asset.files[0].size = 1000;
        let mut last = 0;
        let error = store
            .ensure(&long_asset, &mut |p| last = p.received)
            .unwrap_err();
        assert!(
            matches!(error, SpeechError::Size { actual, .. } if actual > 1000),
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
        assert!(ModelAsset::silero_vad().files[0].url.is_some());
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
