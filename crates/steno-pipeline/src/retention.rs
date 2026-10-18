//! Removes the audio of every asset whose expiry has passed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use steno_core::{RecordingLayout, Store, StoreError, paths::file_url_path};

use crate::sample_clips;

/// Why a sweep stopped short: files it could not remove, or a store read or
/// write that failed.
#[derive(Debug, thiserror::Error)]
pub enum SweepIncomplete {
    #[error("retention sweep could not remove {}", listed(.0))]
    Files(Vec<(PathBuf, std::io::Error)>),
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn listed(failures: &[(PathBuf, std::io::Error)]) -> String {
    failures
        .iter()
        .map(|(path, error)| format!("{} ({error})", path.display()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Master, sidecars and mixdown go together with the sample clips of the
/// meeting's confirmed speakers and the clip files no row names
/// ([`sample_clips`]); unconfirmed speakers keep their clips. The asset row
/// loses `expires_at` once every file is gone, so a sweep runs once per
/// expiry. A file whose presence cannot be read counts as there, so its
/// removal fails and the stamp stays for the next sweep. Each asset is
/// checked again in the write that removes its files
/// ([`Store::sweep_expired_asset`]), so a meeting queued since keeps its
/// audio.
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
        for listed in self.store.expired_assets(now)? {
            self.store.sweep_expired_asset(listed.id, now, |expired| {
                let files: Vec<PathBuf> = expired
                    .asset
                    .expirable_files()
                    .iter()
                    .chain(
                        expired
                            .confirmed_with_clips()
                            .filter_map(|s| s.sample_clip_url.as_ref()),
                    )
                    .filter_map(|url| file_url_path(url))
                    .collect();
                let mut clean = true;
                for path in files.into_iter().filter(|path| !known_gone(path)) {
                    match std::fs::remove_file(&path) {
                        Ok(()) => removed.push(path),
                        Err(error) => {
                            clean = false;
                            failures.push((path, error));
                        }
                    }
                }
                // The write lock keeps a run from starting, so no clip here
                // is a run's uncommitted one.
                if let Some(layout) = RecordingLayout::own_folder(&expired.asset) {
                    let owners: BTreeSet<_> = expired.speakers.iter().map(|s| s.id).collect();
                    removed.extend(sample_clips::sweep(
                        &layout.speakers_directory(),
                        &owners,
                        &expired.sample_clip_urls,
                        None,
                    ));
                }
                clean
            })?;
        }
        if failures.is_empty() {
            Ok(removed)
        } else {
            Err(SweepIncomplete::Files(failures))
        }
    }

    /// Every asset whose master file is still on disk, or may be, becomes
    /// keep forever with `expires_at` cleared, in one write. Returns how
    /// many.
    pub fn keep_all(&self) -> Result<usize, StoreError> {
        let ids: Vec<_> = self
            .store
            .assets()?
            .into_iter()
            .filter(|asset| file_url_path(&asset.url).is_some_and(|path| !known_gone(&path)))
            .map(|asset| asset.id)
            .collect();
        self.store.keep_forever(&ids)?;
        Ok(ids.len())
    }
}

/// Whether `path` is known not to exist. Any error reading its metadata,
/// even one that proves absence (`ENOTDIR`, `ELOOP`), is not knowing: the
/// stamp stays and each sweep tries again.
fn known_gone(path: &Path) -> bool {
    matches!(path.try_exists(), Ok(false))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use chrono::Duration;
    use steno_core::testing::sample_data;
    use steno_core::{
        AudioAsset, AudioFormat, AudioLane, AudioRetention, MeetingState, RecordingLayout, Speaker,
        SpeakerAssignment, paths::file_url,
    };
    use uuid::Uuid;

    use super::*;

    /// A master shared with a kept row stays until both rows are due, then
    /// goes and both rows lose `expiresAt`.
    #[test]
    fn the_sweep_keeps_a_master_another_asset_row_still_needs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
        let now = Utc::now();
        let mut meeting = sample_data::meeting();
        meeting.state = MeetingState::Ready;
        let layout = RecordingLayout::new(&dir.path().join("audio"), meeting.id);
        layout.create_directories(true).unwrap();
        let master = layout.master(AudioFormat::Wav16kInt16);
        std::fs::write(&master, b"wav").unwrap();
        let kept = AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: file_url(&master, false),
            format: AudioFormat::Wav16kInt16,
            lanes: vec![AudioLane::Mixed],
            sidecars_16k: BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::KeepForever,
            expires_at: None,
        };
        let expired = AudioAsset {
            id: Uuid::new_v4(),
            retention: AudioRetention::DeleteAfterProcessing,
            expires_at: Some(now - Duration::hours(1)),
            ..kept.clone()
        };
        store.save_meeting_with_asset(&meeting, &kept).unwrap();
        store.save_asset(&expired).unwrap();
        let rows = || {
            let mut rows = store.assets().unwrap();
            rows.sort_by_key(|asset| asset.id);
            rows
        };
        let before = rows();

        let removed = RetentionSweep::new(store.clone()).run(now).unwrap();
        assert_eq!(removed, Vec::<PathBuf>::new());
        assert!(master.exists(), "the kept row's master stays");
        assert_eq!(rows(), before, "neither row changes");

        store
            .save_asset(&AudioAsset {
                retention: AudioRetention::KeepDays(1),
                expires_at: Some(now - Duration::hours(1)),
                ..kept
            })
            .unwrap();
        RetentionSweep::new(store.clone()).run(now).unwrap();
        assert!(!master.exists(), "both rows due: the master goes");
        assert!(
            rows().iter().all(|asset| asset.expires_at.is_none()),
            "both rows lose their stamp"
        );
    }

