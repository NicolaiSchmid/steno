//! One process per database. Two Steno processes on one database fail each
//! other's live recordings at launch (a `recording` row looks interrupted),
//! process the same meetings twice and run two retention sweeps. The
//! desktop app takes [`DatabaseLock`] before it opens the database and
//! holds it until it exits; a second app refuses to start, and the CLI's
//! commands that write refuse while the app holds it. Rust only: the Swift
//! app takes no lock and ships no further release, so a Swift app beside
//! the Rust app on one Mac is not kept out (the launch reconciliation
//! leaves a recording alone while its master is still growing).
//!
//! The lock is an advisory, exclusive lock on `steno.lock` beside the
//! database (`<support>/steno.lock` for the default database): `flock` on
//! macOS and Linux, `LockFileEx` on Windows, through
//! [`std::fs::File::try_lock`]. The operating system drops it when the
//! process ends however it ends, so a crash or a `kill -9` never leaves a
//! stale lock behind; the file itself stays and is reused.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

/// Why [`DatabaseLock::acquire`] did not take the lock.
#[derive(Debug, thiserror::Error)]
pub enum DatabaseLockError {
    /// Another process holds it: another Steno app, or a CLI command, runs
    /// on the same database.
    #[error("Another Steno is already using {}", .0.display())]
    Held(PathBuf),
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
    path: PathBuf,
    /// Holds the lock; closing it releases the lock.
    _file: File,
}

impl DatabaseLock {
    /// The lock file's name, beside the database.
    pub const FILE_NAME: &'static str = "steno.lock";

    /// Where the lock of the database at `database` lives.
    #[must_use]
    pub fn path_for(database: &Path) -> PathBuf {
        database
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(Self::FILE_NAME)
    }

    /// Takes the lock of the database at `database` without waiting,
    /// creating the lock file (not its folder) when it is missing.
    /// `Held` when another process holds it. A second call in the same
    /// process is `Held` too: the lock belongs to the open file, not to the
    /// process.
    pub fn acquire(database: &Path) -> Result<Self, DatabaseLockError> {
        let path = Self::path_for(database);
        let io = |source| DatabaseLockError::Io {
            path: path.clone(),
            source,
        };
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io)?;
        match file.try_lock() {
            Ok(()) => Ok(DatabaseLock { path, _file: file }),
            Err(TryLockError::WouldBlock) => Err(DatabaseLockError::Held(path)),
            Err(TryLockError::Error(source)) => Err(io(source)),
        }
    }

    /// The lock file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
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
    }

    #[test]
    fn a_second_lock_on_one_database_is_refused_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("steno.sqlite");
        let first = DatabaseLock::acquire(&database).unwrap();
        assert_eq!(first.path(), dir.path().join("steno.lock"));
        match DatabaseLock::acquire(&database) {
            Err(DatabaseLockError::Held(path)) => assert_eq!(path, first.path()),
            other => panic!("expected Held, got {other:?}"),
        }
        drop(first);
        DatabaseLock::acquire(&database).unwrap();
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
}
