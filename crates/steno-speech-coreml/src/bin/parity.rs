//! The parity harness: the calibration corpus through the CoreML
//! pipeline, scored against the Swift app's transcripts.
//!
//! ```text
//! steno-coreml-parity [--models DIR] [--out DIR] [--concurrency N] <corpus-dir> <baseline-dir>
//! ```
//!
//! `corpus-dir` holds `<name>.wav` (16 kHz mono), `baseline-dir` holds
//! `<name>.parakeet-v3.json` (the Swift `RawSegment` array the bake-off
//! wrote). Prints the per-file table and writes `<name>.rust.json` into
//! `--out` when given. macOS only; elsewhere the binary says so and exits.

// The crate's lib allows the same: the docs name CoreML and FluidAudio.
#![allow(clippy::doc_markdown)]

#[cfg(target_os = "macos")]
fn main() {
    use std::path::PathBuf;

    use steno_speech_coreml::parity::{Options, run};

    fn usage() -> ! {
        eprintln!(
            "usage: steno-coreml-parity [--models DIR] [--out DIR] [--concurrency N] <corpus-dir> <baseline-dir>"
        );
        std::process::exit(2);
    }

    let mut args = std::env::args().skip(1);
    let mut options = Options::default();
    let mut positional: Vec<PathBuf> = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--models" => options.models = Some(args.next().map_or_else(|| usage(), PathBuf::from)),
            "--out" => options.out = Some(args.next().map_or_else(|| usage(), PathBuf::from)),
            "--concurrency" => {
                options.concurrency = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage());
            }
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    if positional.len() != 2 {
        usage();
    }
    match run(&positional[0], &positional[1], &options) {
        Ok(report) => print!("{report}"),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("steno-coreml-parity runs on macOS only (CoreML)");
    std::process::exit(2);
}
