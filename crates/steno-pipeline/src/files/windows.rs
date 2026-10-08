//! The Windows calls behind the durable writes, which on the other
//! platforms sync the folder after a rename.
//!
//! `std::fs::rename` calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`
//! alone, which may return while the rename is still only in the cache;
//! `tempfile`'s `persist` does the same. [`rename_written_through`] adds
//! `MOVEFILE_WRITE_THROUGH`, with which `MoveFileExW` returns only once the
//! rename is on the disk. Where that call fails (a target another handle
//! holds open, which only std's fallback to POSIX rename semantics
//! replaces), the caller renames with std. Either way the caller then
//! flushes the renamed file. On NTFS that flush commits the volume's
//! journal, which holds the rename and, written in order, the folders
//! created before it. On FAT32 it also flushes every folder above the file
//! (Windows' FAT driver flushes a file's parent folders with it), which a
//! written-through rename leaves in the cache.
//!
//! [`flush_directory`] flushes a folder's entries: on NTFS the ones
//! `create_dir_all_durably` made; the FAT driver does nothing for a folder
//! other than the drive's root, so there the file's flush is the one that
//! counts. exFAT's driver is not published, so Settings warns about an
//! audio folder on a drive that is not NTFS ([`file_system_name`]).
//!
//! A long path is passed with the `\\?\` prefix, from the length at which
//! std adds it, so a long audio folder still takes the written-through
//! rename.

use std::fs::OpenOptions;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;

use windows_sys::Win32::Foundation::MAX_PATH;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_APPEND_DATA, FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_DATA, GetVolumeInformationByHandleW,
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
    // after it returns.
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

/// Flushes the entries of `directory` to the disk (`FlushFileBuffers` on
/// the folder). The flush needs a handle that may write: a folder's
/// `FILE_APPEND_DATA` is the right to add a subfolder and its
/// `FILE_WRITE_DATA` the right to add a file, and a caller that has just
/// made one of the two in it holds that right, so each is tried.
pub(super) fn flush_directory(directory: &Path) -> io::Result<()> {
    let open = |access| {
        OpenOptions::new()
            .access_mode(access)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(directory)
    };
    let folder = match open(FILE_APPEND_DATA) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => open(FILE_WRITE_DATA)?,
        opened => opened?,
    };
    folder.sync_all()
}

/// The name of the file system `path` is on ("NTFS", "FAT32", "exFAT"),
/// read from the nearest folder of it that exists.
pub(super) fn file_system_name(path: &Path) -> io::Result<String> {
    const NAME_UNITS: u32 = MAX_PATH + 1;
    let existing = path
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no part of {} exists", path.display()),
            )
        })?;
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(existing)?;
    let mut name = [0_u16; NAME_UNITS as usize];
    // SAFETY: `handle` is open until the call returns, and `name` is a
    // writable buffer of the length passed, which the function fills with a
    // NUL-terminated string no longer than that. The other buffers and out
    // values are null with a length of 0, which the function documents as
    // "do not return this".
    let read = unsafe {
        GetVolumeInformationByHandleW(
            handle.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            NAME_UNITS,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(name.len());
    Ok(String::from_utf16_lossy(&name[..length]))
}

/// `path` as the NUL-terminated UTF-16 string the Win32 calls take, with
/// the `\\?\` prefix (`\\?\UNC\` for a share) once it is too long for the
/// calls without it.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    /// The length from which std prefixes a path, short enough for a folder
    /// (`MAX_PATH` less a file name of 8.3).
    const PREFIX_FROM: usize = 248;
    let units = |path: &Path| path.as_os_str().encode_wide().collect::<Vec<u16>>();
    let mut wide = units(path);
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} holds a NUL", path.display()),
        ));
    }
    let prefixed = wide.starts_with(&ascii(r"\\?\")) || wide.starts_with(&ascii(r"\\.\"));
    if wide.len() >= PREFIX_FROM && !prefixed {
        let absolute = units(&std::path::absolute(path)?);
        wide = match absolute.strip_prefix(ascii(r"\\").as_slice()) {
            Some(share) => [ascii(r"\\?\UNC\"), share.to_vec()].concat(),
            None => [ascii(r"\\?\"), absolute].concat(),
        };
    }
    wide.push(0);
    Ok(wide)
}

fn ascii(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
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

    /// A path past `MAX_PATH` is renamed written through too, with the
    /// prefix.
    #[test]
    fn a_long_path_is_renamed_written_through() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("a".repeat(120)).join("b".repeat(120));
        std::fs::create_dir_all(&folder).unwrap();
        let from = folder.join(".recording.m4a.x.partial");
        let to = folder.join("recording.m4a");
        assert!(to.as_os_str().len() > MAX_PATH as usize);
        std::fs::write(&from, b"master").unwrap();
        rename_written_through(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"master");
        assert_eq!(names(&folder), ["recording.m4a"]);
    }

    /// A rename from a missing file reports `NotFound` and leaves the
    /// target as it was.
    #[test]
    fn a_rename_from_a_missing_file_fails_and_keeps_the_target() {
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

    /// A long path gets the prefix, a share's the share prefix, and a
    /// short one none.
    #[test]
    fn only_a_long_path_is_prefixed() {
        let text = |path: &str| String::from_utf16(&wide(Path::new(path)).unwrap()).unwrap();
        assert_eq!(text(r"C:\audio\a.m4a"), "C:\\audio\\a.m4a\0");
        let long = format!(r"C:\{}\a.m4a", "a".repeat(250));
        assert_eq!(text(&long), format!("\\\\?\\{long}\0"));
        let share = format!(r"\\server\share\{}\a.m4a", "a".repeat(250));
        assert_eq!(
            text(&share),
            format!("\\\\?\\UNC\\{}\0", share.trim_start_matches('\\'))
        );
    }

    /// The folder flush succeeds on a folder this process made, on NTFS,
    /// and reports a folder that is not there. Were NTFS to refuse the
    /// flush, every phone recording would be answered 500.
    #[test]
    fn a_new_folder_can_be_flushed() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("meeting");
        std::fs::create_dir(&folder).unwrap();
        flush_directory(directory.path()).unwrap();
        flush_directory(&folder).unwrap();
        let error = flush_directory(&directory.path().join("missing")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    /// The runner's temporary folder is on NTFS, and a folder that does not
    /// exist yet is read from the nearest one that does.
    #[test]
    fn the_file_system_is_read_from_the_nearest_existing_folder() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(file_system_name(directory.path()).unwrap(), "NTFS");
        let missing = directory.path().join("audio").join("meeting");
        assert_eq!(file_system_name(&missing).unwrap(), "NTFS");
    }
}
