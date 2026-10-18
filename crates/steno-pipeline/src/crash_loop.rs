//! Launch recovery's guard against a crash loop. Each background run adds
//! one to a count in the meeting's folder (`.processing-runs`); a run that
//! ends while the app lives clears it and the exit takes open runs back, so
//! what remains are runs that ended with the app. Launch recovery fails a
//! meeting with [`TOO_MANY_CRASHED_RUNS`] at [`MAX_CRASHED_RUNS`].

use std::collections::HashMap;
use std::path::PathBuf;

use steno_core::{AudioAsset, RecordingLayout};

use crate::QuitLatch;
use crate::files::{Access, replace_file};

/// Runs that may end with the app before launch recovery gives up on a
/// meeting: one crash can be anything, a third is a pattern.
pub const MAX_CRASHED_RUNS: u32 = 3;

/// The failure reason launch recovery gives a meeting it stops resuming:
/// one sentence, since the list shows only the first.
pub const TOO_MANY_CRASHED_RUNS: &str = "Steno closed unexpectedly 3 times while processing this recording and stopped trying; the recording is kept.";

/// One meeting's count.
#[derive(Debug, Clone)]
pub(crate) struct RunCount {
    /// `None` when the master is not in a folder of its own.
    path: Option<PathBuf>,
}

impl RunCount {
    /// The count in the master's folder when that folder is the meeting's own
    /// ([`RecordingLayout::own_folder`]), so two meetings never share one;
    /// any other folder leaves the meeting unguarded.
    pub(crate) fn of(asset: &AudioAsset) -> Self {
        Self {
            path: RecordingLayout::own_folder(asset).map(|layout| layout.processing_runs()),
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

    fn started(&self) {
        self.write(self.read().saturating_add(1));
    }

    /// The exit stopped a run: it does not count.
    fn taken_back(&self) {
        self.write(self.read().saturating_sub(1));
    }

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

/// The runs counted and still going, behind the [`QuitLatch`]'s lock, so a
/// run is counted only before the exit and the exit takes back every one.
#[derive(Debug, Default)]
pub(crate) struct OpenRuns {
    next: u64,
    runs: HashMap<u64, RunCount>,
}

impl OpenRuns {
    pub(crate) fn open(&mut self, count: RunCount) -> u64 {
        count.started();
        let key = self.next;
        self.next += 1;
        self.runs.insert(key, count);
        key
    }

    /// Ends the run under `key`. A run still open failed before the exit and
    /// its count goes; one the exit took back keeps its count unless it made
    /// the meeting ready.
    pub(crate) fn close(&mut self, key: u64, count: &RunCount, succeeded: bool) {
        if self.runs.remove(&key).is_some() || succeeded {
            count.clear();
        }
    }

    pub(crate) fn take_back_all(&mut self) {
        for (_, count) in self.runs.drain() {
            count.taken_back();
        }
    }
}

/// A background run counted in its meeting's folder. Every end the app
/// lives through drops it; a run that ends with the app leaves its count.
pub(crate) struct CountedRun {
    latch: QuitLatch,
    key: u64,
    count: RunCount,
    succeeded: bool,
}

impl CountedRun {
    /// Counts a run; `None` once the app is exiting.
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

    /// Two meetings whose masters share a folder share no count.
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
