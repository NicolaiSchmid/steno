//! Shared by the integration tests: where the fixtures live and how to read
//! one. Each test binary uses a subset, hence the `dead_code` allowance.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

/// The repository root, from the crate's manifest directory.
pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `apps/macos/web/fixtures/bridge`: the contract fixtures Swift writes.
pub fn bridge_fixtures_dir() -> PathBuf {
    repository_root().join("apps/macos/web/fixtures/bridge")
}

/// A bridge fixture decoded as `T`.
pub fn bridge_fixture<T: DeserializeOwned>(name: &str) -> T {
    let path = bridge_fixtures_dir().join(format!("{name}.json"));
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{name}.json: {e}"))
}

/// A file under this crate's own `tests/fixtures/`, as text.
pub fn crate_fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
