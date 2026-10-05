//! `log_to_stderr` installs the process's one subscriber, so each case runs
//! in a child process of this test binary and reads its stderr.

const CHILD_FILTER: &str = "STENO_LOG_TO_STDERR_CHILD_FILTER";
const NAME: &str = "lines_reach_stderr_from_warn_up_unless_rust_log_says_otherwise";

/// The child's stderr, the default filter `default_filter` and `RUST_LOG`
/// as given.
fn child_stderr(default_filter: &str, rust_log: Option<&str>) -> String {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(CHILD_FILTER, default_filter)
        .env_remove("RUST_LOG");
    if let Some(rust_log) = rust_log {
        command.env("RUST_LOG", rust_log);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn lines_reach_stderr_from_warn_up_unless_rust_log_says_otherwise() {
    if let Ok(filter) = std::env::var(CHILD_FILTER) {
        steno_services::log_to_stderr(&filter);
        tracing::warn!("a warning line");
        tracing::info!("an info line");
        // The writer thread ends with the process, so the queued lines are
        // written out first, as the shell and the CLI do.
        steno_services::flush_logs();
        return;
    }
    let shell = child_stderr(steno_services::LOG_FILTER, None);
    assert!(shell.contains("a warning line"), "{shell}");
    assert!(!shell.contains("an info line"), "{shell}");

    let verbose = child_stderr(steno_services::LOG_FILTER, Some("info"));
    assert!(verbose.contains("an info line"), "{verbose}");

    let target_off = child_stderr("warn,log_to_stderr=off", None);
    assert!(!target_off.contains("a warning line"), "{target_off}");
}
