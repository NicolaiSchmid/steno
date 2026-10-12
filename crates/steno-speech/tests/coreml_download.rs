//! The `CoreML` Parakeet download from Hugging Face into an empty models
//! directory gives the tree the Swift app installed on 2026-09-25: the
//! same 23 files, sizes and SHA-256 (`tests/fixtures/parakeet-v3-coreml.sha256`),
//! beside the store's `.lock` file per file. About 483 MB from the
//! network, so it runs only with `STENO_COREML_DOWNLOAD=1`; set
//! `STENO_COREML_DOWNLOAD_DIR` to keep the download (it must not hold one
//! already), for the `CoreML` backend's FLEURS check. CI never runs it.

use std::fs;
use std::path::{Path, PathBuf};

use steno_speech::{ModelAsset, ModelStore, model_store::sha256_of};

/// `(path, size, sha256)` of every file under `directory` but the locks,
/// with `/`, sorted.
fn tree(directory: &Path) -> Vec<(String, u64, String)> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_none_or(|ext| ext != "lock") {
                let relative: Vec<_> = path
                    .strip_prefix(directory)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let size = fs::metadata(&path).unwrap().len();
                files.push((relative.join("/"), size, sha256_of(&path).unwrap()));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn a_download_into_an_empty_models_directory_is_the_swift_apps_tree() {
    if std::env::var("STENO_COREML_DOWNLOAD").as_deref() != Ok("1") {
        eprintln!("set STENO_COREML_DOWNLOAD=1 to download the CoreML Parakeet (about 483 MB)");
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let models: PathBuf = std::env::var_os("STENO_COREML_DOWNLOAD_DIR")
        .map_or_else(|| scratch.path().join("Models"), PathBuf::from);
    let store = ModelStore::coreml_in_models_directory(&models);
    let asset = ModelAsset::parakeet_v3_coreml();
    let directory = store.directory(&asset);
    assert!(
        !directory.exists(),
        "{} must start empty",
        directory.display()
    );
    let started = std::time::Instant::now();
    let installed = store.ensure(&asset, &mut |_| {}).unwrap();
    eprintln!("downloaded in {:.1} s", started.elapsed().as_secs_f64());
    assert_eq!(
        installed,
        models.join("fluidaudio").join("parakeet-tdt-0.6b-v3")
    );
    store.verify(&asset).unwrap();
    let mut expected: Vec<(String, u64, String)> =
        include_str!("fixtures/parakeet-v3-coreml.sha256")
            .lines()
            .map(|line| {
                let fields: Vec<_> = line.split("  ").collect();
                (
                    fields[2].to_owned(),
                    fields[1].parse().unwrap(),
                    fields[0].to_owned(),
                )
            })
            .collect();
    expected.sort();
    assert_eq!(tree(&installed), expected);
}