    /// A ready meeting whose one asset, in its own folder, expired an hour
    /// before `now`: the layout, the master and the asset.
    #[cfg(unix)]
    fn expired_recording(
        store: &Store,
        dir: &Path,
        now: DateTime<Utc>,
    ) -> (RecordingLayout, PathBuf, AudioAsset) {
        let mut meeting = sample_data::meeting();
        meeting.state = MeetingState::Ready;
        let layout = RecordingLayout::new(&dir.join("audio"), meeting.id);
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
        (layout, master, asset)
    }

    /// Makes a folder unsearchable and searchable again when dropped.
    #[cfg(unix)]
    struct Unsearchable<'a>(&'a Path);

    #[cfg(unix)]
    impl<'a> Unsearchable<'a> {
        fn new(folder: &'a Path) -> Self {
            Self::set(folder, 0o000).unwrap();
            Unsearchable(folder)
        }

        fn set(folder: &Path, mode: u32) -> std::io::Result<()> {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(mode))
        }
    }

    #[cfg(unix)]
    impl Drop for Unsearchable<'_> {
        fn drop(&mut self) {
            // A panic here while a test already panics would abort.
            let _ = Self::set(self.0, 0o755);
        }
    }

    /// An expired master in an unsearchable folder is not removed and the
    /// stamp stays; once searchable, the next sweep removes it. Skipped as
    /// root.
    #[cfg(unix)]
    #[test]
    fn a_master_in_a_folder_the_sweep_cannot_search_keeps_its_stamp_until_it_goes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
        let now = Utc::now();
        let (layout, master, asset) = expired_recording(&store, dir.path(), now);
        let sweep = RetentionSweep::new(store.clone());

        let unsearchable = Unsearchable::new(&layout.directory);
        if master.try_exists().is_ok() {
            return;
        }
        let first = sweep.run(now);
        drop(unsearchable);
        let Err(SweepIncomplete::Files(failures)) = first else {
            panic!("the sweep removed nothing and says so: {first:?}");
        };
        let failed: Vec<&PathBuf> = failures.iter().map(|(path, _)| path).collect();
        assert_eq!(failed, [&master]);
        assert!(master.exists(), "the master stays");
        let due: Vec<Uuid> = store
            .expired_assets(now)
            .unwrap()
            .iter()
            .map(|asset| asset.id)
            .collect();
        assert_eq!(due, [asset.id], "the stamp stays, for the next sweep");

        assert_eq!(sweep.run(now).unwrap(), std::slice::from_ref(&master));
        assert!(!master.exists(), "the next sweep removes the master");
        assert_eq!(
            store.asset_by_id(asset.id).unwrap().unwrap().expires_at,
            None
        );
    }

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

    /// A meeting `reprocess` queued after the list was read keeps its audio
    /// and its row.
    #[test]
    fn an_asset_whose_meeting_was_queued_since_the_list_was_read_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
        let now = Utc::now();
        let mut meeting = sample_data::meeting();
        meeting.state = MeetingState::Ready;
        let layout = RecordingLayout::new(&dir.path().join("audio"), meeting.id);
        layout.create_directories(false).unwrap();
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
        let listed: Vec<Uuid> = store
            .expired_assets(now)
            .unwrap()
            .iter()
            .map(|asset| asset.id)
            .collect();
        assert_eq!(listed, [asset.id]);

        meeting.state = MeetingState::Queued;
        store.save_meeting(&meeting).unwrap();
        let swept = store
            .sweep_expired_asset(asset.id, now, |_| {
                panic!("a queued meeting's files are not removed")
            })
            .unwrap();
        assert!(!swept);
        assert!(master.exists());
        assert!(
            store
                .asset(meeting.id)
                .unwrap()
                .unwrap()
                .expires_at
                .is_some(),
            "the stamp is left for a later sweep"
        );

        meeting.state = MeetingState::Ready;
        store.save_meeting(&meeting).unwrap();
        assert_eq!(
            RetentionSweep::new(store.clone()).run(now).unwrap(),
            [master]
        );
        assert_eq!(store.asset(meeting.id).unwrap().unwrap().expires_at, None);
    }
}
