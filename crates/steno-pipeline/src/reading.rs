//! The breaker for a recording that takes the app down while it is read
//! (A17 of `.plans/2026-10-07-stable-promotion.md`). Symphonia sizes some
//! allocations straight from a file's fields, and a failed allocation
//! aborts the process, which no `catch_unwind` stops; on the Mac, `CoreML`
//! runs in the app's process, and an abort inside it that no `@catch`
//! stops ends the app too. Unguarded, such a file ends the app at every
//! launch: recovery reads it again, and the launch resumes its meeting.
//! Rust only: Swift had no recovery to loop through.
//!
//! - **The marker.** Before a read of a meeting's recording, a file in
//!   [`ReadingMarks::FOLDER`] under the support directory names the
//!   meeting, the asset, the file and the [`ReadStage`]. It is written
//!   durably ([`replace_file`]: synced, renamed, the folder synced), so it
//!   is on disk before the reader sees a byte, and removed the same way
//!   ([`remove_durably`]) when the read returns, with an error or a panic
//!   too. Each read has a file of its own (a random name), so two reads
//!   never share one.
//! - **What is marked.** Every read of a recording in the app's process:
//!   the decode of each lane (for the transcript and the diarizer), the
//!   persist stage's mixdown, recovery's read of an adopted recording's
//!   declared length (`steno-services`); and, where the speech
//!   engine runs in the app's process (`CoreML` on the Mac), the job's load
//!   of the engine and each transcription. Process again and the CLI's
//!   `process` and `reprocess` run the same stages. The speech sidecar and
//!   the diarizer run in a child, whose abort ends only the child.
//! - **An orderly exit** ([`ReadingMarks::exiting`], from
//!   [`ProcessingPipeline::quit`]) removes the markers of the reads still
//!   running and writes no more: the exit ended them, not the file.
//! - **The launch check** ([`ReadingMarks::at_launch`]): every marker left
//!   in the folder is a read that ended with the app (the database lock
//!   keeps a second process out). Its meeting is quarantined: it fails
//!   with [`READ_STOPPED`], its asset loses a retention stamp so no sweep
//!   takes the file, and it is logged once at `warn`. A marker whose
//!   meeting has no row yet (an orphan the launch was adopting) stays for
//!   the adoption, which adopts the recording failed and unread; one whose
//!   meeting and file are both gone is removed.
//! - **Never touched again automatically.** The markers stay on disk: one
//!   that outlived its process is the quarantine. The launch resume, the
//!   resumes after a model install or a busy store, and the adoption skip a
//!   quarantined meeting ([`ReadingMarks::holds`]); the re-exports read no
//!   recording.
//! - **Process again** (the app's, and `steno process --meeting`) releases
//!   the meeting ([`ReadingMarks::release`]) and tries once more; its own
//!   reads are marked, so a second crash quarantines it again.
//!
//! The cost of a false alarm: a power loss or a force quit during a read
//! fails that one meeting with Process again offered, and nothing is lost.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use steno_core::{MeetingState, Store};
use uuid::Uuid;

use crate::files::{Access, create_dir_all_durably, remove_durably, replace_file};

#[cfg(doc)]
use crate::ProcessingPipeline;

/// The reason a quarantined meeting fails with: one sentence, as the list
/// shows only the first.
pub const READ_STOPPED: &str = "Steno stopped while reading this recording; the recording is kept.";

/// What a marker says was going on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReadStage {
    /// A lane decoded for the transcript or the diarizer.
    Decode,
    /// The persist stage's mixdown.
    Mixdown,
    /// Recovery's read of an adopted recording's length.
    Duration,
    /// The job's load of an in-process speech engine, or a transcription.
    Transcribe,
}

/// One marker's body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Marker {
    meeting_id: Uuid,
    asset_id: Option<Uuid>,
    path: PathBuf,
    stage: ReadStage,
    started_at: DateTime<Utc>,
}

