//! Transcription: the engine and the components it needs on this computer,
//! with download progress. Changing the engine rebuilds the pipeline.
//! Swift: `Settings/SpeechSettingsViewModel.swift`.

use std::collections::{BTreeMap, BTreeSet};

use steno_core::Store;
use steno_core::protocols::BoxError;

use super::{SectionError, update_settings};
use crate::labels::file_size;
use crate::services::{Services, SpeechModels};
use crate::speech::{ModelAsset, SpeechEngineId};

/// Swift: `SpeechSettingsViewModel.AssetState`.
#[derive(Debug, Clone, PartialEq)]
pub enum AssetState {
    Absent,
    Downloading { fraction: f64, phase: String },
    Installed { bytes: Option<i64> },
    Failed(String),
}

#[derive(Debug)]
pub struct SpeechSettingsViewModel {
    pub engine_id: SpeechEngineId,
    pub(crate) asset_states: BTreeMap<ModelAsset, AssetState>,
    /// The assets whose download thread still runs. Its reports show while
    /// the asset reads `downloading`; a remove marks it absent, which
    /// detaches the one in flight, so its late progress cannot mark the
    /// asset downloading again. A second download of the asset starts no
    /// thread: it reattaches to the one still running, whose next report
    /// shows. Swift: `downloads`.
    downloads: BTreeSet<ModelAsset>,
    pub errors: SectionError,
}

impl SpeechSettingsViewModel {
    #[must_use]
    pub fn new() -> Self {
        SpeechSettingsViewModel {
            engine_id: SpeechEngineId::ParakeetV3,
            asset_states: BTreeMap::new(),
            downloads: BTreeSet::new(),
            errors: SectionError::default(),
        }
    }

