//! The speech engine per platform (`CoreML` on the Mac, ONNX Runtime
//! elsewhere, behind `SpeechEngine`), the ONNX diarizer, and the host's
//! `SpeechModels` over the two model stores. The ONNX engine runs in this
//! process until the sidecar (`WP4c`) lands; the trait object is the seam.
//! Swift: `makeSpeechEngine`, `makeDiarizer`, `ModelStore`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use steno_core::{Diarizer, Settings, SpeechEngine, StenoPaths, paths::path_from_file_url};
use steno_diarize::{DiarizerConfig, ModelDiarizer};
use steno_host::services::SpeechModels;
use steno_host::speech::ModelAsset;
use steno_speech::{ModelStore, OnnxOptions, OnnxSpeechEngine};

/// Threads for one ONNX operator; the plan measured at four.
pub const ONNX_THREADS: usize = 4;

/// Where the speech models live: `settings.models_directory` when set,
/// else `STENO_MODELS_DIR`, else the support directory's `Models/onnx`.
#[must_use]
pub fn speech_store(settings: &Settings, paths: &StenoPaths) -> ModelStore {
    match settings
        .models_directory
        .as_deref()
        .and_then(|url| path_from_file_url(url).or_else(|| Some(PathBuf::from(url))))
    {
        Some(root) => ModelStore::new(root.join("onnx")),
        None if std::env::var_os(ModelStore::ENVIRONMENT_VARIABLE).is_some() => {
            ModelStore::from_environment()
        }
        None => ModelStore::new(paths.support_directory.join("Models").join("onnx")),
    }
}

/// The diarization models beside the speech ones.
#[must_use]
pub fn diarize_store(speech: &ModelStore) -> steno_diarize::models::ModelStore {
    steno_diarize::models::ModelStore::new(speech.root().join("diarization"))
}

/// The engine the settings name. On the Mac `parakeet-v3` is the `CoreML`
/// engine on the Neural Engine; everywhere else, and for any other id, the
/// fp32 ONNX export of the same model (the engine ids the Swift app
/// offered beyond Parakeet v3 have no Rust counterpart yet; the parity
/// list says so).
#[must_use]
pub fn speech_engine(settings: &Settings, store: &ModelStore) -> Arc<dyn SpeechEngine> {
    #[cfg(target_os = "macos")]
    {
        if settings.speech_engine_id == steno_speech_coreml::ENGINE_ID {
            return Arc::new(steno_speech_coreml::CoreMlParakeetEngine::new(
                store.root().parent().map_or_else(
                    steno_speech_coreml::default_model_directory,
                    |models| steno_speech_coreml::engine::model_directory(models.parent().unwrap_or(models)),
                ),
            ));
        }
    }
    let _ = &settings.speech_engine_id;
    Arc::new(OnnxSpeechEngine::new(
        store.clone(),
        OnnxOptions {
            intra_threads: ONNX_THREADS,
            ..OnnxOptions::default()
        },
    ))
}

/// The ONNX diarizer, loading its two models from the store on first use.
#[must_use]
pub fn diarizer(store: &ModelStore) -> Arc<dyn Diarizer> {
    Arc::new(ModelDiarizer::onnx(
        DiarizerConfig::default(),
        diarize_store(store),
        ONNX_THREADS,
    ))
}

/// The host's model service over the two stores.
pub struct RealSpeechModels {
    pub speech: ModelStore,
}

impl RealSpeechModels {
    fn speech_asset(asset: ModelAsset) -> Option<steno_speech::ModelAsset> {
        match asset {
            ModelAsset::ParakeetV3 => Some(steno_speech::ModelAsset::parakeet_v3_fp32()),
            _ => None,
        }
    }

    fn diarizer_paths(&self) -> [PathBuf; 2] {
        let store = diarize_store(&self.speech);
        [
            store.path(&steno_diarize::models::PYANNOTE_SEGMENTATION_3_0),
            store.path(&steno_diarize::models::WESPEAKER_RESNET34_LM),
        ]
    }

    fn size_of(path: &Path) -> i64 {
        fn walk(path: &Path) -> u64 {
            if path.is_file() {
                return std::fs::metadata(path).map_or(0, |m| m.len());
            }
            std::fs::read_dir(path).map_or(0, |entries| {
                entries.flatten().map(|entry| walk(&entry.path())).sum()
            })
        }
        i64::try_from(walk(path)).unwrap_or(i64::MAX)
    }
}

impl SpeechModels for RealSpeechModels {
    fn is_installed(&self, asset: ModelAsset) -> bool {
        match asset {
            ModelAsset::OfflineDiarizer => self.diarizer_paths().iter().all(|path| path.is_file()),
            other => Self::speech_asset(other).is_some_and(|asset| self.speech.is_installed(&asset)),
        }
    }

    fn installed_size(&self, asset: ModelAsset) -> Option<i64> {
        if !self.is_installed(asset) {
            return None;
        }
        Some(match asset {
            ModelAsset::OfflineDiarizer => self.diarizer_paths().iter().map(|p| Self::size_of(p)).sum(),
            other => Self::size_of(&self.speech.directory(&Self::speech_asset(other)?)),
        })
    }

    fn download(&self, asset: ModelAsset, progress: &mut dyn FnMut(f64, &str)) -> Result<(), String> {
        match asset {
            ModelAsset::OfflineDiarizer => {
                let store = diarize_store(&self.speech);
                progress(0.0, "Segmentation model");
                store
                    .ensure(&steno_diarize::models::PYANNOTE_SEGMENTATION_3_0)
                    .map_err(|error| error.to_string())?;
                progress(0.5, "Speaker embedding model");
                store
                    .ensure(&steno_diarize::models::WESPEAKER_RESNET34_LM)
                    .map_err(|error| error.to_string())?;
                progress(1.0, "Installed");
                Ok(())
            }
            other => {
                let asset = Self::speech_asset(other)
                    .ok_or_else(|| format!("{} has no Rust engine yet", other.as_str()))?;
                let total = asset.total_size().max(1);
                let mut received_before: u64 = 0;
                let mut current_file = String::new();
                self.speech
                    .ensure(&asset, &mut |report| {
                        if report.file != current_file {
                            if !current_file.is_empty() {
                                received_before += asset
                                    .files
                                    .iter()
                                    .find(|f| f.name == current_file)
                                    .map_or(0, |f| f.size);
                            }
                            report.file.clone_into(&mut current_file);
                        }
                        #[allow(clippy::cast_precision_loss)]
                        let fraction = (received_before + report.received) as f64 / total as f64;
                        progress(fraction.min(1.0), report.file);
                    })
                    .map_err(|error| error.to_string())?;
                progress(1.0, "Installed");
                Ok(())
            }
        }
    }

    fn remove(&self, asset: ModelAsset) -> Result<(), String> {
        match asset {
            ModelAsset::OfflineDiarizer => {
                for path in self.diarizer_paths() {
                    if path.exists() {
                        std::fs::remove_file(&path).map_err(|error| error.to_string())?;
                    }
                }
                Ok(())
            }
            other => {
                let asset = Self::speech_asset(other)
                    .ok_or_else(|| format!("{} has no Rust engine yet", other.as_str()))?;
                self.speech.remove(&asset).map_err(|error| error.to_string())
            }
        }
    }
}
