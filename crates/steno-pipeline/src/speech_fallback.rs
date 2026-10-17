//! The fallback after an in-process speech engine took the app down (A19
//! of `.plans/2026-10-07-stable-promotion.md`). On the Mac `CoreML` runs in
//! the app's process, and an abort inside it that no `@catch` stops (a
//! failed allocation, an `IOSurface` the system refuses) ends the app. The
//! breakers before this one limit the damage: the recording's warm-up turns
//! itself off ([`WarmUpMark`]), and a meeting whose processing ended the
//! app twice is quarantined ([`ReadingMarks`]). But an abort that comes
//! back would still end the app once for every meeting. So once one is on
//! record, the jobs run the speech sidecar instead, where an abort ends
//! only the child and the meeting stays retryable. Rust only: the Swift
//! app had no second engine.
//!
//! - **What turns it on.** The launch check ([`SpeechFallback::at_launch`])
//!   over what the last process left: a warm-up mark
//!   ([`WarmUpMark::left_over`]) or a reading mark of the engine's load or
//!   a transcription ([`ReadingMarks::speech_marks_left`]). A force quit, a
//!   power loss or a crash elsewhere while the engine runs leaves the same
//!   marks, and turns it on too: the cost is the speech sidecar's models
//!   to download (the meetings wait for them in Settings) and its slower
//!   runs, never a meeting.
//! - **The file.** [`SpeechFallback::FILE_NAME`] in the support directory
//!   says whether the fallback is on, and lists the marks it has seen. It
//!   is written durably, so the fallback stays on at every launch until
//!   the user turns the engine back on. The launch logs it at `warn`.
//! - **Turning it off.** Only the user ([`SpeechFallback::turn_off`], from
//!   Settings), logged at `warn`. The marks seen so far stay listed, so a
//!   warm-up mark left over (which stays until a meeting loads the engine)
//!   or a quarantined meeting's marks do not turn it on again at the next
//!   launch; a new mark does. A recording's warm-up stays off until a
//!   meeting's processing has loaded the engine, so the first load after
//!   it never runs beside a recording.
//!
//! A file that cannot be read or written leaves the fallback to what this
//! launch found, logged at `warn`. The `steno` command neither reads nor
//! writes the file: a command names its engine itself, and an abort there
//! ends only the command, whose marks the app's next launch reads.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::files::{read_json, remove_durably, write_json};
use crate::reading::ReadingMarks;
use crate::warm_up_mark::WarmUpMark;

/// Whether the jobs run the speech sidecar instead of the engine in this
/// process; see the module doc.
///
/// ```
/// use steno_pipeline::SpeechFallback;
///
/// let fallback = SpeechFallback::in_memory();
/// assert!(!fallback.is_on());
/// ```
#[derive(Debug)]
pub struct SpeechFallback {
    /// `None` in memory: nothing is written.
    path: Option<PathBuf>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    on: bool,
    /// What the launch found left over, as the file lists it.
    found: BTreeSet<String>,
}

/// The file's body.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    on: bool,
    /// The marks the fallback has seen: the reading marks' file names and
    /// `warm-up:<id>` for a warm-up mark.
    seen: BTreeSet<String>,
}

impl SpeechFallback {
    /// The file in the support directory.
    pub const FILE_NAME: &'static str = "speech-fallback.json";

    /// Off, and writes nothing: the tests.
    #[must_use]
    pub fn in_memory() -> Self {
        SpeechFallback {
            path: None,
            state: Mutex::default(),
        }
    }

    /// The launch check in `support_directory` (see the module doc), after
    /// the launch read `warm_up` and `marks`: on when the file says so, or
    /// when either left a mark the file has not seen, which is then written
    /// down. Call it once per launch, before a pipeline is built.
    #[must_use]
    pub fn at_launch(support_directory: &Path, warm_up: &WarmUpMark, marks: &ReadingMarks) -> Self {
        let path = support_directory.join(Self::FILE_NAME);
        let mut found = marks.speech_marks_left();
        found.extend(warm_up.left_over().map(|id| format!("warm-up:{id}")));
        let (record, writable) = read_json::<Record>(&path);
        let on = if record.on {
            tracing::warn!(
                "the speech engine in this process is off since it ended the app; meetings are \
                 transcribed in the speech sidecar until it is turned back on in Settings"
            );
            true
        } else if found.is_subset(&record.seen) {
            // Forget the marks that are gone, so the list never grows, and
            // the file once it lists none.
            if writable && found != record.seen {
                save(&path, false, &found);
            }
            false
        } else {
            tracing::warn!(
                marks = found.difference(&record.seen).count(),
                "the speech engine in this process ended the app; from now on meetings are \
                 transcribed in the speech sidecar until it is turned back on in Settings"
            );
            if writable {
                save(&path, true, &found);
            }
            true
        };
        SpeechFallback {
            path: Some(path),
            state: Mutex::new(State { on, found }),
        }
    }

    /// Whether the jobs run the speech sidecar now.
    #[must_use]
    pub fn is_on(&self) -> bool {
        self.state().on
    }

