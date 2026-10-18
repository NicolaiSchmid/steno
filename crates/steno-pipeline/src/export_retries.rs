//! How many launch re-exports in a row of a meeting did not deliver every
//! row, kept in `export-retries.json` in the support directory, so the
//! launch stops retrying an export that keeps failing and the meeting's
//! detail says so. Separate from the crash-loop count: only the user's
//! export starts this count again.

use std::path::{Path, PathBuf};

use chrono::TimeDelta;
use uuid::Uuid;

use crate::files::MeetingValues;

/// The launch re-exports in a row of each meeting that did not deliver every
/// row, counted before each run; any re-export the user causes
/// [resets](Self::reset) the count. A missing or corrupt file counts 0; a
/// file that may not be written leaves the counts in memory only.
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
    counts: MeetingValues<u32>,
}

impl ExportRetries {
    pub const FILE_NAME: &'static str = "export-retries.json";

    /// The launch re-exports in a row, short of delivering every row, after
    /// which the launch stops retrying.
    pub const LIMIT: u32 = 3;

    /// How long after its last attempt a failed export is retried at launch.
    pub const INTERVAL: TimeDelta = TimeDelta::hours(24);

    /// The counts in `path`, read now.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        ExportRetries {
            counts: MeetingValues::new(path.into()),
        }
    }

    /// The counts in [`FILE_NAME`](Self::FILE_NAME) in `directory`, read now.
    #[must_use]
    pub fn in_directory(directory: &Path) -> Self {
        Self::new(directory.join(Self::FILE_NAME))
    }

    /// Counts that live in memory only and are never written.
    #[must_use]
    pub fn in_memory() -> Self {
        ExportRetries {
            counts: MeetingValues::in_memory(),
        }
    }

    #[must_use]
    pub fn count(&self, meeting_id: Uuid) -> u32 {
        self.counts.get(meeting_id).unwrap_or(0)
    }

    /// Whether the launch stopped retrying `meeting_id`'s failed export.
    #[must_use]
    pub fn stopped(&self, meeting_id: Uuid) -> bool {
        self.count(meeting_id) >= Self::LIMIT
    }

    /// Starts `meeting_id`'s count again from 0.
    pub fn reset(&self, meeting_id: Uuid) {
        // A failed write is logged; the count stays in memory.
        let _ = self
            .counts
            .change(|counts| counts.remove(&meeting_id).is_some());
    }

    /// One more launch re-export, counted before it runs so an exit
    /// mid-export counts too.
    pub(crate) fn attempted(&self, meeting_id: Uuid) {
        let _ = self.counts.change(|counts| {
            let count = counts.entry(meeting_id).or_insert(0);
            *count = count.saturating_add(1);
            true
        });
    }

    /// Keeps only the counts of the meetings `keep` accepts.
    pub(crate) fn retain(&self, keep: impl FnMut(Uuid) -> bool) {
        self.counts.retain(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three launches, once a day.
    #[test]
    fn the_limits_are_three_launches_once_a_day() {
        assert_eq!(ExportRetries::LIMIT, 3);
        assert_eq!(ExportRetries::INTERVAL, TimeDelta::hours(24));
    }

    /// The counts survive a restart, a reset removes the meeting, and a
    /// meeting stops at the limit.
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

    /// A file that does not parse is set aside and the counts start from 0.
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

    /// An unreadable file is never written; the counts live in memory only.
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
