//! The breaker for a recording that takes the app down while it is read
//! (A17 of `.plans/2026-10-07-stable-promotion.md`). Symphonia sizes some
//! allocations straight from a file's fields, and a failed allocation
//! aborts the process, which no `catch_unwind` stops; on the Mac, `CoreML`
//! runs in the app's process, and an abort inside it that no `@catch`
//! stops ends the app too. Unguarded, such a file ends the app at every
//! launch: recovery reads it again, and the launch resumes its meeting.
//! Rust only: Swift had no recovery to loop through.
//!
//! - **The mark.** Before a read of a meeting's recording, a file in
//!   [`ReadingMarks::FOLDER`] under the support directory names the
//!   meeting, the asset, the file, the [`ReadStage`] and the attempt. It is
//!   written durably (synced, renamed, the folder synced), so it is on disk
//!   before the reader sees a byte, and removed the same way when the read
//!   returns, with an error or a panic too. Each read has a file of its own
//!   (a random name), so two reads never share one. The pipeline writes
//!   and removes it off the async workers ([`ReadingMarks::marked`]), as
//!   both sync the disk.
//! - **What is marked.** Every read of a recording in the app's process:
//!   the decode of each lane (for the transcript and the diarizer), the
//!   persist stage's mixdown, recovery's read of an adopted recording's
//!   declared length (`steno-services`); and, where the speech engine runs
//!   in the app's process (`CoreML` on the Mac), the job's load of the
//!   engine and each transcription. Process again and the CLI's `process`
//!   and `reprocess` run the same stages. The speech sidecar and the
//!   diarizer run in a child, whose abort ends only the child.
//! - **An orderly exit** ([`ReadingMarks::exiting`], from
//!   [`ProcessingPipeline::quit`]) removes the marks of the reads still
//!   running and writes no more: the exit ended them, not the file.
//! - **The launch check** ([`ReadingMarks::at_launch`]): every mark left
//!   in the folder is a read that ended with the app (the database lock
//!   keeps a second process out). A force quit, a power loss or a restart
//!   for an update ends a healthy read the same way, so the breaker
//!   counts:
//!   - a meeting's first read that ended with the app costs nothing: the
//!     launch says so at `warn`, and the meeting is processed again as any
//!     unfinished one is; its reads are marked as a second attempt;
//!   - a second attempt that ended with the app quarantines the meeting:
//!     it fails with [`READ_STOPPED`], its asset loses a retention stamp so
//!     no sweep takes the file, and the launch logs it once at `warn`;
//!   - a run of the meeting whose reads all returned starts the count
//!     again ([`ReadingMarks::read_through`]). A read alone does not: a run
//!     reads a recording several times, and an earlier read that returns
//!     must not hide a later one that ends the app.
//!
//!   A mark whose meeting has no row yet (an orphan the launch was
//!   adopting) counts the same for the adoption, which adopts a
//!   quarantined recording failed and unread; one whose meeting and file
//!   are both gone is removed.
//! - **Never touched again automatically.** A quarantined meeting's
//!   marks stay on disk: they are the quarantine. The launch resume, the
//!   resumes after a model install or a busy store, and the adoption skip
//!   it ([`ReadingMarks::holds`]); the re-exports read no recording.
//! - **Process again** (the app's, and `steno process --meeting` or
//!   `steno reprocess`) releases the meeting ([`ReadingMarks::release`])
//!   and tries once more, as a second attempt: a crash in it quarantines
//!   the meeting again.
//!
//! The cost of a false alarm: two unclean ends in a row while the same
//! recording is read fail that one meeting with Process again offered, and
//! nothing is lost. So does a `CoreML` abort in another meeting's
//! transcription while this one waits for its turn at the engine
//! (`OneCallAtATime` in `steno-services`, which the pipeline cannot see):
//! the Transcribe mark spans the wait.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use steno_core::{MeetingState, Store};
use uuid::Uuid;

use crate::files::{
    Access, Disk, Syncs, create_dir_all_durably_with, remove_durably_with, replace_file_with,
};
use crate::pipeline::off_the_workers;

#[cfg(doc)]
use crate::ProcessingPipeline;

/// The reason a quarantined meeting fails with: one sentence, as the list
/// shows only the first.
pub const READ_STOPPED: &str = "Steno stopped while reading this recording; the recording is kept.";

/// What a mark says was going on.
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

