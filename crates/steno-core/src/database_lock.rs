//! One process per database. Two Steno processes on one database fail each
//! other's live recordings at launch (a `recording` row looks interrupted),
//! process the same meetings twice and run two retention sweeps. The
//! desktop app takes [`DatabaseLock`] before it opens the database and
//! holds it until it exits; a second app refuses to start, and the CLI's
//! commands that write refuse while the app holds it. Rust only: the Swift
//! app takes no lock and ships no further release; the shell refuses to
//! start beside it (it looks for its bundle id), but a Swift app started
//! after the Rust app is not kept out (the launch reconciliation leaves a
//! recording alone while its master is still growing, #233).
//!
//! The lock is an advisory, exclusive lock per database: on the file
//! beside it with the extension `lock` (`<support>/steno.lock` for the
//! default `steno.sqlite`), through [`std::fs::File::try_lock`]: `flock`
//! on macOS and Linux, `LockFileEx` on Windows. The operating system drops
//! it when the process ends however it ends, so a crash or a `kill -9`
//! never leaves a stale lock behind; the file itself stays and is reused.
//! A lock file this user cannot write (left by `sudo steno`) is opened
//! read-only and locked all the same: both calls lock a read handle. A
//! filesystem without locks answers [`DatabaseLockError::Unsupported`],
//! and the app then runs without the lock and says so in its log. Some
//! network filesystems accept the lock without enforcing it (`WebDAV` on the
//! Mac, measured), so there two processes can still both hold it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often [`DatabaseLock::acquire_within`] tries again while the lock
/// is held.
const RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// Why [`DatabaseLock::acquire`] did not take the lock.
#[derive(Debug, thiserror::Error)]
pub enum DatabaseLockError {
    /// Another process holds it: another Steno app, or a CLI command, runs
    /// on the same database. The payload is the lock file.
    #[error("Another Steno holds {}", .0.display())]
    Held(PathBuf),
    /// The filesystem does not lock files (`ENOLCK`, `ENOTSUP`, or the call
    /// is not supported), so no process can hold the database.
    #[error("{} cannot be locked on this filesystem: {source}", .path.display())]
    Unsupported {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The lock file could not be opened or locked.
    #[error("Could not lock {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// The exclusive lock on a database, held until it is dropped; see the
/// module doc.
///
/// ```
/// use steno_core::DatabaseLock;
///
/// let dir = tempfile::tempdir()?;
/// let database = dir.path().join("steno.sqlite");
/// let held = DatabaseLock::acquire(&database)?;
/// assert!(DatabaseLock::acquire(&database).is_err());
/// drop(held);
/// assert!(DatabaseLock::acquire(&database).is_ok());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct DatabaseLock {
    /// Holds the lock; closing it releases the lock.
    _file: File,
}

impl DatabaseLock {
    /// Where the lock of the database at `database` lives: beside it, with
    /// the extension `lock`.
    #[must_use]
    pub fn path_for(database: &Path) -> PathBuf {
        database.with_extension("lock")
    }

    /// Takes the lock of the database at `database` without waiting,
    /// creating the lock file (not its folder) when it is missing.
    /// `Held` when another process holds it. A second call in the same
    /// process is `Held` too: the lock belongs to the open file, not to the
    /// process.
    pub fn acquire(database: &Path) -> Result<Self, DatabaseLockError> {
        Self::acquire_within(database, Duration::ZERO)
    }

    /// [`DatabaseLock::acquire`], trying again every 100 ms while the lock
    /// is `Held` until `patience` has passed. For an app an update
    /// relaunches: the new process starts before the old one has exited.
    /// Any other error returns at once.
    pub fn acquire_within(database: &Path, patience: Duration) -> Result<Self, DatabaseLockError> {
        let path = Self::path_for(database);
        let file = open(&path).map_err(|source| DatabaseLockError::Io {
            path: path.clone(),
            source,
        })?;
        let deadline = Instant::now() + patience;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(DatabaseLock { _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(RETRY_INTERVAL);
                }
                Err(TryLockError::WouldBlock) => return Err(DatabaseLockError::Held(path)),
                Err(TryLockError::Error(source)) if source.kind() == ErrorKind::Interrupted => {}
                Err(TryLockError::Error(source)) if is_unsupported(&source) => {
                    return Err(DatabaseLockError::Unsupported { path, source });
                }
                Err(TryLockError::Error(source)) => {
                    return Err(DatabaseLockError::Io { path, source });
                }
            }
        }
    }
}

