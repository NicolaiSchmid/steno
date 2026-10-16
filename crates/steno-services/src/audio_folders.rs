//! Where recordings were written, in files under the support directory,
//! beside the database and outside its schema. `RECORDED_FILE` and
//! `KNOWN_FILE` are shared with the Swift app; `UPLOADS_FILE` is Rust's
//! alone.
//! Each write replaces its file in one durable write
//! ([`files::write_json`]), so a crash leaves the old file or the new one.
//! They are not read through [`files::read_json`], which starts afresh from
//! a file that does not parse: recovery must tell such a file from an empty
//! one.
//!
//! | File | What it holds | Written by | Read by |
//! |------|---------------|------------|---------|
//! | `RECORDED_FILE` | The audio folder of each recording, by meeting id, from before its row is written: a recording's from its start until its start fails (nothing was recorded), it is deleted (forgotten before its rows go, and recorded again when the delete is refused, its commit fails or a panic unwinds it) or a launch finds its row durable and no longer `recording`; a phone upload's from before its copy until its admission commits durably or its copy is gone (`AdmissionRecord`); one with no row until a launch adopts its master (from then on as any row's) or finds that its folder holds none (no master, a CAF short of one whole frame, or a header with no audio) | `record`, `forget` | `recorded`: crash recovery looks in a meeting's folder first, and adopts only a recording with no meeting that this record names ([`crate::recovery`]) |
//! | `UPLOADS_FILE` | The upload each phone admission's copy holds (recording id, size, SHA-256), by meeting id, from before the copy until its entry in `RECORDED_FILE` is forgotten, the folder's note fails (the admission is refused), or a launch finds the upload with no entry in `RECORDED_FILE` (a crash between the two notes). A file of its own, so an older build reads `RECORDED_FILE` as before and adopts the copy as a recording with no meeting | `note_upload`, `forget`, `forget_uploads_without_folders` | `uploads`: the launch adopts the noted copy that holds its upload with the upload's ledger row, and removes it when the ledger holds those bytes as a meeting the user deleted, since the ledger row commits with its meeting and so names bytes the user deleted ([`crate::recovery`]); the phone's retry admits into the noted copy (`AdmissionRecord`). An entry missing or unreadable is an adoption as a recording with no meeting, without a ledger row |
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
use steno_pipeline::{AdmissionFolders, NotedUpload};
use uuid::Uuid;

use crate::files;

/// The name of the per-meeting record in the support directory.
pub(crate) const RECORDED_FILE: &str = "recording-folders.json";

/// The name of the record of the uploads phone admissions copied, in the
/// support directory.
pub(crate) const UPLOADS_FILE: &str = "admission-uploads.json";

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

/// Drops the entries of `meeting_ids`, and the uploads noted for them:
/// their meetings completed, failed or went. Returns the folders it
/// dropped, so a delete that does not go through can record its entry
/// again; an upload is not noted again, so such a copy is adopted as any
/// recording with no meeting. An upload that cannot be forgotten is
/// logged: no copy is adopted by it without its folder's entry.
pub(crate) fn forget(
    support_directory: &Path,
    meeting_ids: &[Uuid],
) -> std::io::Result<BTreeMap<Uuid, PathBuf>> {
    let mut forgotten = BTreeMap::new();
    change(
        support_directory,
        RECORDED_FILE,
        salvage_recorded,
        |recorded: &mut BTreeMap<Uuid, PathBuf>| {
            forgotten.extend(
                meeting_ids
                    .iter()
                    .filter_map(|id| recorded.remove_entry(id)),
            );
            !forgotten.is_empty()
        },
    )?;
    if let Err(error) = forget_uploads(support_directory, |id, _| meeting_ids.contains(id)) {
        tracing::debug!(%error, "the uploads of forgotten recording folders not forgotten");
    }
    Ok(forgotten)
}

/// Drops the noted uploads for which `goes` holds.
fn forget_uploads(
    support_directory: &Path,
    mut goes: impl FnMut(&Uuid, &NotedUpload) -> bool,
) -> std::io::Result<()> {
    change(
        support_directory,
        UPLOADS_FILE,
        |_| BTreeMap::new(),
        |uploads: &mut BTreeMap<Uuid, NotedUpload>| {
            let before = uploads.len();
            uploads.retain(|id, upload| !goes(id, upload));
            uploads.len() != before
        },
    )
}

