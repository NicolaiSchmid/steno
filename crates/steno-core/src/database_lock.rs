//! One process per database: two Steno processes on one database would fail
//! each other's recordings and process meetings twice. The lock is an advisory
//! exclusive lock on the `lock` file beside the database, which the OS drops
//! when the process ends. A read-only lock file is locked all the same; a
//! filesystem without locks answers [`DatabaseLockError::Unsupported`].

use std::fs::{File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// Why [`DatabaseLock::acquire`] did not take the lock.
#[derive(Debug, thiserror::Error)]
pub enum DatabaseLockError {
    /// Another process holds it. The payload is the lock file.
    #[error("Another Steno holds {}", .0.display())]
    Held(PathBuf),
    /// The filesystem does not lock files.
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

/// The exclusive lock on a database, held until it is dropped.
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
    _file: File,
}

impl DatabaseLock {
    /// The lock file beside `database`, with the extension `lock`.
    #[must_use]
    pub fn path_for(database: &Path) -> PathBuf {
        database.with_extension("lock")
    }

    /// Takes the lock without waiting, creating the lock file when missing. A
    /// second call in the same process is `Held` too: the lock belongs to the open
    /// file.
    pub fn acquire(database: &Path) -> Result<Self, DatabaseLockError> {
        Self::acquire_within(database, Duration::ZERO)
    }

    /// [`DatabaseLock::acquire`], retrying while `Held` until `patience` has
    /// passed, for an app an update relaunches before the old one exited.
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

/// The lock file for writing, created when missing; read-only when this user
/// may not write it.
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

/// Whether the filesystem, not another process, refused the lock. `ENOTSUP`
/// goes by number because the standard library gives it no kind on the Mac.
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

    /// The old process lets go 300 ms into the new one's wait.
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

    /// A lock file this user may not write is locked through a read-only handle.
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