    /// The engines the settings pane offers.
    #[must_use]
    pub fn engines() -> &'static [SpeechEngineId] {
        &SpeechEngineId::USER_SELECTABLE
    }

    /// The assets the selected engine needs: its model and the diarizer.
    #[must_use]
    pub fn assets(&self) -> [ModelAsset; 2] {
        [self.engine_id.asset(), ModelAsset::OfflineDiarizer]
    }

    /// The picker is only worth a row when there is a choice.
    #[must_use]
    pub fn shows_engine_picker() -> bool {
        Self::engines().len() > 1
    }

    #[must_use]
    pub fn all_installed(&self) -> bool {
        self.assets()
            .iter()
            .all(|asset| matches!(self.state_of(*asset), AssetState::Installed { .. }))
    }

    pub fn load(&mut self, store: &Store, services: &Services) {
        match store.settings() {
            Ok(settings) => {
                self.engine_id = settings
                    .speech_engine_id
                    .parse()
                    .unwrap_or(SpeechEngineId::ParakeetV3);
            }
            Err(error) => self.errors.fail("Settings could not be loaded.", error),
        }
        self.refresh_states(services);
    }

    pub fn refresh_states(&mut self, services: &Services) {
        for asset in ModelAsset::ALL {
            if matches!(
                self.asset_states.get(asset),
                Some(AssetState::Downloading { .. })
            ) {
                continue;
            }
            let state = if services.speech_models.is_installed(*asset) {
                AssetState::Installed {
                    bytes: services.speech_models.installed_size(*asset),
                }
            } else {
                AssetState::Absent
            };
            self.asset_states.insert(*asset, state);
        }
    }

    #[must_use]
    pub fn state_of(&self, asset: ModelAsset) -> AssetState {
        self.asset_states
            .get(&asset)
            .cloned()
            .unwrap_or(AssetState::Absent)
    }

    /// What the component does, not what the model is called.
    #[must_use]
    pub fn component_title(asset: ModelAsset) -> &'static str {
        if asset == ModelAsset::OfflineDiarizer {
            "Speaker recognition"
        } else {
            "Speech recognition"
        }
    }

    /// "Parakeet · fast · 25 languages".
    #[must_use]
    pub fn engine_title(id: SpeechEngineId) -> String {
        let (name, speed) = match id {
            SpeechEngineId::ParakeetV3 => ("Parakeet", Some("fast")),
            SpeechEngineId::ParakeetUltra => ("Parakeet Ultra", None),
            SpeechEngineId::ParakeetDe => ("Parakeet (German)", None),
            SpeechEngineId::WhisperKitLargeV3Turbo => ("Whisper", Some("slower")),
        };
        let languages = id.supported_language_count();
        let language_text = if languages == 1 {
            "1 language".to_owned()
        } else {
            format!("{languages} languages")
        };
        [Some(name), speed, Some(language_text.as_str())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// "Installed · 485 MB", "Downloading… 40%", "Not downloaded · 485 MB",
    /// the size before a download (or of an installed asset whose size
    /// could not be read) as `models` expects it.
    #[must_use]
    pub fn status_text(&self, asset: ModelAsset, models: &dyn SpeechModels) -> String {
        match self.state_of(asset) {
            AssetState::Absent | AssetState::Failed(_) => {
                format!(
                    "Not downloaded · {}",
                    file_size(models.expected_bytes(asset))
                )
            }
            AssetState::Downloading { fraction, .. } => {
                if fraction > 0.0 {
                    // A percentage in 0...100 fits any integer type.
                    #[allow(clippy::cast_possible_truncation)]
                    let percent = (fraction * 100.0).round() as i64;
                    format!("Downloading… {percent}%")
                } else {
                    "Downloading…".to_owned()
                }
            }
            AssetState::Installed { bytes } => format!(
                "Installed · {}",
                file_size(bytes.unwrap_or_else(|| models.expected_bytes(asset)))
            ),
        }
    }

    pub fn set_engine(&mut self, id: SpeechEngineId, store: &Store, services: &Services) {
        if id == self.engine_id {
            return;
        }
        self.engine_id = id;
        let outcome = update_settings(store, |settings| {
            id.as_str().clone_into(&mut settings.speech_engine_id);
        })
        .map_err(BoxError::from)
        .and_then(|_| services.pipeline.reload());
        match outcome {
            Ok(()) => self.errors.clear(),
            Err(error) => self
                .errors
                .fail("The language model could not be changed.", error),
        }
        self.refresh_states(services);
    }

    /// Marks the download started: whether the caller should run it. When
    /// one of the asset's still runs, nothing new starts; a remove had
    /// detached it, so the asset reads `downloading` again and the running
    /// thread's next report shows, as Swift's did. Swift: `download(_:)`'s
    /// guard.
    pub fn begin_download(&mut self, asset: ModelAsset) -> bool {
        let starts = self.downloads.insert(asset);
        if starts || !self.shows_download(asset) {
            self.asset_states.insert(
                asset,
                AssetState::Downloading {
                    fraction: 0.0,
                    phase: "starting".to_owned(),
                },
            );
        }
        starts
    }

    /// One progress report, shown unless a remove detached the download.
    pub fn download_progress(&mut self, asset: ModelAsset, fraction: f64, phase: &str) {
        if self.shows_download(asset) {
            self.asset_states.insert(
                asset,
                AssetState::Downloading {
                    fraction,
                    phase: phase.to_owned(),
                },
            );
        }
    }

    /// Whether a running download still shows: only `begin_download` marks
    /// an asset `downloading`, and while its thread runs only a remove
    /// takes it out again.
    fn shows_download(&self, asset: ModelAsset) -> bool {
        matches!(
            self.asset_states.get(&asset),
            Some(AssetState::Downloading { .. })
        )
    }

    /// The download's thread ended (or never started): installed or failed;
    /// a detached download leaves what the model store reports.
    pub fn finish_download(
        &mut self,
        asset: ModelAsset,
        outcome: Result<(), BoxError>,
        services: &Services,
    ) {
        self.downloads.remove(&asset);
        if !self.shows_download(asset) {
            self.refresh_states(services);
            return;
        }
        let state = match outcome {
            Ok(()) => AssetState::Installed {
                bytes: services.speech_models.installed_size(asset),
            },
            Err(error) => AssetState::Failed(error.to_string()),
        };
        self.asset_states.insert(asset, state);
    }

    pub fn remove(&mut self, asset: ModelAsset, services: &Services) {
        match services.speech_models.remove(asset) {
            Ok(()) => {
                self.asset_states.insert(asset, AssetState::Absent);
            }
            Err(error) => self
                .errors
                .fail("The download could not be removed.", error),
        }
    }
}

impl Default for SpeechSettingsViewModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::FakeServices;

    /// The host's path when the download thread cannot start: the asset
    /// leaves `downloading` for the failure, and may be downloaded again.
    #[test]
    fn a_download_that_never_ran_shows_its_failure_and_may_start_again() {
        let fakes = FakeServices::new(chrono::Utc::now());
        let services = fakes.services();
        let mut model = SpeechSettingsViewModel::new();
        assert!(model.begin_download(ModelAsset::OfflineDiarizer));
        assert!(!model.begin_download(ModelAsset::OfflineDiarizer));
        model.finish_download(
            ModelAsset::OfflineDiarizer,
            Err("The download could not start: no threads".into()),
            &services,
        );
        assert_eq!(
            model.state_of(ModelAsset::OfflineDiarizer),
            AssetState::Failed("The download could not start: no threads".to_owned())
        );
        assert!(model.begin_download(ModelAsset::OfflineDiarizer));
    }

    /// Download, remove, Download again while the first thread runs: no
    /// second thread, the asset reads `downloading` again and the running
    /// thread's reports and its outcome show. Swift kept showing that
    /// download's progress.
    #[test]
    fn a_download_again_after_a_remove_reattaches_to_the_running_thread() {
        let fakes = FakeServices::new(chrono::Utc::now());
        let services = fakes.services();
        let asset = ModelAsset::OfflineDiarizer;
        let mut model = SpeechSettingsViewModel::new();
        assert!(model.begin_download(asset));
        model.download_progress(asset, 0.4, "downloading");
        model.remove(asset, &services);
        assert_eq!(model.state_of(asset), AssetState::Absent);
        model.download_progress(asset, 0.5, "downloading");
        assert_eq!(model.state_of(asset), AssetState::Absent, "detached");

        assert!(!model.begin_download(asset), "no second thread");
        assert_eq!(
            model.state_of(asset),
            AssetState::Downloading {
                fraction: 0.0,
                phase: "starting".to_owned()
            }
        );
        model.download_progress(asset, 0.6, "downloading");
        assert_eq!(
            model.state_of(asset),
            AssetState::Downloading {
                fraction: 0.6,
                phase: "downloading".to_owned()
            },
            "reattached"
        );
        assert!(!model.begin_download(asset));
        assert_eq!(
            model.state_of(asset),
            AssetState::Downloading {
                fraction: 0.6,
                phase: "downloading".to_owned()
            },
            "a click while it shows changes nothing"
        );
        fakes.speech_models.set_installed(asset, Some(42));
        model.finish_download(asset, Ok(()), &services);
        assert_eq!(
            model.state_of(asset),
            AssetState::Installed { bytes: Some(42) }
        );
    }
}
