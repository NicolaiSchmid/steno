//! A file another handle holds for a moment. On Windows a sync or
//! antivirus client, a media player or another writer may open a file
//! without sharing the access a rename or an open asks for, and Windows
//! then refuses the call until that handle lets go, usually within
//! milliseconds. Every rename and reopen of a file Steno writes goes
//! through [`retried`] (or [`rename`]), which tries such a call again for
//! about 0.9 s before it reports the error; elsewhere, where no handle
//! blocks a rename, the call runs once. `steno_pipeline::files`' durable
//! writes, the vault writer, the handover inbox, the Codex sign-in file,
//! the model downloads and the speaker clips share it, so their retries
//! cannot drift apart. Rust only: the Swift app runs on macOS alone.

use std::io;
use std::path::Path;
use std::time::Duration;

/// How often [`retried`] tries a busy file again: nine times on Windows,
/// never elsewhere.
pub const RETRIES: u32 = if cfg!(windows) { 9 } else { 0 };

/// The first wait before a busy file is tried again, which doubles with
/// each retry up to [`LONGEST_WAIT`]: about 0.9 s over nine retries.
pub const FIRST_WAIT: Duration = Duration::from_millis(5);

/// The longest wait between two tries of a busy file.
pub const LONGEST_WAIT: Duration = Duration::from_millis(200);

/// `ERROR_ACCESS_DENIED`.
const ACCESS_DENIED: i32 = 5;
/// `ERROR_SHARING_VIOLATION`.
const SHARING_VIOLATION: i32 = 32;
/// `ERROR_LOCK_VIOLATION`.
const LOCK_VIOLATION: i32 = 33;

/// Whether `error` is Windows refusing a file another handle holds, usually
/// for a moment: a sharing violation (a sync or antivirus client opened it
/// without sharing the access asked for), a lock violation (it locked a
/// range of it), or "access denied" (a replace of a file such a handle
/// holds open, or an open of a file being deleted or replaced that
/// instant). Never elsewhere, where nothing is retried.
#[must_use]
pub fn is_busy(error: &io::Error) -> bool {
    cfg!(windows) && error.raw_os_error().is_some_and(names_a_busy_file)
}

/// Whether the Win32 error `code` is one of [`is_busy`]'s.
fn names_a_busy_file(code: i32) -> bool {
    matches!(code, SHARING_VIOLATION | LOCK_VIOLATION | ACCESS_DENIED)
}

/// Runs `attempt`, and on Windows tries it again while it fails with a busy
/// file ([`is_busy`]): [`RETRIES`] times, after waits of 5 ms doubling to
/// 200 ms. Any other error, or a success, ends the tries.
pub fn retried<T>(attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retried_with(RETRIES, is_busy, std::thread::sleep, attempt)
}

/// `std::fs::rename`, tried again on Windows while a file is busy
/// ([`retried`]). On a failure nothing was renamed.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    retried(|| std::fs::rename(from, to))
}

