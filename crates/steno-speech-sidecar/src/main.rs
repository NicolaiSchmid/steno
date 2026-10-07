//! `steno-speech-sidecar`: see the library docs. Started by the app with
//! piped stdin and stdout; not meant to be run by hand.

use std::io::Write as _;
use std::process::ExitCode;

use steno_speech_sidecar::{Options, serve};

fn main() -> ExitCode {
    // The app names its support directory, where a panic leaves its log
    // beside the app's (`steno_core::crash_log`); stderr still carries the
    // panic to the parent's crash report.
    if let Some(directory) = std::env::var_os(steno_core::crash_log::DIRECTORY_VARIABLE) {
        steno_core::crash_log::write_crash_logs(directory.into(), Some("sidecar"));
    }
    match Options::parse(std::env::args_os().skip(1)) {
        Ok(options) => serve(&options),
        Err(error) => {
            // Not `eprintln!`, which panics with status 101 once stderr
            // is gone.
            let _ = writeln!(std::io::stderr(), "steno-speech-sidecar: {error}");
            ExitCode::from(2)
        }
    }
}