/// Drops the uploads in `seen` (the launch's read of `UPLOADS_FILE`) whose
/// folder `RECORDED_FILE` does not name when they go: a crash between the
/// upload's note and the folder's, or a `forget` whose second write
/// failed. The folders are read under the lock every note takes, so an
/// admission that noted its folder since is kept. One that noted its
/// upload before `seen` was read and has not noted its folder yet loses
/// only the upload, and its copy is adopted as a recording with no
/// meeting. Once per launch ([`crate::recovery`]), so the record never
/// grows.
pub(crate) fn forget_uploads_without_folders(
    support_directory: &Path,
    seen: &BTreeMap<Uuid, NotedUpload>,
) -> std::io::Result<()> {
    if seen.is_empty() {
        return Ok(());
    }
    // Read once, under the lock the edit holds.
    let mut folders = None;
    forget_uploads(support_directory, |id, upload| {
        let folders = folders.get_or_insert_with(|| recorded(support_directory));
        seen.get(id) == Some(upload)
            && folders
                .as_ref()
                .is_ok_and(|folders| !folders.contains_key(id))
    })
}

/// The upload each phone admission's copy under `support_directory`
/// holds, by meeting id.
pub(crate) fn uploads(support_directory: &Path) -> std::io::Result<BTreeMap<Uuid, NotedUpload>> {
    read(&support_directory.join(UPLOADS_FILE))
}

/// Notes `upload` as the one meeting `meeting_id`'s copy holds. A record
/// that does not parse is set aside and started afresh: its copies are
/// adopted as recordings with no meeting, without a ledger row.
pub(crate) fn note_upload(
    support_directory: &Path,
    meeting_id: Uuid,
    upload: &NotedUpload,
) -> std::io::Result<()> {
    change(
        support_directory,
        UPLOADS_FILE,
        |_| BTreeMap::new(),
        |uploads: &mut BTreeMap<Uuid, NotedUpload>| {
            uploads.insert(meeting_id, upload.clone()).as_ref() != Some(upload)
        },
    )
}

/// The phone intake's notes of the folder each upload is copied into and
/// of the upload ([`AdmissionFolders`]), in the records under the support
/// directory (`note_upload`, `record`, `forget`). Rust only.
pub(crate) struct AdmissionRecord {
    pub(crate) support_directory: PathBuf,
}

impl AdmissionFolders for AdmissionRecord {
    /// The upload first, so a folder this build records names its upload
    /// unless that note failed; then the copy is adopted as a recording
    /// with no meeting, without a ledger row. When the folder cannot be
    /// noted, the admission is refused and its upload is forgotten again.
    fn copying(
        &self,
        meeting_id: Uuid,
        audio_folder: &Path,
        upload: &NotedUpload,
    ) -> std::io::Result<()> {
        if let Err(error) = note_upload(&self.support_directory, meeting_id, upload) {
            tracing::debug!(%meeting_id, %error, "an admission's upload not noted");
        }
        #[cfg(test)]
        crate::admission_kill_tests::gate("after-upload-note");
        let recorded = record(&self.support_directory, meeting_id, audio_folder).inspect_err(|error| {
            // The admission is refused; the error can name the user's
            // folder: debug alone.
            tracing::warn!(%meeting_id, "a phone upload's folder could not be noted; the upload is refused for now");
            tracing::debug!(%meeting_id, %error, "an admission's folder not noted");
            if let Err(error) = forget_uploads(&self.support_directory, |id, _| *id == meeting_id) {
                // The next launch forgets an upload with no folder.
                tracing::debug!(%meeting_id, %error, "a refused admission's upload not forgotten");
            }
        });
        #[cfg(test)]
        if recorded.is_ok() {
            crate::admission_kill_tests::gate("after-folder-note");
        }
        recorded
    }

    fn settled(&self, meeting_id: Uuid) {
        #[cfg(test)]
        crate::admission_kill_tests::gate("before-settled");
        if let Err(error) = forget(&self.support_directory, &[meeting_id]) {
            // The next launch forgets an entry whose meeting has a row.
            tracing::debug!(%meeting_id, %error, "an admission's folder not forgotten");
        }
    }

