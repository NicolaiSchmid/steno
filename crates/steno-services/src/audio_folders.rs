//! Where recordings were written, in two files under the support directory,
//! beside the database and outside its schema, which the Swift app shares.
//! Each write replaces its file in one durable write
//! ([`files::write_json`]), so a crash leaves the old file or the new one.
//! They are not read through [`files::read_json`], which starts afresh from
//! a file that does not parse: recovery must tell such a file from an empty
//! one.
//!
//! | File | What it holds | Written by | Read by |
//! |------|---------------|------------|---------|
//! | `RECORDED_FILE` | The audio folder of each recording, by meeting id, from before its row is written until the meeting completes, fails or is deleted | `record`, `forget` | `recorded`: crash recovery looks in a meeting's folder first ([`crate::recovery`]) |
//! | `KNOWN_FILE` | Every audio folder a recording was written to or the setting left, oldest first | `remember` | `known`: crash recovery looks in these folders after the two that decide a meeting ([`crate::recovery`]) |
//!
//! A reader returns the error of a file that cannot be read or does not
//! parse; a missing file is empty. A writer sets a file that does not parse
//! aside ([`files::set_aside`]) and keeps what can still be read of it.
//! Rust only: Swift had no recovery.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::files;

/// The name of the per-meeting record in the support directory.
pub(crate) const RECORDED_FILE: &str = "recording-folders.json";

/// The name of the list of known folders in the support directory.
pub(crate) const KNOWN_FILE: &str = "audio-folders.json";

/// Held from the read to the write of every change, so two threads of this
/// process (a start, a stop, a change of the setting) cannot each drop the
/// other's entry. Another process does not write the files: the database
/// lock lets one Rust app run at a time, and the Swift app never writes
/// them.
static WRITES: Mutex<()> = Mutex::new(());

/// The folder each recording under `support_directory` was recorded into,
/// by meeting id.
pub(crate) fn recorded(support_directory: &Path) -> std::io::Result<BTreeMap<Uuid, PathBuf>> {
    read(&support_directory.join(RECORDED_FILE))
}

/// Records `folder` as the one the recording of meeting `meeting_id` is
/// written to. The recorder calls it before it writes the meeting's row,
/// and logs a failure and goes on: a recording is not refused for it.
pub(crate) fn record(
    support_directory: &Path,
    meeting_id: Uuid,
    folder: &Path,
) -> std::io::Result<()> {
    change(
        support_directory,
        RECORDED_FILE,
        salvage_recorded,
        |recorded: &mut BTreeMap<Uuid, PathBuf>| {
            recorded.insert(meeting_id, folder.to_path_buf()).as_deref() != Some(folder)
        },
    )
}

/// Drops the entries of `meeting_ids`: their meetings completed, failed or
/// went.
pub(crate) fn forget(support_directory: &Path, meeting_ids: &[Uuid]) -> std::io::Result<()> {
    change(
        support_directory,
        RECORDED_FILE,
        salvage_recorded,
        |recorded: &mut BTreeMap<Uuid, PathBuf>| {
            let before = recorded.len();
            recorded.retain(|id, _| !meeting_ids.contains(id));
            recorded.len() != before
        },
    )
}

/// The known folders under `support_directory`, oldest first.
pub(crate) fn known(support_directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    read(&support_directory.join(KNOWN_FILE))
}

/// Adds `folder` to the known folders under `support_directory` unless it
/// is there. The caller logs a failure and goes on.
pub(crate) fn remember(support_directory: &Path, folder: &Path) -> std::io::Result<()> {
    change(
        support_directory,
        KNOWN_FILE,
        salvage_known,
        |folders: &mut Vec<PathBuf>| {
            let new = !folders.iter().any(|known| known == folder);
            if new {
                folders.push(folder.to_path_buf());
            }
            new
        },
    )
}

/// The file at `path`; no file is empty, and one that does not parse is
/// `InvalidData`.
fn read<T: Default + DeserializeOwned>(path: &Path) -> std::io::Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => parse(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error),
    }
}

