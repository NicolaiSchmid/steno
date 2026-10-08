//! Launch recovery's guard against a crash loop. Rust only: the Swift
//! pipeline resumed an unfinished meeting at every launch, however often
//! its processing had taken the app down.
//!
//! - **What is counted.** Each background run of a meeting adds one to a
//!   count in the meeting's audio folder (`.processing-runs`,
//!   [`RecordingLayout::processing_runs`]) before it starts
//!   (`CountedRun`). A run that ends while the app lives (ready, failed
//!   or panicked) clears the count, earlier crashes included. The exit
//!   ([`QuitLatch::set`]) takes each open run back off, one each, on every
//!   pipeline that shares the latch, so earlier crashes still count. What
//!   is left is the runs that ended with the app: an abort, an
//!   out-of-memory kill, a power loss. A panic the app survives already
//!   fails the meeting through the pipeline's panic wrap, so it is not
//!   counted as well.
//! - **Who is charged.** Every run alive at a crash. So launch recovery
//!   runs a meeting with a count alone, after the meetings without one
//!   and oldest first, and counts it only when its turn comes: a meeting
//!   that takes the app down is charged for its own crashes, and another
//!   meeting at most once, for a run it shared with one before it had a
//!   count. The price: a run started at once that never ends (a
//!   destination on a share that does not answer, say; the language model
//!   has a timeout) holds every meeting with a count back for the rest of
//!   the session. Nothing is lost: those runs were never counted, so the
//!   next launch resumes them.
//! - **When it gives up.** Launch recovery resumes a meeting while fewer
//!   than [`MAX_CRASHED_RUNS`] of its runs ended with the app, and after
//!   that marks it failed with [`TOO_MANY_CRASHED_RUNS`], so a meeting
//!   whose processing takes the app down is not retried at every launch
//!   for ever.
//! - **What the user sees.** The meeting, failed with that reason, and its
//!   audio, which is kept.
//! - **Starting over.** `enqueue` and `reprocess` (a new recording, or the
//!   meeting processed again) clear the count before their run.
//!
//! A file and not a column: the count lives only while a meeting is
//! processed, the Swift app (which shares the database until the Mac
//! cutover) never sees it, and it needs no migration. Every write goes
//! through [`replace_file`], so a crash leaves the old count or the new
//! one. A folder that cannot be written, or a master outside a folder
//! named after its meeting, leaves the meeting unguarded, as before.

use std::collections::HashMap;
use std::path::PathBuf;

use steno_core::json::uuid_string;
use steno_core::{AudioAsset, RecordingLayout};

use crate::QuitLatch;
use crate::files::{Access, replace_file};

/// Runs that may end with the app before launch recovery gives up on a
/// meeting. One such end can be anything (another program's memory, a
/// power loss, a forced restart for an update); a second on the same
/// meeting is suspect; a third is a pattern. Each costs a launch's worth of
/// processing and can take a live recording down with the app, so three
/// bounds the damage at two more crashes without giving up on a meeting
/// over one or two that were not its fault.
pub const MAX_CRASHED_RUNS: u32 = 3;

/// The failure reason launch recovery gives a meeting it stops resuming:
/// one sentence, since the meeting list and the detail show only the
/// first.
pub const TOO_MANY_CRASHED_RUNS: &str = "Steno closed unexpectedly 3 times while processing this recording and stopped trying; the recording is kept.";

/// One meeting's count; see the module doc.
#[derive(Debug, Clone)]
pub(crate) struct RunCount {
    /// `None` when the master is not in a folder of its own.
    path: Option<PathBuf>,
}

impl RunCount {
    /// The count in the master's folder when that folder is named after
    /// the meeting's id, as the recording layout names it, so two meetings
    /// never share one count. Any other folder (`files_to_remove` makes the
    /// same check) leaves the meeting unguarded.
    pub(crate) fn of(asset: &AudioAsset) -> Self {
        let folder = uuid_string(asset.meeting_id);
        Self {
            path: RecordingLayout::from_asset(asset)
                .filter(|layout| {
                    layout
                        .directory
                        .file_name()
                        .is_some_and(|name| name == folder.as_str())
                })
                .map(|layout| layout.processing_runs()),
        }
    }

