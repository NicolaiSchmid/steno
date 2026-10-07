//! The one sync every recording file goes through, the writer thread's
//! periodic one and both closes: the full sync first and, on the Mac, a
//! plain `fsync` when the filesystem refuses it. `File::sync_data` and
//! `File::sync_all` are `F_FULLFSYNC` there, which a network share can
//! refuse (a WebDAV mount answers ENOTTY, an uncategorized error kind)
//! while `fsync` succeeds; SQLite falls back the same way, on any error.
//! The fallback retries an interrupted `fsync`. Only a failure of both is
//! an error, and it is `fsync`'s. Elsewhere there is nothing to fall back
//! to: `fdatasync`, `fsync` and `FlushFileBuffers` are the plain syncs, and
//! a second try after a failed one proves nothing. Swift: `finish` in
//! `Sources/StenoAudio/Writer/CAFFile.swift` and
//! `Sources/StenoAudio/Writer/WAVStreamWriter.swift`, whose
//! `FileHandle.synchronize` is a plain `fsync` alone.

use std::fs::File;
use std::io;

/// A sync of an open file.
type SyncFn = fn(&File) -> io::Result<()>;

/// What [`sync`] tries when the full sync fails.
#[cfg(target_os = "macos")]
const FALLBACK: Option<SyncFn> =
    Some(|file| Ok(rustix::io::retry_on_intr(|| rustix::fs::fsync(file))?));
#[cfg(not(target_os = "macos"))]
const FALLBACK: Option<SyncFn> = None;

/// Makes `file` durable with `full` (`File::sync_data` or
/// `File::sync_all`), falling back as the module doc says.
pub(crate) fn sync(file: &File, full: SyncFn) -> io::Result<()> {
    sync_with(file, full, FALLBACK)
}

/// [`sync`] over the two syncs it is given, so a test can refuse either.
fn sync_with(file: &File, full: SyncFn, fallback: Option<SyncFn>) -> io::Result<()> {
    full(file).or_else(|error| fallback.map_or(Err(error), |fallback| fallback(file)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a WebDAV mount on the Mac answers `F_FULLFSYNC`.
    fn refused(_: &File) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(25))
    }

    /// A sync that succeeds.
    const SYNCED: SyncFn = |_| Ok(());

    fn failed(_: &File) -> io::Result<()> {
        Err(io::ErrorKind::StorageFull.into())
    }

    fn untouched(_: &File) -> io::Result<()> {
        panic!("the fallback ran after a full sync that succeeded")
    }

    fn file() -> File {
        tempfile::tempfile().unwrap()
    }

    #[test]
    fn a_full_sync_that_succeeds_is_all_it_takes() {
        sync_with(&file(), SYNCED, Some(untouched)).unwrap();
    }

    #[test]
    fn a_refused_full_sync_falls_back_and_succeeds() {
        sync_with(&file(), refused, Some(SYNCED)).unwrap();
    }

    /// When both fail the error is the fallback's, the one that says
    /// whether the data is on disk.
    #[test]
    fn a_failed_fallback_is_the_error() {
        let error = sync_with(&file(), refused, Some(failed)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    }

    /// With no fallback (Linux, Windows) the full sync's error stands.
    #[test]
    fn without_a_fallback_the_full_syncs_error_stands() {
        let error = sync_with(&file(), refused, None).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(25));
    }

    /// The platform's order through [`sync`] itself: the full sync runs
    /// once, and the fallback only after it fails. On the Mac the refused
    /// full sync falls back to `fsync`, which succeeds on a temporary file;
    /// elsewhere the refusal stands.
    #[test]
    fn the_full_sync_runs_once_and_the_fallback_only_after_it_fails() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SUCCEEDED: AtomicUsize = AtomicUsize::new(0);
        static REFUSED: AtomicUsize = AtomicUsize::new(0);

        sync(&file(), |_| {
            SUCCEEDED.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .unwrap();
        assert_eq!(SUCCEEDED.load(Ordering::Relaxed), 1);

        let result = sync(&file(), |file| {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            refused(file)
        });
        assert_eq!(REFUSED.load(Ordering::Relaxed), 1);
        if cfg!(target_os = "macos") {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().raw_os_error(), Some(25));
        }
    }

    /// The syncs and the fallback this platform has, on a real file: on
    /// the Mac this passes with the temporary directory on a WebDAV mount.
    #[test]
    fn the_platforms_sync_works_on_a_temporary_file() {
        sync(&file(), File::sync_all).unwrap();
    }
}