/// One mark's body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Mark {
    meeting_id: Uuid,
    asset_id: Option<Uuid>,
    path: PathBuf,
    stage: ReadStage,
    started_at: DateTime<Utc>,
    /// 0 for a first attempt, 1 for one after a read that ended with the
    /// app.
    #[serde(default)]
    attempt: u32,
}

/// The marks of one support directory; see the module doc.
pub struct ReadingMarks {
    /// `None` in memory: nothing is written.
    folder: Option<PathBuf>,
    syncs: Arc<dyn Syncs + Send + Sync>,
    /// What the launch found; never held across a sync, so
    /// [`holds`](Self::holds) never waits on the disk.
    found: Mutex<Found>,
    /// Every mark of this process that may be on disk: each write or
    /// removal of one happens under this lock, so the exit, which takes
    /// it, removes every one and no write follows.
    open: Mutex<Open>,
}

impl std::fmt::Debug for ReadingMarks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadingMarks")
            .field("folder", &self.folder)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct Found {
    /// The meetings quarantined, with their marks' files.
    held: HashMap<Uuid, Vec<PathBuf>>,
    /// The meetings on their second attempt, with the marks of the
    /// first, which go once a run reads through.
    retrying: HashMap<Uuid, Retry>,
}

#[derive(Debug, Default)]
struct Retry {
    files: Vec<PathBuf>,
    /// Whether a read of the second attempt began.
    read: bool,
}

#[derive(Debug, Default)]
struct Open {
    /// Set by the exit: no read is marked any more.
    exiting: bool,
    files: HashSet<PathBuf>,
}

impl ReadingMarks {
    /// The folder of the marks in the support directory.
    pub const FOLDER: &'static str = "reading";

