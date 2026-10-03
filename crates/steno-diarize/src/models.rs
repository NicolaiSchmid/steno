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
    /// A store over `root`; the directory is created on the first fetch.
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

    /// The directory the model files live in.
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
    /// or fails its checksum. The download lands in a temporary file of
    /// its own in the store's directory, so two processes fetching the
    /// same model at once (the app and the `steno` command on first use)
    /// never share a partial file, and moves into place only after it
    /// verifies; whichever finishes last wins, both end with a verified
    /// file. A failed or interrupted download leaves nothing behind, and a
    /// partial file a killed process left is removed on a later call.
    pub fn ensure(&self, asset: &ModelAsset) -> Result<PathBuf, ModelError> {
        self.remove_stale_parts(asset);
        let path = self.path(asset);
        if verifies(&path, asset) {
            return Ok(path);
        }
        let io_error = |path: &Path| {
            let path = path.to_path_buf();
            move |source| ModelError::Io { path, source }
        };
        fs::create_dir_all(&self.root).map_err(io_error(&self.root))?;
        let mut partial = tempfile::Builder::new()
            .prefix(asset.file_name)
            .suffix(".part")
            .tempfile_in(&self.root)
            .map_err(io_error(&self.root))?;
        tracing::info!(url = asset.url, "downloading model");
        download(asset.url, partial.as_file_mut()).map_err(|error| match error {
            DownloadError::Transfer(source) => ModelError::Download {
                url: asset.url,
                source: Box::new(source),
            },
            DownloadError::Write(source) => io_error(partial.path())(source),
        })?;
        let got = sha256_of(partial.path())?;
        if got != asset.sha256 {
            return Err(ModelError::Checksum {
                path,
                expected: asset.sha256.to_owned(),
                got,
            });
        }
        if let Err(error) = partial.persist(&path) {
            // Another fetch may have put the file there first and still
            // hold it open, which a Windows rename refuses: a destination
            // that verifies is as good as our own.
            if verifies(&path, asset) {
                return Ok(path);
            }
            return Err(io_error(&path)(error.error));
        }
        Ok(path)
    }

    /// Removes the partial downloads of `asset` (`<file_name>*.part`) that
    /// were last written more than [`STALE_PART_AGE`] ago. A process
    /// killed mid-download, an app quit on first run, skips the temporary
    /// file's own cleanup; a download still under way is never that old.
    /// Best effort: a file that cannot be removed is logged and left.
    fn remove_stale_parts(&self, asset: &ModelAsset) {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_part = path
                .extension()
                .is_some_and(|extension| extension == "part")
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(asset.file_name));
            let is_stale = || {
                entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > STALE_PART_AGE)
            };
            if is_part
                && is_stale()
                && let Err(error) = fs::remove_file(&path)
            {
                tracing::warn!(path = %path.display(), %error, "stale partial download left in place");
            }
        }
    }
}

/// Whether the file at `path` exists and matches `asset`'s checksum.
fn verifies(path: &Path, asset: &ModelAsset) -> bool {
    path.is_file() && sha256_of(path).is_ok_and(|got| got == asset.sha256)
}

/// Time to reach the host.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Time for the whole transfer: the larger file is 26 MB, so ten minutes
/// covers a slow connection without letting a stalled one hang the first
/// run.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(600);
/// Age past which a partial download is taken as abandoned: a day, not
/// [`TRANSFER_TIMEOUT`], because the file's age is wall-clock time and
/// the timeout's clock stops while the machine sleeps, so a download
/// still under way after a long sleep can be older than the timeout.
const STALE_PART_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Why a download did not land in its file: the transfer or the file.
enum DownloadError {
    Transfer(ureq::Error),
    Write(io::Error),
}

