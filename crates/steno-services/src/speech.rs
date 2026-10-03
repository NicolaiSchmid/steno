//! The models root and the speech engine per platform (`CoreML` on the
//! Mac, ONNX Runtime elsewhere, behind `SpeechEngine`), the ONNX diarizer,
//! and the host's `SpeechModels` over the two model stores. The ONNX
//! engine runs in this process until the speech sidecar lands (plan:
//! `WP4c`); the trait object is the seam.
//! Swift: `makeSpeechEngine`, `makeDiarizer`, `ModelStore`,
//! `Sources/StenoSpeech/Engines/SpeechEngineID.swift`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use steno_core::{
    AudioBuffer16k, Diarizer, LanguageTag, RawSegment, Settings, SpeechEngine, StenoPaths,
    async_trait, paths::path_from_file_url, protocols::BoundaryResult,
};
use steno_diarize::{DiarizerConfig, ModelDiarizer};
use steno_host::services::SpeechModels;
use steno_host::speech::ModelAsset;
use steno_speech::{LanguageTagger, ModelStore, OnnxOptions, OnnxSpeechEngine};

/// Threads for one ONNX operator; the plan measured at four.
pub const ONNX_THREADS: usize = 4;

/// Where every model lives, one root for every engine:
/// `settings.models_directory` when set, else `STENO_MODELS_DIR`, else
/// the support directory's `Models`. The ONNX stores sit under `onnx/`,
/// the `CoreML` Parakeet under `fluidaudio/parakeet-tdt-0.6b-v3/` (where
/// the Swift app's `ModelStore` installs it).
#[must_use]
pub fn models_root(settings: &Settings, paths: &StenoPaths) -> PathBuf {
    settings
        .models_directory
        .as_deref()
        .and_then(|url| path_from_file_url(url).or_else(|| Some(PathBuf::from(url))))
        .or_else(|| {
            std::env::var_os(ModelStore::ENVIRONMENT_VARIABLE)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        })
        .unwrap_or_else(|| paths.support_directory.join("Models"))
}

/// The ONNX speech store under `root`.
#[must_use]
pub fn speech_store_under(root: &Path) -> ModelStore {
    ModelStore::new(root.join("onnx"))
}

/// The ONNX speech store under [`models_root`].
#[must_use]
pub fn speech_store(settings: &Settings, paths: &StenoPaths) -> ModelStore {
    speech_store_under(&models_root(settings, paths))
}

/// The `CoreML` Parakeet directory under `root`.
#[must_use]
pub fn coreml_model_directory(root: &Path) -> PathBuf {
    root.join("fluidaudio").join("parakeet-tdt-0.6b-v3")
}

/// The diarization models beside the speech ones.
#[must_use]
pub fn diarize_store(speech: &ModelStore) -> steno_diarize::models::ModelStore {
    steno_diarize::models::ModelStore::new(speech.root().join("diarization"))
}

/// The models root a speech store was made under: the parent of `onnx/`.
fn root_of(store: &ModelStore) -> PathBuf {
    store
        .root()
        .parent()
        .map_or_else(|| store.root().to_path_buf(), Path::to_path_buf)
}

/// The engine the settings name. On the Mac `parakeet-v3` is the `CoreML`
/// engine on the Neural Engine; everywhere else, and for any other id, the
/// fp32 ONNX export of the same model (the engine ids the Swift app
/// offered beyond Parakeet v3 have no Rust counterpart yet; the parity
/// list says so). The result's id says which was chosen.
#[must_use]
pub fn speech_engine(settings: &Settings, store: &ModelStore) -> Arc<dyn SpeechEngine> {
    #[cfg(target_os = "macos")]
    {
        if settings.speech_engine_id == steno_speech_coreml::ENGINE_ID {
            return Arc::new(LanguageTaggingEngine::new(Arc::new(
                steno_speech_coreml::CoreMlParakeetEngine::new(coreml_model_directory(&root_of(
                    store,
                ))),
            )));
        }
    }
    let _ = (&settings.speech_engine_id, root_of);
    Arc::new(OnnxSpeechEngine::new(
        store.clone(),
        OnnxOptions {
            intra_threads: ONNX_THREADS,
            ..OnnxOptions::default()
        },
    ))
}

/// An engine whose segments come back without a language, with
/// `steno_speech`'s tagger run over them, so the meeting's language is
/// elected the way the ONNX path elects it. The `CoreML` backend writes
/// `language: None`; the Swift app tagged after the engine too.
pub struct LanguageTaggingEngine {
    inner: Arc<dyn SpeechEngine>,
    tagger: LanguageTagger,
}

impl LanguageTaggingEngine {
    #[must_use]
    pub fn new(inner: Arc<dyn SpeechEngine>) -> Self {
        LanguageTaggingEngine {
            inner,
            tagger: LanguageTagger::new(),
        }
    }
}

