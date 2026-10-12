//! A volume's free space as macOS reports it for important use: what a
//! file the user asked for can take, the space APFS frees on demand
//! (local Time Machine snapshots, purgeable caches) included. `statvfs`
//! leaves that space out, tens of GB on a Mac with local snapshots, so
//! the recorder's disk floor reads this beside `statvfs`, the larger
//! counting. The value comes from `CacheDelete`, which manages only some
//! volumes; on the others (an external drive, a disk image, `/nix`) it is
//! 0 however much is free, a real answer, not a full disk. Rust only: the
//! Swift app had no disk check.
//!
//! Every call is safe in `objc2-foundation` but for the resource key
//! constants, which are `extern` statics.
#![allow(unsafe_code)]

use std::io;
use std::path::Path;

use objc2_foundation::{
    NSArray, NSError, NSFileNoSuchFileError, NSFileReadNoSuchFileError, NSNumber, NSURL,
    NSURLResourceKey, NSURLVolumeAvailableCapacityForImportantUsageKey,
    NSURLVolumeTotalCapacityKey,
};

/// The bytes free for important use on the volume that holds `path`,
/// which must exist (`NSURLVolumeAvailableCapacityForImportantUsageKey`):
/// the space macOS would purge for it included; 0 on a volume
/// `CacheDelete` does not manage, whatever is free there, a value the
/// volume reports and not an error: a caller reads `statvfs` beside it
/// (see the module doc). A path that does not exist, or a volume that
/// reports no value or something other than a count of bytes, is an
/// error. Only this key is asked for, so no other value can fail the
/// read.
pub fn available_for_important_usage(path: &Path) -> io::Result<u64> {
    // SAFETY: Foundation's constant `NSString`, defined in the framework's
    // image (since macOS 10.13; Steno needs 15), so it is initialised
    // before any Rust code runs and lives, unchanged, for the whole
    // process. Reading it copies a `&'static` reference and writes nothing.
    let key = unsafe { NSURLVolumeAvailableCapacityForImportantUsageKey };
    read(path, key)
}

/// The size of the volume that holds `path`, which must exist
/// (`NSURLVolumeTotalCapacityKey`), with the errors of
/// [`available_for_important_usage`]. The disk floor does not read it;
/// tests and probes check the important-use value against it.
pub fn total_capacity(path: &Path) -> io::Result<u64> {
    // SAFETY: as in `available_for_important_usage`, a constant `NSString`
    // of Foundation's image.
    let key = unsafe { NSURLVolumeTotalCapacityKey };
    read(path, key)
}

/// The byte count `key` reports for the volume of `path`, on an `NSURL`
/// made for this read. Never reuse one: a URL caches its resource values,
/// so a second read on it answers the first one's (on a Mac it stayed the
/// same while the free space moved 2.7 GB).
fn read(path: &Path, key: &NSURLResourceKey) -> io::Result<u64> {
    let url = NSURL::from_file_path(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file URL", path.display()),
        )
    })?;
    let values = url
        .resourceValuesForKeys_error(&NSArray::from_slice(&[key]))
        .map_err(|error| ns_error(path, &error))?;
    let number = values
        .objectForKey(key)
        .and_then(|value| value.downcast::<NSNumber>().ok())
        .ok_or_else(|| io::Error::other(format!("the volume reports no {key}")))?;
    u64::try_from(number.longLongValue())
        .map_err(|_| io::Error::other(format!("the volume reports a negative {key}")))
}

/// `error` from reading the resource values of `path`, as an I/O error:
/// `NotFound` for Foundation's no-such-file codes.
fn ns_error(path: &Path, error: &NSError) -> io::Error {
    let kind = if [NSFileNoSuchFileError, NSFileReadNoSuchFileError].contains(&error.code()) {
        io::ErrorKind::NotFound
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(
        kind,
        format!(
            "the volume of {} could not be read: {}",
            path.display(),
            error.localizedDescription()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{available_for_important_usage, total_capacity};

    /// The volume of a temporary folder reports some space free for
    /// important use, no more than it holds.
    #[test]
    fn a_folder_s_volume_reports_its_free_space_within_its_size() {
        let folder = tempfile::tempdir().unwrap();
        let available = available_for_important_usage(folder.path()).unwrap();
        let total = total_capacity(folder.path()).unwrap();
        assert!(available > 0, "{available} of {total}");
        assert!(available <= total, "{available} of {total}");
    }

    /// A path that does not exist is an error, not an empty volume.
    #[test]
    fn a_missing_path_is_an_error() {
        let folder = tempfile::tempdir().unwrap();
        let missing = folder.path().join("missing");
        for error in [
            available_for_important_usage(&missing).unwrap_err(),
            total_capacity(&missing).unwrap_err(),
        ] {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
        }
    }
}
