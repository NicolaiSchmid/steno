//! The models directory and the speech engine per platform (`CoreML` on
//! the Mac, ONNX Runtime elsewhere, behind `SpeechEngine`), the ONNX
//! diarizer, and the host's `SpeechModels` over the installed models. The
//! ONNX engine runs in this process until the speech sidecar lands (plan:
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

/// The directory every engine's models live under:
/// `settings.models_directory` when set, else `STENO_MODELS_DIR` (a
/// relative path is taken from the working directory), else `Models` in
/// the support directory. The ONNX models sit in its `onnx/`, the `CoreML`
/// Parakeet in `fluidaudio/parakeet-tdt-0.6b-v3/` (where the Swift app's
/// `ModelStore` installs it).
#[must_use]
pub fn models_directory(settings: &Settings, paths: &StenoPaths) -> PathBuf {
    models_directory_with(
        settings,
        paths,
        std::env::var_os(ModelStore::ENVIRONMENT_VARIABLE),
    )
}

/// [`models_directory`] with the variable's value passed in.
fn models_directory_with(
    settings: &Settings,
    paths: &StenoPaths,
    variable: Option<std::ffi::OsString>,
) -> PathBuf {
    settings
        .models_directory
        .as_deref()
        .and_then(|url| path_from_file_url(url).or_else(|| Some(PathBuf::from(url))))
        .or_else(|| {
            variable
                .filter(|value| !value.is_empty())
                .map(|value| absolute(Path::new(&value)))
        })
        .unwrap_or_else(|| paths.support_directory.join("Models"))
}

/// `path` from the working directory when it is relative, as Swift's
/// `URL(fileURLWithPath:)` reads a command-line path.
#[must_use]
pub fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The ONNX speech models under `models_directory`.
#[must_use]
pub fn speech_store_under(models_directory: &Path) -> ModelStore {
    ModelStore::new(models_directory.join("onnx"))
}

/// The `CoreML` Parakeet directory under `models_directory`.
#[must_use]
pub fn coreml_model_directory(models_directory: &Path) -> PathBuf {
    models_directory
        .join("fluidaudio")
        .join("parakeet-tdt-0.6b-v3")
}

/// What the `CoreML` Parakeet loads from its directory; the bundles count
/// when their `coremldata.bin` is in place, as Swift's
/// `ModelStore.isInstalled` checks. `steno_speech_coreml::backend` names
/// the same files (a macOS test pins them).
pub const COREML_PARAKEET_FILES: [&str; 5] = [
    "Preprocessor.mlmodelc",
    "Encoder.mlmodelc",
    "Decoder.mlmodelc",
    "JointDecisionv3.mlmodelc",
    "parakeet_v3_vocab.json",
];

/// Whether every file the `CoreML` Parakeet loads is complete in `directory`.
#[must_use]
pub fn coreml_parakeet_installed(directory: &Path) -> bool {
    COREML_PARAKEET_FILES.iter().all(|name| {
        let path = directory.join(name);
        if Path::new(name)
            .extension()
            .is_some_and(|ext| ext == "mlmodelc")
        {
            path.join("coremldata.bin").is_file()
        } else {
            path.is_file()
        }
    })
}

/// The diarization models beside the speech ones.
#[must_use]
pub fn diarize_store(speech: &ModelStore) -> steno_diarize::models::ModelStore {
    steno_diarize::models::ModelStore::new(speech.root().join("diarization"))
}

/// The engine the settings name, its models under `models_directory`. On
/// the Mac `parakeet-v3` is the `CoreML` engine on the Neural Engine;
/// everywhere else, and for any other id, the fp32 ONNX export of the same
/// model (the engine ids the Swift app offered beyond Parakeet v3 have no
/// Rust counterpart yet; the parity list says so). The result's id says
/// which was chosen.
#[must_use]
pub fn speech_engine(settings: &Settings, models_directory: &Path) -> Arc<dyn SpeechEngine> {
    #[cfg(target_os = "macos")]
    {
        if settings.speech_engine_id == steno_speech_coreml::ENGINE_ID {
            return Arc::new(LanguageTaggingEngine::new(Arc::new(
                steno_speech_coreml::CoreMlParakeetEngine::new(coreml_model_directory(
                    models_directory,
                )),
            )));
        }
    }
    let _ = settings;
    Arc::new(OnnxSpeechEngine::new(
        speech_store_under(models_directory),
        OnnxOptions {
            intra_threads: ONNX_THREADS,
            ..OnnxOptions::default()
        },
    ))
}

/// An engine whose segments come back without a language, with
/// `steno_speech`'s tagger run over them, so the meeting's language is
/// elected the way the ONNX path elects it. The `CoreML` backend writes
/// `language: None`; Swift tagged after the engine the same way
/// (`Sources/StenoSpeech/Engines/ParakeetMapping.swift`).
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

/// The ONNX diarizer under `models_directory`, loading its two models on
/// first use.
#[must_use]
pub fn diarizer(models_directory: &Path) -> Arc<dyn Diarizer> {
    Arc::new(ModelDiarizer::onnx(
        DiarizerConfig::default(),
        diarize_store(&speech_store_under(models_directory)),
        ONNX_THREADS,
    ))
}

/// The host's model service over the models under one directory. On the
/// Mac, Parakeet v3 is the `CoreML` model the engine runs (installed by
/// the Swift app); elsewhere it is the ONNX export.
pub struct ModelStoreSpeechModels {
    pub speech: ModelStore,
    /// The `CoreML` Parakeet's directory.
    pub coreml: PathBuf,
}