/// The markers of one support directory; see the module doc.
#[derive(Debug)]
pub struct ReadingMarks {
    /// `None` in memory: nothing is written.
    folder: Option<PathBuf>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// The meetings the launch quarantined, with their markers' files.
    held: HashMap<Uuid, Vec<PathBuf>>,
    /// Set by the exit: no read is marked any more.
    exiting: bool,
    /// The files of this process's reads now running.
    open: HashSet<PathBuf>,
}

impl ReadingMarks {
    /// The folder of the markers in the support directory.
    pub const FOLDER: &'static str = "reading";

    /// Marks nothing and holds no meeting: the tests.
    #[must_use]
    pub fn in_memory() -> Self {
        ReadingMarks {
            folder: None,
            state: Mutex::default(),
        }
    }

    /// The launch check over the markers in `support_directory`: each one
    /// left there quarantines its meeting in `store` (see the module doc).
    /// Call it once per launch, before anything reads a recording. A store
    /// that refuses the write leaves the meeting as it is, still held for
    /// this session, and the next launch tries again.
    pub fn at_launch(support_directory: &Path, store: &Store, now: DateTime<Utc>) -> Self {
        let folder = support_directory.join(Self::FOLDER);
        let mut markers: BTreeMap<Uuid, Vec<(PathBuf, Marker)>> = BTreeMap::new();
        for (file, marker) in read_folder(&folder) {
            markers
                .entry(marker.meeting_id)
                .or_default()
                .push((file, marker));
        }
        let mut held = HashMap::new();
        for (meeting_id, found) in markers {
            match store.meeting(meeting_id) {
                Ok(Some(meeting)) => {
                    let stage = found[0].1.stage;
                    tracing::warn!(%meeting_id, ?stage, "Steno stopped while reading this recording; it is kept and not read again until Process again");
                    quarantine(store, &meeting, now);
                }
                Ok(None) if found.iter().any(|(_, marker)| marker.path.exists()) => {}
                Ok(None) => {
                    for (file, _) in found {
                        let _ = remove_durably(&file);
                    }
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%meeting_id, %error, "a quarantined meeting could not be read; it stays held");
                }
            }
            held.insert(
                meeting_id,
                found.into_iter().map(|(file, _)| file).collect(),
            );
        }
        ReadingMarks {
            folder: Some(folder),
            state: Mutex::new(State {
                held,
                ..State::default()
            }),
        }
    }

    /// Whether the launch quarantined `meeting_id` and nothing released it.
    #[must_use]
    pub fn holds(&self, meeting_id: Uuid) -> bool {
        self.state().held.contains_key(&meeting_id)
    }

    /// Process again: `meeting_id` is no longer held and its markers go,
    /// so its next run reads the recording once more.
    pub fn release(&self, meeting_id: Uuid) {
        let files = self.state().held.remove(&meeting_id).unwrap_or_default();
        for path in files {
            if let Err(error) = remove_durably(&path) {
                tracing::debug!(%meeting_id, %error, "a reading marker could not be removed");
            }
        }
    }

    /// A read of `path`, the recording of `meeting_id` (`asset_id` once it
    /// has one), in `stage`, begins: the marker is on disk before this
    /// returns, and goes when the guard drops. A marker that cannot be
    /// written leaves the read unguarded, logged.
    pub fn reading(
        &self,
        meeting_id: Uuid,
        asset_id: Option<Uuid>,
        path: &Path,
        stage: ReadStage,
    ) -> Reading<'_> {
        let unmarked = Reading {
            marks: self,
            file: None,
        };
        let Some(folder) = &self.folder else {
            return unmarked;
        };
        let mut open = self.state();
        if open.exiting {
            return unmarked;
        }
        let marker = Marker {
            meeting_id,
            asset_id,
            path: path.to_path_buf(),
            stage,
            started_at: Utc::now(),
        };
        let file = folder.join(format!("{}.json", Uuid::new_v4()));
        let written = create_dir_all_durably(folder)
            .and_then(|()| serde_json::to_vec(&marker).map_err(std::io::Error::other))
            .and_then(|bytes| replace_file(&file, &bytes, Access::Default));
        if let Err(error) = written {
            tracing::debug!(%meeting_id, %error, "a reading marker could not be written; the read runs unguarded");
            return unmarked;
        }
        open.open.insert(file.clone());
        Reading {
            marks: self,
            file: Some(file),
        }
    }

    /// The orderly exit: the markers of the reads still running go, and no
    /// read is marked any more.
    pub fn exiting(&self) {
        let open: Vec<PathBuf> = {
            let mut state = self.state();
            state.exiting = true;
            state.open.drain().collect()
        };
        for file in open {
            let _ = remove_durably(&file);
        }
    }

    fn close(&self, file: &Path) {
        let open = self.state().open.remove(file);
        if open && let Err(error) = remove_durably(file) {
            tracing::debug!(%error, "a reading marker could not be removed");
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A read in progress; see [`ReadingMarks::reading`].
#[must_use = "the read is marked only while the guard lives"]
pub struct Reading<'a> {
    marks: &'a ReadingMarks,
    /// The marker's file; `None` for a read left unmarked.
    file: Option<PathBuf>,
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            self.marks.close(file);
        }
    }
}