/// Fetches `url` into `file`, which is positioned at its start.
fn download(url: &'static str, file: &mut fs::File) -> Result<(), DownloadError> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(TRANSFER_TIMEOUT))
        .build()
        .new_agent();
    let response = agent.get(url).call().map_err(DownloadError::Transfer)?;
    let mut reader = response.into_body().into_reader();
    // Copied by hand rather than with `io::copy` so that a short or broken
    // body, an error from the reader, is a transfer error with the URL and
    // only a failed write is the file's.
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(DownloadError::Transfer(ureq::Error::from(error))),
        };
        file.write_all(&buffer[..read])
            .map_err(DownloadError::Write)?;
    }
    file.flush().map_err(DownloadError::Write)?;
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
    use std::net::{TcpListener, TcpStream};

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
        fs::write(&path, ABC).unwrap();
        assert_eq!(sha256_of(&path).unwrap(), ABC_SHA256);
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
        assert_eq!(entries(dir.path()), vec!["x.bin".to_owned()]);
    }

    /// The file names in `dir`, sorted.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A loopback HTTP server that answers `connections` requests with
    /// `body`, declaring `declared_length` bytes; the URL it serves. It
    /// accepts every connection before it answers any, so concurrent
    /// fetches are all under way, each with its partial file, before the
    /// first one can finish.
    fn serve(body: &'static [u8], declared_length: usize, connections: usize) -> &'static str {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let streams: Vec<TcpStream> = listener
                .incoming()
                .take(connections)
                .map(Result::unwrap)
                .collect();
            for mut stream in streams {
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
                    request.push(byte[0]);
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {declared_length}\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(body).unwrap();
                stream.flush().unwrap();
            }
        });
        Box::leak(format!("http://{address}/model.onnx").into_boxed_str())
    }

    const ABC: &[u8] = b"abc";
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    /// Two fetches of one model into a cold store at once, as the app and
    /// the `steno` command on first use: the server holds both responses
    /// until both requests are in, so each fetch has its own temporary
    /// file open while the other downloads; both end with the verified
    /// file in place, and nothing else is left in the directory.
    #[test]
    fn concurrent_fetches_of_one_model_both_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let asset = ModelAsset {
            file_name: "model.onnx",
            url: serve(ABC, ABC.len(), 2),
            sha256: ABC_SHA256,
            licence: "",
        };
        let store = ModelStore::new(dir.path());
        let results: Vec<Result<PathBuf, ModelError>> = std::thread::scope(|scope| {
            let fetches: Vec<_> = (0..2)
                .map(|_| scope.spawn(|| store.ensure(&asset)))
                .collect();
            fetches
                .into_iter()
                .map(|fetch| fetch.join().unwrap())
                .collect()
        });
        for result in &results {
            assert_eq!(result.as_ref().unwrap(), &dir.path().join("model.onnx"));
        }
        assert_eq!(fs::read(dir.path().join("model.onnx")).unwrap(), ABC);
        assert_eq!(entries(dir.path()), vec!["model.onnx".to_owned()]);
    }

    /// A partial download a killed process left behind goes on the next
    /// call once no download could still be writing it; a recent one,
    /// which may belong to a fetch under way, and another model's stay.
    #[test]
    fn stale_partial_downloads_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("model.onnx"), ABC).unwrap();
        let old = std::time::SystemTime::now() - STALE_PART_AGE - Duration::from_secs(60);
        for name in ["model.onnxAbC123.part", "other.onnxAbC123.part"] {
            let file = fs::File::create(dir.path().join(name)).unwrap();
            file.set_modified(old).unwrap();
        }
        fs::write(dir.path().join("model.onnxXyZ789.part"), b"").unwrap();
        let asset = ModelAsset {
            file_name: "model.onnx",
            url: "http://127.0.0.1:9/never",
            sha256: ABC_SHA256,
            licence: "",
        };
        let path = ModelStore::new(dir.path()).ensure(&asset).unwrap();
        assert_eq!(path, dir.path().join("model.onnx"));
        assert_eq!(
            entries(dir.path()),
            vec![
                "model.onnx".to_owned(),
                "model.onnxXyZ789.part".to_owned(),
                "other.onnxAbC123.part".to_owned()
            ]
        );
    }

    /// A body that is not the published file fails the checksum and
    /// leaves no file behind, not even the one with the wrong content.
    #[test]
    fn a_body_with_the_wrong_checksum_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let asset = ModelAsset {
            file_name: "model.onnx",
            url: serve(b"not the model", 13, 1),
            sha256: ABC_SHA256,
            licence: "",
        };
        let error = ModelStore::new(dir.path()).ensure(&asset).unwrap_err();
        assert!(matches!(error, ModelError::Checksum { .. }), "{error}");
        assert_eq!(entries(dir.path()), Vec::<String>::new());
    }

    /// A connection that closes before the declared length arrives is a
    /// download error that names the URL, and the partial file goes with
    /// it.
    #[test]
    fn a_truncated_body_names_the_url_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let asset = ModelAsset {
            file_name: "model.onnx",
            url: serve(ABC, 1 << 20, 1),
            sha256: ABC_SHA256,
            licence: "",
        };
        let error = ModelStore::new(dir.path()).ensure(&asset).unwrap_err();
        assert!(matches!(error, ModelError::Download { .. }), "{error}");
        assert!(error.to_string().contains(asset.url), "{error}");
        assert_eq!(entries(dir.path()), Vec::<String>::new());
    }
}
