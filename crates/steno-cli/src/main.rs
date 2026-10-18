//! The `steno` command line tool: `record`, `process`, `export`, `deliver`
//! and the developer tools under `steno dev`. Exits 0 on success, 1 on a
//! usage error, 2 on a runtime failure.

mod commands;
mod wiring;

use clap::Parser;

use crate::commands::{Command, Steno};
use crate::wiring::Failure;

fn main() {
    // Without the pipeline's line for a failed background run: the CLI
    // prints the failure itself.
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
    // Flush the queued log lines after the runtime's tasks end and before
    // the failure message. A worker parked in a blocking call would hold
    // the exit, hence the one-second cap.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    steno_services::flush_logs();
    let Err(failure) = outcome else {
        return;
    };
    let (code, message) = match failure {
        Failure::Usage(message) => (1, message),
        Failure::Runtime(message) => (2, message),
    };
    eprintln!("{message}");
    std::process::exit(code);
}
