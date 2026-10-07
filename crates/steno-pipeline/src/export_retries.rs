//! How many launch re-exports of a meeting failed in a row, kept in
//! `export-retries.json` in the support directory (meeting id to count),
//! so the launch stops retrying an export that keeps failing and the
//! meeting's detail says so. The database has no column for it, and the
//! Swift app never reads the file, so a rollback ignores it. Rust only:
//! Swift retried a failed export only when asked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::TimeDelta;
use uuid::Uuid;

use crate::files::{Access, create_dir_all_durably, replace_file, set_aside};

/// The failed launch re-exports in a row of each meeting
/// ([`ProcessingPipeline::redeliver_unfinished`](crate::ProcessingPipeline::redeliver_unfinished)
/// counts them; the user's Export again [resets](Self::reset) the count).
/// A write replaces the file in one durable step ([`replace_file`]). A
/// missing file counts 0 for every meeting. A file that does not parse is
/// moved aside ([`set_aside`]) and logged before the counts start at 0; a
/// file that cannot be read for another reason, or that cannot be moved
/// aside, is left alone and never written, and the counts of this run live
/// in memory only, as `preferences.json`'s flags do.
///
/// ```
/// use steno_pipeline::ExportRetries;
/// use uuid::Uuid;
///
/// let dir = tempfile::tempdir()?;
/// let retries = ExportRetries::new(dir.path().join(ExportRetries::FILE_NAME));
/// let meeting = Uuid::new_v4();
/// assert_eq!(retries.count(meeting), 0);
/// assert!(!retries.stopped(meeting));
/// retries.reset(meeting);
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug)]
pub struct ExportRetries {
    path: PathBuf,
    counts: Mutex<BTreeMap<Uuid, u32>>,
    /// False when the file on disk could not be read or set aside.
    writable: bool,
}

impl ExportRetries {
    /// The file's name in the support directory.
    pub const FILE_NAME: &'static str = "export-retries.json";

    /// The failed launch re-exports in a row after which the launch stops
    /// retrying a meeting's failed export.
    pub const LIMIT: u32 = 3;

    /// How long after its last attempt a failed export is retried at
    /// launch, so a launch retries it at most once a day.
    pub const INTERVAL: TimeDelta = TimeDelta::hours(24);