impl ModelStoreSpeechModels {
    #[must_use]
    pub fn new(models_directory: &Path) -> Self {
        ModelStoreSpeechModels {
            speech: speech_store_under(models_directory),
            coreml: coreml_model_directory(models_directory),
        }
    }

    /// Whether Parakeet v3 is the `CoreML` model on this platform.
    const COREML_PARAKEET: bool = cfg!(target_os = "macos");

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
            ModelAsset::ParakeetV3 if Self::COREML_PARAKEET => {
                coreml_parakeet_installed(&self.coreml)
            }
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
            ModelAsset::ParakeetV3 if Self::COREML_PARAKEET => Self::size_of(&self.coreml),
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
            ModelAsset::ParakeetV3 if Self::COREML_PARAKEET => Err(
                "This build cannot download the CoreML Parakeet v3 model; install it from the Steno Mac app."
                    .to_owned(),
            ),
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
            ModelAsset::ParakeetV3 if Self::COREML_PARAKEET => {
                match std::fs::remove_dir_all(&self.coreml) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        Err(error.to_string())
                    }
                    _ => Ok(()),
                }
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
    fn the_models_directory_is_the_settings_then_the_variable_then_the_support_directory() {
        let support = Path::new("/tmp/steno-support");
        let paths = StenoPaths::new(support);
        let mut settings = Settings::default();
        assert_eq!(
            models_directory_with(&settings, &paths, None),
            support.join("Models")
        );
        assert_eq!(
            models_directory_with(&settings, &paths, Some("/tmp/steno-env-models".into())),
            Path::new("/tmp/steno-env-models")
        );
        assert_eq!(
            models_directory_with(&settings, &paths, Some("relative/models".into())),
            std::env::current_dir().unwrap().join("relative/models"),
            "a relative variable is taken from the working directory"
        );
        let chosen = Path::new("/tmp/steno-models");
        settings.models_directory = Some(file_url(chosen, true));
        assert_eq!(
            models_directory_with(&settings, &paths, Some("/tmp/steno-env-models".into())),
            chosen
        );
        assert_eq!(speech_store_under(chosen).root(), chosen.join("onnx"));
        assert_eq!(
            coreml_model_directory(chosen),
            chosen.join("fluidaudio").join("parakeet-tdt-0.6b-v3")
        );
    }

    #[test]
    fn the_coreml_parakeet_counts_as_installed_only_when_complete() {
        let dir = tempfile::tempdir().unwrap();
        let directory = dir.path().join("parakeet");
        assert!(!coreml_parakeet_installed(&directory));
        for name in COREML_PARAKEET_FILES {
            let path = directory.join(name);
            if name.ends_with(".mlmodelc") {
                std::fs::create_dir_all(&path).unwrap();
            } else {
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(&path, b"{}").unwrap();
            }
        }
        assert!(
            !coreml_parakeet_installed(&directory),
            "bundles without their coremldata.bin are incomplete"
        );
        for name in COREML_PARAKEET_FILES
            .iter()
            .filter(|n| n.ends_with(".mlmodelc"))
        {
            std::fs::write(directory.join(name).join("coremldata.bin"), b"x").unwrap();
        }
        assert!(coreml_parakeet_installed(&directory));
    }

    #[test]
    fn parakeet_v3_status_follows_the_model_the_engine_runs() {
        let dir = tempfile::tempdir().unwrap();
        let models = ModelStoreSpeechModels::new(dir.path());
        assert!(!models.is_installed(ModelAsset::ParakeetV3));
        for name in COREML_PARAKEET_FILES {
            let path = models.coreml.join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("coremldata.bin"), b"x").unwrap();
        }
        // Only the vocabulary needs to be a plain file.
        let vocabulary = models.coreml.join("parakeet_v3_vocab.json");
        std::fs::remove_dir_all(&vocabulary).unwrap();
        std::fs::write(&vocabulary, b"{}").unwrap();
        assert_eq!(
            models.is_installed(ModelAsset::ParakeetV3),
            cfg!(target_os = "macos"),
            "the CoreML model counts on the Mac only"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(models.installed_size(ModelAsset::ParakeetV3), Some(6));
            models.remove(ModelAsset::ParakeetV3).unwrap();
            assert!(!models.coreml.exists());
            assert!(!models.is_installed(ModelAsset::ParakeetV3));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_coreml_directory_and_files_are_the_engine_s() {
        let support = Path::new("/tmp/steno-support");
        assert_eq!(
            coreml_model_directory(&support.join("Models")),
            steno_speech_coreml::engine::model_directory(support)
        );
        assert_eq!(
            COREML_PARAKEET_FILES,
            [
                steno_speech_coreml::backend::PREPROCESSOR_FILE,
                steno_speech_coreml::backend::ENCODER_FILE,
                steno_speech_coreml::backend::DECODER_FILE,
                steno_speech_coreml::backend::JOINT_FILE,
                steno_speech_coreml::backend::VOCABULARY_FILE,
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parakeet_v3_selects_the_coreml_engine_on_the_mac() {
        let mut settings = Settings::default();
        steno_speech_coreml::ENGINE_ID.clone_into(&mut settings.speech_engine_id);
        let engine = speech_engine(&settings, Path::new("/tmp/steno-models"));
        assert_eq!(engine.id(), steno_speech_coreml::ENGINE_ID);
        assert_eq!(engine.supported_languages().len(), 25);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn every_engine_id_selects_the_onnx_engine_off_the_mac() {
        for id in ["parakeet-v3", "whisperkit-large-v3-turbo", "anything"] {
            let mut settings = Settings::default();
            id.clone_into(&mut settings.speech_engine_id);
            assert_eq!(
                speech_engine(&settings, Path::new("/tmp/steno-models")).id(),
                OnnxSpeechEngine::ID
            );
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
