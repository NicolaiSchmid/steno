//! A file for every panic, since an app opened from the Finder or at login has
//! no stderr. [`install_crash_log_hook`] writes `crash-<UTC time>.log` with the
//! message, location, thread and backtrace, keeps the newest
//! [`KEPT_CRASH_LOGS`], then runs the hook it replaced. A panic inside
//! [`expected`] writes nothing. The file stays on the computer.

use std::cell::Cell;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

/// How many crash logs the folder keeps; the oldest beyond go.
pub const KEPT_CRASH_LOGS: usize = 20;

const PREFIX: &str = "crash-";

/// The variable naming the folder a child process (the speech sidecar) writes
/// its crash logs into.
pub const DIRECTORY_VARIABLE: &str = "STENO_CRASH_LOG_DIRECTORY";

thread_local! {
    static EXPECTED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `body` with its panics marked as expected on this thread: the caller
/// catches and reports them, so the hooks leave them alone.
///
/// ```
/// let caught = std::panic::catch_unwind(|| {
///     steno_core::crash_log::expected(|| panic!("a corrupt packet"))
/// });
/// assert!(caught.is_err());
/// assert!(!steno_core::crash_log::is_expected());
/// ```
pub fn expected<R>(body: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            EXPECTED.with(|expected| expected.set(self.0));
        }
    }
    let _restore = Restore(EXPECTED.with(|expected| expected.replace(true)));
    body()
}

/// Whether a panic raised now on this thread is inside [`expected`].
#[must_use]
pub fn is_expected() -> bool {
    EXPECTED.with(Cell::get)
}

/// Installs the panic hook over the current one. `process` names a child
/// process in the file name (`Some("sidecar")`).
///
/// ```no_run
/// steno_core::crash_log::install_crash_log_hook(
///     steno_core::StenoPaths::default_support_directory(),
///     None,
/// );
/// ```
pub fn install_crash_log_hook(folder: PathBuf, process: Option<&'static str>) {
    prune(&folder, KEPT_CRASH_LOGS);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        if is_expected() {
            return;
        }
        let location = panic
            .location()
            .map_or_else(|| "an unknown place".to_owned(), ToString::to_string);
        let message = panic_message(panic.payload());
        let backtrace = std::backtrace::Backtrace::force_capture();
        // A failed write must not panic inside the panic hook.
        let written = write_crash_log(
            &folder,
            process,
            Utc::now(),
            &message,
            &location,
            &backtrace,
        );
        if let Ok(path) = written {
            prune(&folder, KEPT_CRASH_LOGS);
            // Not `eprintln!`, which panics once stderr is gone.
            let _ = writeln!(
                std::io::stderr(),
                "steno: the crash is described in {}",
                path.display()
            );
        }
        previous(panic);
    }));
}

/// The text a panic was raised with, or "a panic without a message".
#[must_use]
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "a panic without a message".to_owned()
    }
}

/// Writes one crash log into `folder` and returns its path; a second panic in
/// the same millisecond gets a numbered name.
fn write_crash_log(
    folder: &Path,
    process: Option<&str>,
    at: DateTime<Utc>,
    message: &str,
    location: &str,
    backtrace: &dyn std::fmt::Display,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let stamp = at.format("%Y-%m-%dT%H-%M-%S%.3fZ");
    let suffix = process
        .map(|process| format!("-{process}"))
        .unwrap_or_default();
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
            format!("{PREFIX}{stamp}{suffix}.log")
        } else {
            format!("{PREFIX}{stamp}{suffix}-{attempt}.log")
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

/// Deletes the oldest crash logs beyond the newest `keep`; anything that cannot
/// be read or deleted stays.
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
            None,
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
        let first = write_crash_log(dir.path(), None, at(5), "one", "a", &"").unwrap();
        let second = write_crash_log(dir.path(), None, at(5), "two", "a", &"").unwrap();
        assert_ne!(first, second);
        assert!(std::fs::read_to_string(second).unwrap().contains("two"));
    }

    #[test]
    fn only_the_newest_crash_logs_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        for second in 0..4 {
            write_crash_log(dir.path(), None, at(second), "boom", "a", &"").unwrap();
        }
        write_crash_log(dir.path(), Some("sidecar"), at(4), "boom", "a", &"").unwrap();
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
                "crash-2026-10-07T12-00-04.000Z-sidecar.log",
                "steno.sqlite"
            ]
        );
    }
}
