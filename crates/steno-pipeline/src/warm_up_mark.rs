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
//!   loads the models in its processing, after the recording. A mark that
//!   cannot be read is given a fresh id at each launch
//!   (`unreadable-<uuid>`), so the fallback counts it as new.
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
//!   (`WarmUpMark::hold_off`, its id `held-<uuid>`), so recordings skip the
//!   warm-up, at this launch and every later one, until a meeting's
//!   processing has loaded the engine: that first load never runs beside a
//!   recording. The launch that finds it says the warm-up is held off, not
//!   that it ended with the app.
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

    /// The start of a held mark's id (`WarmUpMark::hold_off`).
    const HELD_PREFIX: &'static str = "held-";

    /// The start of the id a launch gives a mark it cannot read.
    const UNREADABLE_PREFIX: &'static str = "unreadable-";

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
    /// once at `warn`. On the Mac a mark whose presence cannot be checked
    /// counts as one that cannot be read; elsewhere, where no warm-up mark
    /// is written, it is no mark, logged at `warn`. Call it once per
    /// launch, before any recording.
    #[must_use]
    pub fn at_launch(support_directory: &Path) -> Self {
        let path = support_directory.join(Self::FILE_NAME);
        // A mark that cannot be read still turns the warm-up off. Its id is
        // lost, so it gets a fresh one and the fallback counts it as new. On
        // the Mac a check that fails is treated the same, so it never hides
        // a crash. Only the Mac writes warm-up marks, so elsewhere a failed
        // check is no mark.
        let left_over = match path.try_exists() {
            Ok(false) => None,
            Err(error) if !cfg!(target_os = "macos") => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "the warm-up mark of the speech models could not be checked; only the Mac \
                     writes one, so it counts as none"
                );
                None
            }
            found => Some(
                found
                    .and_then(|_| std::fs::read_to_string(&path))
                    .map_or_else(
                        |_| format!("{}{}", Self::UNREADABLE_PREFIX, uuid::Uuid::new_v4()),
                        |id| id.trim().to_owned(),
                    ),
            ),
        };
        let skipping = left_over.is_some();
        match &left_over {
            Some(id) if id.starts_with(Self::HELD_PREFIX) => tracing::warn!(
                mark = %id,
                "the warm-up of the speech models is held off; recordings skip it until a \
                 meeting's processing loads the models"
            ),
            Some(id) => tracing::warn!(
                mark = %id,
                "the last warm-up of the speech models ended with the app; recordings skip it \
                 until a meeting's processing loads the models"
            ),
            None => {}
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
    /// with the app, or `held-<uuid>` for one the user's end of the
    /// fallback held off. Earlier builds wrote `1` in every mark; an empty
    /// mark gives an empty id. A mark that cannot be read gives a fresh
    /// `unreadable-<uuid>` at each launch, so the fallback counts it as new.
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

    /// Holds the warm-up off, at this launch and every later one, until a
    /// meeting's processing has loaded the engine: the mark is written
    /// durably with a fresh id (`held-<uuid>`), which this returns. The
    /// user's end of the fallback ([`crate::SpeechFallback::turn_off`]), so
    /// the engine's first load after it never runs beside a recording. A
    /// mark that cannot be written returns the error, and the fallback
    /// stays on; the warm-up is off for this run all the same, until a
    /// load.
    pub(crate) fn hold_off(&self) -> std::io::Result<String> {
        let id = format!("{}{}", Self::HELD_PREFIX, uuid::Uuid::new_v4());
        let mut state = self.state();
        state.skipping = true;
        self.write(&id)?;
        Ok(id)
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
/// was held off meanwhile (`WarmUpMark::hold_off`), whose mark stays.
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
        let id = std::fs::read_to_string(&path).unwrap().trim().to_owned();
        assert!(uuid::Uuid::parse_str(&id).is_ok(), "{id}");

        let mark = Arc::new(WarmUpMark::at_launch(dir.path()));
        assert!(mark.skipping());
        let text = log.text();
        assert_eq!(lines_of(&text, &id, "ended with the app;"), 1, "{text}");
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

    /// The captured lines that name the mark `id` and say `what`. Every
    /// test in the binary logs into the capture, and others find marks of
    /// their own left over, so only the id picks out this test's lines.
    fn lines_of(text: &str, id: &str, what: &str) -> usize {
        text.lines()
            .filter(|line| line.contains(id) && line.contains(what))
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
        let id = mark.hold_off().unwrap();
        assert!(id.starts_with("held-"), "{id}");
        assert!(mark.skipping());
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), id);
        drop(running);
        assert!(path.exists(), "the held mark stays");
        assert!(mark.begin().is_none());

        let next = WarmUpMark::at_launch(dir.path());
        assert!(next.skipping(), "the next launch");
        assert_eq!(next.left_over(), Some(id.as_str()));
        assert_ne!(mark.hold_off().unwrap(), id, "a fresh id each time");

        mark.speech_engine_loaded();
        assert!(!mark.skipping());
        assert!(!path.exists());
        drop(mark.begin().unwrap());
    }

    /// A launch that finds a held mark says the warm-up is held off, once,
    /// not that a warm-up ended with the app.
    #[test]
    fn a_launch_over_a_held_mark_says_it_is_held_off() {
        let log = crate::fixtures::CapturedLog::warnings();
        let dir = tempfile::tempdir().unwrap();
        let id = WarmUpMark::at_launch(dir.path()).hold_off().unwrap();
        let next = WarmUpMark::at_launch(dir.path());
        assert!(next.skipping());
        let text = log.text();
        assert_eq!(lines_of(&text, &id, "is held off;"), 1, "{text}");
        // No semicolon, unlike the left-over test's count, so no rewording
        // of that line slips past.
        assert_eq!(lines_of(&text, &id, "ended with the app"), 0, "{text}");
    }

    /// A load while a held-off warm-up still runs leaves that warm-up its
    /// mark until it returns.
    #[test]
    fn a_load_while_a_held_warm_up_runs_keeps_its_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WarmUpMark::FILE_NAME);
        let mark = Arc::new(WarmUpMark::at_launch(dir.path()));
        let running = mark.begin().unwrap();
        mark.hold_off().unwrap();
        mark.speech_engine_loaded();
        assert!(path.exists(), "the running warm-up is still guarded");
        drop(running);
        assert!(!path.exists());
    }

    /// A mark that cannot be read skips the warm-up, under a fresh id at
    /// each launch, and the launch says a warm-up ended with the app.
    #[test]
    fn an_unreadable_mark_gets_a_fresh_id_at_each_launch() {
        let log = crate::fixtures::CapturedLog::warnings();
        let dir = tempfile::tempdir().unwrap();
        // Bytes that are not UTF-8 fail the read on every platform.
        std::fs::write(dir.path().join(WarmUpMark::FILE_NAME), b"\xff\xfe").unwrap();
        let first = WarmUpMark::at_launch(dir.path());
        let second = WarmUpMark::at_launch(dir.path());
        assert!(first.skipping() && second.skipping());
        let (first, second) = (first.left_over().unwrap(), second.left_over().unwrap());
        assert!(first.starts_with("unreadable-"), "{first}");
        assert_ne!(first, second);
        let text = log.text();
        assert_eq!(lines_of(&text, first, "ended with the app;"), 1, "{text}");
        assert_eq!(lines_of(&text, first, "is held off"), 0, "{text}");
    }

    /// On the Mac a mark whose presence cannot be checked counts as one
    /// that cannot be read; elsewhere it is no mark. Unix: a support path
    /// through a regular file (`ENOTDIR`).
    #[cfg(unix)]
    #[test]
    fn a_mark_that_cannot_be_checked_counts_as_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("support");
        std::fs::write(&file, b"").unwrap();
        let mark = WarmUpMark::at_launch(&file);
        if cfg!(target_os = "macos") {
            assert!(mark.skipping());
            let id = mark.left_over().unwrap();
            assert!(id.starts_with("unreadable-"), "{id}");
        } else {
            assert!(!mark.skipping());
            assert_eq!(mark.left_over(), None);
        }
    }

    #[test]
    fn in_memory_it_writes_nothing() {
        let mark = Arc::new(WarmUpMark::in_memory());
        drop(mark.begin().unwrap());
        mark.speech_engine_loaded();
        assert!(!mark.skipping());
    }
}