/// The markers in `folder` with their files; a file that does not parse
/// (not one this module wrote) is logged and left.
fn read_folder(folder: &Path) -> Vec<(PathBuf, Marker)> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let marker = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok());
            if marker.is_none() {
                tracing::debug!(path = %path.display(), "not a reading marker; left alone");
            }
            marker.map(|marker| (path, marker))
        })
        .collect()
}

/// Fails `meeting` with [`READ_STOPPED`] unless it already says so, and
/// takes its asset's retention stamp, so no sweep removes the file.
fn quarantine(store: &Store, meeting: &steno_core::Meeting, now: DateTime<Utc>) {
    let failed = MeetingState::Failed {
        reason: READ_STOPPED.to_owned(),
    };
    if meeting.state != failed
        && let Err(error) = store.set_state(meeting.id, failed, now)
    {
        tracing::warn!(meeting_id = %meeting.id, %error, "a quarantined meeting could not be marked failed; it stays held");
    }
    if let Ok(Some(mut asset)) = store.asset(meeting.id)
        && asset.expires_at.take().is_some()
        && let Err(error) = store.save_asset(&asset)
    {
        tracing::warn!(meeting_id = %meeting.id, %error, "a quarantined recording's retention stamp could not be cleared");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use steno_core::paths::file_url;
    use steno_core::{AudioAsset, AudioFormat, AudioLane, AudioRetention};

    use super::*;

    /// A store with one ready meeting whose asset carries a retention
    /// stamp, its master written under `dir`.
    fn store_with_meeting(dir: &Path) -> (Store, steno_core::Meeting, AudioAsset) {
        let store = Store::open(dir.join("steno.sqlite")).unwrap();
        let mut meeting = steno_core::testing::sample_data::meeting();
        meeting.state = MeetingState::Ready;
        let master = dir.join("recording.m4a");
        std::fs::write(&master, b"audio").unwrap();
        let asset = AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: file_url(&master, false),
            format: AudioFormat::M4aAac,
            lanes: vec![AudioLane::Mixed],
            sidecars_16k: BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::DeleteAfterProcessing,
            expires_at: Some(Utc::now()),
        };
        store.save_meeting_with_asset(&meeting, &asset).unwrap();
        (store, meeting, asset)
    }

    fn markers(dir: &Path) -> usize {
        std::fs::read_dir(dir.join("support").join(ReadingMarks::FOLDER)).map_or(0, Iterator::count)
    }

    #[test]
    fn a_read_is_marked_on_disk_until_it_returns() {
        let dir = tempfile::tempdir().unwrap();
        let (store, meeting, asset) = store_with_meeting(dir.path());
        let marks = ReadingMarks::at_launch(&dir.path().join("support"), &store, Utc::now());
        let master = dir.path().join("recording.m4a");
        let first = marks.reading(meeting.id, Some(asset.id), &master, ReadStage::Decode);
        let second = marks.reading(meeting.id, Some(asset.id), &master, ReadStage::Mixdown);
        assert_eq!(markers(dir.path()), 2, "on disk before the read, one each");
        drop(first);
        assert_eq!(markers(dir.path()), 1);
        drop(second);
        assert_eq!(markers(dir.path()), 0);
        assert!(!marks.holds(meeting.id));
    }

    /// A marker left over fails its meeting with the plain reason, takes
    /// the retention stamp off its asset and keeps the file; the meeting
    /// stays held, at the next launch too, until Process again releases it.
    #[test]
    fn a_marker_left_over_quarantines_its_meeting_until_it_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        let (store, meeting, asset) = store_with_meeting(dir.path());
        let master = dir.path().join("recording.m4a");
        let crashed = ReadingMarks::at_launch(&support, &store, Utc::now());
        std::mem::forget(crashed.reading(meeting.id, Some(asset.id), &master, ReadStage::Decode));

        let marks = ReadingMarks::at_launch(&support, &store, Utc::now());
        assert!(marks.holds(meeting.id));
        let failed = MeetingState::Failed {
            reason: READ_STOPPED.to_owned(),
        };
        assert_eq!(store.meeting(meeting.id).unwrap().unwrap().state, failed);
        assert_eq!(store.asset(meeting.id).unwrap().unwrap().expires_at, None);
        assert_eq!(std::fs::read(&master).unwrap(), b"audio");
        assert_eq!(markers(dir.path()), 1, "the marker is the quarantine");
        let again = ReadingMarks::at_launch(&support, &store, Utc::now());
        assert!(again.holds(meeting.id));

        again.release(meeting.id);
        assert!(!again.holds(meeting.id));
        assert_eq!(markers(dir.path()), 0);
        assert!(!ReadingMarks::at_launch(&support, &store, Utc::now()).holds(meeting.id));
    }

    /// An orphan the launch was adopting has no row: its marker stays held
    /// while the file is there, and goes once the file is gone too.
    #[test]
    fn an_orphans_marker_waits_for_the_adoption_and_goes_with_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
        let orphan = Uuid::new_v4();
        let master = dir.path().join("orphan.m4a");
        std::fs::write(&master, b"audio").unwrap();
        let crashed = ReadingMarks::at_launch(&support, &store, Utc::now());
        std::mem::forget(crashed.reading(orphan, None, &master, ReadStage::Duration));

        assert!(ReadingMarks::at_launch(&support, &store, Utc::now()).holds(orphan));
        assert_eq!(markers(dir.path()), 1);
        std::fs::remove_file(&master).unwrap();
        assert!(!ReadingMarks::at_launch(&support, &store, Utc::now()).holds(orphan));
        assert_eq!(markers(dir.path()), 0);
    }

    /// The exit ended the reads still running, not their files: their
    /// markers go, and a read after it is not marked.
    #[test]
    fn the_exit_removes_the_open_markers_and_marks_no_more() {
        let dir = tempfile::tempdir().unwrap();
        let (store, meeting, asset) = store_with_meeting(dir.path());
        let marks = ReadingMarks::at_launch(&dir.path().join("support"), &store, Utc::now());
        let master = dir.path().join("recording.m4a");
        let running = marks.reading(meeting.id, Some(asset.id), &master, ReadStage::Transcribe);
        marks.exiting();
        assert_eq!(markers(dir.path()), 0);
        std::mem::forget(marks.reading(meeting.id, Some(asset.id), &master, ReadStage::Decode));
        assert_eq!(markers(dir.path()), 0);
        drop(running);
        assert!(
            !ReadingMarks::at_launch(&dir.path().join("support"), &store, Utc::now())
                .holds(meeting.id)
        );
    }

    #[test]
    fn in_memory_it_writes_and_holds_nothing() {
        let marks = ReadingMarks::in_memory();
        let meeting = Uuid::new_v4();
        std::mem::forget(marks.reading(meeting, None, Path::new("x"), ReadStage::Decode));
        assert!(!marks.holds(meeting));
        marks.release(meeting);
        marks.exiting();
    }
}