/// The lock file for writing, created when missing; read-only when this
/// user may not write it.
fn open(path: &Path) -> std::io::Result<File> {
    match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
    {
        Err(error) if error.kind() == ErrorKind::PermissionDenied => File::open(path),
        other => other,
    }
}

/// Whether the filesystem, not another process, refused the lock.
/// `ENOTSUP` goes by number: on the Mac it is not `EOPNOTSUPP` and the
/// standard library gives it no kind (on Linux the two are one number).
fn is_unsupported(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    if matches!(error.raw_os_error(), Some(libc::ENOLCK | libc::ENOTSUP)) {
        return true;
    }
    error.kind() == ErrorKind::Unsupported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_sits_beside_the_database() {
        assert_eq!(
            DatabaseLock::path_for(Path::new("/support/Steno/steno.sqlite")),
            Path::new("/support/Steno/steno.lock")
        );
        assert_eq!(
            DatabaseLock::path_for(Path::new("/support/Steno/scratch.sqlite")),
            Path::new("/support/Steno/scratch.lock")
        );
    }

    #[test]
    fn a_second_lock_on_one_database_is_refused_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("steno.sqlite");
        let first = DatabaseLock::acquire(&database).unwrap();
        match DatabaseLock::acquire(&database) {
            Err(DatabaseLockError::Held(path)) => assert_eq!(path, dir.path().join("steno.lock")),
            other => panic!("expected Held, got {other:?}"),
        }
        drop(first);
        DatabaseLock::acquire(&database).unwrap();
    }

    #[test]
    fn two_databases_in_one_folder_lock_independently() {
        let dir = tempfile::tempdir().unwrap();
        let _first = DatabaseLock::acquire(&dir.path().join("steno.sqlite")).unwrap();
        DatabaseLock::acquire(&dir.path().join("scratch.sqlite")).unwrap();
    }

    #[test]
    fn databases_in_two_folders_lock_independently() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let _first = DatabaseLock::acquire(&a.path().join("steno.sqlite")).unwrap();
        DatabaseLock::acquire(&b.path().join("steno.sqlite")).unwrap();
    }

    #[test]
    fn a_missing_folder_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("missing").join("steno.sqlite");
        assert!(matches!(
            DatabaseLock::acquire(&database),
            Err(DatabaseLockError::Io { .. })
        ));
    }

    /// The relaunch after an update: the old process lets go 300 ms after
    /// the new one started waiting, well inside the patience.
    #[test]
    fn a_lock_released_within_the_patience_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("steno.sqlite");
        let old = DatabaseLock::acquire(&database).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(old);
        });
        DatabaseLock::acquire_within(&database, Duration::from_secs(5)).unwrap();
        release.join().unwrap();
    }

    #[test]
    fn a_lock_never_released_is_refused_after_the_patience() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("steno.sqlite");
        let _held = DatabaseLock::acquire(&database).unwrap();
        let patience = Duration::from_millis(300);
        let started = Instant::now();
        assert!(matches!(
            DatabaseLock::acquire_within(&database, patience),
            Err(DatabaseLockError::Held(_))
        ));
        assert!(started.elapsed() >= patience, "{:?}", started.elapsed());
    }

    /// A lock file this user may not write (`sudo steno` left it owned by
    /// root) is locked through a read-only handle. Root writes it anyway,
    /// so there the test passes on the first open.
    #[cfg(unix)]
    #[test]
    fn a_lock_file_this_user_cannot_write_is_locked_read_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("steno.sqlite");
        let path = DatabaseLock::path_for(&database);
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let first = DatabaseLock::acquire(&database).unwrap();
        assert!(matches!(
            DatabaseLock::acquire(&database),
            Err(DatabaseLockError::Held(_))
        ));
        drop(first);
        DatabaseLock::acquire(&database).unwrap();
    }

    #[test]
    fn only_the_filesystem_refusing_the_call_is_unsupported() {
        assert!(is_unsupported(&std::io::Error::from(
            ErrorKind::Unsupported
        )));
        #[cfg(unix)]
        for code in [libc::ENOLCK, libc::ENOTSUP] {
            assert!(is_unsupported(&std::io::Error::from_raw_os_error(code)));
        }
        assert!(!is_unsupported(&std::io::Error::from(
            ErrorKind::PermissionDenied
        )));
    }
}
