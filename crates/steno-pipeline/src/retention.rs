//! Removes the audio of every asset whose expiry has passed.
//! Swift: `Sources/StenoCore/Storage/RetentionSweep.swift`.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use steno_core::{Store, StoreError, paths::path_from_file_url};

/// Every file the sweep could not remove, with the reason.
#[derive(Debug, thiserror::Error)]
pub enum SweepIncomplete {
    #[error("retention sweep could not remove {}", .0.iter().map(|(path, error)| format!("{} ({error})", path.display())).collect::<Vec<_>>().join(", "))]
    Files(Vec<(PathBuf, std::io::Error)>),
    #[error(transparent)]
    Store(#[from] StoreError),
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
                .filter(|speaker| speaker.assignment.is_confirmed() && speaker.sample_clip_url.is_some())
                .collect();
            let files: Vec<PathBuf> = asset
                .expirable_files()
                .iter()
                .chain(confirmed_with_clips.iter().filter_map(|s| s.sample_clip_url.as_ref()))
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