    /// The counts in `path`, read now.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (counts, writable) = load(&path);
        ExportRetries {
            path,
            counts: Mutex::new(counts),
            writable,
        }
    }

    /// The counts in [`FILE_NAME`](Self::FILE_NAME) in `directory` (the
    /// support directory), read now.
    #[must_use]
    pub fn in_directory(directory: &Path) -> Self {
        Self::new(directory.join(Self::FILE_NAME))
    }

    /// Counts that live in memory only and are never written, for a caller
    /// without a support directory.
    #[must_use]
    pub fn in_memory() -> Self {
        ExportRetries {
            path: PathBuf::new(),
            counts: Mutex::default(),
            writable: false,
        }
    }

    /// The failed launch re-exports in a row of `meeting_id`.
    #[must_use]
    pub fn count(&self, meeting_id: Uuid) -> u32 {
        self.lock().get(&meeting_id).copied().unwrap_or(0)
    }

    /// Whether the launch stopped retrying `meeting_id`'s failed export:
    /// it failed [`LIMIT`](Self::LIMIT) launches in a row.
    #[must_use]
    pub fn stopped(&self, meeting_id: Uuid) -> bool {
        self.count(meeting_id) >= Self::LIMIT
    }

    /// Starts `meeting_id`'s count again from 0: the user exported it
    /// again, or a launch re-export left no delivery failed.
    pub fn reset(&self, meeting_id: Uuid) {
        let mut counts = self.lock();
        if counts.remove(&meeting_id).is_some() {
            self.write(&counts);
        }
    }

    /// One more failed launch re-export of `meeting_id`.
    pub(crate) fn failed(&self, meeting_id: Uuid) {
        let mut counts = self.lock();
        let count = counts.entry(meeting_id).or_insert(0);
        *count = count.saturating_add(1);
        self.write(&counts);
    }

    /// Keeps the counts of the meetings `keep` names, so a meeting deleted
    /// or exported since its last failure starts again from 0.
    pub(crate) fn retain(&self, mut keep: impl FnMut(Uuid) -> bool) {
        let mut counts = self.lock();
        let before = counts.len();
        counts.retain(|meeting_id, _| keep(*meeting_id));
        if counts.len() != before {
            self.write(&counts);
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<Uuid, u32>> {
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Replaces the file with `counts`, logging a failure: the counts of
    /// this run stay in memory then.
    fn write(&self, counts: &BTreeMap<Uuid, u32>) {
        if !self.writable {
            return;
        }
        let written = serde_json::to_vec_pretty(counts)
            .map_err(std::io::Error::from)
            .and_then(|data| {
                if let Some(parent) = self.path.parent() {
                    create_dir_all_durably(parent)?;
                }
                replace_file(&self.path, &data, Access::Default)
            });
        if let Err(error) = written {
            tracing::warn!("{} could not be written: {error}", self.path.display());
        }
    }
}

/// The counts in `path` and whether it may be written.
fn load(path: &Path) -> (BTreeMap<Uuid, u32>, bool) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (BTreeMap::new(), true);
        }
        Err(error) => {
            tracing::warn!(
                "{} could not be read ({error}); it stays and is not written",
                path.display()
            );
            return (BTreeMap::new(), false);
        }
    };
    let error = match serde_json::from_slice(&bytes) {
        Ok(counts) => return (counts, true),
        Err(error) => error,
    };
    match set_aside(path) {
        Ok(aside) => {
            tracing::warn!(
                "{} did not parse ({error}); moved it to {} and started from 0",
                path.display(),
                aside.display()
            );
            (BTreeMap::new(), true)
        }
        Err(move_error) => {
            tracing::warn!(
                "{} did not parse ({error}) and could not be moved aside \
                 ({move_error}); it stays and is not written",
                path.display()
            );
            (BTreeMap::new(), false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retries_in(dir: &Path) -> ExportRetries {
        ExportRetries::new(dir.join(ExportRetries::FILE_NAME))
    }

    /// The counts survive a restart, a reset removes the meeting from the
    /// file, and a meeting stops at the limit.
    #[test]
    fn counts_are_kept_across_launches_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let (meeting, other) = (Uuid::new_v4(), Uuid::new_v4());
        let retries = retries_in(dir.path());
        for _ in 0..ExportRetries::LIMIT {
            assert!(!retries.stopped(meeting));
            retries.failed(meeting);
        }
        retries.failed(other);
        let relaunched = retries_in(dir.path());
        assert_eq!(relaunched.count(meeting), ExportRetries::LIMIT);
        assert!(relaunched.stopped(meeting));
        assert_eq!(relaunched.count(other), 1);

        relaunched.reset(meeting);
        relaunched.retain(|id| id != other);
        let relaunched = retries_in(dir.path());
        assert_eq!(relaunched.count(meeting), 0);
        assert_eq!(relaunched.count(other), 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(ExportRetries::FILE_NAME)).unwrap(),
            "{}"
        );
    }

    /// A file that does not parse is moved aside and the counts start
    /// from 0; the next write puts a new file in its place.
    #[test]
    fn a_file_that_does_not_parse_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ExportRetries::FILE_NAME);
        std::fs::write(&path, b"{not json").unwrap();
        let meeting = Uuid::new_v4();
        let retries = retries_in(dir.path());
        assert_eq!(retries.count(meeting), 0);
        retries.failed(meeting);
        assert_eq!(retries_in(dir.path()).count(meeting), 1);
        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read(aside[0].path()).unwrap(), b"{not json");
    }
}
