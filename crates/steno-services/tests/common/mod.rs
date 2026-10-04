//! What the test binaries share.

use std::path::PathBuf;

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
