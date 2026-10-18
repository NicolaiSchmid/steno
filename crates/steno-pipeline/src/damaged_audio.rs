//! What of each meeting's recording the decoder could not read and replaced
//! by silence, kept in `damaged-audio.json` in the support directory so the
//! meeting's detail can say so. The decode stage records every run's damage;
//! a clean decode has no entry. A file that cannot be read or parsed is left
//! alone, and every recording counts as possibly damaged until it reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use steno_core::AudioDamage;
use uuid::Uuid;

use crate::files::MeetingValues;

/// The damage of each meeting's recording, as its last decode counted it.
///
/// Unlike [`ExportRetries`](crate::ExportRetries), this file is never set
/// aside: one that cannot be read or parsed stays as it is and is never
/// written, and [`may_be_damaged`](Self::may_be_damaged) then says yes for
/// every meeting, so retention keeps their recordings. A write that fails is
/// returned by [`record`](Self::record), and the decode stage fails the
/// meeting.
///
/// ```
/// use steno_core::AudioDamage;
/// use steno_pipeline::DamagedAudio;
/// use uuid::Uuid;
///
/// let dir = tempfile::tempdir()?;
/// let damaged = DamagedAudio::in_directory(dir.path());
/// let meeting = Uuid::new_v4();
/// damaged.record(meeting, AudioDamage { parts: 3, seconds: 0.07, salvaged: false })?;
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
                     meeting's recording counts as possibly damaged until it reads again: \
                     remove the file or repair it",
                    path.display()
                );
                DamagedAudio {
                    damage: MeetingValues::from_read(path, BTreeMap::new(), false),
                    known: false,
                }
            }
        }
    }

    /// The damage in [`FILE_NAME`](Self::FILE_NAME) in `directory`, read now.
    #[must_use]
    pub fn in_directory(directory: &Path) -> Self {
        Self::new(directory.join(Self::FILE_NAME))
    }

    /// Damage that lives in memory only and is never written.
    #[must_use]
    pub fn in_memory() -> Self {
        DamagedAudio {
            damage: MeetingValues::in_memory(),
            known: true,
        }
    }

    /// What of `meeting_id`'s recording was replaced by silence; none when it
    /// has no entry.
    #[must_use]
    pub fn damage(&self, meeting_id: Uuid) -> AudioDamage {
        self.damage.get(meeting_id).unwrap_or_default()
    }

    /// The parts of `meeting_id`'s recording replaced by silence.
    #[must_use]
    pub fn parts(&self, meeting_id: Uuid) -> u32 {
        self.damage(meeting_id).parts
    }

    /// Whether the file read; while false a meeting with no entry may still
    /// be damaged.
    #[must_use]
    pub fn known(&self) -> bool {
        self.known
    }

    /// Whether `meeting_id`'s recording has damaged parts, was salvaged, or
    /// may be either because the file did not read: what retention keeps a
    /// recording for.
    #[must_use]
    pub fn may_be_damaged(&self, meeting_id: Uuid) -> bool {
        !self.known || !self.damage(meeting_id).is_none()
    }

    /// Records `damage` for `meeting_id`, replacing an earlier run's; none
    /// removes the meeting. On a failed write the damage stays in memory.
    pub fn record(&self, meeting_id: Uuid, damage: AudioDamage) -> std::io::Result<()> {
        self.damage.change(|all| {
            if damage.is_none() {
                all.remove(&meeting_id).is_some()
            } else {
                all.insert(meeting_id, damage) != Some(damage)
            }
        })
    }

    /// Keeps only the damage of the meetings `keep` accepts; a failed write
    /// is logged.
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
            salvaged: false,
        }
    }

    /// The damage survives a restart, none removes the meeting, and the
    /// launch drops the meetings it does not keep.
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

    /// A salvaged recording counts as damaged with no part lost, also after
    /// a restart.
    #[test]
    fn a_salvaged_recording_may_be_damaged_with_no_part_lost() {
        let dir = tempfile::tempdir().unwrap();
        let meeting = Uuid::new_v4();
        let salvaged = AudioDamage {
            salvaged: true,
            ..AudioDamage::default()
        };
        DamagedAudio::in_directory(dir.path())
            .record(meeting, salvaged)
            .unwrap();
        let relaunched = DamagedAudio::in_directory(dir.path());
        assert_eq!(relaunched.damage(meeting), salvaged);
        assert_eq!(relaunched.parts(meeting), 0);
        assert!(relaunched.may_be_damaged(meeting));
        assert!(!relaunched.may_be_damaged(Uuid::new_v4()));
    }

    /// A file that does not parse is never written, and every meeting may
    /// then be damaged.
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

    /// A file that cannot be read is not written either.
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

    /// A failed write is an error; a clean decode writes nothing and
    /// succeeds.
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
