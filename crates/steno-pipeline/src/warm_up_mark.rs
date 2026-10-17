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
//!   failed or not, since the app lived through it. It holds a random id
//!   of its own, so the fallback ([`crate::SpeechFallback`]) tells a mark
//!   it has seen from a new one.
//! - **The launch check**: [`WarmUpMark::at_launch`]. A mark there is a
//!   warm-up that ended with the app, so from then on recordings skip the
//!   warm-up, and the launch says so once at `warn`. The first meeting then
//!   loads the models in its processing, after the recording.
//! - **Clearing it**: only the engine in this process, loaded in a
//!   meeting's processing (the speech sidecar standing in for it does not
//!   count): the mark goes and the next recording warms up again. A
//!   load that takes the app down there leaves the mark, and its meeting
//!   is counted by the reading marks ([`crate::reading`]): read once
//!   more, and quarantined the second time in a row. A mark left over
//!   also turns the fallback on: the jobs run the speech sidecar instead
//!   ([`crate::SpeechFallback`]), and no meeting loads the models in this
//!   process until the user turns the engine back on.
//! - **Held off**: the user's end of the fallback writes a mark of its own
//!   ([`WarmUpMark::hold_off`]), so recordings skip the warm-up, at this
//!   launch and the next, until a meeting's processing has loaded the
//!   engine: that first load never runs beside a recording.
//!
//! A folder that cannot be written leaves the warm-up unguarded, as
//! before: the failure is logged at `warn` and the warm-up runs. The
//! pipeline writes and removes the mark off the async workers, since both
//! sync the disk.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::files::{Access, remove_durably, replace_file};

#[cfg(doc)]
use crate::ProcessingPipeline;