    /// Marks nothing and holds no meeting: the tests.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::with_syncs(None, Arc::new(Disk))
    }

    fn with_syncs(folder: Option<PathBuf>, syncs: Arc<dyn Syncs + Send + Sync>) -> Self {
        ReadingMarks {
            folder,
            syncs,
            found: Mutex::default(),
            open: Mutex::default(),
        }
    }

    /// The launch check over the marks in `support_directory` (see the
    /// module doc): a meeting with a mark of a second attempt left there
    /// is quarantined in `store`, one with only a first attempt's is tried
    /// once more. Call it once per launch, before anything reads a
    /// recording. A store that refuses the write leaves the meeting as it
    /// is, still held for this session, and the next launch tries again.
    pub fn at_launch(support_directory: &Path, store: &Store, now: DateTime<Utc>) -> Self {
        Self::at_launch_with(support_directory, store, now, Arc::new(Disk))
    }

    fn at_launch_with(
        support_directory: &Path,
        store: &Store,
        now: DateTime<Utc>,
        syncs: Arc<dyn Syncs + Send + Sync>,
    ) -> Self {
        let folder = support_directory.join(Self::FOLDER);
        let mut marks: BTreeMap<Uuid, Vec<(PathBuf, Mark)>> = BTreeMap::new();
        for (file, mark) in read_folder(&folder) {
            marks.entry(mark.meeting_id).or_default().push((file, mark));
        }
        let mut found = Found::default();
        for (meeting_id, left) in marks {
            let second = left.iter().any(|(_, mark)| mark.attempt > 0);
            let stages: Vec<ReadStage> = left.iter().map(|(_, mark)| mark.stage).collect();
            let asset_id = left.iter().find_map(|(_, mark)| mark.asset_id);
            let started = left.iter().map(|(_, mark)| mark.started_at).min();
            let files: Vec<PathBuf> = match store.meeting(meeting_id) {
                Ok(Some(meeting)) if second => {
                    tracing::warn!(%meeting_id, ?asset_id, ?stages, ?started, "Steno stopped twice while reading this recording; it is kept and not read again until Process again");
                    quarantine(store, &meeting, now);
                    left.into_iter().map(|(file, _)| file).collect()
                }
                Ok(None) if !left.iter().any(|(_, mark)| mark.path.exists()) => {
                    for (file, _) in left {
                        let _ = remove_durably_with(&*syncs, &file);
                    }
                    continue;
                }
                Ok(_) if second => {
                    tracing::warn!(%meeting_id, ?stages, ?started, "Steno stopped twice while reading a recording with no meeting; it is listed failed and unread");
                    left.into_iter().map(|(file, _)| file).collect()
                }
                Ok(_) => {
                    tracing::warn!(%meeting_id, ?asset_id, ?stages, ?started, "Steno stopped while reading this recording; it is read once more");
                    let files = left.into_iter().map(|(file, _)| file).collect();
                    found
                        .retrying
                        .insert(meeting_id, Retry { files, read: false });
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%meeting_id, %error, "a meeting whose read ended with the app could not be read; it stays held");
                    left.into_iter().map(|(file, _)| file).collect()
                }
            };
            found.held.insert(meeting_id, files);
        }
        let marks = Self::with_syncs(Some(folder), syncs);
        *marks.found() = found;
        marks
    }

    /// Whether the launch quarantined `meeting_id` and nothing released it.
    #[must_use]
    pub fn holds(&self, meeting_id: Uuid) -> bool {
        self.found().held.contains_key(&meeting_id)
    }

    /// Process again: `meeting_id` is no longer held, and its next run
    /// reads the recording once more, as a second attempt. Its marks stay
    /// on disk until that run reads through, so a crash before it reads
    /// quarantines it again. Writes nothing: callers need not leave the
    /// async workers.
    pub fn release(&self, meeting_id: Uuid) {
        let mut found = self.found();
        if let Some(files) = found.held.remove(&meeting_id) {
            found
                .retrying
                .insert(meeting_id, Retry { files, read: false });
        }
    }

    /// A run of `meeting_id` ended with every read it began returned: the
    /// count starts again, and the marks of an attempt that ended with
    /// the app go. Syncs the disk for a meeting on its second attempt.
    pub fn read_through(&self, meeting_id: Uuid) {
        let files = {
            let mut found = self.found();
            if !found
                .retrying
                .get(&meeting_id)
                .is_some_and(|retry| retry.read)
            {
                return;
            }
            found
                .retrying
                .remove(&meeting_id)
                .map(|retry| retry.files)
                .unwrap_or_default()
        };
        for file in files {
            if let Err(error) = remove_durably_with(&*self.syncs, &file) {
                tracing::warn!(%meeting_id, %error, "a reading mark of an earlier attempt could not be removed");
            }
        }
    }

    /// A read of `path`, the recording of `meeting_id` (`asset_id` once it
    /// has one), in `stage`, begins: the mark is on disk before this
    /// returns, and goes when the guard drops. Syncs the disk: the pipeline
    /// calls [`marked`](Self::marked) instead. A mark that cannot be
    /// written leaves the read unguarded, logged at `warn`; whatever of it
    /// reached the disk still goes with the guard.
    pub fn reading(
        self: &Arc<Self>,
        meeting_id: Uuid,
        asset_id: Option<Uuid>,
        path: &Path,
        stage: ReadStage,
    ) -> Reading {
        let mut reading = Reading {
            marks: Arc::clone(self),
            file: None,
        };
        let Some(folder) = &self.folder else {
            return reading;
        };
        let attempt = self
            .found()
            .retrying
            .get_mut(&meeting_id)
            .map_or(0, |retry| {
                retry.read = true;
                1
            });
        let mut open = self.open();
        if open.exiting {
            return reading;
        }
        let mark = Mark {
            meeting_id,
            asset_id,
            path: path.to_path_buf(),
            stage,
            started_at: Utc::now(),
            attempt,
        };
        let file = folder.join(format!("{}.json", Uuid::new_v4()));
        // Listed before the write: a write that fails after its rename
        // leaves the file, which the guard and the exit still remove.
        open.files.insert(file.clone());
        reading.file = Some(file.clone());
        let written = create_dir_all_durably_with(&*self.syncs, folder)
            .and_then(|()| serde_json::to_vec(&mark).map_err(std::io::Error::other))
            .and_then(|bytes| replace_file_with(&*self.syncs, &file, &bytes, Access::Default));
        if let Err(error) = written {
            tracing::warn!(%meeting_id, %error, "a reading mark could not be written; the read runs unguarded");
        }
        reading
    }

    /// `read`, marked as [`reading`](Self::reading) marks it, with the
    /// mark written and removed off the async workers. The guard is made
    /// on the blocking pool and handed back, so a caller cancelled while
    /// the write runs drops it there, and its mark goes. With the runtime
    /// shutting down, `read` runs unmarked.
    pub async fn marked<T>(
        self: &Arc<Self>,
        meeting_id: Uuid,
        asset_id: Option<Uuid>,
        path: PathBuf,
        stage: ReadStage,
        read: impl Future<Output = T>,
    ) -> T {
        let marks = Arc::clone(self);
        let guard =
            off_the_workers(move || marks.reading(meeting_id, asset_id, &path, stage)).await;
        let value = read.await;
        if let Ok(guard) = guard {
            let _ = off_the_workers(move || drop(guard)).await;
        }
        value
    }

    /// The orderly exit: the marks of the reads still running go, and no
    /// read is marked any more.
    pub fn exiting(&self) {
        let mut open = self.open();
        open.exiting = true;
        let syncs = &*self.syncs;
        open.files.retain(|file| {
            remove_durably_with(syncs, file)
                .inspect_err(|error| {
                    tracing::warn!(%error, "a reading mark could not be removed at the exit");
                })
                .is_err()
        });
    }

    /// The guard of `file` dropped: the read returned. A removal that fails
    /// keeps the file listed, so the exit tries again.
    fn close(&self, file: &Path) {
        let mut open = self.open();
        if !open.files.contains(file) {
            return;
        }
        match remove_durably_with(&*self.syncs, file) {
            Ok(()) => {
                open.files.remove(file);
            }
            Err(error) => {
                tracing::warn!(%error, "a reading mark could not be removed; the next launch counts it as a read that ended with the app");
            }
        }
    }

    fn found(&self) -> MutexGuard<'_, Found> {
        self.found.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn open(&self) -> MutexGuard<'_, Open> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A read in progress; see [`ReadingMarks::reading`].
