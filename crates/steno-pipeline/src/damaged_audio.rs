//! How many parts of each meeting's recording the decoder could not read
//! and replaced by silence of their length
//! ([`AudioBuffer16k::damaged_parts`](steno_core::AudioBuffer16k::damaged_parts)),
//! kept in `damaged-audio.json` in the support directory (meeting id to
//! count), so the meeting's detail can say so after the run. The database
//! has no column for it, and the Swift app never reads the file, so a
//! rollback ignores it. Rust only: `AVFoundation` conceals a damaged packet
//! and reports nothing.
//!
//! The decode stage records the count of every run, so a meeting
//! processed again shows what that run found; a meeting whose recording
//! decoded whole has no entry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use uuid::Uuid;

use crate::files::{read_json, write_json};

/// The damaged parts of each meeting's recording, as its last decode
/// counted them. Read with [`read_json`], so a missing or corrupt file
/// counts 0 for every meeting, and replaced with [`write_json`] on every
/// change; a file that may not be written keeps the counts of this run in
/// memory only, as [`ExportRetries`](crate::ExportRetries) does.
///
/// ```
/// use steno_pipeline::DamagedAudio;
/// use uuid::Uuid;
///
/// let dir = tempfile::tempdir()?;
/// let damaged = DamagedAudio::in_directory(dir.path());
/// let meeting = Uuid::new_v4();
/// damaged.record(meeting, 3);
/// assert_eq!(DamagedAudio::in_directory(dir.path()).parts(meeting), 3);
/// damaged.record(meeting, 0);
/// assert_eq!(DamagedAudio::in_directory(dir.path()).parts(meeting), 0);
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug)]
pub struct DamagedAudio {
    path: PathBuf,
    counts: Mutex<BTreeMap<Uuid, u32>>,
    /// False for [`in_memory`](Self::in_memory), and when the file on disk
    /// could not be read or set aside.
    writable: bool,
}

impl DamagedAudio {
    /// The file's name in the support directory.
    pub const FILE_NAME: &'static str = "damaged-audio.json";

    /// The counts in `path`, read now.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (counts, writable) = read_json(&path);
        DamagedAudio {
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
        DamagedAudio {
            path: PathBuf::new(),
            counts: Mutex::default(),
            writable: false,
        }
    }

    /// The parts of `meeting_id`'s recording replaced by silence.
    #[must_use]
    pub fn parts(&self, meeting_id: Uuid) -> u32 {
        self.lock().get(&meeting_id).copied().unwrap_or(0)
    }

    /// `parts` for `meeting_id`, replacing what an earlier run counted; 0
    /// removes the meeting.
    pub fn record(&self, meeting_id: Uuid, parts: u32) {
        let mut counts = self.lock();
        let changed = if parts == 0 {
            counts.remove(&meeting_id).is_some()
        } else {
            counts.insert(meeting_id, parts) != Some(parts)
        };
        if changed {
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

    /// The counts survive a restart, a count of 0 removes the meeting from
    /// the file, and a file that does not parse counts 0 for every meeting.
    #[test]
    fn counts_are_kept_across_launches_and_cleared_by_a_whole_decode() {
        let dir = tempfile::tempdir().unwrap();
        let (meeting, other) = (Uuid::new_v4(), Uuid::new_v4());
        let damaged = DamagedAudio::in_directory(dir.path());
        damaged.record(meeting, 2);
        damaged.record(meeting, 5);
        damaged.record(other, 1);
        let relaunched = DamagedAudio::in_directory(dir.path());
        assert_eq!((relaunched.parts(meeting), relaunched.parts(other)), (5, 1));
        relaunched.record(meeting, 0);
        relaunched.record(other, 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(DamagedAudio::FILE_NAME)).unwrap(),
            "{}"
        );

        std::fs::write(dir.path().join(DamagedAudio::FILE_NAME), b"{not json").unwrap();
        assert_eq!(DamagedAudio::in_directory(dir.path()).parts(meeting), 0);
    }
}
