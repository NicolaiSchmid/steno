//! `steno-speech-sidecar`: see the library docs. Started by the app with
//! piped stdin and stdout; not meant to be run by hand.

use std::io::Write as _;
use std::process::ExitCode;

use steno_speech_sidecar::{Options, serve};

fn main() -> ExitCode {
    // A panic leaves its log in the support directory the app names; stderr
    // still carries it to the parent.
    if let Some(directory) = std::env::var_os(steno_core::crash_log::DIRECTORY_VARIABLE) {
        steno_core::crash_log::install_crash_log_hook(directory.into(), Some("sidecar"));
    }
    match Options::parse(std::env::args_os().skip(1)) {
        Ok(options) => serve(&options),
        Err(error) => {
            // Not `eprintln!`, which panics once stderr is gone.
            let _ = writeln!(std::io::stderr(), "steno-speech-sidecar: {error}");
            ExitCode::from(2)
        }
    }
}
