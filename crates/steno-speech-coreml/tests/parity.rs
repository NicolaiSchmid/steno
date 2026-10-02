//! The parity harness as an ignored test, for Forge:
//!
//! ```text
//! STENO_CALIBRATION_CORPUS=~/steno-spikes/corpus \
//! STENO_CALIBRATION_BASELINE=~/steno-spikes/baseline-bakeoff \
//! cargo test --release -p steno-speech-coreml --test parity -- --ignored --nocapture
//! ```
//!
//! `STENO_CALIBRATION_BASELINE` defaults to `<corpus>/../baseline-bakeoff`;
//! `STENO_PARITY_MAX_WER` (percent, default 2) is the gate on the mean of
//! the per-file WERs. Without the corpus variable the test passes and says
//! so, so `--ignored` runs elsewhere do not fail for the wrong reason.

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
        .unwrap_or(2.0);
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
        "mean per-file WER {mean:.2} % exceeds the {max_wer:.2} % gate"
    );
}