    /// The user turned the engine in this process back on: the fallback is
    /// off from now on, the marks the launch found listed as seen. Logged
    /// at `warn`. Syncs the disk. A file that cannot be written leaves it
    /// off for this run only, and the error is returned.
    ///
    /// # Errors
    ///
    /// The file could not be written.
    pub fn turn_off(&self) -> std::io::Result<()> {
        let mut state = self.state();
        if !state.on {
            return Ok(());
        }
        state.on = false;
        tracing::warn!(
            "the speech engine in this process is turned back on; the speech sidecar no longer \
             stands in for it"
        );
        match &self.path {
            Some(path) => write(path, false, &state.found),
            None => Ok(()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Writes the record of `on` and `seen` to `path`, durably, or removes
/// the file when it would say nothing (off, no mark seen).
fn write(path: &Path, on: bool, seen: &BTreeSet<String>) -> std::io::Result<()> {
    if on || !seen.is_empty() {
        write_json(
            path,
            &Record {
                on,
                seen: seen.clone(),
            },
        )
    } else {
        remove_durably(path)
    }
}

/// [`write`], logging a failure at `warn`: what this launch found then
/// holds for this run only.
fn save(path: &Path, on: bool, seen: &BTreeSet<String>) {
    if let Err(error) = write(path, on, seen) {
        tracing::warn!(%error, "the speech fallback's file could not be written; what this launch found holds for this run only");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use steno_core::Store;

    use super::*;

    /// A launch over `support` with a store that has no meetings.
    fn launch(support: &Path) -> SpeechFallback {
        let store = Store::in_memory().unwrap();
        let warm_up = WarmUpMark::at_launch(support);
        let marks = ReadingMarks::at_launch(support, &store, chrono::Utc::now());
        SpeechFallback::at_launch(support, &warm_up, &marks)
    }

    fn record(support: &Path) -> Option<Record> {
        let bytes = std::fs::read(support.join(SpeechFallback::FILE_NAME)).ok()?;
        Some(serde_json::from_slice(&bytes).unwrap())
    }

    #[test]
    fn nothing_left_over_leaves_it_off_and_writes_nothing() {
        let support = tempfile::tempdir().unwrap();
        assert!(!launch(support.path()).is_on());
        assert_eq!(record(support.path()), None);
    }

    /// A warm-up that ended with the app turns it on, durably, at every
    /// launch; the user turns it off, and the same mark, which stays until
    /// a meeting loads the engine, does not turn it on again; a new one
    /// does.
    #[test]
    fn a_warm_up_mark_turns_it_on_until_the_user_turns_it_off() {
        let support = tempfile::tempdir().unwrap();
        std::mem::forget(Arc::new(WarmUpMark::at_launch(support.path())).begin());

        let fallback = launch(support.path());
        assert!(fallback.is_on());
        let saved = record(support.path()).unwrap();
        assert!(saved.on);
        assert_eq!(saved.seen.len(), 1);
        assert!(launch(support.path()).is_on(), "the next launch too");

        launch(support.path()).turn_off().unwrap();
        assert!(!record(support.path()).unwrap().on);
        let after = launch(support.path());
        assert!(!after.is_on(), "the mark seen does not count again");

        // A meeting loads the engine, and a later warm-up ends the app.
        let warm_up = Arc::new(WarmUpMark::at_launch(support.path()));
        warm_up.speech_engine_loaded();
        std::mem::forget(warm_up.begin());
        assert!(launch(support.path()).is_on(), "a new mark counts");
    }

    /// A mark of the engine's load or transcription turns it on; the
    /// marks of a decode do not.
    #[test]
    fn only_a_speech_reading_mark_turns_it_on() {
        let support = tempfile::tempdir().unwrap();
        let store = Store::in_memory().unwrap();
        let marks = Arc::new(ReadingMarks::at_launch(
            support.path(),
            &store,
            chrono::Utc::now(),
        ));
        let meeting = uuid::Uuid::new_v4();
        let path = support.path().join("master.caf");
        std::fs::write(&path, b"audio").unwrap();
        std::mem::forget(marks.reading(meeting, None, &path, crate::ReadStage::Decode));
        assert!(!launch(support.path()).is_on(), "a decode's mark");

        let marks = Arc::new(ReadingMarks::at_launch(
            support.path(),
            &store,
            chrono::Utc::now(),
        ));
        std::mem::forget(marks.reading(meeting, None, &path, crate::ReadStage::Transcribe));
        let fallback = launch(support.path());
        assert!(fallback.is_on(), "a transcription's mark");
        fallback.turn_off().unwrap();
        assert!(!launch(support.path()).is_on());
    }

    /// Once the marks it saw are gone, the file forgets them: it is
    /// removed when it lists none.
    #[test]
    fn the_file_forgets_the_marks_that_are_gone() {
        let support = tempfile::tempdir().unwrap();
        std::mem::forget(Arc::new(WarmUpMark::at_launch(support.path())).begin());
        launch(support.path()).turn_off().unwrap();
        assert_eq!(record(support.path()).unwrap().seen.len(), 1);
        WarmUpMark::at_launch(support.path()).speech_engine_loaded();
        assert!(!launch(support.path()).is_on());
        assert_eq!(record(support.path()), None);
    }

    #[test]
    fn in_memory_it_is_off_and_turning_it_off_writes_nothing() {
        let fallback = SpeechFallback::in_memory();
        assert!(!fallback.is_on());
        fallback.turn_off().unwrap();
    }
}
