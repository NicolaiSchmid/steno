//! The audio folders recordings were written to, in
//! `<support>/audio-folders.json` beside the database and outside its
//! schema, which the Swift app shares: crash recovery looks for an
//! interrupted recording's master in each of them, so a recording is found
//! in the folder it started in after the user picked another while it ran.
//! The recorder adds its folder before it writes a recording's row
//! ([`remember`]), and a change of the setting adds the folder it leaves.
//! A list that is missing or cannot be read is empty ([`known`]), never an
//! error: recovery also looks in the settings' folder and in the folder of
//! every stored asset. Rust only: Swift had no recovery.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::files;

/// The list's name in the support directory.
pub const FILE_NAME: &str = "audio-folders.json";

/// Held from the read to the write of [`remember`], so two threads of this
/// process (a start and a change of the setting) cannot each drop the
/// other's folder. Another process does not write the list: the database
/// lock lets one Rust app run at a time, and the Swift app never writes it.
static WRITES: Mutex<()> = Mutex::new(());

fn path(support_directory: &Path) -> PathBuf {
    support_directory.join(FILE_NAME)
}

/// The folders in the list under `support_directory`, oldest first; empty
/// when there is no list or it cannot be read or parsed (logged at debug).
#[must_use]
pub fn known(support_directory: &Path) -> Vec<PathBuf> {
    match read(&path(support_directory)) {
        Ok(folders) => folders,
        Err(error) => {
            tracing::debug!(%error, "the known audio folders could not be read");
            Vec::new()
        }
    }
}

/// The list at `path`; no file is an empty list, and one that does not
/// parse is `InvalidData`.
fn read(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Adds `folder` to the list under `support_directory` unless it is there,
/// in one durable replace ([`files::replace_file`]), so a crash leaves the
/// old list or the new one. A list that does not parse is set aside
/// ([`files::set_aside`]) and started afresh with `folder`. The caller
/// logs a failure and goes on: a recording is not refused for it.
pub fn remember(support_directory: &Path, folder: &Path) -> std::io::Result<()> {
    let _writing = WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = path(support_directory);
    let mut folders = match read(&path) {
        Ok(folders) => folders,
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            files::set_aside(&path)?;
            Vec::new()
        }
        Err(error) => return Err(error),
    };
    if folders.iter().any(|known| known == folder) {
        return Ok(());
    }
    folders.push(folder.to_path_buf());
    let data = serde_json::to_vec_pretty(&folders)?;
    files::create_dir_all_durably(support_directory)?;
    files::replace_file(&path, &data, files::Access::Default)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each folder is listed once, in the order it was first remembered,
    /// and a missing list is empty.
    #[test]
    fn a_folder_is_remembered_once() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        assert_eq!(known(&support), Vec::<PathBuf>::new());
        for folder in ["/a", "/b", "/a"] {
            remember(&support, Path::new(folder)).unwrap();
        }
        assert_eq!(known(&support), [PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    /// A list that does not parse reads as empty; the next folder starts a
    /// new list, and the old bytes are kept beside it.
    #[test]
    fn a_list_that_does_not_parse_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), b"{not json").unwrap();
        assert_eq!(known(dir.path()), Vec::<PathBuf>::new());
        remember(dir.path(), Path::new("/a")).unwrap();
        assert_eq!(known(dir.path()), [PathBuf::from("/a")]);
        let set_aside = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(set_aside, 1);
    }
}
