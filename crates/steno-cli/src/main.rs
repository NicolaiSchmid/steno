//! The `steno` command line tool. Product commands are `record`, `process`,
//! `export` and `deliver`; every developer tool sits under `steno dev`.
//! Swift: `Sources/steno/`. Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`, `WP6b`.
//!
//! Exit codes: 0 success, 1 usage (bad arguments, unknown template, an
//! unknown engine or asset), 2 a runtime failure (database, files, a failed
//! pipeline run).

mod commands;
mod wiring;

use clap::Parser;

use crate::commands::{Command, Steno};
use crate::wiring::Failure;

fn main() {
    // `warn`, without the pipeline's line for a failed background run: the
    // CLI waits for its run and prints the failure itself, as Swift's
    // `steno process` did, so the line would say it twice.
    steno_services::log_to_stderr(&format!(
        "{},{}=off",
        steno_services::LOG_FILTER,
        steno_pipeline::BACKGROUND_RUN_LOG
    ));
    let steno = match Steno::try_parse() {
        Ok(steno) => steno,
        Err(error) => {
            let usage = error.use_stderr();
            let _ = error.print();
            std::process::exit(i32::from(usage));
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let outcome = runtime.block_on(async {
        match steno.command {
            Command::Record(command) => command.run().await,
            Command::Process(command) => command.run().await,
            Command::Export(command) => command.run(),
            Command::Deliver(command) => command.run().await,
            Command::Dev(command) => command.run().await,
        }
    });
    let failure = match outcome {
        Ok(()) => {
            drop(runtime);
            steno_services::flush_logs();
            return;
        }
        Err(failure) => failure,
    };
    // The queued log lines first, so they come out before the failure they
    // led to, as when they were written at once.
    steno_services::flush_logs();
    let (code, message) = match failure {
        Failure::Usage(message) => (1, message),
        Failure::Runtime(message) => (2, message),
    };
    eprintln!("{message}");
    std::process::exit(code);
}