#[must_use = "the read is marked only while the guard lives"]
pub struct Reading {
    marks: Arc<ReadingMarks>,
    /// The mark's file; `None` for a read left unmarked.
    file: Option<PathBuf>,
}

impl Drop for Reading {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            self.marks.close(file);
        }
    }
}

/// The marks in `folder` with their files; a file that does not parse
/// (not one this module wrote) is logged and left.
fn read_folder(folder: &Path) -> Vec<(PathBuf, Mark)> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let mark = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Mark>(&bytes).ok());
            if mark.is_none() {
                tracing::debug!(path = %path.display(), "not a reading mark; left alone");
            }
            mark.map(|mark| (path, mark))
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
    use std::fs::File;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};

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

    fn on_disk(dir: &Path) -> usize {
        std::fs::read_dir(dir.join("support").join(ReadingMarks::FOLDER)).map_or(0, Iterator::count)
    }

    fn launch(dir: &Path, store: &Store) -> Arc<ReadingMarks> {
        Arc::new(ReadingMarks::at_launch(
            &dir.join("support"),
            store,
            Utc::now(),
        ))
    }

    /// A launch whose read of `meeting_id`'s recording ended with the app.
    fn killed_in_a_read(marks: &Arc<ReadingMarks>, meeting_id: Uuid, master: &Path) {
        std::mem::forget(marks.reading(meeting_id, None, master, ReadStage::Decode));
    }

    #[test]
    fn a_read_is_marked_on_disk_until_it_returns() {
        let dir = tempfile::tempdir().unwrap();
        let (store, meeting, asset) = store_with_meeting(dir.path());
        let marks = launch(dir.path(), &store);
        let master = dir.path().join("recording.m4a");
        let first = marks.reading(meeting.id, Some(asset.id), &master, ReadStage::Decode);
        let second = marks.reading(meeting.id, Some(asset.id), &master, ReadStage::Mixdown);
        assert_eq!(on_disk(dir.path()), 2, "on disk before the read, one each");
        drop(first);
        assert_eq!(on_disk(dir.path()), 1);
        drop(second);
        assert_eq!(on_disk(dir.path()), 0);
        assert!(!marks.holds(meeting.id));
    }

    /// The first read that ended with the app costs nothing: the meeting
    /// is read once more. The second in a row fails it with the plain
    /// reason, takes the retention stamp off its asset and keeps the file;
    /// it stays held, at the next launch too, until Process again releases
    /// it, whose run is a second attempt again.
    #[test]
    fn the_second_read_in_a_row_that_ends_with_the_app_quarantines_its_meeting() {
        let dir = tempfile::tempdir().unwrap();
        let (store, meeting, _) = store_with_meeting(dir.path());
        let master = dir.path().join("recording.m4a");
        killed_in_a_read(&launch(dir.path(), &store), meeting.id, &master);

        let once = launch(dir.path(), &store);
        assert!(!once.holds(meeting.id), "a first crash costs nothing");
        assert_eq!(
            store.meeting(meeting.id).unwrap().unwrap().state,
            MeetingState::Ready
        );
        assert_eq!(on_disk(dir.path()), 1, "kept until a run reads through");
        killed_in_a_read(&once, meeting.id, &master);

        let marks = launch(dir.path(), &store);
        assert!(marks.holds(meeting.id));
        let failed = MeetingState::Failed {
            reason: READ_STOPPED.to_owned(),
        };
        assert_eq!(store.meeting(meeting.id).unwrap().unwrap().state, failed);
        assert_eq!(store.asset(meeting.id).unwrap().unwrap().expires_at, None);
        assert_eq!(std::fs::read(&master).unwrap(), b"audio");
        assert_eq!(on_disk(dir.path()), 2, "the marks are the quarantine");
        assert!(launch(dir.path(), &store).holds(meeting.id));

        let again = launch(dir.path(), &store);
        again.release(meeting.id);
        assert!(!again.holds(meeting.id));
        assert_eq!(on_disk(dir.path()), 2, "until the run reads through");
        killed_in_a_read(&again, meeting.id, &master);
        assert!(
            launch(dir.path(), &store).holds(meeting.id),
            "Process again is a second attempt"
        );

        let released = launch(dir.path(), &store);
        released.release(meeting.id);
        drop(released.reading(meeting.id, None, &master, ReadStage::Decode));
        released.read_through(meeting.id);
        assert_eq!(on_disk(dir.path()), 0);
        assert!(!launch(dir.path(), &store).holds(meeting.id));
    }

    /// A run that reads through starts the count again, so a later crash
    /// is a first one; a run that read nothing (refused before it read)
    /// keeps the count.
    #[test]
    fn a_run_that_reads_through_starts_the_count_again() {
        let dir = tempfile::tempdir().unwrap();
        let (store, meeting, _) = store_with_meeting(dir.path());
        let master = dir.path().join("recording.m4a");
        killed_in_a_read(&launch(dir.path(), &store), meeting.id, &master);
        let marks = launch(dir.path(), &store);
        marks.read_through(meeting.id);
        assert_eq!(on_disk(dir.path()), 1, "nothing read: the count stays");
        drop(marks.reading(meeting.id, None, &master, ReadStage::Decode));
        marks.read_through(meeting.id);
        assert_eq!(on_disk(dir.path()), 0);
        killed_in_a_read(&marks, meeting.id, &master);
        assert!(
            !launch(dir.path(), &store).holds(meeting.id),
            "a first crash again"
        );
    }

    /// An orphan the launch was adopting has no row: its marks count the
    /// same, and stay held while the file is there; they go once the file
    /// is gone too.
    #[test]
    fn an_orphans_marks_wait_for_the_adoption_and_go_with_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
        let orphan = Uuid::new_v4();
        let master = dir.path().join("orphan.m4a");
        std::fs::write(&master, b"audio").unwrap();
        killed_in_a_read(&launch(dir.path(), &store), orphan, &master);
        let once = launch(dir.path(), &store);
        assert!(!once.holds(orphan));
        killed_in_a_read(&once, orphan, &master);

        assert!(launch(dir.path(), &store).holds(orphan));
        assert_eq!(on_disk(dir.path()), 2);
        std::fs::remove_file(&master).unwrap();
        assert!(!launch(dir.path(), &store).holds(orphan));
        assert_eq!(on_disk(dir.path()), 0);
    }

    /// The exit ended the reads still running, not their files: their
    /// marks go, and a read after it is not marked.
    #[test]
    fn the_exit_removes_the_open_marks_and_marks_no_more() {
        let dir = tempfile::tempdir().unwrap();
        let (store, meeting, asset) = store_with_meeting(dir.path());
        let marks = launch(dir.path(), &store);
        let master = dir.path().join("recording.m4a");
        let running = marks.reading(meeting.id, Some(asset.id), &master, ReadStage::Transcribe);
        marks.exiting();
        assert_eq!(on_disk(dir.path()), 0);
        std::mem::forget(marks.reading(meeting.id, Some(asset.id), &master, ReadStage::Decode));
        assert_eq!(on_disk(dir.path()), 0);
        drop(running);
        let launched = launch(dir.path(), &store);
        assert!(!launched.holds(meeting.id));
        drop(launched.reading(meeting.id, None, &master, ReadStage::Decode));
        assert_eq!(on_disk(dir.path()), 0);
    }

    /// The syncs of the tests below: the disk's, with the folder's sync
    /// failing once `fail_folder` is set, and every file's sync waiting at
    /// `gate` until it opens, or for five seconds (`timed_out`): a sync on
    /// the one async worker would hold the test there.
    #[derive(Default)]
    struct Gated {
        fail_folder: AtomicBool,
        gate: Mutex<Option<bool>>,
        changed: Condvar,
        timed_out: AtomicBool,
    }

    impl Gated {
        /// Closes the gate: the next file sync says so and waits.
        fn close(&self) {
            *self.gate.lock().unwrap() = Some(false);
        }

        fn open(&self) {
            *self.gate.lock().unwrap() = None;
            self.changed.notify_all();
        }

        /// Whether a file sync waits at the gate now, or gave up.
        fn waiting(&self) -> bool {
            *self.gate.lock().unwrap() == Some(true) || self.timed_out.load(Ordering::SeqCst)
        }
    }

    impl Syncs for Gated {
        fn file(&self, file: &File, _: &Path) -> std::io::Result<()> {
            let mut gate = self.gate.lock().unwrap();
            if gate.is_some() {
                *gate = Some(true);
                let (_gate, waited) = self
                    .changed
                    .wait_timeout_while(gate, std::time::Duration::from_secs(5), |gate| {
                        gate.is_some()
                    })
                    .unwrap();
                if waited.timed_out() {
                    self.timed_out.store(true, Ordering::SeqCst);
                }
            }
            file.sync_all()
        }

        fn directory(&self, _: &Path) -> std::io::Result<()> {
            if self.fail_folder.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("the folder did not sync"));
            }
            Ok(())
        }
    }

    fn launch_with(dir: &Path, store: &Store, syncs: Arc<Gated>) -> Arc<ReadingMarks> {
        Arc::new(ReadingMarks::at_launch_with(
            &dir.join("support"),
            store,
            Utc::now(),
            syncs,
        ))
    }

    /// A mark whose write fails after its rename (the folder's sync, as
    /// on Windows) is on disk all the same, and goes with its guard: the
    /// next launch finds nothing.
    #[test]
    fn a_mark_whose_write_failed_after_the_rename_goes_with_its_read() {
        let dir = tempfile::tempdir().unwrap();
        let (store, meeting, _) = store_with_meeting(dir.path());
        std::fs::create_dir_all(dir.path().join("support").join(ReadingMarks::FOLDER)).unwrap();
        let syncs = Arc::new(Gated::default());
        let marks = launch_with(dir.path(), &store, syncs.clone());
        syncs.fail_folder.store(true, Ordering::SeqCst);
        let master = dir.path().join("recording.m4a");
        let reading = marks.reading(meeting.id, None, &master, ReadStage::Decode);
        assert_eq!(on_disk(dir.path()), 1, "renamed into place");
        drop(reading);
        assert_eq!(on_disk(dir.path()), 0);
        let _ = std::fs::remove_dir_all(dir.path().join("support").join(ReadingMarks::FOLDER));
        assert!(!launch(dir.path(), &store).holds(meeting.id));
    }

    /// A slow sync of the mark keeps the one async worker free: another
    /// task runs, and [`ReadingMarks::holds`] answers, while it waits; the
    /// read begins once the mark is on disk, and the mark goes after.
    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_mark_sync_does_not_hold_the_async_worker() {
        let dir = tempfile::tempdir().unwrap();
        let (store, meeting, asset) = store_with_meeting(dir.path());
        let syncs = Arc::new(Gated::default());
        let marks = launch_with(dir.path(), &store, syncs.clone());
        syncs.close();
        let master = dir.path().join("recording.m4a");
        let folder = dir.path().to_path_buf();
        let read = tokio::spawn({
            let marks = Arc::clone(&marks);
            async move {
                marks
                    .marked(
                        meeting.id,
                        Some(asset.id),
                        master,
                        ReadStage::Decode,
                        async move { on_disk(&folder) },
                    )
                    .await
            }
        });
        while !syncs.waiting() {
            tokio::task::yield_now().await;
        }
        let other = tokio::spawn(async { 7 });
        assert_eq!(other.await.unwrap(), 7, "the worker ran another task");
        assert!(!marks.holds(meeting.id));
        assert!(!read.is_finished());
        syncs.open();
        assert_eq!(read.await.unwrap(), 1, "the mark was on disk for the read");
        assert_eq!(on_disk(dir.path()), 0);
        assert!(
            !syncs.timed_out.load(Ordering::SeqCst),
            "the sync held the worker"
        );
    }

    #[test]
    fn in_memory_it_writes_and_holds_nothing() {
        let marks = Arc::new(ReadingMarks::in_memory());
        let meeting = Uuid::new_v4();
        std::mem::forget(marks.reading(meeting, None, Path::new("x"), ReadStage::Decode));
        assert!(!marks.holds(meeting));
        marks.release(meeting);
        marks.read_through(meeting);
        marks.exiting();
    }
}
