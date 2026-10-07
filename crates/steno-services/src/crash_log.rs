//! A file for every panic. A release build unwinds a panic, and an app
//! opened from the Finder, the Dock or at login has its stderr thrown
//! away, so a panic on the Mac left no trace at all. [`write_crash_logs`]
//! installs a panic hook that writes `crash-<UTC time>.log` into a folder
//! (the shell passes the support directory) with the panic's message, its
//! location, the thread and a backtrace, keeps the newest
//! [`KEPT_CRASH_LOGS`] of them, and then runs the hook it replaced (the
//! log queue's flush and the default message on stderr). Rust only: the
//! Swift app had the system's crash reporter.
//!
//! The file stays on the computer, as every log does; nothing sends it.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

/// How many crash logs the folder keeps; the oldest beyond go.
pub const KEPT_CRASH_LOGS: usize = 20;

/// Every crash log's name starts with this and ends in `.log`.
const PREFIX: &str = "crash-";

/// Installs the panic hook (see the module doc) over the one installed
/// now, so call it after [`log_to_stderr`](crate::log_to_stderr), and
/// trims `folder` to the newest [`KEPT_CRASH_LOGS`] files. The folder is
/// created at the first panic if it is missing.
///
/// ```no_run
/// steno_services::log_to_stderr(steno_services::LOG_FILTER);
/// steno_services::crash_log::write_crash_logs(
///     steno_core::StenoPaths::default_support_directory(),
/// );
/// ```
pub fn write_crash_logs(folder: PathBuf) {
    prune(&folder, KEPT_CRASH_LOGS);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let location = panic
            .location()
            .map_or_else(|| "an unknown place".to_owned(), ToString::to_string);
        let message = panic_message(panic.payload());
        let backtrace = std::backtrace::Backtrace::force_capture();
        // A failed write must not panic inside the panic hook.
        if let Ok(path) = write_crash_log(&folder, Utc::now(), &message, &location, &backtrace) {
            prune(&folder, KEPT_CRASH_LOGS);
            eprintln!("steno: the crash is described in {}", path.display());
        }
        previous(panic);
    }));
}

/// The text a panic was raised with, when it is a string.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "a panic without a message".to_owned()
    }
}

/// Writes one crash log into `folder`, created if missing, and returns its
/// path. A second panic in the same millisecond gets a numbered name.
fn write_crash_log(
    folder: &Path,
    at: DateTime<Utc>,
    message: &str,
    location: &str,
    backtrace: &dyn std::fmt::Display,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let stamp = at.format("%Y-%m-%dT%H-%M-%S%.3fZ");
    let thread = std::thread::current();
    let text = format!(
        "Steno {} panicked at {location} on thread '{}' ({})\n{message}\n\nbacktrace:\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
        thread.name().unwrap_or("unnamed"),
        at.to_rfc3339(),
    );
    let mut attempt = 0u32;
    loop {
        let name = if attempt == 0 {
            format!("{PREFIX}{stamp}.log")
        } else {
            format!("{PREFIX}{stamp}-{attempt}.log")
        };
        let path = folder.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt < 100 => {
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Deletes the oldest crash logs in `folder` beyond the newest `keep`;
/// the names sort by time. Anything that cannot be read or deleted stays.
fn prune(folder: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "log")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(PREFIX))
        })
        .collect();
    logs.sort();
    let surplus = logs.len().saturating_sub(keep);
    for path in &logs[..surplus] {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(second: u32) -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 10, 7, 12, 0, second).unwrap()
    }

    #[test]
    fn a_crash_log_holds_the_message_the_location_and_the_backtrace() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("support");
        let path = write_crash_log(
            &folder,
            at(5),
            "index out of range",
            "src/x.rs:1:2",
            &"frame 0",
        )
        .unwrap();
        assert_eq!(
            path.file_name().unwrap(),
            "crash-2026-10-07T12-00-05.000Z.log"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("panicked at src/x.rs:1:2"), "{text}");
        assert!(text.contains("\nindex out of range\n"), "{text}");
        assert!(text.ends_with("backtrace:\nframe 0\n"), "{text}");
    }

    #[test]
    fn two_crashes_in_one_millisecond_get_two_files() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_crash_log(dir.path(), at(5), "one", "a", &"").unwrap();
        let second = write_crash_log(dir.path(), at(5), "two", "a", &"").unwrap();
        assert_ne!(first, second);
        assert!(std::fs::read_to_string(second).unwrap().contains("two"));
    }

    #[test]
    fn only_the_newest_crash_logs_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        for second in 0..5 {
            write_crash_log(dir.path(), at(second), "boom", "a", &"").unwrap();
        }
        std::fs::write(dir.path().join("steno.sqlite"), b"").unwrap();
        prune(dir.path(), 2);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "crash-2026-10-07T12-00-03.000Z.log",
                "crash-2026-10-07T12-00-04.000Z.log",
                "steno.sqlite"
            ]
        );
    }
}
