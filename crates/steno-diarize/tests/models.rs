//! The diarizer's two models as an asset of `steno-speech`'s model store:
//! the asset validates and keeps the folder and file names the diarizer's
//! own store used, so an installed copy stays where it is; a mirror serves
//! it from `<mirror>/diarization/<file name>`; an installed folder is used
//! without a request. The store's mechanics (lock, resume, ranges, the
//! checksum gate) are tested in `crates/steno-speech/tests/download.rs`.
//! No network beyond 127.0.0.1.

#![cfg(feature = "onnx")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use steno_diarize::models::{self, EMBEDDING_FILE, ModelPaths, SEGMENTATION_FILE};
use steno_speech::{ModelSource, ModelStore};

/// The asset keeps the folder (`<models directory>/onnx/diarization/`) and
/// the file names of the store it replaces, and every file has a host, a
/// lower-case SHA-256 and its size.
#[test]
fn the_asset_validates_and_keeps_the_diarizers_folder_and_names() {
    let asset = models::asset();
    asset.validate().unwrap();
    assert_eq!(asset.id, "diarization");
    let names: Vec<&str> = asset.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, [SEGMENTATION_FILE, EMBEDDING_FILE]);
    assert_eq!(
        names,
        [
            "pyannote-segmentation-3.0.onnx",
            "wespeaker-en-voxceleb-resnet34-lm.onnx"
        ]
    );
    for file in &asset.files {
        assert!(
            matches!(file.source, Some(ModelSource::Url(_))),
            "{}",
            file.name
        );
        assert_eq!(file.sha256.len(), 64, "{}", file.name);
        assert!(
            file.sha256
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{}",
            file.name
        );
    }
    assert_eq!(asset.total_size(), 5_992_913 + 26_530_550);
    assert!(asset.licence.contains("MIT") && asset.licence.contains("Apache-2.0"));

    let models_directory = Path::new("/tmp/steno-models");
    let store = ModelStore::in_models_directory(models_directory);
    let folder = models_directory.join("onnx").join("diarization");
    assert_eq!(store.directory(&asset), folder);
    assert_eq!(
        ModelPaths::in_directory(&folder),
        ModelPaths {
            segmentation: folder.join("pyannote-segmentation-3.0.onnx"),
            embedding: folder.join("wespeaker-en-voxceleb-resnet34-lm.onnx"),
        }
    );
}

/// With a mirror, both files come from `<mirror>/diarization/<file name>`;
/// without one, from the hosts the diarizer always used.
#[test]
fn a_mirror_serves_the_files_from_its_diarization_folder() {
    let asset = models::asset();
    let mirrored = ModelStore::new("/tmp/steno-models/onnx")
        .with_mirror(Some("http://mirror.example:8000/models/".to_owned()));
    let urls: Vec<String> = asset
        .files
        .iter()
        .map(|file| mirrored.url_for(&asset, file).unwrap())
        .collect();
    assert_eq!(
        urls,
        [
            "http://mirror.example:8000/models/diarization/pyannote-segmentation-3.0.onnx",
            "http://mirror.example:8000/models/diarization/wespeaker-en-voxceleb-resnet34-lm.onnx",
        ]
    );
    let hosts = ModelStore::new("/tmp/steno-models/onnx");
    let urls: Vec<String> = asset
        .files
        .iter()
        .map(|file| hosts.url_for(&asset, file).unwrap())
        .collect();
    assert_eq!(
        urls,
        [
            "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx",
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx",
        ]
    );
}

/// A loopback mirror that answers every request with 404 and counts them;
/// its URL and the count.
fn counting_mirror() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            counted.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    (url, requests)
}

/// A folder the diarizer's own store installed is used as it is: no
/// request, and the store reports it installed, which is what Settings and
/// a recording's warm-up ask. Once a file is missing, the next call goes
/// to the mirror for it and fails with the URL; the other file stays.
#[test]
fn an_installed_folder_is_used_without_a_request() {
    let dir = tempfile::tempdir().unwrap();
    let (mirror, requests) = counting_mirror();
    let store = ModelStore::in_models_directory(dir.path()).with_mirror(Some(mirror.clone()));
    let asset = models::asset();
    let folder = dir.path().join("onnx").join("diarization");
    std::fs::create_dir_all(&folder).unwrap();
    for file in &asset.files {
        std::fs::File::create(folder.join(&file.name))
            .unwrap()
            .set_len(file.size)
            .unwrap();
    }
    assert!(store.is_installed(&asset));
    assert_eq!(
        models::ensure(&store).unwrap(),
        ModelPaths::in_directory(&folder)
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    std::fs::remove_file(folder.join(EMBEDDING_FILE)).unwrap();
    assert!(!store.is_installed(&asset));
    let error = models::ensure(&store).unwrap_err().to_string();
    assert!(
        error.contains(&format!("{mirror}/diarization/{EMBEDDING_FILE}")),
        "{error}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(folder.join(SEGMENTATION_FILE).is_file());
}