fn parse<T: DeserializeOwned>(bytes: &[u8]) -> std::io::Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Applies `edit` to the file `name` under `support_directory` and writes
/// it back when `edit` says it changed. A file that does not parse is set
/// aside, and `salvage` keeps what can still be read of it.
fn change<T: Default + Serialize + DeserializeOwned>(
    support_directory: &Path,
    name: &str,
    salvage: fn(&[u8]) -> T,
    edit: impl FnOnce(&mut T) -> bool,
) -> std::io::Result<()> {
    let _writing = WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = support_directory.join(name);
    let mut value = match std::fs::read(&path) {
        Ok(bytes) => {
            if let Ok(value) = parse(&bytes) {
                value
            } else {
                files::set_aside(&path)?;
                salvage(&bytes)
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(error) => return Err(error),
    };
    if !edit(&mut value) {
        return Ok(());
    }
    files::write_json(&path, &value)
}

/// Every JSON string a file that does not parse still holds whole, in
/// order.
fn strings(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(quote) = bytes[at..].iter().position(|&byte| byte == b'"') {
        let start = at + quote;
        let mut stream =
            serde_json::Deserializer::from_slice(&bytes[start..]).into_iter::<String>();
        match stream.next() {
            Some(Ok(string)) => {
                found.push(string);
                at = start + stream.byte_offset();
            }
            _ => at = start + 1,
        }
    }
    found
}

/// The absolute paths among `strings`.
fn folders(strings: impl IntoIterator<Item = String>) -> impl Iterator<Item = PathBuf> {
    strings
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// The known folders a list that does not parse still names.
fn salvage_known(bytes: &[u8]) -> Vec<PathBuf> {
    let mut known: Vec<PathBuf> = Vec::new();
    for folder in folders(strings(bytes)) {
        if !known.contains(&folder) {
            known.push(folder);
        }
    }
    known
}

/// The entries a record that does not parse still holds: each meeting id
/// followed by an absolute path.
fn salvage_recorded(bytes: &[u8]) -> BTreeMap<Uuid, PathBuf> {
    let strings = strings(bytes);
    strings
        .windows(2)
        .filter_map(|pair| {
            let id = Uuid::parse_str(&pair[0]).ok()?;
            let folder = folders([pair[1].clone()]).next()?;
            Some((id, folder))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_aside_count(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
            .count()
    }

    /// Each folder is listed once, in the order it was first remembered,
    /// and a missing list is empty.
    #[test]
    fn a_folder_is_remembered_once() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        assert_eq!(known(&support).unwrap(), Vec::<PathBuf>::new());
        for folder in ["/a", "/b", "/a"] {
            remember(&support, Path::new(folder)).unwrap();
        }
        assert_eq!(
            known(&support).unwrap(),
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    /// An absolute folder under `dir` on every platform, and how JSON
    /// writes it.
    fn folder(dir: &Path, name: &str) -> (PathBuf, String) {
        let folder = dir.join(name);
        let json = serde_json::to_string(&folder).unwrap();
        (folder, json)
    }

    /// A list that does not parse is an error to its reader. The next
    /// folder sets it aside and keeps every folder it still names whole,
    /// here the two before the cut.
    #[test]
    fn a_list_that_does_not_parse_is_set_aside_and_its_folders_kept() {
        let dir = tempfile::tempdir().unwrap();
        let [(a, a_json), (b, b_json), (_, c_json), (d, _)] =
            ["a", "b", "c", "d"].map(|name| folder(dir.path(), name));
        let cut = &c_json[..c_json.len() - 1];
        std::fs::write(
            dir.path().join(KNOWN_FILE),
            format!("[{a_json}, {b_json}, {cut}"),
        )
        .unwrap();
        assert_eq!(
            known(dir.path()).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        remember(dir.path(), &d).unwrap();
        assert_eq!(known(dir.path()).unwrap(), [a, b, d]);
        assert_eq!(set_aside_count(dir.path()), 1);
    }

    /// A recording's folder is recorded until its meeting is forgotten;
    /// forgetting one leaves the others.
    #[test]
    fn a_recordings_folder_is_kept_until_it_is_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(recorded(dir.path()).unwrap().is_empty());
        record(dir.path(), first, Path::new("/a")).unwrap();
        record(dir.path(), second, Path::new("/b")).unwrap();
        forget(dir.path(), &[first]).unwrap();
        assert_eq!(
            recorded(dir.path()).unwrap(),
            BTreeMap::from([(second, PathBuf::from("/b"))])
        );
    }

    /// A record that does not parse is an error to its reader; the next
    /// write sets it aside and keeps every whole entry.
    #[test]
    fn a_record_that_does_not_parse_is_set_aside_and_its_entries_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, cut, new) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let [(a, a_json), (_, b_json), (c, _)] =
            ["a", "b", "c"].map(|name| folder(dir.path(), name));
        let b_cut = &b_json[..b_json.len() - 1];
        std::fs::write(
            dir.path().join(RECORDED_FILE),
            format!("{{\"{kept}\": {a_json}, \"{cut}\": {b_cut}"),
        )
        .unwrap();
        assert_eq!(
            recorded(dir.path()).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        record(dir.path(), new, &c).unwrap();
        assert_eq!(
            recorded(dir.path()).unwrap(),
            BTreeMap::from([(kept, a), (new, c)])
        );
        assert_eq!(set_aside_count(dir.path()), 1);
    }
}
