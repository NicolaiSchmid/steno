//! Shared by the model-gated integration tests: the environment gate, the
//! PCM-16 WAV reader and spike F's word error rate scorer
//! ([`steno_speech::wer`]).

// `dead_code` and `unused_imports`: the `transcribe` example includes this
// module for `read_wav` alone. The cast allows repeat the crate's because a test target does not
// inherit `lib.rs` attributes and `[lints] workspace = true` leaves no room
// for a per-package table.
#![allow(
    dead_code,
    unused_imports,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::assert_is_empty
)]

use std::path::{Path, PathBuf};

pub use steno_speech::wer::{normalise, word_errors as score};
use steno_speech::{ModelAsset, ModelStore};

/// The model store, when both assets are installed in the models directory
/// `STENO_MODELS_DIR` names; read the way [`ModelStore::from_environment`] reads it.
pub fn installed_store() -> Option<ModelStore> {
    let directory = ModelStore::environment_models_directory()?;
    let store = ModelStore::in_models_directory(&directory);
    [ModelAsset::parakeet_v3_fp32(), ModelAsset::silero_vad()]
        .iter()
        .all(|asset| store.is_installed(asset))
        .then_some(store)
}

/// The FLEURS directory (`cat/`, `utt/`), when present.
pub fn fleurs_dir() -> Option<PathBuf> {
    let root = std::env::var_os("STENO_FLEURS_DIR").map(PathBuf::from)?;
    root.join("cat").is_dir().then_some(root)
}

/// Says which variable would have let the model-gated test run, and what
/// the directory it names needs.
pub fn skip(variable: &str, needs: &str) {
    eprintln!("skipped: {variable} is unset or lacks {needs}");
}

/// Reads a 16 kHz mono PCM-16 WAV into `f32` samples.
pub fn read_wav(path: &Path) -> Vec<f32> {
    steno_speech::wav::read_pcm16(path).unwrap_or_else(|e| panic!("{e}"))
}