#[async_trait]
impl SpeechEngine for LanguageTaggingEngine {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn supported_languages(&self) -> &BTreeSet<LanguageTag> {
        self.inner.supported_languages()
    }

    async fn prepare(&self) -> BoundaryResult<()> {
        self.inner.prepare().await
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        let segments = self.inner.transcribe(audio, hint).await?;
        Ok(self.tagger.tag(segments, hint))
    }
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
pub struct ModelStoreSpeechModels {
    pub speech: ModelStore,
}

impl ModelStoreSpeechModels {
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

impl SpeechModels for ModelStoreSpeechModels {
    fn is_installed(&self, asset: ModelAsset) -> bool {
        match asset {
            ModelAsset::OfflineDiarizer => self.diarizer_paths().iter().all(|path| path.is_file()),
            other => {
                Self::speech_asset(other).is_some_and(|asset| self.speech.is_installed(&asset))
            }
        }
    }

    fn installed_size(&self, asset: ModelAsset) -> Option<i64> {
        if !self.is_installed(asset) {
            return None;
        }
        Some(match asset {
            ModelAsset::OfflineDiarizer => {
                self.diarizer_paths().iter().map(|p| Self::size_of(p)).sum()
            }
            other => Self::size_of(&self.speech.directory(&Self::speech_asset(other)?)),
        })
    }

    fn download(
        &self,
        asset: ModelAsset,
        progress: &mut dyn FnMut(f64, &str),
    ) -> Result<(), String> {
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
                self.speech
                    .remove(&asset)
                    .map_err(|error| error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use steno_core::paths::file_url;
    use steno_core::testing::FakeSpeechEngine;

    use super::*;

    #[test]
    fn every_source_of_the_models_root_puts_onnx_and_coreml_under_one_root() {
        let support = Path::new("/tmp/steno-support");
        let paths = StenoPaths::new(support);
        let mut settings = Settings::default();
        assert_eq!(
            models_root(&settings, &paths),
            std::env::var_os(ModelStore::ENVIRONMENT_VARIABLE)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| support.join("Models"))
        );
        let chosen = Path::new("/tmp/steno-models");
        settings.models_directory = Some(file_url(chosen, true));
        assert_eq!(models_root(&settings, &paths), chosen);
        assert_eq!(
            speech_store(&settings, &paths).root(),
            chosen.join("onnx").as_path()
        );
        assert_eq!(
            coreml_model_directory(chosen),
            chosen.join("fluidaudio").join("parakeet-tdt-0.6b-v3")
        );
        assert_eq!(root_of(&speech_store(&settings, &paths)), chosen);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_coreml_directory_is_where_the_swift_model_store_installs() {
        let support = Path::new("/tmp/steno-support");
        assert_eq!(
            coreml_model_directory(&support.join("Models")),
            steno_speech_coreml::model_directory(support)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parakeet_v3_selects_the_coreml_engine_on_the_mac() {
        let store = speech_store_under(Path::new("/tmp/steno-models"));
        let mut settings = Settings::default();
        steno_speech_coreml::ENGINE_ID.clone_into(&mut settings.speech_engine_id);
        let engine = speech_engine(&settings, &store);
        assert_eq!(engine.id(), steno_speech_coreml::ENGINE_ID);
        assert_eq!(engine.supported_languages().len(), 25);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn every_engine_id_selects_the_onnx_engine_off_the_mac() {
        let store = speech_store_under(Path::new("/tmp/steno-models"));
        for id in ["parakeet-v3", "whisperkit-large-v3-turbo", "anything"] {
            let mut settings = Settings::default();
            id.clone_into(&mut settings.speech_engine_id);
            assert_eq!(speech_engine(&settings, &store).id(), OnnxSpeechEngine::ID);
        }
    }

    #[tokio::test]
    async fn the_tagging_engine_fills_in_the_language_an_engine_left_out() {
        let inner = Arc::new(FakeSpeechEngine {
            language: None,
            text_prefix: "the quick brown fox jumps over the lazy dog and".to_owned(),
            ..FakeSpeechEngine::default()
        });
        let engine = LanguageTaggingEngine::new(inner.clone());
        assert_eq!(engine.id(), inner.id());
        let audio = AudioBuffer16k::new(vec![0.0; 16_000 * 3]);
        let segments = engine.transcribe(&audio, None).await.unwrap();
        assert_eq!(segments.len(), 3);
        assert!(
            segments
                .iter()
                .all(|segment| segment.language.as_ref().map(LanguageTag::as_str) == Some("en")),
            "{segments:?}"
        );
        let hinted = engine.transcribe(&audio, Some(&"de".into())).await.unwrap();
        assert!(hinted.iter().all(|segment| segment.language.is_some()));
    }
}
