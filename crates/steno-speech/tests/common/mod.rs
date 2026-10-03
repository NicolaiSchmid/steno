//! Shared by the model-gated integration tests: the environment gate, a
//! PCM-16 WAV reader and the word error rate scorer of spike F
//! (`spikes/onnx-speech/fleurs/score_fleurs.py`), ported so the gate is
//! measured the way the spike table was.

// `dead_code`: the `transcribe` example includes this module for `read_wav`
// alone. The cast allows repeat the crate's because a test target does not
// inherit `lib.rs` attributes and `[lints] workspace = true` leaves no room
// for a per-package table.
#![allow(
    dead_code,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::assert_is_empty
)]

use std::path::{Path, PathBuf};

use steno_speech::{ModelAsset, ModelStore};

/// The model store, when both assets are installed under the root
/// `STENO_MODELS_DIR` names; read the way [`ModelStore::from_environment`] reads it.
pub fn installed_store() -> Option<ModelStore> {
    let store = ModelStore::new(ModelStore::environment_root()?);
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

/// Spike F's normaliser: lower case, `%` to `prozent`, `€` to `euro`, `$` to
/// `dollar`, every run of non-alphanumerics a word boundary.
pub fn normalise(text: &str) -> Vec<String> {
    let text = text
        .to_lowercase()
        .replace('%', " prozent ")
        .replace('€', " euro ")
        .replace('$', " dollar ")
        .replace('_', " ");
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Word-level Levenshtein: (errors, substitutions, deletions, insertions).
pub fn edit_distance(reference: &[String], hypothesis: &[String]) -> (usize, usize, usize, usize) {
    let (n, m) = (reference.len(), hypothesis.len());
    let mut previous: Vec<(usize, usize, usize, usize)> = (0..=m).map(|j| (j, 0, 0, j)).collect();
    for i in 1..=n {
        let mut current = vec![(i, 0, i, 0); m + 1];
        for j in 1..=m {
            if reference[i - 1] == hypothesis[j - 1] {
                current[j] = previous[j - 1];
            } else {
                let substitute = (
                    previous[j - 1].0 + 1,
                    previous[j - 1].1 + 1,
                    previous[j - 1].2,
                    previous[j - 1].3,
                );
                let delete = (
                    previous[j].0 + 1,
                    previous[j].1,
                    previous[j].2 + 1,
                    previous[j].3,
                );
                let insert = (
                    current[j - 1].0 + 1,
                    current[j - 1].1,
                    current[j - 1].2,
                    current[j - 1].3 + 1,
                );
                current[j] = [substitute, delete, insert]
                    .into_iter()
                    .min_by_key(|t| t.0)
                    .unwrap();
            }
        }
        previous = current;
    }
    previous[m]
}

pub struct Score {
    pub reference_words: usize,
    pub errors: usize,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
}

impl Score {
    pub fn wer(&self) -> f64 {
        if self.reference_words == 0 {
            0.0
        } else {
            self.errors as f64 / self.reference_words as f64
        }
    }
}

pub fn score(reference: &str, hypothesis: &str) -> Score {
    let reference = normalise(reference);
    let hypothesis = normalise(hypothesis);
    let (errors, substitutions, deletions, insertions) = edit_distance(&reference, &hypothesis);
    Score {
        reference_words: reference.len(),
        errors,
        substitutions,
        deletions,
        insertions,
    }
}

#[test]
fn the_scorer_matches_the_spike_normaliser() {
    assert_eq!(
        normalise("Der pH-Wert liegt bei 7, 50% also!"),
        [
            "der", "ph", "wert", "liegt", "bei", "7", "50", "prozent", "also"
        ]
    );
    let s = score("eins zwei drei vier", "eins zwo drei");
    assert_eq!(
        (s.errors, s.substitutions, s.deletions, s.insertions),
        (2, 1, 1, 0)
    );
    assert!((s.wer() - 0.5).abs() < 1e-12);
    assert_eq!(score("", "x").reference_words, 0);
}
