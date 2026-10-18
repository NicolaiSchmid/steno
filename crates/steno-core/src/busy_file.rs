//! A file another handle holds for a moment. On Windows a sync or antivirus
//! client may open a file without sharing, and a rename then fails until it
//! lets go. Every rename of a file Steno writes goes through [`retried`] (or
//! [`rename`]), which retries for about 0.9 s on Windows and runs once
//! elsewhere.

use std::io;
use std::path::Path;
use std::time::Duration;

/// How often [`retried`] tries a busy file again.
pub const RETRIES: u32 = if cfg!(windows) { 9 } else { 0 };

/// The first wait before a retry, doubling up to [`LONGEST_WAIT`].
pub const FIRST_WAIT: Duration = Duration::from_millis(5);

pub const LONGEST_WAIT: Duration = Duration::from_millis(200);

/// `ERROR_ACCESS_DENIED`.
const ACCESS_DENIED: i32 = 5;
/// `ERROR_SHARING_VIOLATION`.
const SHARING_VIOLATION: i32 = 32;
/// `ERROR_LOCK_VIOLATION`.
const LOCK_VIOLATION: i32 = 33;

/// Whether `error` is Windows refusing a file another handle holds: a sharing
/// violation, a lock violation, or "access denied" (a replace or open of a file
/// held or being deleted). Never elsewhere.
#[must_use]
pub fn is_busy(error: &io::Error) -> bool {
    cfg!(windows) && error.raw_os_error().is_some_and(names_a_busy_file)
}

fn names_a_busy_file(code: i32) -> bool {
    matches!(code, SHARING_VIOLATION | LOCK_VIOLATION | ACCESS_DENIED)
}

/// Runs `attempt`, retrying it on Windows while it fails with [`is_busy`].
pub fn retried<T>(attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retried_with(RETRIES, is_busy, std::thread::sleep, attempt)
}

/// `std::fs::rename` through [`retried`]. On a failure nothing was renamed.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    retried(|| std::fs::rename(from, to))
}

/// [`retried`] with its parts given, for tests and for callers whose waits go
/// through their own clock.
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

    /// Runs `retried_with` over an attempt that fails busy `failures` times;
    /// returns the outcome, the tries and the waits.
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

    /// A file busy every time is tried ten times in under a second.
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

    #[test]
    fn only_windows_retries() {
        assert_eq!(RETRIES, if cfg!(windows) { 9 } else { 0 });
    }

    #[test]
    fn only_the_codes_of_a_held_file_are_busy() {
        for busy in [5, 32, 33] {
            assert!(names_a_busy_file(busy), "{busy}");
        }
        for other in [0, 2, 3, 112, 183] {
            assert!(!names_a_busy_file(other), "{other}");
        }
        assert!(!is_busy(&io::Error::other("not from the OS")));
        assert_eq!(
            is_busy(&io::Error::from_raw_os_error(SHARING_VIOLATION)),
            cfg!(windows)
        );
    }

    /// A rename onto a target held without delete sharing waits and lands once the
    /// handle lets go.
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
