//! `steno-speech-sidecar`: see the library docs. Started by the app with
//! piped stdin and stdout; not meant to be run by hand.

use std::io::Write as _;
use std::process::ExitCode;

use steno_speech_sidecar::{Options, serve};

fn main() -> ExitCode {
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