/// The mark, and whether the warm-up is off; see the module doc.
#[derive(Debug)]
pub struct WarmUpMark {
    /// `None` in memory (the tests, the CLI): nothing is written.
    path: Option<PathBuf>,
    /// The id of the mark the launch found left over, if any.
    left_over: Option<String>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// A mark outlived its process, or the warm-up is held off: no
    /// warm-up until a load clears it.
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
            left_over: None,
            state: Mutex::default(),
        }
    }

    /// The launch check over the mark in `support_directory`: a mark left
    /// there turns the warm-up off until a load clears it, and is logged
    /// once at `warn`. Call it once per launch, before any recording.
    #[must_use]
    pub fn at_launch(support_directory: &Path) -> Self {
        let path = support_directory.join(Self::FILE_NAME);
        // A mark that cannot be read still turns the warm-up off.
        let left_over = path.exists().then(|| {
            std::fs::read_to_string(&path)
                .map_or_else(|_| "unreadable".to_owned(), |id| id.trim().to_owned())
        });
        let skipping = left_over.is_some();
        if skipping {
            tracing::warn!(
                "the last warm-up of the speech models ended with the app; recordings skip it \
                 until a meeting's processing loads the models"
            );
        }
        WarmUpMark {
            path: Some(path),
            left_over,
            state: Mutex::new(State {
                skipping,
                running: 0,
            }),
        }
    }

    /// The id of the mark the launch found left over: a warm-up that ended
    /// with the app. Earlier builds wrote `1` in every mark.
    #[must_use]
    pub fn left_over(&self) -> Option<&str> {
        self.left_over.as_deref()
    }

    /// Whether recordings skip the warm-up now.
    #[must_use]
    pub fn skipping(&self) -> bool {
        self.state().skipping
    }

    /// A warm-up starts: the mark is on disk before this returns. `None`
    /// while the warm-up is off.
    pub(crate) fn begin(self: &Arc<Self>) -> Option<WarmingUp> {
        let mut state = self.state();
        if state.skipping {
            return None;
        }
        state.running += 1;
        if state.running == 1
            && let Err(error) = self.write(&uuid::Uuid::new_v4().to_string())
        {
            tracing::warn!(%error, "the warm-up mark could not be written; the warm-up runs unguarded");
        }
        Some(WarmingUp(Arc::clone(self)))
    }

    /// Holds the warm-up off until a meeting's processing loads the speech
    /// engine, at this launch and the next: the mark is written durably
    /// with a fresh id, which this returns. The user's end of the fallback
    /// ([`crate::SpeechFallback::turn_off`]), so the engine's first load
    /// after it never runs beside a recording. A mark that cannot be
    /// written is logged at `warn`, and holds the warm-up off for this run
    /// only.
    pub(crate) fn hold_off(&self) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let mut state = self.state();
        state.skipping = true;
        if let Err(error) = self.write(&id) {
            tracing::warn!(%error, "the warm-up mark could not be written; the warm-up is off for this run only");
        }
        id
    }

    /// A meeting's processing loaded the speech engine: a mark left over
    /// goes and recordings warm up again (the mark of a warm-up running now
    /// stays until it returns).
    pub(crate) fn speech_engine_loaded(&self) {
        // A mark while skipping is the one left over or held off; a
        // warm-up still running (held off meanwhile) removes it as it ends.
        let mut state = self.state();
        if std::mem::take(&mut state.skipping) && state.running == 0 {
            self.remove();
        }
    }

    /// Writes the mark with `id`, durably.
    fn write(&self, id: &str) -> std::io::Result<()> {
        match &self.path {
            Some(path) => replace_file(path, format!("{id}\n").as_bytes(), Access::Default),
            None => Ok(()),
        }
    }

    fn remove(&self) {
        if let Some(path) = &self.path
            && let Err(error) = remove_durably(path)
        {
            tracing::warn!(%error, "the warm-up mark could not be removed");
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A warm-up in progress; dropping it ends it, which the app lived
/// through, so the last one running removes the mark, unless the warm-up
/// was held off meanwhile ([`WarmUpMark::hold_off`]), whose mark stays.
pub(crate) struct WarmingUp(Arc<WarmUpMark>);

impl Drop for WarmingUp {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.running -= 1;
        if state.running == 0 && !state.skipping {
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
        let mark = Arc::new(WarmUpMark::at_launch(dir.path()));
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
    /// every launch, until a load in processing clears it; each launch
    /// that finds it says so in one `warn` line.
    #[test]
    fn a_mark_left_over_skips_the_warm_up_until_a_load_clears_it() {
        let log = crate::fixtures::CapturedLog::warnings();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WarmUpMark::FILE_NAME);
        std::mem::forget(Arc::new(WarmUpMark::at_launch(dir.path())).begin());
        assert!(path.exists());

        let before = launch_lines(log);
        let mark = Arc::new(WarmUpMark::at_launch(dir.path()));
        assert!(mark.skipping());
        assert_eq!(launch_lines(log), before + 1, "{}", log.text());
        assert!(mark.begin().is_none());
        assert!(path.exists(), "a skipped warm-up leaves the mark");
        assert!(
            WarmUpMark::at_launch(dir.path()).skipping(),
            "the next launch too"
        );

        mark.speech_engine_loaded();
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
        let mark = Arc::new(WarmUpMark::at_launch(dir.path()));
        let running = mark.begin().unwrap();
        mark.speech_engine_loaded();
        assert!(path.exists());
        drop(running);
        assert!(!path.exists());
        mark.speech_engine_loaded();
        assert!(!path.exists());
    }

    /// The launch's lines captured so far, from every test in the binary.
    fn launch_lines(log: &crate::fixtures::CapturedLog) -> usize {
        log.text()
            .lines()
            .filter(|line| {
                line.contains("the last warm-up of the speech models ended with the app")
            })
            .count()
    }

    /// The user's end of the fallback holds the warm-up off: a fresh mark,
    /// which the next launch finds too, and no warm-up until a load clears
    /// it; a warm-up running meanwhile leaves the held mark in place.
    #[test]
    fn a_warm_up_held_off_stays_off_until_a_load_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WarmUpMark::FILE_NAME);
        let mark = Arc::new(WarmUpMark::at_launch(dir.path()));
        let running = mark.begin().unwrap();
        let id = mark.hold_off();
        assert!(mark.skipping());
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), id);
        drop(running);
        assert!(path.exists(), "the held mark stays");
        assert!(mark.begin().is_none());

        let next = WarmUpMark::at_launch(dir.path());
        assert!(next.skipping(), "the next launch");
        assert_eq!(next.left_over(), Some(id.as_str()));
        assert_ne!(mark.hold_off(), id, "a fresh id each time");

        mark.speech_engine_loaded();
        assert!(!mark.skipping());
        assert!(!path.exists());
        drop(mark.begin().unwrap());
    }

    #[test]
    fn in_memory_it_writes_nothing() {
        let mark = Arc::new(WarmUpMark::in_memory());
        drop(mark.begin().unwrap());
        mark.speech_engine_loaded();
        assert!(!mark.skipping());
    }
}
