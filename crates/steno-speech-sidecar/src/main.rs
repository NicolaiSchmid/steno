//! `steno-speech-sidecar`: see the library docs. Started by the app with
//! piped stdin and stdout; not meant to be run by hand.

use std::process::ExitCode;

use steno_speech_sidecar::{Options, serve};

fn main() -> ExitCode {
    match Options::parse(std::env::args_os().skip(1)) {
        Ok(options) => serve(&options),
        Err(error) => {
            eprintln!("steno-speech-sidecar: {error}");
            ExitCode::from(2)
        }
    }
}
