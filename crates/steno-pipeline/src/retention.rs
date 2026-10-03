//! Removes the audio of every asset whose expiry has passed.
//! Swift: `Sources/StenoCore/Storage/RetentionSweep.swift`.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use steno_core::{Store, StoreError, paths::path_from_file_url};

/// Why a sweep stopped short: files it could not remove, or a store read
/// that failed.
#[derive(Debug, thiserror::Error)]
pub enum SweepIncomplete {
    #[error("retention sweep could not remove {}", listed(.0))]
    Files(Vec<(PathBuf, std::io::Error)>),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// `path (reason), path (reason)`.
fn listed(failures: &[(PathBuf, std::io::Error)]) -> String {
    failures
        .iter()
        .map(|(path, error)| format!("{} ({error})", path.display()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Master, sidecars and mixdown go together with the sample clips of the
/// meeting's confirmed speakers. Unconfirmed speakers keep their clips
/// until confirmation or deletion. The asset row keeps its URLs and loses
/// `expires_at` once every file is gone, so a sweep runs once per expiry.
/// A missing file is skipped; a file that cannot be removed leaves the
/// stamp in place for the next sweep and never stops the sweep from
/// reaching the other assets.
#[derive(Debug, Clone)]
pub struct RetentionSweep {
    store: Arc<Store>,
}

impl RetentionSweep {
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        RetentionSweep { store }
    }

    /// The files actually removed, in asset order. Fails with
    /// [`SweepIncomplete::Files`] after visiting every asset when any file
    /// resisted.
    pub fn run(&self, now: DateTime<Utc>) -> Result<Vec<PathBuf>, SweepIncomplete> {
        let mut removed = Vec::new();
        let mut failures = Vec::new();
        for asset in self.store.expired_assets(now)? {
            let confirmed_with_clips: Vec<_> = self
                .store
                .speakers(asset.meeting_id)?
                .into_iter()
                .filter(|speaker| {
                    speaker.assignment.is_confirmed() && speaker.sample_clip_url.is_some()
                })
                .collect();
            let files: Vec<PathBuf> = asset
                .expirable_files()
                .iter()
                .chain(
                    confirmed_with_clips
                        .iter()
                        .filter_map(|s| s.sample_clip_url.as_ref()),
                )
                .filter_map(|url| path_from_file_url(url))
                .collect();
            let mut clean = true;
            for path in files.into_iter().filter(|path| path.exists()) {
                match std::fs::remove_file(&path) {
                    Ok(()) => removed.push(path),
                    Err(error) => {
                        clean = false;
                        failures.push((path, error));
                    }
                }
            }
            if !clean {
                continue;
            }
            let ids: Vec<_> = confirmed_with_clips.iter().map(|s| s.id).collect();
            self.store.clear_sample_clips(asset.meeting_id, &ids)?;
            let mut swept = asset;
            swept.expires_at = None;
            self.store.save_asset(&swept)?;
        }
        if failures.is_empty() {
            Ok(removed)
        } else {
            Err(SweepIncomplete::Files(failures))
        }
    }

    /// Every asset whose master file is still on disk becomes keep forever
    /// with `expires_at` cleared, in one write. Returns how many.
    pub fn keep_all(&self) -> Result<usize, StoreError> {
        let ids: Vec<_> = self
            .store
            .assets()?
            .into_iter()
            .filter(|asset| path_from_file_url(&asset.url).is_some_and(|path| path.exists()))
            .map(|asset| asset.id)
            .collect();
        self.store.keep_forever(&ids)?;
        Ok(ids.len())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Duration;
    use steno_core::testing::sample_data;
    use steno_core::{
        AudioAsset, AudioFormat, AudioLane, AudioRetention, MeetingState, RecordingLayout, Speaker,
        SpeakerAssignment, paths::file_url,
    };
    use uuid::Uuid;

    use super::*;

    #[test]
    fn the_sweep_removes_the_clips_of_confirmed_speakers_and_keeps_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
        let now = Utc::now();
        let person = sample_data::person(0, "Anna");
        store.save_person(&person).unwrap();
        let mut meeting = sample_data::meeting();
        meeting.state = MeetingState::Ready;
        let layout = RecordingLayout::new(&dir.path().join("audio"), meeting.id);
        layout.create_directories(true).unwrap();
        let master = layout.master(AudioFormat::Wav16kInt16);
        std::fs::write(&master, b"wav").unwrap();
        let asset = AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: file_url(&master, false),
            format: AudioFormat::Wav16kInt16,
            lanes: vec![AudioLane::Mixed],
            sidecars_16k: BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::KeepDays(1),
            expires_at: Some(now - Duration::hours(1)),
        };
        store.save_meeting_with_asset(&meeting, &asset).unwrap();
        let confirmed = Uuid::new_v4();
        let unconfirmed = Uuid::new_v4();
        for (id, assignment) in [
            (
                confirmed,
                SpeakerAssignment::Confirmed {
                    person_id: person.id,
                },
            ),
            (unconfirmed, SpeakerAssignment::Unknown),
        ] {
            std::fs::write(layout.sample_clip(id), b"clip").unwrap();
            store
                .save_speaker(&Speaker {
                    id,
                    meeting_id: meeting.id,
                    cluster_label: format!("SPEAKER_{id}"),
                    assignment,
                    embedding: None,
                    sample_clip_range: None,
                    sample_clip_url: Some(file_url(&layout.sample_clip(id), false)),
                    cluster_confidence: 1.0,
                })
                .unwrap();
        }

        let removed = RetentionSweep::new(store.clone()).run(now).unwrap();
        assert_eq!(removed, vec![master.clone(), layout.sample_clip(confirmed)]);
        assert!(!master.exists());
        assert!(!layout.sample_clip(confirmed).exists());
        assert!(
            layout.sample_clip(unconfirmed).exists(),
            "an unconfirmed speaker keeps its clip until confirmation"
        );
        let speakers = store.speakers(meeting.id).unwrap();
        let clip_of = |id| {
            speakers
                .iter()
                .find(|speaker| speaker.id == id)
                .unwrap()
                .sample_clip_url
                .clone()
        };
        assert_eq!(clip_of(confirmed), None);
        assert!(clip_of(unconfirmed).is_some());
        assert_eq!(
            store.asset(meeting.id).unwrap().unwrap().expires_at,
            None,
            "the stamp is cleared once every file is gone"
        );
    }
}
