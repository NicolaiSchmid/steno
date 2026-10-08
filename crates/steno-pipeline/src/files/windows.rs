//! Renames that are on the disk once they return, on Windows. The other
//! platforms sync the folder after a rename; Windows cannot sync a folder,
//! and `std::fs::rename` calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`
//! alone, which may return while the rename is still only in the cache (and
//! `tempfile`'s `persist` the same). [`rename_written_through`] adds
//! `MOVEFILE_WRITE_THROUGH`, with which `MoveFileExW` returns only once the
//! rename is on the disk. Where that call fails (a target another handle
//! holds open, which only std's fallback to POSIX rename semantics replaces,
//! or a path longer than `MAX_PATH`, which std makes verbatim), the caller
//! renames with std and then [`flush`]es the renamed file: on NTFS a file's
//! flush commits the volume's journal, which holds the rename. NTFS writes
//! its journal in order, so either way the folders created before the rename
//! (`create_dir_all_durably`) are on the disk with it.

use std::fs::OpenOptions;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

/// Renames `from` onto `to`, replacing a file there, and returns once the
/// rename is on the disk (`MOVEFILE_WRITE_THROUGH`).
pub(super) fn rename_written_through(from: &Path, to: &Path) -> io::Result<()> {
    let from = wide(from)?;
    let to = wide(to)?;
    // SAFETY: `from` and `to` are NUL-terminated UTF-16 strings without an
    // interior NUL (`wide`), owned by this frame and alive until the call
    // returns; `MoveFileExW` only reads them and keeps no pointer to them
    // after it returns. The flags are two the function documents, and it
    // touches no memory of ours besides the two strings.
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Flushes the file at `path` to the disk (`FlushFileBuffers`), after a
/// rename that was not written through.
pub(super) fn flush(path: &Path) -> io::Result<()> {
    OpenOptions::new().write(true).open(path)?.sync_all()
}

/// `path` as the NUL-terminated UTF-16 string the Win32 calls take.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} holds a NUL", path.display()),
        ));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The rename moves the bytes to a new name and leaves nothing at the
    /// old one.
    #[test]
    fn a_written_through_rename_moves_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let from = directory.path().join(".audio.m4a.x.partial");
        let to = directory.path().join("audio.m4a");
        std::fs::write(&from, b"master").unwrap();
        rename_written_through(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"master");
        assert_eq!(names(directory.path()), ["audio.m4a"]);
    }

    /// The rename replaces a file at the target, and no temporary is left.
    #[test]
    fn a_written_through_rename_replaces_an_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let from = directory.path().join(".preferences.json.x.partial");
        let to = directory.path().join("preferences.json");
        std::fs::write(&to, b"old").unwrap();
        std::fs::write(&from, b"new").unwrap();
        rename_written_through(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert_eq!(names(directory.path()), ["preferences.json"]);
    }

    /// A failed rename reports the OS error and leaves both files as they
    /// were.
    #[test]
    fn a_failed_rename_leaves_both_files() {
        let directory = tempfile::tempdir().unwrap();
        let from = directory.path().join("missing.partial");
        let to = directory.path().join("kept.json");
        std::fs::write(&to, b"kept").unwrap();
        let error = rename_written_through(&from, &to).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(&to).unwrap(), b"kept");
    }

    /// A path with a NUL is refused before any call.
    #[test]
    fn a_path_with_a_nul_is_refused() {
        let error = wide(Path::new("a\0b")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    /// The flush after a rename with std succeeds on the renamed file.
    #[test]
    fn a_renamed_file_can_be_flushed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.m4a");
        std::fs::write(&path, b"master").unwrap();
        flush(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"master");
    }
}
