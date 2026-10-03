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
    async_trait, paths::file_url_path, protocols::BoundaryResult,
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
        .and_then(|url| file_url_path(url).or_else(|| Some(PathBuf::from(url))))
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
        if runs_on_coreml(&settings.speech_engine_id) {
            let coreml = steno_speech_coreml::CoreMlParakeetEngine::new(coreml_model_directory(
                models_directory,
            ));
            return Arc::new(LanguageTaggingEngine::new(Arc::new(OneCallAtATime::new(
                Arc::new(coreml),
            ))));
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

/// An engine whose calls do long synchronous model work without yielding
/// (the `CoreML` Parakeet loads and transcribes that way): one call at a
/// time, as Swift's `AsrManager` actor ran them, each off the runtime's
/// workers (`block_in_place` on a multi-thread runtime), so two meetings
/// processing at once queue on the model instead of parking two workers
/// for minutes.
pub struct OneCallAtATime {
    inner: Arc<dyn SpeechEngine>,
    turn: tokio::sync::Mutex<()>,
}

impl OneCallAtATime {
    #[must_use]
    pub fn new(inner: Arc<dyn SpeechEngine>) -> Self {
        OneCallAtATime {
            inner,
            turn: tokio::sync::Mutex::new(()),
        }
    }
}

#[async_trait]
impl SpeechEngine for OneCallAtATime {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn supported_languages(&self) -> &BTreeSet<LanguageTag> {
        self.inner.supported_languages()
    }

    async fn prepare(&self) -> BoundaryResult<()> {
        let _turn = self.turn.lock().await;
        crate::off_the_workers(self.inner.prepare()).await
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        let _turn = self.turn.lock().await;
        crate::off_the_workers(self.inner.transcribe(audio, hint)).await
    }
}

/// Whether [`speech_engine`] builds the `CoreML` engine for `engine_id`:
/// `parakeet-v3` on the Mac; every other id, and every id elsewhere, gets
/// the ONNX engine.
#[must_use]
pub fn runs_on_coreml(engine_id: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        engine_id == steno_speech_coreml::ENGINE_ID
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = engine_id;
        false
    }
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
    /// The engine [`speech_engine`] builds for `engine_id`: the `CoreML`
    /// Parakeet's files, or every model the ONNX engine loads (the VAD and
    /// the fp32 Parakeet), whatever id the settings hold.
    fn engine_installed(&self, engine_id: &str) -> bool {
        if runs_on_coreml(engine_id) {
            coreml_parakeet_installed(&self.coreml)
        } else {
            steno_speech::ModelAsset::all()
                .iter()
                .all(|asset| self.speech.is_installed(asset))
        }
    }

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
    ) -> BoundaryResult<()> {
        match asset {
            ModelAsset::OfflineDiarizer => {
                let store = diarize_store(&self.speech);
                progress(0.0, "Segmentation model");
                store.ensure(&steno_diarize::models::PYANNOTE_SEGMENTATION_3_0)?;
                progress(0.5, "Speaker embedding model");
                store.ensure(&steno_diarize::models::WESPEAKER_RESNET34_LM)?;
                progress(1.0, "Installed");
                Ok(())
            }
            ModelAsset::ParakeetV3 if Self::COREML_PARAKEET => Err(
                "This build cannot download the CoreML Parakeet v3 model; install it from the Steno Mac app."
                    .into(),
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
                    })?;
                progress(1.0, "Installed");
                Ok(())
            }
        }
    }

    fn remove(&self, asset: ModelAsset) -> BoundaryResult<()> {
        match asset {
            ModelAsset::OfflineDiarizer => {
                for path in self.diarizer_paths() {
                    if path.exists() {
                        std::fs::remove_file(&path)?;
                    }
                }
                Ok(())
            }
            ModelAsset::ParakeetV3 if Self::COREML_PARAKEET => {
                match std::fs::remove_dir_all(&self.coreml) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
                    _ => Ok(()),
                }
            }
            other => {
                let asset = Self::speech_asset(other)
                    .ok_or_else(|| format!("{} has no Rust engine yet", other.as_str()))?;
                Ok(self.speech.remove(&asset)?)
            }
        }
    }

    /// Off the Mac, Parakeet v3 is the fp32 ONNX export of NVIDIA's model,
    /// not the Swift app's `CoreML` int8 build; on the Mac it is that build,
    /// under the Swift app's name.
    fn display_name(&self, asset: ModelAsset) -> String {
        match asset {
            ModelAsset::ParakeetV3 if !Self::COREML_PARAKEET => ONNX_PARAKEET_NAME.to_owned(),
            other => other.display_name().to_owned(),
        }
    }

    fn source_repo(&self, asset: ModelAsset) -> String {
        match asset {
            ModelAsset::ParakeetV3 if !Self::COREML_PARAKEET => ONNX_PARAKEET_SOURCE.to_owned(),
            other => other.source_repo().to_owned(),
        }
    }
}

/// What the acknowledgements call the fp32 ONNX Parakeet v3.
pub const ONNX_PARAKEET_NAME: &str = "Parakeet TDT 0.6B v3 (fp32)";

/// The model the ONNX export was converted from.
pub const ONNX_PARAKEET_SOURCE: &str = "nvidia/parakeet-tdt-0.6b-v3";

