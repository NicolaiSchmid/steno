//! `log_to_stderr` installs the process's one subscriber, so each case runs
//! in a child process of this test binary and reads its stderr.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const CHILD_FILTER: &str = "STENO_LOG_TO_STDERR_CHILD_FILTER";
const NAME: &str = "lines_reach_stderr_from_warn_up_unless_rust_log_says_otherwise";

/// Set in the child of the burst and the panic test.
const CHILD: &str = "STENO_LOG_TO_STDERR_CHILD";

/// The burst the burst test logs, within the queue's 4096.
const BURST: usize = 1000;

/// The burst the panic test logs: within the queue, and more than any
/// platform's pipe buffer holds.
const PANIC_BURST: usize = 3000;

/// What the panic test's child prints right before it panics.
const PANICKING: &str = "panicking now";

/// A child running the test `name` alone, with `RUST_LOG` unset.
fn child(name: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env_remove("RUST_LOG");
    command
}

/// The child's stderr, the default filter `default_filter` and `RUST_LOG`
/// as given.
fn child_stderr(default_filter: &str, rust_log: Option<&str>) -> String {
    let mut command = child(NAME);
    command.env(CHILD_FILTER, default_filter);
    if let Some(rust_log) = rust_log {
        command.env("RUST_LOG", rust_log);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stderr).unwrap()
}

/// `count` numbered warnings.
fn log_burst(count: usize) {
    for line in 0..count {
        tracing::warn!("burst line {line}");
    }
}

/// How many of the burst's lines `stderr` holds.
fn burst_lines(stderr: &str) -> usize {
    stderr
        .lines()
        .filter(|line| line.contains("burst line "))
        .count()
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

/// A burst the queue holds comes out whole, nothing dropped.
#[test]
fn a_burst_the_queue_holds_reaches_stderr_whole() {
    const ME: &str = "a_burst_the_queue_holds_reaches_stderr_whole";
    if std::env::var_os(CHILD).is_some_and(|name| name == ME) {
        steno_services::log_to_stderr(steno_services::LOG_FILTER);
        log_burst(BURST);
        steno_services::flush_logs();
        return;
    }
    let Output { status, stderr, .. } = child(ME).env(CHILD, ME).output().unwrap();
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(status.success(), "{status}: {stderr}");
    assert_eq!(burst_lines(&stderr), BURST, "{stderr}");
    assert!(!stderr.contains("lines dropped"), "{stderr}");
}

/// A panic writes out the queued lines before the hook it wraps runs, here
/// one that ends the process at once, as an abort would. The child says
/// on stdout when it is about to panic; the parent reads its stderr only a
/// moment later, so the writer thread is still stuck on a full pipe then,
/// and only the panic's flush lets the lines out.
#[test]
fn a_panic_writes_out_the_queued_lines_first() {
    const ME: &str = "a_panic_writes_out_the_queued_lines_first";
    if std::env::var_os(CHILD).is_some_and(|name| name == ME) {
        std::panic::set_hook(Box::new(|_| std::process::exit(3)));
        steno_services::log_to_stderr(steno_services::LOG_FILTER);
        log_burst(PANIC_BURST);
        println!("{PANICKING}");
        std::io::stdout().flush().unwrap();
        panic!("the crash the lines explain");
    }
    let mut running = child(ME)
        .env(CHILD, ME)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Past the harness's own lines.
    let said = BufReader::new(running.stdout.take().unwrap())
        .lines()
        .map_while(Result::ok)
        .any(|line| line.ends_with(PANICKING));
    assert!(said, "the child never got to its panic");
    std::thread::sleep(Duration::from_millis(50));
    let Output { status, stderr, .. } = running.wait_with_output().unwrap();
    let stderr = String::from_utf8(stderr).unwrap();
    assert_eq!(status.code(), Some(3), "{stderr}");
    assert_eq!(burst_lines(&stderr), PANIC_BURST, "{stderr}");
}
