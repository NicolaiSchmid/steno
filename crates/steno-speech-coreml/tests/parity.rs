//! The parity harness as an ignored test (`STENO_CALIBRATION_CORPUS`,
//! `STENO_CALIBRATION_BASELINE`). It tracks drift from the Swift output; the
//! accuracy gate is FLEURS (`crates/steno-speech/tests/fleurs.rs`). Limits:
//! `STENO_PARITY_MAX_WER` (percent, default 10) on the mean WER and
//! `STENO_PARITY_MAX_START_OFFSET_MS` (default 15) on the mean word-start offset.

#![cfg(target_os = "macos")]

use std::path::PathBuf;

use steno_speech_coreml::parity::{Options, run};

#[test]
#[ignore = "needs the Parakeet models and the calibration corpus (Forge)"]
fn rust_transcripts_match_the_swift_baseline() {
    let Some(corpus) = std::env::var_os("STENO_CALIBRATION_CORPUS").map(PathBuf::from) else {
        eprintln!("STENO_CALIBRATION_CORPUS unset: nothing to compare");
        return;
    };
    let baseline = std::env::var_os("STENO_CALIBRATION_BASELINE")
        .map_or_else(|| corpus.join("..").join("baseline-bakeoff"), PathBuf::from);
    let max_wer: f64 = std::env::var("STENO_PARITY_MAX_WER")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10.0);
    let max_offset_ms: f64 = std::env::var("STENO_PARITY_MAX_START_OFFSET_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(15.0);
    let options = Options {
        out: std::env::var_os("STENO_PARITY_OUT").map(PathBuf::from),
        ..Options::default()
    };
    let report = run(&corpus, &baseline, &options).expect("harness runs");
    println!("{report}");
    assert!(
        !report.files.is_empty(),
        "no corpus files under {}",
        corpus.display()
    );
    let mean = report.mean_file_wer() * 100.0;
    assert!(
        mean <= max_wer,
        "mean per-file WER {mean:.2} % exceeds the {max_wer:.2} % limit"
    );
    let offset = report.mean_start_offset_seconds() * 1000.0;
    assert!(
        offset.abs() <= max_offset_ms,
        "mean start offset {offset:+.1} ms is outside the {max_offset_ms:.1} ms limit"
    );
}