/// Model files on disk for the tests, so no test downloads one.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use super::{COREML_PARAKEET_FILES, ModelStoreSpeechModels};

    /// The `CoreML` Parakeet in `directory`, complete: each bundle with a
    /// one-byte `coremldata.bin`, the vocabulary as `{}`.
    pub fn install_coreml_parakeet(directory: &Path) {
        for name in COREML_PARAKEET_FILES {
            let path = directory.join(name);
            if name.ends_with(".mlmodelc") {
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(path.join("coremldata.bin"), b"x").unwrap();
            } else {
                std::fs::create_dir_all(directory).unwrap();
                std::fs::write(&path, b"{}").unwrap();
            }
        }
    }

    /// The two ONNX diarizer models, as placeholder files.
    pub fn install_onnx_diarizer(models: &ModelStoreSpeechModels) {
        for path in models.diarizer_paths() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"onnx").unwrap();
        }
    }

    /// Every file under `directory`.
    pub fn files_under(directory: &Path) -> BTreeSet<PathBuf> {
        let mut files = BTreeSet::new();
        let mut pending = vec![directory.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
                if entry.path().is_dir() {
                    pending.push(entry.path());
                } else {
                    files.insert(entry.path());
                }
            }
        }
        files
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
        testing::install_coreml_parakeet(&models.coreml);
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

    /// The warm-up's question: with the `CoreML` Parakeet on disk, only
    /// `parakeet-v3` on the Mac runs on it; any other id, and every id off
    /// the Mac, needs the ONNX models, which are missing.
    #[test]
    fn the_configured_engine_is_installed_only_when_the_model_it_loads_is() {
        let dir = tempfile::tempdir().unwrap();
        let models = ModelStoreSpeechModels::new(dir.path());
        assert!(!models.engine_installed("parakeet-v3"));
        testing::install_coreml_parakeet(&models.coreml);
        assert_eq!(
            models.engine_installed("parakeet-v3"),
            cfg!(target_os = "macos")
        );
        assert!(!models.engine_installed("whisperkit-large-v3-turbo"));
        assert!(!models.engine_installed("anything"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_coreml_directory_and_files_are_those_the_engine_loads() {
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

    /// The first acknowledgement row, Parakeet v3, as Settings > General
    /// shows it over this platform's model store.
    fn parakeet_acknowledgement() -> (String, String) {
        let models = ModelStoreSpeechModels::new(Path::new("/tmp/steno-models"));
        let rows = steno_host::settings::snapshots::acknowledgements(&models);
        assert_eq!(
            rows[4].name,
            ModelAsset::OfflineDiarizer.display_name(),
            "the other assets keep their names"
        );
        (rows[0].name.clone(), rows[0].source.clone())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn on_the_mac_parakeet_v3_is_acknowledged_as_the_swift_app_names_it() {
        assert_eq!(
            parakeet_acknowledgement(),
            (
                "Parakeet TDT 0.6B v3 (int8)".to_owned(),
                "FluidInference/parakeet-tdt-0.6b-v3-coreml".to_owned()
            )
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_the_mac_parakeet_v3_is_acknowledged_as_the_fp32_onnx_export() {
        assert_eq!(
            parakeet_acknowledgement(),
            (
                "Parakeet TDT 0.6B v3 (fp32)".to_owned(),
                "nvidia/parakeet-tdt-0.6b-v3".to_owned()
            )
        );
    }

    /// An engine that transcribes synchronously, without yielding, until
    /// `go` lets it, and counts how many calls run at once.
    struct BlockingEngine {
        languages: BTreeSet<LanguageTag>,
        go: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        running: std::sync::atomic::AtomicUsize,
        most_at_once: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl SpeechEngine for BlockingEngine {
        fn id(&self) -> &'static str {
            "blocking"
        }

        fn supported_languages(&self) -> &BTreeSet<LanguageTag> {
            &self.languages
        }

        async fn prepare(&self) -> BoundaryResult<()> {
            Ok(())
        }

        async fn transcribe(
            &self,
            _audio: &AudioBuffer16k,
            _hint: Option<&LanguageTag>,
        ) -> BoundaryResult<Vec<RawSegment>> {
            use std::sync::atomic::Ordering::SeqCst;
            let now = self.running.fetch_add(1, SeqCst) + 1;
            self.most_at_once.fetch_max(now, SeqCst);
            self.go
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("released");
            self.running.fetch_sub(1, SeqCst);
            Ok(Vec::new())
        }
    }

    /// Two transcriptions on a runtime with one worker: they run one after
    /// the other, and while one blocks, the task that releases it still
    /// runs, because the blocked call left the worker first.
    #[test]
    fn coreml_style_calls_run_one_at_a_time_off_the_workers() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (go, released) = std::sync::mpsc::channel();
        let inner = Arc::new(BlockingEngine {
            languages: BTreeSet::new(),
            go: std::sync::Mutex::new(released),
            running: 0.into(),
            most_at_once: 0.into(),
        });
        let engine = Arc::new(OneCallAtATime::new(inner.clone()));
        let (done, finished) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let audio = AudioBuffer16k::new(vec![0.0; 16_000]);
            let calls = (0..2).map(|_| {
                let (engine, audio) = (engine.clone(), audio.clone());
                tokio::spawn(async move { engine.transcribe(&audio, None).await.unwrap() })
            });
            let calls: Vec<_> = calls.collect();
            // The releasing task needs a worker while a call blocks.
            tokio::spawn(async move {
                for _ in 0..2 {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    go.send(()).unwrap();
                }
            });
            for call in calls {
                call.await.unwrap();
            }
            done.send(()).unwrap();
        });
        finished
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("both calls finished while the worker was free for the release");
        assert_eq!(
            inner.most_at_once.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
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
