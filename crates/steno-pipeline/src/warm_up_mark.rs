//! The breaker for a warm-up that takes the app down: the recording's
//! warm-up ([`ProcessingPipeline::warm_up_while_recording`]) is marked on
//! disk ([`WarmUpMark::FILE_NAME`]) while it runs, and a mark left over at
//! launch, or a held mark from the fallback's turn-off, turns warm-ups off
//! until a meeting's processing loads the engine.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::files::{Access, remove_durably, replace_file};

#[cfg(doc)]
use crate::ProcessingPipeline;

/// The mark, and whether the warm-up is off.
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
    /// A mark outlived its process, or the warm-up is held off.
    skipping: bool,
    /// Warm-ups running now; the first writes the mark, the last removes it.
    running: usize,
}

impl WarmUpMark {
    pub const FILE_NAME: &'static str = "warm-up-in-progress";

    const HELD_PREFIX: &'static str = "held-";

    /// A mark that writes nothing and never turns the warm-up off.
    #[must_use]
    pub fn in_memory() -> Self {
        WarmUpMark {
            path: None,
            left_over: None,
            state: Mutex::default(),
        }
    }

    /// The launch check over the mark in `support_directory`, logged once at
    /// `warn` when one is left. Call it once per launch, before any
    /// recording.
    #[must_use]
    pub fn at_launch(support_directory: &Path) -> Self {
        let path = support_directory.join(Self::FILE_NAME);
        // A mark that cannot be read still turns the warm-up off.
        let left_over = path.exists().then(|| {
            std::fs::read_to_string(&path)
                .map_or_else(|_| "unreadable".to_owned(), |id| id.trim().to_owned())
        });
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

    /// The id of the mark the launch found left over (`held-<uuid>` for a
    /// held one; `1` or empty from older builds).
    #[must_use]
    pub fn left_over(&self) -> Option<&str> {
        self.left_over.as_deref()
    }

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

    /// Holds the warm-up off until a meeting's processing has loaded the
    /// engine, across launches, with a fresh `held-<uuid>` mark it returns.
    /// On a write error the warm-up is still off for this run.
    pub(crate) fn hold_off(&self) -> std::io::Result<String> {
        let id = format!("{}{}", Self::HELD_PREFIX, uuid::Uuid::new_v4());
        let mut state = self.state();
        state.skipping = true;
        self.write(&id)?;
        Ok(id)
    }

    /// A meeting's processing loaded the speech engine: a mark left over goes
    /// and recordings warm up again.
    pub(crate) fn speech_engine_loaded(&self) {
        // A warm-up still running removes the mark as it ends.
        let mut state = self.state();
        if std::mem::take(&mut state.skipping) && state.running == 0 {
            self.remove();
        }
    }

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

/// A warm-up in progress; dropping the last one removes the mark, unless the
/// warm-up was held off meanwhile.
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

    /// A mark left over turns the warm-up off at every launch until a load
    /// clears it, with one `warn` line per launch.
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

    /// A load while a warm-up runs keeps that warm-up's mark.
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

    /// The captured lines that name the mark `id` and say `what`; the id
    /// picks out this test's lines among every test's.
    fn lines_of(text: &str, id: &str, what: &str) -> usize {
        text.lines()
            .filter(|line| line.contains(id) && line.contains(what))
            .count()
    }

    /// Holding off writes a mark the next launch finds too, and no warm-up
    /// runs until a load clears it.
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

    /// A launch that finds a held mark says the warm-up is held off.
    #[test]
    fn a_launch_over_a_held_mark_says_it_is_held_off() {
        let log = crate::fixtures::CapturedLog::warnings();
        let dir = tempfile::tempdir().unwrap();
        let id = WarmUpMark::at_launch(dir.path()).hold_off().unwrap();
        let next = WarmUpMark::at_launch(dir.path());
        assert!(next.skipping());
        let text = log.text();
        assert_eq!(lines_of(&text, &id, "is held off;"), 1, "{text}");
        // No semicolon, so no rewording of that line slips past.
        assert_eq!(lines_of(&text, &id, "ended with the app"), 0, "{text}");
    }

    /// A load while a held-off warm-up runs leaves its mark until it returns.
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

    #[test]
    fn in_memory_it_writes_nothing() {
        let mark = Arc::new(WarmUpMark::in_memory());
        drop(mark.begin().unwrap());
        mark.speech_engine_loaded();
        assert!(!mark.skipping());
    }
}
