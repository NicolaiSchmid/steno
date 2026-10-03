//! The model files the ONNX backend needs, fetched once into the support
//! directory and verified by checksum. Nothing is committed; a build
//! without the files downloads them on first use, a build without network
//! fails with the URL in the error. This is the only network access in
//! the crate, and it only receives.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use steno_core::StenoPaths;

/// One published model file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAsset {
    /// The file name under the store's directory.
    pub file_name: &'static str,
    pub url: &'static str,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: &'static str,
    /// The licence the model ships under, shown in the app's notices.
    pub licence: &'static str,
}

/// pyannote segmentation 3.0 as exported by sherpa-onnx (MIT; the
/// Hugging Face original is gated, the export is public).
pub const PYANNOTE_SEGMENTATION_3_0: ModelAsset = ModelAsset {
    file_name: "pyannote-segmentation-3.0.onnx",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx",
    sha256: "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079",
    licence: "MIT (pyannote.audio, CNRS)",
};

/// `WeSpeaker` ResNet34-LM trained on `VoxCeleb`, from the sherpa-onnx
/// speaker recognition assets (Apache-2.0).
pub const WESPEAKER_RESNET34_LM: ModelAsset = ModelAsset {
    file_name: "wespeaker-en-voxceleb-resnet34-lm.onnx",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx",
    sha256: "e9848563da86f263117134dfd7ad63c92355b37de492b55e325400c9d9c39012",
    licence: "Apache-2.0 (WeSpeaker)",
};

/// What fetching or verifying a model can fail with.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("downloading {url}: {source}")]
    Download {
        url: &'static str,
        #[source]
        source: Box<ureq::Error>,
    },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} does not match its checksum: expected {expected}, got {got}")]
    Checksum {
        path: PathBuf,
        expected: String,
        got: String,
    },
}

/// A directory of model files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelStore {
    root: PathBuf,
}

impl ModelStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ModelStore { root: root.into() }
    }

    /// `<support>/Models/onnx/diarization`, beside the Swift app's
    /// `Models/fluidaudio`.
    #[must_use]
    pub fn for_paths(paths: &StenoPaths) -> Self {
        ModelStore::new(
            paths
                .support_directory
                .join("Models")
                .join("onnx")
                .join("diarization"),
        )
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where `asset` lives once fetched.
    #[must_use]
    pub fn path(&self, asset: &ModelAsset) -> PathBuf {
        self.root.join(asset.file_name)
    }

    /// The verified path of `asset`, downloading it when it is missing
    /// or fails its checksum. The download lands in a `.part` file and is
    /// renamed only after it verifies.
    pub fn ensure(&self, asset: &ModelAsset) -> Result<PathBuf, ModelError> {
        let path = self.path(asset);
        if path.is_file() && sha256_of(&path)? == asset.sha256 {
            return Ok(path);
        }
        fs::create_dir_all(&self.root).map_err(|source| ModelError::Io {
            path: self.root.clone(),
            source,
        })?;
        let partial = path.with_extension("onnx.part");
        tracing::info!(url = asset.url, "downloading model");
        let verified = download(asset.url, &partial).and_then(|()| sha256_of(&partial));
        let got = match verified {
            Ok(got) => got,
            Err(error) => {
                // A failed or interrupted download leaves nothing behind.
                let _ = fs::remove_file(&partial);
                return Err(error);
            }
        };
        if got != asset.sha256 {
            let _ = fs::remove_file(&partial);
            return Err(ModelError::Checksum {
                path,
                expected: asset.sha256.to_owned(),
                got,
            });
        }
        fs::rename(&partial, &path).map_err(|source| ModelError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }
}

/// Time to reach the host, and for the whole transfer: the larger file is
/// 26 MB, so ten minutes covers a slow connection without letting a
/// stalled one hang the first run forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(600);

fn download(url: &'static str, target: &Path) -> Result<(), ModelError> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(TRANSFER_TIMEOUT))
        .build()
        .new_agent();
    let response = agent
        .get(url)
        .call()
        .map_err(|source| ModelError::Download {
            url,
            source: Box::new(source),
        })?;
    let mut reader = response.into_body().into_reader();
    let mut file = fs::File::create(target).map_err(|source| ModelError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    io::copy(&mut reader, &mut file).map_err(|source| ModelError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    file.flush().map_err(|source| ModelError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Lowercase hex SHA-256 of a file.
pub fn sha256_of(path: &Path) -> Result<String, ModelError> {
    let io_error = |source| ModelError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = fs::File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_sits_beside_the_swift_models() {
        let store = ModelStore::for_paths(&StenoPaths::new("/tmp/support"));
        assert_eq!(
            store.path(&WESPEAKER_RESNET34_LM),
            PathBuf::from(
                "/tmp/support/Models/onnx/diarization/wespeaker-en-voxceleb-resnet34-lm.onnx"
            )
        );
    }

    #[test]
    fn checksums_are_lowercase_hex_and_a_bad_file_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.bin");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_of(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let store = ModelStore::new(dir.path());
        let asset = ModelAsset {
            file_name: "x.bin",
            url: "http://127.0.0.1:9/never",
            sha256: "00",
            licence: "",
        };
        // The stale file fails its checksum, so the store tries the URL,
        // which is unreachable; no partial file stays behind.
        assert!(matches!(
            store.ensure(&asset),
            Err(ModelError::Download { .. })
        ));
        assert!(!dir.path().join("x.onnx.part").exists());
        assert!(!dir.path().join("x.bin.part").exists());
    }
}