    /// The first copy noted for `upload` whose folder is still recorded. A
    /// record that cannot be read gives none: the upload is copied again.
    fn noted_copy(&self, upload: &NotedUpload) -> Option<(Uuid, PathBuf)> {
        let uploads = uploads(&self.support_directory).ok()?;
        let mut recorded = recorded(&self.support_directory).ok()?;
        uploads
            .into_iter()
            .filter(|(_, noted)| noted == upload)
            .find_map(|(meeting_id, _)| Some((meeting_id, recorded.remove(&meeting_id)?)))
    }
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
    /// forgetting one returns its entry and leaves the others.
    #[test]
    fn a_recordings_folder_is_kept_until_it_is_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(recorded(dir.path()).unwrap().is_empty());
        record(dir.path(), first, Path::new("/a")).unwrap();
        record(dir.path(), second, Path::new("/b")).unwrap();
        assert_eq!(
            forget(dir.path(), &[first]).unwrap(),
            BTreeMap::from([(first, PathBuf::from("/a"))])
        );
        assert!(forget(dir.path(), &[first]).unwrap().is_empty());
        assert_eq!(
            recorded(dir.path()).unwrap(),
            BTreeMap::from([(second, PathBuf::from("/b"))])
        );
    }

    /// A phone copy's upload is noted with its folder and found by its
    /// bytes while the folder is recorded; forgetting the folder forgets
    /// the upload. The record of folders keeps the shape an older build
    /// reads.
    #[test]
    fn an_admissions_upload_is_noted_with_its_folder_and_forgotten_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let folders = AdmissionRecord {
            support_directory: dir.path().to_path_buf(),
        };
        let upload = NotedUpload {
            recording_id: Uuid::new_v4(),
            byte_count: 3,
            sha256: vec![7; 32],
        };
        let other = NotedUpload {
            sha256: vec![8; 32],
            ..upload.clone()
        };
        let (meeting_id, audio) = (Uuid::new_v4(), dir.path().join("audio"));
        folders.copying(meeting_id, &audio, &upload).unwrap();

        assert_eq!(
            folders.noted_copy(&upload),
            Some((meeting_id, audio.clone()))
        );
        assert_eq!(folders.noted_copy(&other), None, "other bytes");
        let older: BTreeMap<Uuid, PathBuf> =
            serde_json::from_slice(&std::fs::read(dir.path().join(RECORDED_FILE)).unwrap())
                .unwrap();
        assert_eq!(older, BTreeMap::from([(meeting_id, audio)]));

        folders.settled(meeting_id);
        assert_eq!(folders.noted_copy(&upload), None);
        assert!(uploads(dir.path()).unwrap().is_empty());
    }

    /// The launch forgets the uploads it read whose folder has no entry
    /// (a crash between the two notes), and keeps one whose folder is
    /// noted, one noted since its read, and one whose folder was noted
    /// after its read.
    #[test]
    fn the_launch_forgets_the_uploads_with_no_folder() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path();
        let upload = |byte| NotedUpload {
            recording_id: Uuid::new_v4(),
            byte_count: 3,
            sha256: vec![byte; 32],
        };
        let audio = dir.path().join("audio");
        let (with_folder, without, late_folder, since) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        for (id, byte) in [(with_folder, 1), (without, 2), (late_folder, 3)] {
            note_upload(support, id, &upload(byte)).unwrap();
        }
        record(support, with_folder, &audio).unwrap();
        let seen = uploads(support).unwrap();
        note_upload(support, since, &upload(4)).unwrap();
        record(support, late_folder, &audio).unwrap();

        forget_uploads_without_folders(support, &seen).unwrap();
        let left: Vec<Uuid> = uploads(support).unwrap().into_keys().collect();
        let mut kept = vec![with_folder, late_folder, since];
        kept.sort();
        assert_eq!(left, kept);
    }

    /// Writers on many threads at once (phone admissions on several
    /// connections, the recorder) each keep their entry: every change
    /// holds one lock from its read to its write.
    #[test]
    fn entries_recorded_at_once_are_all_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ids: Vec<Uuid> = (0..16).map(|_| Uuid::new_v4()).collect();
        std::thread::scope(|scope| {
            for id in &ids {
                scope.spawn(|| record(dir.path(), *id, Path::new("/a")).unwrap());
            }
        });
        let recorded = recorded(dir.path()).unwrap();
        assert!(
            ids.iter().all(|id| recorded.contains_key(id)),
            "{recorded:?}"
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