/// [`retried`] with its parts given: `attempt` runs, and an error `busy`
/// accepts is tried again up to `retries` times, after `wait`s of
/// [`FIRST_WAIT`] doubling to [`LONGEST_WAIT`]. The tests give a predicate
/// and a clock of their own; `steno_pipeline::files` waits through its
/// recorded syncs.
pub fn retried_with<T>(
    retries: u32,
    busy: impl Fn(&io::Error) -> bool,
    mut wait: impl FnMut(Duration),
    mut attempt: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut delay = FIRST_WAIT;
    for _ in 0..retries {
        match attempt() {
            Err(error) if busy(&error) => {
                wait(delay);
                delay = (delay * 2).min(LONGEST_WAIT);
            }
            outcome => return outcome,
        }
    }
    attempt()
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// Runs `retried_with` over an attempt that fails busy `failures`
    /// times and then succeeds; returns the outcome, the tries and the
    /// waits.
    fn run(retries: u32, failures: u32) -> (io::Result<u32>, u32, Vec<Duration>) {
        let tries = Cell::new(0);
        let waits = RefCell::new(Vec::new());
        let outcome = retried_with(
            retries,
            |error| error.kind() == io::ErrorKind::ResourceBusy,
            |delay| waits.borrow_mut().push(delay),
            || {
                tries.set(tries.get() + 1);
                if tries.get() <= failures {
                    Err(io::Error::from(io::ErrorKind::ResourceBusy))
                } else {
                    Ok(tries.get())
                }
            },
        );
        (outcome, tries.get(), waits.into_inner())
    }

    fn millis(waits: &[u64]) -> Vec<Duration> {
        waits.iter().copied().map(Duration::from_millis).collect()
    }

    /// A file busy every time is tried ten times, after waits of 5 ms
    /// doubling to 200 ms, under a second in all, and the last error is
    /// returned.
    #[test]
    fn a_file_busy_every_time_is_tried_ten_times_within_a_second() {
        let (outcome, tries, waits) = run(9, u32::MAX);
        assert_eq!(
            outcome.unwrap_err().kind(),
            io::ErrorKind::ResourceBusy,
            "the last error"
        );
        assert_eq!(tries, 10);
        assert_eq!(waits, millis(&[5, 10, 20, 40, 80, 160, 200, 200, 200]));
        assert!(waits.iter().sum::<Duration>() < Duration::from_secs(1));
    }

    /// A file that lets go after two busy tries is renamed on the third,
    /// and nothing waits after it.
    #[test]
    fn a_file_that_lets_go_succeeds_on_the_next_try() {
        let (outcome, tries, waits) = run(9, 2);
        assert_eq!(outcome.unwrap(), 3);
        assert_eq!(tries, 3);
        assert_eq!(waits, millis(&[5, 10]));
    }

    /// An error the predicate does not accept ends the tries at once.
    #[test]
    fn another_error_is_not_tried_again() {
        let tries = Cell::new(0);
        let outcome: io::Result<()> = retried_with(
            9,
            |_| false,
            |_| panic!("no wait"),
            || {
                tries.set(tries.get() + 1);
                Err(io::Error::from(io::ErrorKind::NotFound))
            },
        );
        assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(tries.get(), 1);
    }

    /// Without retries, as off Windows, the attempt runs once.
    #[test]
    fn without_retries_the_attempt_runs_once() {
        let (outcome, tries, waits) = run(0, u32::MAX);
        assert!(outcome.is_err());
        assert_eq!(tries, 1);
        assert_eq!(waits, Vec::<Duration>::new());
    }

    /// The product tries a busy file nine more times on Windows and never
    /// elsewhere.
    #[test]
    fn only_windows_retries() {
        assert_eq!(RETRIES, if cfg!(windows) { 9 } else { 0 });
    }

    /// Sharing, lock and access violations name a busy file; a missing
    /// file (2), a full disk (112) and any other code do not.
    #[test]
    fn only_the_codes_of_a_held_file_are_busy() {
        for busy in [5, 32, 33] {
            assert!(names_a_busy_file(busy), "{busy}");
        }
        for other in [0, 2, 3, 112, 183] {
            assert!(!names_a_busy_file(other), "{other}");
        }
        assert!(!is_busy(&io::Error::other("not from the OS")));
        // Elsewhere the same numbers are other errors (`EIO`, `EPIPE`,
        // `EDOM` on Linux) and never busy.
        assert_eq!(
            is_busy(&io::Error::from_raw_os_error(SHARING_VIOLATION)),
            cfg!(windows)
        );
    }

    /// On Windows a target another handle holds without sharing its
    /// deletion refuses the rename with an error [`is_busy`] accepts; the
    /// rename waits and tries again, and lands once the handle lets go
    /// (here at the first wait).
    #[cfg(windows)]
    #[test]
    fn a_held_target_is_renamed_onto_once_it_lets_go() {
        use std::os::windows::fs::OpenOptionsExt as _;
        /// `FILE_SHARE_READ | FILE_SHARE_WRITE`, without `FILE_SHARE_DELETE`.
        const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("clip.wav.partial");
        let to = dir.path().join("clip.wav");
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old").unwrap();
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ_WRITE)
            .open(&to)
            .unwrap();
        let refused = std::fs::rename(&from, &to).unwrap_err();
        assert!(is_busy(&refused), "{refused:?} is busy");
        let mut holder = Some(holder);
        let mut waits = Vec::new();
        retried_with(
            RETRIES,
            is_busy,
            |delay| {
                waits.push(delay);
                drop(holder.take());
            },
            || std::fs::rename(&from, &to),
        )
        .unwrap();
        assert_eq!(waits, [FIRST_WAIT]);
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
    }
}
