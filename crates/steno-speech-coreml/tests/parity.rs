//! The parity harness as an ignored test, for Forge:
//!
//! ```text
//! STENO_CALIBRATION_CORPUS=~/steno-spikes/corpus \
//! STENO_CALIBRATION_BASELINE=~/steno-spikes/baseline-bakeoff \
//! cargo test --release -p steno-speech-coreml --test parity -- --ignored --nocapture
//! ```
//!
//! `STENO_CALIBRATION_BASELINE` defaults to `<corpus>/../baseline-bakeoff`.
//!
//! The test tracks drift from the old Swift output; it is not the accuracy
//! gate. FLEURS, which has references, is the accuracy gate
//! (`crates/steno-speech/tests/fleurs.rs`, and the `CoreML` run in
//! `scripts/coreml-parity-forge.sh`). The Swift transcripts are another
//! engine's output, not references, and the engine runs the shared
//! pipeline, whose windows are not `FluidAudio`'s, so its text is no
//! longer the Swift app's word for word: 8.46 % on the corpus when A2 of
//! `.plans/2026-10-07-stable-promotion.md` landed, from 0.00 % before,
//! while FLEURS German improved from 5.46 % to 4.87 %. Two limits:
//!
//! - `STENO_PARITY_MAX_WER` (percent, default 10) on the mean of the
//!   per-file WERs against the Swift text;
//! - `STENO_PARITY_MAX_START_OFFSET_MS` (default 15) on the mean signed
//!   word-start offset from Swift's starts over the matched words. The
//!   shared pipeline times words from each chunk's exact start (A13), and
//!   its chunks start anywhere while Swift's sit on the frame grid: +2.5 ms
//!   on the corpus when A2 landed, with a wider spread than Swift's grid
//!   (p10 -56 ms, p90 +56 ms); a start counted from the frame below the
//!   chunk's, as before A13, is about -30 ms.
//!
//! Without the corpus variable the test passes and says so, so `--ignored`
//! runs elsewhere do not fail for the wrong reason.

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
        "mean per-file WER {mean:.2} % exceeds the {max_wer:.2} % gate"
    );
    let offset = report.mean_start_offset_seconds() * 1000.0;
    assert!(
        offset.abs() <= max_offset_ms,
        "mean start offset {offset:+.1} ms is outside the {max_offset_ms:.1} ms gate"
    );
}
