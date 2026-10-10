//! What the test binaries share: the sidecar binary, and a child of the
//! test binary under a temporary home ([`child_in`], [`printed`]).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use steno_speech::sidecar::SIDECAR_BINARY;

/// The sidecar binary in the target directory: the test binary sits in
/// its `deps/`, the binaries one folder up.
pub fn sidecar_binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let binary = exe
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap()
        .join(SIDECAR_BINARY);
    assert!(
        binary.is_file(),
        "{} is missing: build it with `cargo build -p steno-speech-sidecar` \
         or run `cargo test --workspace`",
        binary.display()
    );
    binary
}

/// The test `name` run alone in a child of this test binary, with `home`
/// as its home (`HOME`, `XDG_DATA_HOME`, `APPDATA`), so the child's
/// default support directory is under it, never the developer's.
pub fn child_in(home: &Path, name: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("APPDATA", home.join("appdata"));
    command
}

/// What the child printed after `label`, to the end of its line. The
/// harness may print the test's name on the same line first.
pub fn printed<'a>(stdout: &'a str, label: &str) -> &'a str {
    stdout
        .lines()
        .find_map(|line| line.split_once(label).map(|(_, value)| value))
        .unwrap_or_else(|| panic!("no {label} in {stdout}"))
}
