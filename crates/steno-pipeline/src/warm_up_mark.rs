//! The breaker for a warm-up that takes the app down. The recorder loads
//! an in-process speech engine (`CoreML` on the Mac) while a recording
//! runs ([`ProcessingPipeline::warm_up_while_recording`]). An Objective-C
//! exception there is caught at the model call, but an abort that no
//! `@catch` stops (a C++ exception, an allocation that fails) ends the app
//! and the recording with it, and would end every later recording at the
//! same point too. So the warm-up is marked on disk while it runs, and a
//! mark left over turns it off. Rust only: Swift warmed up at every
//! recording.
//!
//! - **The mark**: [`WarmUpMark::FILE_NAME`] in the support directory,
//!   written durably ([`replace_file`]: synced, renamed, the folder synced)
//!   before the warm-up starts and removed the same way once it returns,
//!   failed or not, since the app lived through it.
//! - **The launch check**: [`WarmUpMark::at_launch`]. A mark there is a
//!   warm-up that ended with the app, so from then on recordings skip the
//!   warm-up, and the launch says so once at `warn`. The first meeting then
//!   loads the models in its processing, after the recording.
//! - **Clearing it**: only a speech engine that loaded in a meeting's
//!   processing ([`WarmUpMark::loaded`]): the mark goes and the next
//!   recording warms up again. A load that takes the app down there leaves
//!   the mark, and the meeting to the pipeline's crash-loop guard
//!   ([`crate::crash_loop`]).
//!
//! A folder that cannot be written leaves the warm-up unguarded, as
//! before: the failure is logged and the warm-up runs.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::files::{Access, remove_durably, replace_file};

#[cfg(doc)]
use crate::ProcessingPipeline;

/// The mark, and whether the warm-up is off; see the module doc.
#[derive(Debug)]
pub struct WarmUpMark {
    /// `None` in memory (the tests, the CLI): nothing is written.
    path: Option<PathBuf>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// A mark outlived its process: no warm-up until a load clears it.
    skipping: bool,
    /// Warm-ups running now; the first writes the mark, the last removes it.
    running: usize,
}

impl WarmUpMark {
    /// The mark's name in the support directory.
    pub const FILE_NAME: &'static str = "warm-up-in-progress";

    /// A mark that writes nothing and never turns the warm-up off.
    #[must_use]
    pub fn in_memory() -> Self {
        WarmUpMark {
            path: None,
            state: Mutex::default(),
        }
    }

    /// The launch check over the mark in `support_directory`: a mark left
    /// there turns the warm-up off until a load clears it, and is logged
    /// once at `warn`. Call it once per launch, before any recording.
    #[must_use]
    pub fn at_launch(support_directory: &Path) -> Self {
        let path = support_directory.join(Self::FILE_NAME);
        let skipping = path.exists();
        if skipping {
            tracing::warn!(
                "the last warm-up of the speech models ended with the app; recordings skip it \
                 until a meeting's processing loads the models"
            );
        }
        WarmUpMark {
            path: Some(path),
            state: Mutex::new(State {
                skipping,
                running: 0,
            }),
        }
    }

    /// Whether recordings skip the warm-up now.
    #[must_use]
    pub fn skipping(&self) -> bool {
        self.state().skipping
    }

    /// A warm-up starts: the mark is on disk before this returns. `None`
    /// while the warm-up is off.
    pub(crate) fn begin(&self) -> Option<WarmingUp<'_>> {
        let mut state = self.state();
        if state.skipping {
            return None;
        }
        state.running += 1;
        if state.running == 1
            && let Some(path) = &self.path
            && let Err(error) = replace_file(path, b"1\n", Access::Default)
        {
            tracing::debug!(%error, "the warm-up mark could not be written; the warm-up runs unguarded");
        }
        Some(WarmingUp(self))
    }

    /// A meeting's processing loaded the speech engine: the mark goes and
    /// recordings warm up again (the mark of a warm-up running now stays
    /// until it returns).
    pub fn loaded(&self) {
        let mut state = self.state();
        let was_skipping = std::mem::take(&mut state.skipping);
        if state.running == 0 && was_skipping {
            self.remove();
        }
    }

    fn remove(&self) {
        if let Some(path) = &self.path
            && let Err(error) = remove_durably(path)
        {
            tracing::debug!(%error, "the warm-up mark could not be removed");
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A warm-up in progress; dropping it ends it, which the app lived
/// through, so the last one running removes the mark.
pub(crate) struct WarmingUp<'a>(&'a WarmUpMark);

impl Drop for WarmingUp<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.running -= 1;
        if state.running == 0 {
            self.0.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_warm_up_writes_the_mark_and_removes_it_when_it_returns() {
        let dir = tempfile::tempdir().unwrap();
        let mark = WarmUpMark::at_launch(dir.path());
        let path = dir.path().join(WarmUpMark::FILE_NAME);
        assert!(!mark.skipping());
        let first = mark.begin().unwrap();
        assert!(path.exists(), "on disk before the warm-up runs");
        let second = mark.begin().unwrap();
        drop(first);
        assert!(path.exists(), "the second warm-up still runs");
        drop(second);
        assert!(!path.exists());
        assert!(!WarmUpMark::at_launch(dir.path()).skipping());
    }

    /// A mark a previous process left turns the warm-up off at launch, at
    /// every launch, until a load in processing clears it.
    #[test]
    fn a_mark_left_over_skips_the_warm_up_until_a_load_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WarmUpMark::FILE_NAME);
        std::mem::forget(WarmUpMark::at_launch(dir.path()).begin());
        assert!(path.exists());

        let mark = WarmUpMark::at_launch(dir.path());
        assert!(mark.skipping());
        assert!(mark.begin().is_none());
        assert!(path.exists(), "a skipped warm-up leaves the mark");
        assert!(
            WarmUpMark::at_launch(dir.path()).skipping(),
            "the next launch too"
        );

        mark.loaded();
        assert!(!mark.skipping());
        assert!(!path.exists());
        drop(mark.begin().unwrap());
        assert!(!WarmUpMark::at_launch(dir.path()).skipping());
    }

    /// A load while a warm-up runs keeps that warm-up's mark; a load with
    /// nothing left over writes and removes nothing.
    #[test]
    fn a_load_leaves_a_running_warm_up_its_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WarmUpMark::FILE_NAME);
        let mark = WarmUpMark::at_launch(dir.path());
        let running = mark.begin().unwrap();
        mark.loaded();
        assert!(path.exists());
        drop(running);
        assert!(!path.exists());
        mark.loaded();
        assert!(!path.exists());
    }

    #[test]
    fn in_memory_it_writes_nothing() {
        let mark = WarmUpMark::in_memory();
        drop(mark.begin().unwrap());
        mark.loaded();
        assert!(!mark.skipping());
    }
}
