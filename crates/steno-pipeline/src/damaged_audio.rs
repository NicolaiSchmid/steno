//! What of each meeting's recording the decoder could not read and
//! replaced by silence of its length
//! ([`AudioBuffer16k::damage`](steno_core::AudioBuffer16k::damage): the
//! parts and the seconds of silence), kept in `damaged-audio.json` in the
//! support directory (meeting id to damage), so the meeting's detail can
//! say so after the run. The database has no column for it, and the Swift
//! app never reads the file, so a rollback ignores it. Rust only:
//! `AVFoundation` conceals a damaged packet and reports nothing.
//!
//! The decode stage records the damage of every run, so a meeting
//! processed again shows what that run found; a meeting whose recording
//! decoded clean has no entry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use steno_core::AudioDamage;
use uuid::Uuid;

use crate::files::MeetingValues;

/// The damage of each meeting's recording, as its last decode counted it,
/// replaced with [`write_json`](crate::files::write_json) on every change.
///
/// Unlike [`ExportRetries`](crate::ExportRetries), whose counts may be lost,
/// this file is never set aside: a file that is there but cannot be read
/// or does not parse stays as it is and is never written, and the store
/// then does not [know](Self::known) which meetings are damaged. A missing
/// file holds no damage. A write that fails is returned by
/// [`record`](Self::record), and the decode stage fails the meeting, which
/// keeps its recording.
///
/// The retention wiring asks [`may_be_damaged`](Self::may_be_damaged),
/// which says yes for every meeting while the store does not know, and
/// keeps such a meeting's recording as it keeps an incomplete one's; the
/// user's own keep or delete is not affected.
///
/// ```
/// use steno_core::AudioDamage;
/// use steno_pipeline::DamagedAudio;
/// use uuid::Uuid;
///
/// let dir = tempfile::tempdir()?;
/// let damaged = DamagedAudio::in_directory(dir.path());
/// let meeting = Uuid::new_v4();
/// damaged.record(meeting, AudioDamage { parts: 3, seconds: 0.07 })?;
/// assert_eq!(DamagedAudio::in_directory(dir.path()).parts(meeting), 3);
/// damaged.record(meeting, AudioDamage::default())?;
/// assert!(!DamagedAudio::in_directory(dir.path()).may_be_damaged(meeting));
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug)]
pub struct DamagedAudio {
    damage: MeetingValues<AudioDamage>,
    /// False when the file is there but could not be read or parsed.
    known: bool,
}

impl DamagedAudio {
    /// The file's name in the support directory.
    pub const FILE_NAME: &'static str = "damaged-audio.json";

