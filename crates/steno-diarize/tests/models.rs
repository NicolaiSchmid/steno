//! The diarizer's two models as an asset of `steno-speech`'s model store:
//! names, mirror URLs, installed folders used without a request, `NotInstalled`
//! under `Install::Never`, and corrupt files deleted after a failed load. No
//! network beyond 127.0.0.1.

#![cfg(feature = "onnx")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use steno_core::{BoxError, Diarizer};
use steno_diarize::models::{self, EMBEDDING_FILE, ModelPaths, SEGMENTATION_FILE};
use steno_diarize::onnx::OnnxBackend;
use steno_diarize::{DiarizeError, DiarizerConfig, Install, ModelDiarizer};
use steno_speech::{ModelStore, SpeechError};

/// The asset keeps its folder and file names, every file has a lower-case
/// SHA-256 and its size, and the licences are credited.
#[test]
fn the_asset_validates_and_keeps_the_diarizers_folder_and_names() {
    let asset = models::asset();
    asset.validate().unwrap();
    assert_eq!(asset.id, "diarization");
    let names: Vec<&str> = asset.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, [SEGMENTATION_FILE, EMBEDDING_FILE]);
    for file in &asset.files {
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
    assert_eq!(asset.licence, "MIT AND CC-BY-4.0");
    assert_eq!(asset.attribution, models::ATTRIBUTION);
    for credit in [
        "pyannote",
        "Copyright (c) 2020 CNRS",
        "MIT",
        "WeSpeaker",
        "VoxCeleb",
        "CC-BY-4.0",
        "https://creativecommons.org/licenses/by/4.0/",
        "converted to ONNX",
    ] {
        assert!(models::ATTRIBUTION.contains(credit), "{credit}");
    }

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

/// With a mirror, both files come from `<mirror>/diarization/<file name>`.
#[test]
fn a_mirror_serves_the_files_from_its_diarization_folder() {
    let asset = models::asset();
    let urls = |store: &ModelStore| -> Vec<String> {
        asset
            .files
            .iter()
            .map(|file| store.url_for(&asset, file).unwrap())
            .collect()
    };
    let hosts = ModelStore::new("/tmp/steno-models/onnx");
    let mirrored = hosts
        .clone()
        .with_mirror(Some("http://mirror.example:8000/models/".to_owned()));
    assert_eq!(
        urls(&mirrored),
        [
            "http://mirror.example:8000/models/diarization/pyannote-segmentation-3.0.onnx",
            "http://mirror.example:8000/models/diarization/wespeaker-en-voxceleb-resnet34-lm.onnx",
        ]
    );
    assert_eq!(
        urls(&hosts),
        [
            "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/9403a6902bb58e3d5ae8c7e77c3422de279db2e0/model.onnx",
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx",
        ]
    );
}

/// A loopback mirror that answers 404 and counts requests; its URL and count.
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

/// Both files as sparse files of their manifest size (not models); the
/// folder.
fn install_junk(store: &ModelStore) -> std::path::PathBuf {
    let asset = models::asset();
    let folder = store.directory(&asset);
    std::fs::create_dir_all(&folder).unwrap();
    for file in &asset.files {
        std::fs::File::create(folder.join(&file.name))
            .unwrap()
            .set_len(file.size)
            .unwrap();
    }
    folder
}

/// An installed folder is used without a request, under `Install::Never` too.
/// Once a file is missing, `Install::Never` names it as `NotInstalled`
/// without a request, and `ensure` asks the mirror for it and fails with the
/// URL.
#[test]
fn an_installed_folder_is_used_without_a_request() {
    let dir = tempfile::tempdir().unwrap();
    let (mirror, requests) = counting_mirror();
    let store = ModelStore::in_models_directory(dir.path()).with_mirror(Some(mirror.clone()));
    let asset = models::asset();
    let folder = install_junk(&store);
    assert_eq!(folder, dir.path().join("onnx").join("diarization"));
    assert!(store.is_installed(&asset));
    let paths = ModelPaths::in_directory(&folder);
    assert_eq!(models::ensure(&store).unwrap(), paths);
    assert_eq!(models::installed(&store).unwrap(), paths);
    assert_eq!(models::paths(&store, Install::Never).unwrap(), paths);
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    std::fs::remove_file(folder.join(EMBEDDING_FILE)).unwrap();
    assert!(!store.is_installed(&asset));
    let error = models::paths(&store, Install::Never).unwrap_err();
    let DiarizeError::NotInstalled {
        asset,
        directory,
        missing,
    } = &error
    else {
        panic!("not installed: {error:?}");
    };
    assert_eq!(asset, models::ASSET_ID);
    assert_eq!(directory, &folder);
    assert_eq!(missing, &[EMBEDDING_FILE.to_owned()]);
    assert!(error.to_string().contains(EMBEDDING_FILE), "{error}");
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    let error = models::ensure(&store).unwrap_err().to_string();
    assert!(
        error.contains(&format!("{mirror}/diarization/{EMBEDDING_FILE}")),
        "{error}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(folder.join(SEGMENTATION_FILE).is_file());
}

/// `NotInstalled` survives boxing: a `ModelDiarizer` under `Install::Never`
/// over an empty store fails `prepare` with it, and nothing reaches the mirror.
#[tokio::test(flavor = "current_thread")]
async fn not_installed_stays_distinguishable_through_a_box_error() {
    let converted = DiarizeError::from(SpeechError::NotInstalled {
        asset: models::ASSET_ID.to_owned(),
        directory: "/models/onnx/diarization".into(),
        missing: vec![EMBEDDING_FILE.to_owned()],
    });
    let boxed: BoxError = Box::new(converted);
    assert!(matches!(
        boxed.downcast_ref::<DiarizeError>(),
        Some(DiarizeError::NotInstalled { missing, .. }) if missing == &[EMBEDDING_FILE.to_owned()]
    ));

    let dir = tempfile::tempdir().unwrap();
    let (mirror, requests) = counting_mirror();
    let store = ModelStore::in_models_directory(dir.path()).with_mirror(Some(mirror));
    let diarizer = ModelDiarizer::onnx(DiarizerConfig::default(), store, Install::Never, 1);
    let error = diarizer.prepare().await.unwrap_err();
    let Some(DiarizeError::NotInstalled { missing, .. }) = error.downcast_ref::<DiarizeError>()
    else {
        panic!("not installed: {error:?}");
    };
    assert_eq!(missing, &[SEGMENTATION_FILE, EMBEDDING_FILE]);
    // Tried again, still without a request.
    assert!(diarizer.prepare().await.is_err());
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(!dir.path().join("onnx").join("diarization").exists());
}

/// Right-size files that fail to load and fail their checksum are deleted
/// and reported not installed; `Install::Allowed` downloads them again.
#[test]
fn right_size_junk_is_deleted_after_a_failed_load() {
    for install in [Install::Never, Install::Allowed] {
        let dir = tempfile::tempdir().unwrap();
        let (mirror, requests) = counting_mirror();
        let store = ModelStore::in_models_directory(dir.path()).with_mirror(Some(mirror));
        let asset = models::asset();
        let folder = install_junk(&store);
        assert!(store.is_installed(&asset));

        let error = OnnxBackend::from_store(&store, install, 1).unwrap_err();
        let DiarizeError::NotInstalled {
            directory, missing, ..
        } = &error
        else {
            panic!("{install:?}: not installed: {error:?}");
        };
        assert_eq!(directory, &folder);
        assert_eq!(missing, &[SEGMENTATION_FILE, EMBEDDING_FILE]);
        assert!(!folder.join(SEGMENTATION_FILE).exists());
        assert!(!folder.join(EMBEDDING_FILE).exists());
        assert!(!store.is_installed(&asset));
        assert_eq!(requests.load(Ordering::SeqCst), 0, "{install:?}");

        let again = OnnxBackend::from_store(&store, install, 1).unwrap_err();
        match install {
            Install::Never => {
                assert!(
                    matches!(again, DiarizeError::NotInstalled { .. }),
                    "{again}"
                );
                assert_eq!(requests.load(Ordering::SeqCst), 0);
            }
            Install::Allowed => {
                assert!(matches!(again, DiarizeError::Model(_)), "{again}");
                assert!(requests.load(Ordering::SeqCst) > 0);
            }
        }
    }
}

/// Leftover `<file name>*.part` files are deleted on ensure; others stay.
#[test]
fn ensure_deletes_the_earlier_stores_part_files() {
    let dir = tempfile::tempdir().unwrap();
    let (mirror, requests) = counting_mirror();
    let store = ModelStore::in_models_directory(dir.path()).with_mirror(Some(mirror));
    let folder = install_junk(&store);
    let old = [
        format!("{SEGMENTATION_FILE}a1B2c3.part"),
        format!("{EMBEDDING_FILE}Zz9Yy8.part"),
    ];
    for name in &old {
        std::fs::write(folder.join(name), b"old").unwrap();
    }
    std::fs::write(folder.join("notes.part"), b"kept").unwrap();

    models::ensure(&store).unwrap();
    for name in &old {
        assert!(!folder.join(name).exists(), "{name}");
    }
    assert!(folder.join("notes.part").exists());
    assert!(folder.join(SEGMENTATION_FILE).is_file());
    assert_eq!(requests.load(Ordering::SeqCst), 0);
}

/// After a failed load an intact file is kept and only the bad one deleted.
/// Needs `STENO_MODEL_TESTS=1` and `STENO_MODELS_DIR`, which it only reads.
#[test]
fn after_a_failed_load_an_intact_file_is_kept() {
    if std::env::var("STENO_MODEL_TESTS").as_deref() != Ok("1") {
        eprintln!("set STENO_MODEL_TESTS=1 and STENO_MODELS_DIR to run this with the real models");
        return;
    }
    let installed = ModelStore::from_environment();
    let asset = models::asset();
    let real = installed
        .installed_directory(&asset)
        .expect("STENO_MODELS_DIR holds onnx/diarization/")
        .join(SEGMENTATION_FILE);
    for install in [Install::Never, Install::Allowed] {
        let dir = tempfile::tempdir().unwrap();
        let (mirror, requests) = counting_mirror();
        let store = ModelStore::in_models_directory(dir.path()).with_mirror(Some(mirror));
        let folder = install_junk(&store);
        std::fs::copy(&real, folder.join(SEGMENTATION_FILE)).unwrap();

        let error = OnnxBackend::from_store(&store, install, 4).unwrap_err();
        assert!(
            matches!(&error, DiarizeError::NotInstalled { missing, .. } if missing == &[EMBEDDING_FILE.to_owned()]),
            "{install:?}: {error:?}"
        );
        assert!(folder.join(SEGMENTATION_FILE).is_file(), "{install:?}");
        assert!(!folder.join(EMBEDDING_FILE).exists(), "{install:?}");
        assert_eq!(requests.load(Ordering::SeqCst), 0, "{install:?}");
    }
}
