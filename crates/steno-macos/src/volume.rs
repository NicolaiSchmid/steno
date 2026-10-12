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
//! Every call is safe in `objc2-foundation` but for the two resource key
//! constants, which are `extern` statics.
#![allow(unsafe_code)]

use std::io;
use std::path::Path;

use objc2_foundation::{
    NSArray, NSError, NSFileNoSuchFileError, NSFileReadNoSuchFileError, NSNumber, NSURL,
    NSURLResourceKey, NSURLVolumeAvailableCapacityForImportantUsageKey,
    NSURLVolumeTotalCapacityKey,
};

/// What the volume that holds a path reports, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    /// Free for important use
    /// (`NSURLVolumeAvailableCapacityForImportantUsageKey`), the space
    /// macOS would purge for it included; 0 on a volume `CacheDelete` does
    /// not manage, whatever is free there (see the module doc).
    pub available_for_important_usage: u64,
    /// The whole volume (`NSURLVolumeTotalCapacityKey`).
    pub total: u64,
}

/// The capacity of the volume that holds `path`, which must exist. A path
/// that does not, or a volume that reports either value not at all or as
/// something other than a count of bytes, is an error. An important-use
/// reading of 0 is a value the volume reports, not an error: a caller
/// reads `statvfs` beside it (see the module doc).
pub fn capacity(path: &Path) -> io::Result<Capacity> {
    let url = NSURL::from_file_path(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file URL", path.display()),
        )
    })?;
    // SAFETY: both are Foundation's constant `NSString`s, defined in the
    // framework's image (the important-usage key since macOS 10.13; Steno
    // needs 15), so they are initialised before any Rust code runs and
    // live, unchanged, for the whole process. Reading them copies a
    // `&'static` reference and writes nothing.
    let (available_key, total_key) = unsafe {
        (
            NSURLVolumeAvailableCapacityForImportantUsageKey,
            NSURLVolumeTotalCapacityKey,
        )
    };
    let values = url
        .resourceValuesForKeys_error(&NSArray::from_slice(&[available_key, total_key]))
        .map_err(|error| ns_error(path, &error))?;
    let bytes = |key: &NSURLResourceKey| -> io::Result<u64> {
        let number = values
            .objectForKey(key)
            .and_then(|value| value.downcast::<NSNumber>().ok())
            .ok_or_else(|| io::Error::other(format!("the volume reports no {key}")))?;
        u64::try_from(number.longLongValue())
            .map_err(|_| io::Error::other(format!("the volume reports a negative {key}")))
    };
    Ok(Capacity {
        available_for_important_usage: bytes(available_key)?,
        total: bytes(total_key)?,
    })
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
    use super::capacity;

    /// The volume of a temporary folder reports some space free, no more
    /// than it holds.
    #[test]
    fn a_folder_s_volume_reports_its_free_space_within_its_size() {
        let folder = tempfile::tempdir().unwrap();
        let capacity = capacity(folder.path()).unwrap();
        assert!(capacity.available_for_important_usage > 0, "{capacity:?}");
        assert!(
            capacity.available_for_important_usage <= capacity.total,
            "{capacity:?}"
        );
    }

    /// A path that does not exist is an error, not an empty volume.
    #[test]
    fn a_missing_path_is_an_error() {
        let folder = tempfile::tempdir().unwrap();
        let error = capacity(&folder.path().join("missing")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
    }
}