    /// The damage in `path`, read now.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let read = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error.to_string()),
        };
        match read {
            Ok(damage) => DamagedAudio {
                damage: MeetingValues::from_read(path, damage, true),
                known: true,
            },
            Err(error) => {
                tracing::warn!(
                    "{} could not be read ({error}); it stays and is not written, and every \
                     meeting's recording counts as possibly damaged until it reads again",
                    path.display()
                );
                DamagedAudio {
                    damage: MeetingValues::from_read(path, BTreeMap::new(), false),
                    known: false,
                }
            }
        }
    }

    /// The damage in [`FILE_NAME`](Self::FILE_NAME) in `directory` (the
    /// support directory), read now.
    #[must_use]
    pub fn in_directory(directory: &Path) -> Self {
        Self::new(directory.join(Self::FILE_NAME))
    }

    /// Damage that lives in memory only and is never written, for tests
    /// and a caller without a support directory.
    #[must_use]
    pub fn in_memory() -> Self {
        DamagedAudio {
            damage: MeetingValues::in_memory(),
            known: true,
        }
    }

    /// What of `meeting_id`'s recording was replaced by silence, as far as
    /// the store knows; none when it has no entry.
    #[must_use]
    pub fn damage(&self, meeting_id: Uuid) -> AudioDamage {
        self.damage.get(meeting_id).unwrap_or_default()
    }

    /// The parts of `meeting_id`'s recording replaced by silence, as far as
    /// the store knows.
    #[must_use]
    pub fn parts(&self, meeting_id: Uuid) -> u32 {
        self.damage(meeting_id).parts
    }

    /// Whether the file read: false while one is there that could not be
    /// read or parsed, so a meeting with no entry may still be damaged.
    #[must_use]
    pub fn known(&self) -> bool {
        self.known
    }

    /// Whether `meeting_id`'s recording has damaged parts, or may have
    /// them because the store does not [know](Self::known): what the
    /// retention wiring keeps a recording for.
    #[must_use]
    pub fn may_be_damaged(&self, meeting_id: Uuid) -> bool {
        !self.known || self.parts(meeting_id) > 0
    }

    /// `damage` for `meeting_id`, replacing what an earlier run counted;
    /// none removes the meeting. The error of a write that failed, after
    /// which the damage stays in memory; a store that may not write keeps
    /// it in memory without one.
    pub fn record(&self, meeting_id: Uuid, damage: AudioDamage) -> std::io::Result<()> {
        self.damage.change(|all| {
            if damage.is_none() {
                all.remove(&meeting_id).is_some()
            } else {
                all.insert(meeting_id, damage) != Some(damage)
            }
        })
    }

    /// Keeps only the damage of the meetings `keep` accepts: the launch
    /// drops the meetings that were deleted. A failed write is logged.
    pub fn retain(&self, keep: impl FnMut(Uuid) -> bool) {
        self.damage.retain(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn damage(parts: u32) -> AudioDamage {
        AudioDamage {
            parts,
            seconds: f64::from(parts) * 0.023,
        }
    }

    /// The damage survives a restart, none removes the meeting from the
    /// file, and the launch drops the meetings it does not keep.
    #[test]
    fn damage_is_kept_across_launches_and_cleared_by_a_clean_decode() {
        let dir = tempfile::tempdir().unwrap();
        let (meeting, other, deleted) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let damaged = DamagedAudio::in_directory(dir.path());
        damaged.record(meeting, damage(2)).unwrap();
        damaged.record(meeting, damage(5)).unwrap();
        damaged.record(other, damage(1)).unwrap();
        damaged.record(deleted, damage(4)).unwrap();
        let relaunched = DamagedAudio::in_directory(dir.path());
        assert!(relaunched.known());
        assert_eq!(relaunched.damage(meeting), damage(5));
        assert_eq!(relaunched.parts(other), 1);
        relaunched.retain(|id| id != deleted);
        assert_eq!(DamagedAudio::in_directory(dir.path()).parts(deleted), 0);
        relaunched.record(meeting, AudioDamage::default()).unwrap();
        relaunched.record(other, AudioDamage::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(DamagedAudio::FILE_NAME)).unwrap(),
            "{}"
        );
        assert!(!DamagedAudio::in_directory(dir.path()).may_be_damaged(meeting));
    }

    /// A file that does not parse stays as it is and is never written, and
    /// every meeting may then be damaged, also after a restart: the marks
    /// it held are not lost by starting empty.
    #[test]
    fn a_file_that_does_not_parse_leaves_every_meeting_possibly_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DamagedAudio::FILE_NAME);
        std::fs::write(&path, b"{not json").unwrap();
        let meeting = Uuid::new_v4();
        for _launch in 0..2 {
            let damaged = DamagedAudio::in_directory(dir.path());
            assert!(!damaged.known());
            assert!(damaged.may_be_damaged(meeting));
            damaged.record(meeting, damage(3)).unwrap();
            assert_eq!(damaged.parts(meeting), 3, "kept in memory");
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"{not json");
    }

    /// A file that cannot be read (a folder in its place) is not written
    /// either, and leaves every meeting possibly damaged.
    #[test]
    fn a_file_that_cannot_be_read_leaves_every_meeting_possibly_damaged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(DamagedAudio::FILE_NAME)).unwrap();
        let damaged = DamagedAudio::in_directory(dir.path());
        assert!(!damaged.known());
        assert!(damaged.may_be_damaged(Uuid::new_v4()));
        damaged.record(Uuid::new_v4(), damage(1)).unwrap();
        assert!(dir.path().join(DamagedAudio::FILE_NAME).is_dir());
    }

    /// A write that fails (a support directory that may not be written) is
    /// an error for the decode stage to fail the meeting with; a clean
    /// decode, which changes nothing, writes nothing and succeeds.
    #[cfg(unix)]
    #[test]
    fn a_write_that_fails_is_an_error() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        std::fs::create_dir(&support).unwrap();
        let damaged = DamagedAudio::in_directory(&support);
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o500)).unwrap();
        let meeting = Uuid::new_v4();
        let written = damaged.record(meeting, damage(3));
        let clean = damaged.record(Uuid::new_v4(), AudioDamage::default());
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Root writes anywhere; the rest of the test still holds.
        if !support.join(DamagedAudio::FILE_NAME).exists() {
            assert!(written.is_err());
        }
        assert!(clean.is_ok());
        assert_eq!(damaged.parts(meeting), 3, "kept in memory");
    }
}
