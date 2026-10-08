//! How many launch re-exports in a row of a meeting did not deliver every
//! row, kept in `export-retries.json` in the support directory (meeting id
//! to count), so the launch stops retrying an export that keeps failing
//! and the meeting's detail says so. The database has no column for it,
//! and the Swift app never reads the file, so a rollback ignores it. Rust
//! only: Swift retried a failed export only when asked.
//!
//! The processing's launch recovery keeps a count of its own, of the
//! processing runs that ended with the app. The two stay apart: an export
//! also fails without taking the app down, a ready meeting's note is
//! retried rather than its processing failed, and only the user's export
//! starts this count again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::TimeDelta;
use uuid::Uuid;

use crate::files::{read_json, write_json};

/// The launch re-exports in a row of each meeting that did not deliver
/// every row
/// ([`ProcessingPipeline::redeliver_unfinished`](crate::ProcessingPipeline::redeliver_unfinished)
/// counts each before it runs; any re-export the user causes
/// [resets](Self::reset) the count).
/// Read with [`read_json`], so a missing or corrupt file counts 0 for every
/// meeting, and replaced with [`write_json`] on every change; a file that
/// may not be written leaves the counts of this run in memory only, as
/// `preferences.json`'s flags do.
///
/// ```
/// use steno_pipeline::ExportRetries;
/// use uuid::Uuid;
///
/// let dir = tempfile::tempdir()?;
/// let retries = ExportRetries::in_directory(dir.path());
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

    /// The launch re-exports in a row that did not deliver every row after
    /// which the launch stops retrying a meeting's failed export.
    pub const LIMIT: u32 = 3;

    /// How long after its last attempt a failed export is retried at
    /// launch, so a launch retries it at most once a day.
    pub const INTERVAL: TimeDelta = TimeDelta::hours(24);

    /// The counts in `path`, read now.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (counts, writable) = read_json(&path);
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

    /// The launch re-exports in a row of `meeting_id` that did not deliver
    /// every row.
    #[must_use]
    pub fn count(&self, meeting_id: Uuid) -> u32 {
        self.lock().get(&meeting_id).copied().unwrap_or(0)
    }

    /// Whether the launch stopped retrying `meeting_id`'s failed export:
    /// [`LIMIT`](Self::LIMIT) launches in a row did not deliver every row.
    #[must_use]
    pub fn stopped(&self, meeting_id: Uuid) -> bool {
        self.count(meeting_id) >= Self::LIMIT
    }

    /// Starts `meeting_id`'s count again from 0: the user caused a
    /// re-export, or a launch re-export delivered every row.
    pub fn reset(&self, meeting_id: Uuid) {
        let mut counts = self.lock();
        if counts.remove(&meeting_id).is_some() {
            self.write(&counts);
        }
    }

    /// One more launch re-export of `meeting_id`, counted before it runs so
    /// an exit mid-export counts too.
    pub(crate) fn attempted(&self, meeting_id: Uuid) {
        let mut counts = self.lock();
        let count = counts.entry(meeting_id).or_insert(0);
        *count = count.saturating_add(1);
        self.write(&counts);
    }

    /// Keeps the counts of the meetings `keep` names, so a meeting that
    /// delivered every row since (or that was deleted) starts again from 0.
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
        if let Err(error) = write_json(&self.path, counts) {
            tracing::warn!("{} could not be written: {error}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The limits P28 of `.plans/2026-10-07-stable-promotion.md` names:
    /// three launches, once a day.
    #[test]
    fn the_limits_are_three_launches_once_a_day() {
        assert_eq!(ExportRetries::LIMIT, 3);
        assert_eq!(ExportRetries::INTERVAL, TimeDelta::hours(24));
    }

    /// The counts survive a restart, a reset removes the meeting from the
    /// file, and a meeting stops at the limit.
    #[test]
    fn counts_are_kept_across_launches_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let (meeting, other) = (Uuid::new_v4(), Uuid::new_v4());
        let retries = ExportRetries::in_directory(dir.path());
        for _ in 0..ExportRetries::LIMIT {
            assert!(!retries.stopped(meeting));
            retries.attempted(meeting);
        }
        retries.attempted(other);
        let relaunched = ExportRetries::in_directory(dir.path());
        assert_eq!(relaunched.count(meeting), ExportRetries::LIMIT);
        assert!(relaunched.stopped(meeting));
        assert_eq!(relaunched.count(other), 1);

        relaunched.reset(meeting);
        relaunched.retain(|id| id != other);
        let relaunched = ExportRetries::in_directory(dir.path());
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
        let retries = ExportRetries::in_directory(dir.path());
        assert_eq!(retries.count(meeting), 0);
        retries.attempted(meeting);
        assert_eq!(ExportRetries::in_directory(dir.path()).count(meeting), 1);
        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read(aside[0].path()).unwrap(), b"{not json");
    }

    /// A file that cannot be read for another reason than its absence is
    /// left as it is and never written: the counts of this run live in
    /// memory only.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_never_written() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ExportRetries::FILE_NAME);
        let (meeting, other) = (Uuid::new_v4(), Uuid::new_v4());
        let stored = format!("{{\"{other}\": 2}}");
        std::fs::write(&path, &stored).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            // Root reads past the mode; nothing to test.
            return;
        }
        let retries = ExportRetries::new(&path);
        retries.attempted(meeting);
        assert_eq!(retries.count(meeting), 1, "the run still has its count");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), stored);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
