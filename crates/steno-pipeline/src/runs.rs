//! Launch recovery's guard against a crash loop. Rust only: the Swift
//! pipeline resumed an unfinished meeting at every launch, however often
//! its processing had taken the app down.
//!
//! Each background run of a meeting is counted in the meeting's audio
//! folder (`.processing-runs`, [`RecordingLayout::processing_runs`]) before
//! it starts, and the count is settled whenever the process lives to see
//! the run end: its return, ready or failed; a panic unwinding through it;
//! the exit stopping it (which takes back its own run only). A count left
//! behind is a run that ended with the process: an abort, an out-of-memory
//! kill, a power loss. That is the one kind of end it is for: a panic the
//! process survives is the pipeline's to report, and counting it too would
//! cap it twice. Launch recovery resumes a meeting while fewer than
//! [`MAX_UNSETTLED_RUNS`] runs have ended so, and marks it failed with
//! [`TOO_MANY_UNSETTLED_RUNS`] after that, so a meeting whose processing
//! takes the app down is not retried at every launch for ever. The audio
//! is kept; `enqueue` and `reprocess` (a new recording, or the meeting
//! processed again) start the count afresh.
//!
//! A file and not a column: the count lives only while a meeting is
//! processed, the Swift app (which shares the database until the Mac
//! cutover) never sees it, and it needs no migration. Every write goes
//! through [`replace_file`], so a crash leaves the old count or the new
//! one. A folder that cannot be written leaves the meeting unguarded, as
//! before.

use std::path::PathBuf;

use steno_core::{AudioAsset, RecordingLayout};

use crate::files::{Access, replace_file};

/// Runs that may end with the app before launch recovery gives up on a
/// meeting. One such end can be anything (another program's memory, a
/// power loss, a forced restart for an update); a second on the same
/// meeting is suspect; a third is a pattern. Each costs a launch's worth of
/// processing and can take a live recording down with the app, so three
/// bounds the damage at two more crashes without giving up on a meeting
/// over one or two that were not its fault.
pub const MAX_UNSETTLED_RUNS: u32 = 3;

/// The failure reason launch recovery gives a meeting it stops resuming.
pub const TOO_MANY_UNSETTLED_RUNS: &str = "Steno closed unexpectedly 3 times while processing this recording, so it was not tried again. The recording is kept.";

/// One meeting's count; see the module doc.
#[derive(Debug, Clone)]
pub(crate) struct Runs {
    /// `None` when the asset has no folder on disk.
    path: Option<PathBuf>,
}

impl Runs {
    pub(crate) fn of(asset: &AudioAsset) -> Self {
        Self {
            path: RecordingLayout::from_asset(asset).map(|layout| layout.processing_runs()),
        }
    }

    /// Runs that ended with the app since the count was last cleared.
    pub(crate) fn unsettled(&self) -> u32 {
        self.path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    /// A run starts.
    pub(crate) fn started(&self) {
        self.write(self.unsettled().saturating_add(1));
    }

    /// The exit stopped the run: it is not counted.
    pub(crate) fn stopped_by_exit(&self) {
        self.write(self.unsettled().saturating_sub(1));
    }

    /// The meeting settled (ready, failed, or given up on), or is processed
    /// afresh: the count goes.
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
