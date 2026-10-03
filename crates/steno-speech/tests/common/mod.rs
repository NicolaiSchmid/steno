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
/// `STENO_MODELS_DIR` names; read the way the engine reads it.
pub fn models_dir() -> Option<ModelStore> {
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

/// Says which variable would have let the model-gated test run.
pub fn skip(variable: &str) {
    eprintln!("skipped: {variable} is unset or does not point at the data");
}

/// Reads a 16 kHz mono PCM-16 WAV into `f32` samples.
pub fn read_wav(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&bytes[0..4], b"RIFF", "{}: not a WAV", path.display());
    assert_eq!(&bytes[8..12], b"WAVE");
    let mut offset = 12;
    let mut channels = 1u16;
    let mut sample_rate = 0u32;
    let mut bits = 16u16;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = &bytes[offset + 8..(offset + 8 + size).min(bytes.len())];
        if id == b"fmt " {
            channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
            sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
        } else if id == b"data" {
            assert_eq!(sample_rate, 16_000, "{}: {sample_rate} Hz", path.display());
            assert_eq!(bits, 16, "{}: {bits} bits", path.display());
            let step = 2 * channels as usize;
            return body
                .chunks_exact(step)
                .map(|frame| f32::from(i16::from_le_bytes([frame[0], frame[1]])) / 32_768.0)
                .collect();
        }
        offset += 8 + size + (size & 1);
    }
    panic!("{}: no data chunk", path.display());
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
