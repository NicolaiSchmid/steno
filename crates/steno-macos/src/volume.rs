//! A volume's free space for important use, the space APFS frees on demand
//! included, which `statvfs` leaves out. It comes from `CacheDelete`, which
//! manages only some volumes: on the others (an external drive, `/nix`) it
//! is 0 however much is free, so callers read `statvfs` beside it.
#![allow(unsafe_code)]

use std::io;
use std::path::Path;

use objc2_foundation::{
    NSArray, NSError, NSFileNoSuchFileError, NSFileReadNoSuchFileError, NSNumber, NSURL,
    NSURLResourceKey, NSURLVolumeAvailableCapacityForImportantUsageKey,
    NSURLVolumeTotalCapacityKey,
};

/// The bytes free for important use on the volume that holds `path`, which
/// must exist; 0 on a volume `CacheDelete` does not manage. A missing path
/// or a volume with no byte count is an error.
pub fn available_for_important_usage(path: &Path) -> io::Result<u64> {
    // SAFETY: Foundation's constant `NSString`, defined in the framework's
    // image (since macOS 10.13; Steno needs 15), so it is initialised
    // before any Rust code runs and lives, unchanged, for the whole
    // process. Reading it copies a `&'static` reference and writes nothing.
    let key = unsafe { NSURLVolumeAvailableCapacityForImportantUsageKey };
    read(path, key)
}

/// The size of the volume that holds `path`, with the errors of
/// [`available_for_important_usage`].
pub fn total_capacity(path: &Path) -> io::Result<u64> {
    // SAFETY: as in `available_for_important_usage`, a constant `NSString`
    // of Foundation's image.
    let key = unsafe { NSURLVolumeTotalCapacityKey };
    read(path, key)
}

/// The byte count `key` reports for the volume of `path`, on a fresh
/// `NSURL`: a URL caches its resource values, so a reused one goes stale.
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

/// `error` as an I/O error: `NotFound` for Foundation's no-such-file codes.
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

    /// A temporary folder's volume reports some free space, within its size.
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