    /// Runs that ended with the app since the count was last cleared; a
    /// missing or unreadable file is none.
    pub(crate) fn read(&self) -> u32 {
        self.path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    /// A run starts.
    fn started(&self) {
        self.write(self.read().saturating_add(1));
    }

    /// The exit stopped a run: it does not count.
    fn taken_back(&self) {
        self.write(self.read().saturating_sub(1));
    }

    /// The meeting ended ready or failed, or is processed afresh: the
    /// count goes.
    pub(crate) fn clear(&self) {
        if let Some(path) = &self.path
            && let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(%error, "the processing run count could not be cleared");
        }
    }

    fn write(&self, count: u32) {
        if count == 0 {
            return self.clear();
        }
        let Some(path) = &self.path else {
            return;
        };
        if let Err(error) = replace_file(path, count.to_string().as_bytes(), Access::Default) {
            tracing::debug!(%error, "the processing run count could not be written");
        }
    }
}

/// The runs counted and still going, behind the [`QuitLatch`]'s lock with
/// its flag, so a run is counted only before the exit and the exit takes
/// back every run counted before it.
#[derive(Debug, Default)]
pub(crate) struct OpenRuns {
    next: u64,
    runs: HashMap<u64, RunCount>,
}

impl OpenRuns {
    /// Counts a run; returns its key.
    pub(crate) fn open(&mut self, count: RunCount) -> u64 {
        count.started();
        let key = self.next;
        self.next += 1;
        self.runs.insert(key, count);
        key
    }

    /// Ends the run under `key`, counted under `count`. If the run is still
    /// open, it failed or panicked before the exit, which the pipeline
    /// reports, and the count goes. If the exit took it back, the count
    /// stays, so earlier crashes still count, unless the run made the
    /// meeting ready.
    pub(crate) fn close(&mut self, key: u64, count: &RunCount, succeeded: bool) {
        if self.runs.remove(&key).is_some() || succeeded {
            count.clear();
        }
    }

    /// The exit: every open run is taken back.
    pub(crate) fn take_back_all(&mut self) {
        for (_, count) in self.runs.drain() {
            count.taken_back();
        }
    }
}

/// A background run counted in its meeting's folder; see the module doc.
/// Dropping it ends the run: every end the app lives through drops it,
/// a return or a panic unwinding through the task. A run that ends with
/// the app is never dropped and leaves its count.
pub(crate) struct CountedRun {
    latch: QuitLatch,
    key: u64,
    count: RunCount,
    succeeded: bool,
}

impl CountedRun {
    /// Counts a run of `count`'s meeting; `None` once the app is exiting,
    /// when nothing starts.
    pub(crate) fn start(latch: &QuitLatch, count: RunCount) -> Option<Self> {
        let key = latch.open_run(count.clone())?;
        Some(Self {
            latch: latch.clone(),
            key,
            count,
            succeeded: false,
        })
    }

    /// The run made the meeting ready: the count goes, whether or not the
    /// exit took the run back first.
    pub(crate) fn succeeded(&mut self) {
        self.succeeded = true;
    }
}

impl Drop for CountedRun {
    fn drop(&mut self) {
        self.latch.close_run(self.key, &self.count, self.succeeded);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use steno_core::paths::file_url;
    use steno_core::{AudioFormat, AudioLane, AudioRetention};
    use uuid::Uuid;

    use super::*;

    fn asset_in(folder: &Path, meeting_id: Uuid) -> AudioAsset {
        AudioAsset {
            id: Uuid::new_v4(),
            meeting_id,
            url: file_url(&folder.join("recording.caf"), false),
            format: AudioFormat::Caf48kFloat32,
            lanes: vec![AudioLane::Mixed],
            sidecars_16k: BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::KeepForever,
            expires_at: None,
        }
    }

    /// The count lives only in a folder named after the meeting's id, so
    /// two meetings whose masters share a folder share no count.
    #[test]
    fn a_master_outside_its_meetings_folder_is_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let meeting_id = Uuid::new_v4();
        let own = RecordingLayout::new(dir.path(), meeting_id);
        own.create_directories(false).unwrap();
        let counted = RunCount::of(&asset_in(&own.directory, meeting_id));
        counted.started();
        assert_eq!(counted.read(), 1);
        assert!(own.processing_runs().exists());

        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        for _ in 0..2 {
            let count = RunCount::of(&asset_in(&shared, Uuid::new_v4()));
            count.started();
            assert_eq!(count.read(), 0);
        }
        assert!(
            !RecordingLayout::from_directory(&shared)
                .processing_runs()
                .exists()
        );
    }
}
