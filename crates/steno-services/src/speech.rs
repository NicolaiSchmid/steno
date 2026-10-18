//! The models directory, the speech settings, the speech engine per
//! platform ([`SpeechSetup::runtime`]: `CoreML` in this process on the Mac,
//! the speech sidecar elsewhere and when the settings choose it), the ONNX
//! diarizer in the sidecar's child, and the host's `SpeechModels`.
//! [`SpeechEngines`] keeps the engines so the app never runs two sidecars.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use steno_core::{
    AudioBuffer16k, Diarizer, LanguageTag, RawSegment, Settings, SpeechEngine, StenoPaths,
    async_trait, paths::file_url_path, protocols::BoundaryResult,
};
use steno_diarize::{Install, SidecarDiarizer};
use steno_host::services::{ModelNotice, SpeechModels};
use steno_host::speech::ModelAsset;
use steno_pipeline::{SharedSpeechEngine, WeakSpeechEngine};
use steno_speech::{
    ModelStore, OnnxSpeechEngine, SidecarConfig, SidecarSpeechEngine, SpeechRuntime, SpeechSettings,
};

use crate::model_gate::{GatedDiarizer, GatedSpeechEngine, InstalledCheck};
use crate::pipeline::ModelsInstalled;

/// Threads for one ONNX operator.
pub const ONNX_THREADS: usize = 4;

/// The speech settings' file in the support directory. Not a row of the
/// `setting` table: the Swift app rewrites that table whole on every save
/// and would drop rows it does not know. Read once, at launch.
pub const SPEECH_SETTINGS_FILE: &str = "speech.json";

/// The speech settings from [`SPEECH_SETTINGS_FILE`] (absent or
/// unreadable: the defaults), its mirror replaced by `STENO_MODELS_MIRROR`
/// when that is set.
#[must_use]
pub fn speech_settings(paths: &StenoPaths) -> SpeechSettings {
    speech_settings_with(
        &paths.support_directory.join(SPEECH_SETTINGS_FILE),
        std::env::var(ModelStore::MIRROR_ENVIRONMENT_VARIABLE).ok(),
    )
}

fn speech_settings_with(file: &Path, mirror: Option<String>) -> SpeechSettings {
    let mut settings = match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            tracing::warn!("the speech settings file is not valid; using the defaults");
            tracing::debug!(%error, "speech settings file");
            SpeechSettings::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SpeechSettings::default(),
        Err(error) => {
            tracing::warn!(kind = ?error.kind(), "the speech settings file could not be read; using the defaults");
            SpeechSettings::default()
        }
    };
    if let Some(mirror) = mirror.filter(|m| !m.trim().is_empty()) {
        settings.models_mirror = Some(mirror);
    }
    settings
}

/// The speech sidecar binary beside the running executable, its sessions
/// on [`ONNX_THREADS`]. Where it is missing (a bundle built without the
/// release configuration, or an unreadable executable path) `prepare`
/// fails with "could not start"; the client never looks it up on `PATH`.
#[must_use]
pub fn sidecar_config() -> SidecarConfig {
    let mut config = SidecarConfig::beside_current_exe().unwrap_or_else(|error| {
        tracing::warn!(kind = ?error.kind(), "the running executable's path could not be read; the speech sidecar cannot start");
        SidecarConfig::new(steno_speech::sidecar::SIDECAR_BINARY)
    });
    // Set here so the app's count stays whatever that default becomes.
    config.options.intra_threads = ONNX_THREADS;
    config
}

/// What the speech engine and the model service are built from: the
/// models directory, the speech settings and the sidecar binary.
///
/// ```no_run
/// use steno_core::{Settings, SpeechEngine as _, StenoPaths};
/// use steno_services::speech::{SpeechSetup, speech_engine};
///
/// let settings = Settings::default();
/// let paths = StenoPaths::new(StenoPaths::default_support_directory());
/// let setup = SpeechSetup::new(&settings, &paths);
/// // `CoreML` on the Mac by default, the speech sidecar elsewhere.
/// let engine = speech_engine(&settings.speech_engine_id, &setup);
/// assert_eq!(engine.id(), "parakeet-v3");
/// ```
#[derive(Debug, Clone)]
pub struct SpeechSetup {
    /// The directory every engine's models live under ([`models_directory`]).
    pub models_directory: PathBuf,
    /// The speech settings ([`speech_settings`]), not the app's `Settings`.
    pub speech_settings: SpeechSettings,
    /// How the speech sidecar starts, its encoder on `DirectML` when the
    /// speech settings ask for it on Windows. It never downloads a missing
    /// model unless the `steno` command turns that on.
    pub sidecar: SidecarConfig,
}

impl SpeechSetup {
    /// The app's: [`models_directory`], [`speech_settings`] and
    /// [`sidecar_config`].
    #[must_use]
    pub fn new(settings: &Settings, paths: &StenoPaths) -> Self {
        Self::in_models_directory(models_directory(settings, paths), paths)
    }

    /// [`SpeechSetup::new`] over a models directory resolved elsewhere.
    #[must_use]
    pub fn in_models_directory(models_directory: PathBuf, paths: &StenoPaths) -> Self {
        let speech_settings = speech_settings(paths);
        let mut sidecar = sidecar_config();
        sidecar.options.directml = speech_settings.directml_on_windows;
        sidecar.crash_log_directory = Some(paths.support_directory.clone());
        SpeechSetup {
            models_directory,
            speech_settings,
            sidecar,
        }
    }

    /// Where the engine `engine_id` runs: `parakeet-v3` on `CoreML` in this
    /// process on the Mac, unless the settings choose the sidecar; every
    /// other case in the speech sidecar.
    #[must_use]
    pub fn runtime(&self, engine_id: &str) -> SpeechRuntime {
        engine_runtime(engine_id, &self.speech_settings)
    }

    /// The ONNX store in the models directory's `onnx/`, with the mirror.
    #[must_use]
    pub fn model_store(&self) -> ModelStore {
        self.speech_settings.model_store(&self.models_directory)
    }
}

const PARAKEET_V3: &str = OnnxSpeechEngine::ID;

/// [`SpeechSetup::runtime`] over the speech settings alone, for the model
/// service.
pub(crate) fn engine_runtime(engine_id: &str, speech_settings: &SpeechSettings) -> SpeechRuntime {
    if engine_id == PARAKEET_V3 {
        speech_settings.runtime()
    } else {
        SpeechRuntime::OnnxSidecar
    }
}

/// The directory every engine's models live under:
/// `settings.models_directory` when set, else `STENO_MODELS_DIR`, else
/// `Models` in the support directory.
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
        .or_else(|| ModelStore::models_directory_named(variable))
        .unwrap_or_else(|| paths.support_directory.join("Models"))
}

/// The `CoreML` Parakeet directory under `models_directory`.
#[must_use]
pub fn coreml_model_directory(models_directory: &Path) -> PathBuf {
    ModelStore::coreml_in_models_directory(models_directory)
        .directory(&steno_speech::ModelAsset::parakeet_v3_coreml())
}

/// The engine `engine_id` names, where [`SpeechSetup::runtime`] runs it:
/// the `CoreML` Parakeet v3 or [`sidecar_engine`], the ONNX export of the
/// same model; both report `parakeet-v3`. It decides by the runtime
/// alone, which [`BuiltEngine`](crate::pipeline::BuiltEngine) relies on.
#[must_use]
pub fn speech_engine(engine_id: &str, setup: &SpeechSetup) -> Arc<dyn SpeechEngine> {
    speech_engine_in(engine_id, setup, &Arc::new(sidecar_engine(setup)))
}

/// [`speech_engine`] with `sidecar` as the sidecar engine, so a diarizer
/// over it ([`diarizer_in`]) shares its child.
#[must_use]
pub fn speech_engine_in(
    engine_id: &str,
    setup: &SpeechSetup,
    sidecar: &Arc<SidecarSpeechEngine>,
) -> Arc<dyn SpeechEngine> {
    engine_on(setup.runtime(engine_id), setup, sidecar)
}

/// The engine on `runtime` over `setup`, `sidecar` for the speech sidecar.
fn engine_on(
    runtime: SpeechRuntime,
    setup: &SpeechSetup,
    sidecar: &Arc<SidecarSpeechEngine>,
) -> Arc<dyn SpeechEngine> {
    #[cfg(target_os = "macos")]
    {
        if runtime == SpeechRuntime::CoreMlInProcess {
            let coreml = steno_speech_coreml::CoreMlParakeetEngine::new(coreml_model_directory(
                &setup.models_directory,
            ));
            return Arc::new(OneCallAtATime::new(Arc::new(coreml)));
        }
    }
    let _ = (runtime, setup);
    sidecar.clone()
}

/// Builds the engine for a runtime; a fake in the tests.
pub(crate) type BuildEngine = Box<dyn Fn(SpeechRuntime) -> Arc<dyn SpeechEngine> + Send + Sync>;

/// The speech engines and the diarizer the app's pipelines share, so a
/// pipeline reload keeps the current engine with its claims
/// ([`SharedSpeechEngine`]) and the diarizer, and a retired job and a new
/// one share one sidecar child. The setup is read once at launch.
///
/// The sidecar engine is kept for the app's run and never runs two
/// children. The in-process engine is kept only while a pipeline holds it
/// ([`WeakSpeechEngine`]). The diarizer runs over the sidecar engine
/// ([`diarizer_in`]), in its child or in one of its own while none runs.
///
/// ```no_run
/// use steno_core::{Settings, StenoPaths};
/// use steno_services::speech::{SpeechEngines, SpeechSetup};
///
/// let settings = Settings::default();
/// let paths = StenoPaths::new(StenoPaths::default_support_directory());
/// let engines = SpeechEngines::new(SpeechSetup::new(&settings, &paths));
/// let runtime = engines.setup().runtime(&settings.speech_engine_id);
/// // Every pipeline over this runtime gets the same engine and claims.
/// assert!(engines.engine(runtime).ptr_eq(&engines.engine(runtime)));
/// ```
pub struct SpeechEngines {
    setup: SpeechSetup,
    build: BuildEngine,
    diarizer: Arc<dyn Diarizer>,
    kept: std::sync::Mutex<KeptEngines>,
    installed: ModelsInstalled,
    #[cfg(test)]
    sidecar: Option<Arc<SidecarSpeechEngine>>,
}

#[derive(Default)]
struct KeptEngines {
    /// For the app's run, once built.
    sidecar: Option<SharedSpeechEngine>,
    /// While a pipeline holds it.
    in_process: Option<WeakSpeechEngine>,
}

impl SpeechEngines {
    /// The engines over `setup` and the diarizer, each behind a gate that
    /// refuses a call while its models are not installed; no pipeline run
    /// downloads a model ([`crate::model_gate`]).
    #[must_use]
    pub fn new(setup: SpeechSetup) -> Self {
        let speech = ModelStoreSpeechModels::new(&setup);
        let store = setup.model_store();
        Self::with_checks(
            setup,
            Arc::new(move |runtime| speech.installed_on(runtime)),
            Arc::new(move || steno_diarize::models::installed(&store).is_ok()),
        )
    }

    /// [`Self::new`] with the gates' checks passed in, for the tests.
    pub(crate) fn with_checks(
        mut setup: SpeechSetup,
        speech_installed: ModelsInstalled,
        diarizer_installed: InstalledCheck,
    ) -> Self {
        // So a setup that allows downloads (the CLI's) never reaches the app's
        // engines.
        setup.sidecar.install = Install::Never;
        let sidecar = Arc::new(sidecar_engine(&setup));
        #[cfg(test)]
        let for_tests = sidecar.clone();
        let diarizer = Arc::new(GatedDiarizer::new(
            Self::app_diarizer(sidecar.clone()),
            diarizer_installed.clone(),
        ));
        let over = setup.clone();
        let build = Box::new({
            let speech_installed = speech_installed.clone();
            move |runtime| {
                let installed = speech_installed.clone();
                Arc::new(GatedSpeechEngine::new(
                    engine_on(runtime, &over, &sidecar),
                    Arc::new(move || installed(runtime)),
                )) as Arc<dyn SpeechEngine>
            }
        });
        let installed = Arc::new(move |runtime| speech_installed(runtime) && diarizer_installed());
        Self {
            #[cfg(test)]
            sidecar: Some(for_tests),
            ..Self::with_parts(setup, build, diarizer, installed)
        }
    }

    /// Engines from `build`, with no gates, for the tests.
    #[cfg(test)]
    pub(crate) fn with_builder(setup: SpeechSetup, build: BuildEngine) -> Self {
        let diarizer = Self::app_diarizer(Arc::new(sidecar_engine(&setup)));
        Self::with_parts(setup, build, diarizer, Arc::new(|_| true))
    }

    /// The app's diarizer over `sidecar`. Never downloads: a missing file is
    /// the models-missing refusal, so the meeting waits with its audio
    /// instead of ending with one room speaker.
    fn app_diarizer(sidecar: Arc<SidecarSpeechEngine>) -> Arc<dyn Diarizer> {
        diarizer_in(sidecar, Install::Never)
    }

    fn with_parts(
        setup: SpeechSetup,
        build: BuildEngine,
        diarizer: Arc<dyn Diarizer>,
        installed: ModelsInstalled,
    ) -> Self {
        SpeechEngines {
            diarizer,
            setup,
            build,
            kept: std::sync::Mutex::default(),
            installed,
            #[cfg(test)]
            sidecar: None,
        }
    }

    /// Whether the gates would let a pipeline on `runtime` through now: its
    /// speech models and the diarizer's are installed. Always for the tests'
    /// ungated engines.
    #[must_use]
    pub fn models_installed(&self, runtime: SpeechRuntime) -> bool {
        (self.installed)(runtime)
    }

    /// What the engines are built from.
    #[must_use]
    pub fn setup(&self) -> &SpeechSetup {
        &self.setup
    }

    /// The engine on `runtime` with its claims: the kept one, else a new one,
    /// which is kept.
    #[must_use]
    pub fn engine(&self, runtime: SpeechRuntime) -> SharedSpeechEngine {
        let mut kept = self
            .kept
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let build = || SharedSpeechEngine::new((self.build)(runtime));
        match runtime {
            SpeechRuntime::OnnxSidecar => kept.sidecar.get_or_insert_with(build).clone(),
            SpeechRuntime::CoreMlInProcess => {
                let engine = kept
                    .in_process
                    .as_ref()
                    .and_then(WeakSpeechEngine::upgrade)
                    .unwrap_or_else(build);
                kept.in_process = Some(engine.downgrade());
                engine
            }
        }
    }

    /// The diarizer every pipeline runs.
    #[must_use]
    pub fn diarizer(&self) -> Arc<dyn Diarizer> {
        self.diarizer.clone()
    }
}

/// Parakeet v3 in the speech sidecar.
#[must_use]
pub fn sidecar_engine(setup: &SpeechSetup) -> SidecarSpeechEngine {
    SidecarSpeechEngine::new(setup.model_store(), setup.sidecar.clone())
}

/// An engine whose calls do long synchronous model work (the `CoreML`
/// Parakeet): one call at a time, each off the runtime's workers, so two
/// meetings queue on the model instead of parking two workers for
/// minutes.
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
        off_the_workers(self.inner.prepare()).await
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        let _turn = self.turn.lock().await;
        off_the_workers(self.inner.transcribe(audio, hint)).await
    }

    async fn release(&self) -> BoundaryResult<()> {
        let _turn = self.turn.lock().await;
        off_the_workers(self.inner.release()).await
    }
}

/// Awaits `future`, which does long synchronous work: on a multi-thread
/// runtime via `block_in_place`, so it parks one thread rather than the
/// tasks queued behind it.
async fn off_the_workers<T>(future: impl std::future::Future<Output = T>) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(future))
        }
        _ => future.await,
    }
}

/// The ONNX diarizer in the child of `sidecar`, its sessions on
/// [`ONNX_THREADS`], its two models under the store's `diarization/`.
/// `install` says whether a missing file is downloaded first or fails the
/// call with `DiarizeError::NotInstalled`. A failed call fails that call
/// only; the next tries again, in a new child if the last one died.
#[must_use]
pub fn diarizer_in(sidecar: Arc<SidecarSpeechEngine>, install: Install) -> Arc<dyn Diarizer> {
    Arc::new(SidecarDiarizer::new(sidecar, install, ONNX_THREADS))
}

/// The host's model service over the models under one directory.
pub struct ModelStoreSpeechModels {
    /// The ONNX store: the speech models and the diarizer's.
    pub speech: ModelStore,
    /// The `CoreML` store, the models directory's `fluidaudio/`.
    pub coreml_store: ModelStore,
    speech_settings: SpeechSettings,
}

impl ModelStoreSpeechModels {
    /// The model service over `setup`.
    #[must_use]
    pub fn new(setup: &SpeechSetup) -> Self {
        ModelStoreSpeechModels {
            speech: setup.model_store(),
            coreml_store: setup.speech_settings.coreml_store(&setup.models_directory),
            speech_settings: setup.speech_settings.clone(),
        }
    }

    #[must_use]
    pub fn coreml_directory(&self) -> PathBuf {
        self.coreml_store
            .directory(&steno_speech::ModelAsset::parakeet_v3_coreml())
    }

    /// Whether every model file the engine on `runtime` loads is on disk at
    /// its manifest size.
    pub(crate) fn installed_on(&self, runtime: SpeechRuntime) -> bool {
        match runtime {
            SpeechRuntime::CoreMlInProcess => self
                .coreml_store
                .is_installed(&steno_speech::ModelAsset::parakeet_v3_coreml()),
            SpeechRuntime::OnnxSidecar => steno_speech::ModelAsset::onnx()
                .iter()
                .all(|asset| self.speech.is_installed(asset)),
        }
    }

    fn runs_on_coreml(&self, engine_id: &str) -> bool {
        self.runtime(engine_id) == SpeechRuntime::CoreMlInProcess
    }

    fn runtime(&self, engine_id: &str) -> SpeechRuntime {
        engine_runtime(engine_id, &self.speech_settings)
    }

    fn parakeet_on_coreml(&self) -> bool {
        self.runs_on_coreml(PARAKEET_V3)
    }

    /// The ONNX store's asset behind a row.
    fn onnx_asset(asset: ModelAsset) -> Option<steno_speech::ModelAsset> {
        match asset {
            ModelAsset::ParakeetV3 => Some(steno_speech::ModelAsset::parakeet_v3_fp32()),
            ModelAsset::OfflineDiarizer => Some(steno_diarize::models::asset()),
            _ => None,
        }
    }

    /// Why a row Settings does not offer cannot be downloaded or removed.
    fn not_offered(asset: ModelAsset) -> String {
        format!("{} is not part of this version", asset.as_str())
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
    /// Whether the models of the engine for `engine_id` are installed.
    fn engine_installed(&self, engine_id: &str) -> bool {
        self.installed_on(self.runtime(engine_id))
    }

    fn is_installed(&self, asset: ModelAsset) -> bool {
        match asset {
            ModelAsset::ParakeetV3 => self.engine_installed(PARAKEET_V3),
            other => Self::onnx_asset(other).is_some_and(|asset| self.speech.is_installed(&asset)),
        }
    }

    /// Where the sidecar runs Parakeet v3, its size counts the export only,
    /// not Silero VAD.
    fn installed_size(&self, asset: ModelAsset) -> Option<i64> {
        if !self.is_installed(asset) {
            return None;
        }
        Some(match asset {
            ModelAsset::ParakeetV3 if self.parakeet_on_coreml() => {
                Self::size_of(&self.coreml_directory())
            }
            other => Self::size_of(&self.speech.directory(&Self::onnx_asset(other)?)),
        })
    }

    fn download(
        &self,
        asset: ModelAsset,
        progress: &mut dyn FnMut(f64, &str),
    ) -> BoundaryResult<()> {
        match asset {
            ModelAsset::OfflineDiarizer => {
                steno_diarize::models::remove_old_parts(&self.speech);
                install_with_progress(&self.speech, &[steno_diarize::models::asset()], progress)
            }
            ModelAsset::ParakeetV3 if self.parakeet_on_coreml() => install_with_progress(
                &self.coreml_store,
                &[steno_speech::ModelAsset::parakeet_v3_coreml()],
                progress,
            ),
            // Silero VAD too, so the ONNX engine is ready offline.
            ModelAsset::ParakeetV3 => {
                install_with_progress(&self.speech, &steno_speech::ModelAsset::onnx(), progress)
            }
            other => Err(Self::not_offered(other).into()),
        }
    }

    /// Where the sidecar runs Parakeet v3, removing it keeps Silero VAD,
    /// which is small.
    fn remove(&self, asset: ModelAsset) -> BoundaryResult<()> {
        match asset {
            ModelAsset::ParakeetV3 if self.parakeet_on_coreml() => Ok(self
                .coreml_store
                .remove(&steno_speech::ModelAsset::parakeet_v3_coreml())?),
            other => {
                let asset = Self::onnx_asset(other).ok_or_else(|| Self::not_offered(other))?;
                Ok(self.speech.remove(&asset)?)
            }
        }
    }

    /// Parakeet v3 is the fp32 ONNX export where the sidecar runs it, the
    /// `CoreML` int8 build where `CoreML` does.
    fn display_name(&self, asset: ModelAsset) -> &'static str {
        match asset {
            ModelAsset::ParakeetV3 if !self.parakeet_on_coreml() => "Parakeet TDT 0.6B v3 (fp32)",
            ModelAsset::OfflineDiarizer => steno_diarize::models::DISPLAY_NAME,
            other => other.display_name(),
        }
    }

    fn source_repo(&self, asset: ModelAsset) -> &'static str {
        match asset {
            ModelAsset::ParakeetV3 if !self.parakeet_on_coreml() => "nvidia/parakeet-tdt-0.6b-v3",
            ModelAsset::OfflineDiarizer => ONNX_DIARIZER_NOTICES[0].2,
            other => other.source_repo(),
        }
    }

    /// The diarizer's two models each have their own licence line; every
    /// other asset has the default one.
    fn notices(&self, asset: ModelAsset) -> Vec<ModelNotice> {
        match asset {
            ModelAsset::OfflineDiarizer => ONNX_DIARIZER_NOTICES
                .iter()
                .map(|(name, licence, source)| ModelNotice {
                    name: (*name).to_owned(),
                    licence: (*licence).to_owned(),
                    source: (*source).to_owned(),
                })
                .collect(),
            other => vec![ModelNotice {
                name: self.display_name(other).to_owned(),
                licence: other.licence().to_owned(),
                source: self.source_repo(other).to_owned(),
            }],
        }
    }

    /// The manifest size of the model behind the row.
    fn expected_bytes(&self, asset: ModelAsset) -> i64 {
        let bytes = match asset {
            ModelAsset::ParakeetV3 if self.parakeet_on_coreml() => {
                steno_speech::ModelAsset::parakeet_v3_coreml().total_size()
            }
            ModelAsset::ParakeetV3 => steno_speech::ModelAsset::parakeet_v3_fp32().total_size(),
            ModelAsset::OfflineDiarizer => steno_diarize::models::asset().total_size(),
            other => return other.approximate_bytes(),
        };
        i64::try_from(bytes).unwrap_or(i64::MAX)
    }
}

/// The diarizer's two acknowledgement lines, `(name with attribution,
/// licence, source)`, whose licences `steno_diarize::models::LICENCE`
/// joins.
const ONNX_DIARIZER_NOTICES: [(&str, &str, &str); 2] = [
    (
        "pyannote segmentation 3.0 by pyannote.audio, converted to ONNX",
        "MIT",
        "pyannote/segmentation-3.0",
    ),
    (
        "WeSpeaker ResNet34-LM by WeSpeaker, trained on VoxCeleb, converted to ONNX",
        "CC-BY-4.0",
        "Wespeaker/wespeaker-voxceleb-resnet34-LM",
    ),
];

/// Installs `assets` into `store` in turn, reporting the fraction of all
/// their bytes and the file under way (at most once per percent or file),
/// then `(1.0, "Installed")`. A file inside a bundle is reported as the
/// bundle.
fn install_with_progress(
    store: &ModelStore,
    assets: &[steno_speech::ModelAsset],
    progress: &mut dyn FnMut(f64, &str),
) -> BoundaryResult<()> {
    let total = assets
        .iter()
        .map(steno_speech::ModelAsset::total_size)
        .sum::<u64>()
        .max(1);
    let mut throttle = ProgressThrottle::default();
    let mut received_before: u64 = 0;
    for asset in assets {
        store.ensure(asset, &mut |report| {
            let earlier_files: u64 = asset
                .files
                .iter()
                .take_while(|f| f.name != report.file)
                .map(|f| f.size)
                .sum();
            #[allow(clippy::cast_precision_loss)]
            let fraction = ((received_before + earlier_files + report.received) as f64
                / total as f64)
                .min(1.0);
            let shown = report.file.split('/').next().unwrap_or(report.file);
            if throttle.forwards(fraction, shown) {
                progress(fraction, shown);
            }
        })?;
        received_before += asset.total_size();
    }
    progress(1.0, "Installed");
    Ok(())
}

/// Lets a report through only when its whole percent or its file changes:
/// the host publishes a snapshot for each.
#[derive(Default)]
struct ProgressThrottle {
    last: Option<(i64, String)>,
}

impl ProgressThrottle {
    fn forwards(&mut self, fraction: f64, file: &str) -> bool {
        #[allow(clippy::cast_possible_truncation)]
        let percent = (fraction * 100.0) as i64;
        if self
            .last
            .as_ref()
            .is_some_and(|(last, last_file)| *last == percent && last_file == file)
        {
            return false;
        }
        self.last = Some((percent, file.to_owned()));
        true
    }
}

/// Model files on disk for the tests. Every writer panics outside the
/// temp directory or inside the directory `STENO_MODELS_DIR` names
/// ([`testing::is_scratch`]), so none overwrites real models.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeSet;
    use std::path::{Component, Path, PathBuf};

    use steno_speech::{ModelStore, SidecarConfig, SpeechSettings};

    use super::{ModelStoreSpeechModels, SpeechSetup};

    /// A setup over `models_directory` with a sidecar binary that does not
    /// exist.
    pub fn setup(models_directory: &Path, speech_settings: SpeechSettings) -> SpeechSetup {
        SpeechSetup {
            models_directory: models_directory.to_path_buf(),
            speech_settings,
            sidecar: SidecarConfig::new(models_directory.join("no-such-sidecar")),
        }
    }

    /// The speech settings with the Mac's sidecar fallback chosen.
    pub fn sidecar_chosen() -> SpeechSettings {
        SpeechSettings {
            onnx_sidecar_on_mac: true,
            ..SpeechSettings::default()
        }
    }

    pub fn models_in(models_directory: &Path) -> ModelStoreSpeechModels {
        ModelStoreSpeechModels::new(&setup(models_directory, SpeechSettings::default()))
    }

    /// The complete `CoreML` Parakeet, as sparse files of manifest size.
    pub fn install_coreml_parakeet(models: &ModelStoreSpeechModels) {
        install_in(
            &models.coreml_store,
            &steno_speech::ModelAsset::parakeet_v3_coreml(),
        );
    }

    /// The two ONNX diarizer models, as sparse files of manifest size.
    pub fn install_onnx_diarizer(models: &ModelStoreSpeechModels) {
        install_speech_asset(models, &steno_diarize::models::asset());
    }

    /// Every model the app's engines load.
    pub fn install_every_model(models: &ModelStoreSpeechModels) {
        install_coreml_parakeet(models);
        for asset in steno_speech::ModelAsset::onnx() {
            install_speech_asset(models, &asset);
        }
        install_onnx_diarizer(models);
    }

    /// `asset`'s files in the speech store, as sparse files.
    pub fn install_speech_asset(models: &ModelStoreSpeechModels, asset: &steno_speech::ModelAsset) {
        install_in(&models.speech, asset);
    }

    fn install_in(store: &steno_speech::ModelStore, asset: &steno_speech::ModelAsset) {
        let directory = store.directory(asset);
        assert!(
            is_scratch(&directory),
            "a test writes models only into a temp directory it owns \
             (`models_in(tempdir)`, or the settings' `models_directory`), \
             not into {} (the temp directory is {}, {} names {})",
            directory.display(),
            std::env::temp_dir().display(),
            ModelStore::ENVIRONMENT_VARIABLE,
            ModelStore::environment_models_directory()
                .map_or_else(|| "nothing".to_owned(), |named| named.display().to_string()),
        );
        for file in &asset.files {
            let path = directory.join(&file.name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::File::create(path)
                .unwrap()
                .set_len(file.size)
                .unwrap();
        }
    }

    /// [`is_scratch_in`] the temp directory, outside `STENO_MODELS_DIR`.
    pub fn is_scratch(path: &Path) -> bool {
        is_scratch_in(
            path,
            &std::env::temp_dir(),
            ModelStore::environment_models_directory().as_deref(),
        )
    }

    /// Whether `path` lies inside `temp` and outside `named`, compared as the
    /// file system resolves them. A path with a `..` component is not
    /// scratch, and neither is any path while `named` cannot be resolved.
    pub fn is_scratch_in(path: &Path, temp: &Path, named: Option<&Path>) -> bool {
        if path.components().any(|part| part == Component::ParentDir) {
            return false;
        }
        let (Some(path), Ok(temp)) = (resolved(path), temp.canonicalize()) else {
            return false;
        };
        path.starts_with(temp)
            && named
                .is_none_or(|named| resolved(named).is_some_and(|named| !path.starts_with(named)))
    }

    /// `path` with its deepest existing ancestor canonicalised.
    fn resolved(path: &Path) -> Option<PathBuf> {
        let mut existing = path;
        let mut missing = Vec::new();
        while !existing.exists() {
            missing.push(existing.file_name()?);
            existing = existing.parent()?;
        }
        let mut resolved = existing.canonicalize().ok()?;
        resolved.extend(missing.into_iter().rev());
        Some(resolved)
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

    /// The guard takes paths inside the temp directory and refuses the root,
    /// a look-alike sibling and a path that climbs out.
    #[test]
    fn the_model_writers_write_only_inside_the_temp_directory() {
        let temp = std::env::temp_dir();
        let dir = tempfile::tempdir().unwrap();
        assert!(testing::is_scratch(dir.path()));
        assert!(testing::is_scratch(&dir.path().join("not/yet/there")));
        let root = temp.ancestors().last().unwrap();
        assert!(!testing::is_scratch(&root.join("steno-models")));
        let mut sibling = temp.file_name().unwrap().to_owned();
        sibling.push("-steno-sibling");
        assert!(!testing::is_scratch(
            &temp.with_file_name(sibling).join("models")
        ));
        assert!(!testing::is_scratch(&dir.path().join("../../steno-models")));
    }

    /// The guard refuses the directory `STENO_MODELS_DIR` names and anything
    /// in it, even inside the temp directory or through a link; an
    /// unresolvable named directory refuses every path.
    #[test]
    fn the_model_writers_never_write_inside_the_models_directory_the_environment_names() {
        let root = tempfile::tempdir().unwrap();
        let temp = root.path().join("tmp");
        let named = temp.join("real-models");
        let model = named.join("onnx/diarization/pyannote-segmentation-3.0.onnx");
        let scratch = temp.join("test-models/onnx/diarization");
        std::fs::create_dir_all(&scratch).unwrap();
        for named in [Some(named.as_path()), None] {
            assert!(testing::is_scratch_in(&scratch, &temp, named));
        }
        for path in [&named, &model] {
            assert!(testing::is_scratch_in(path, &temp, None));
            assert!(!testing::is_scratch_in(path, &temp, Some(&named)));
        }
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        std::fs::write(&model, b"a developer's downloaded model").unwrap();
        assert!(!testing::is_scratch_in(&model, &temp, Some(&named)));
        #[cfg(unix)]
        {
            let link = temp.join("models-link");
            std::os::unix::fs::symlink(&named, &link).unwrap();
            let through_link = link.join("onnx/diarization/pyannote-segmentation-3.0.onnx");
            for named in [&named, &link] {
                for path in [&model, &through_link, &link.join("onnx/not-yet.onnx")] {
                    assert!(!testing::is_scratch_in(path, &temp, Some(named)));
                }
                assert!(testing::is_scratch_in(&scratch, &temp, Some(named)));
            }
        }
        assert!(testing::is_scratch_in(
            &temp.join("real-models-other"),
            &temp,
            Some(&named)
        ));
        assert!(!testing::is_scratch_in(
            &scratch,
            &temp,
            Some(&temp.join("no/such/..")),
        ));
    }

    /// An install into a models directory that climbs with `..` panics.
    #[test]
    #[should_panic(expected = "a test writes models only into a temp directory it owns")]
    fn the_model_writers_panic_where_the_guard_refuses() {
        let dir = tempfile::tempdir().unwrap();
        testing::install_onnx_diarizer(&testing::models_in(&dir.path().join("x/..")));
    }

    /// The same runtime gets the same engine; the sidecar engine is kept for
    /// the run, the in-process one only while something holds it.
    #[test]
    fn each_runtime_gets_the_engine_it_got_before_and_the_sidecar_engine_is_kept_for_the_run() {
        use SpeechRuntime::{CoreMlInProcess, OnnxSidecar};

        let builds = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engines = SpeechEngines::with_builder(
            testing::setup(Path::new("/models"), SpeechSettings::default()),
            Box::new({
                let builds = builds.clone();
                move |runtime| {
                    builds.lock().unwrap().push(runtime);
                    Arc::new(FakeSpeechEngine::default())
                }
            }),
        );
        let built = || builds.lock().unwrap().clone();
        let sidecar = engines.engine(OnnxSidecar);
        assert!(sidecar.ptr_eq(&engines.engine(OnnxSidecar)));
        assert_eq!(built(), [OnnxSidecar]);

        let in_process = engines.engine(CoreMlInProcess);
        assert!(!in_process.ptr_eq(&sidecar));
        assert!(in_process.ptr_eq(&engines.engine(CoreMlInProcess)));
        assert_eq!(built(), [OnnxSidecar, CoreMlInProcess]);

        // Back to each while a retired pipeline holds it: the same engines.
        assert!(sidecar.ptr_eq(&engines.engine(OnnxSidecar)));
        assert!(in_process.ptr_eq(&engines.engine(CoreMlInProcess)));
        assert_eq!(built(), [OnnxSidecar, CoreMlInProcess]);

        // Once nothing holds the in-process engine it is rebuilt.
        drop(in_process);
        let _again = engines.engine(CoreMlInProcess);
        assert!(sidecar.ptr_eq(&engines.engine(OnnxSidecar)));
        assert_eq!(built(), [OnnxSidecar, CoreMlInProcess, CoreMlInProcess]);
    }

    #[test]
    fn the_models_directory_is_the_settings_then_the_variable_then_the_support_directory() {
        let support = Path::new("/tmp/steno-support");
        let paths = StenoPaths::new(support);
        let mut settings = Settings::default();
        assert_eq!(
            models_directory_with(&settings, &paths, None),
            support.join("Models")
        );
        // Absolute on the platform: `/tmp/...` has no drive on Windows.
        let from_variable = if cfg!(windows) {
            r"C:\steno-env-models"
        } else {
            "/tmp/steno-env-models"
        };
        assert_eq!(
            models_directory_with(&settings, &paths, Some(from_variable.into())),
            Path::new(from_variable)
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
        assert_eq!(
            ModelStore::in_models_directory(chosen).root(),
            chosen.join("onnx")
        );
        assert_eq!(
            coreml_model_directory(chosen),
            chosen.join("fluidaudio").join("parakeet-tdt-0.6b-v3")
        );
    }

    /// The `CoreML` Parakeet counts as installed only with all 23 files at
    /// their manifest size.
    #[test]
    fn the_coreml_parakeet_counts_as_installed_only_when_complete() {
        let dir = tempfile::tempdir().unwrap();
        let models = testing::models_in(dir.path());
        assert!(!models.installed_on(SpeechRuntime::CoreMlInProcess));
        testing::install_coreml_parakeet(&models);
        assert!(models.installed_on(SpeechRuntime::CoreMlInProcess));
        let asset = steno_speech::ModelAsset::parakeet_v3_coreml();
        let weights = asset
            .files
            .iter()
            .find(|file| file.name.starts_with("Encoder.mlmodelc/") && file.size > 1)
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(models.coreml_directory().join(&weights.name))
            .unwrap()
            .set_len(weights.size - 1)
            .unwrap();
        assert!(!models.installed_on(SpeechRuntime::CoreMlInProcess));
    }

    #[test]
    fn parakeet_v3_status_follows_the_model_the_engine_runs() {
        let dir = tempfile::tempdir().unwrap();
        let models = testing::models_in(dir.path());
        assert!(!models.is_installed(ModelAsset::ParakeetV3));
        testing::install_coreml_parakeet(&models);
        assert_eq!(
            models.is_installed(ModelAsset::ParakeetV3),
            cfg!(target_os = "macos"),
            "the CoreML model counts on the Mac only"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(
                models.installed_size(ModelAsset::ParakeetV3),
                Some(
                    i64::try_from(steno_speech::ModelAsset::parakeet_v3_coreml().total_size())
                        .unwrap()
                )
            );
            models.remove(ModelAsset::ParakeetV3).unwrap();
            assert!(!models.coreml_directory().exists());
            assert!(!models.is_installed(ModelAsset::ParakeetV3));
        }
    }

    /// On the ONNX export, Parakeet v3 is installed only with Silero VAD
    /// beside it; removing it keeps the VAD.
    #[test]
    fn on_the_onnx_export_parakeet_v3_needs_the_export_and_the_vad() {
        let dir = tempfile::tempdir().unwrap();
        let models =
            ModelStoreSpeechModels::new(&testing::setup(dir.path(), testing::sidecar_chosen()));
        testing::install_speech_asset(&models, &steno_speech::ModelAsset::parakeet_v3_fp32());
        assert!(!models.is_installed(ModelAsset::ParakeetV3), "no VAD yet");
        assert!(!models.engine_installed("parakeet-v3"));
        testing::install_speech_asset(&models, &steno_speech::ModelAsset::silero_vad());
        assert!(models.is_installed(ModelAsset::ParakeetV3));
        assert!(models.engine_installed("parakeet-v3"));
        models.remove(ModelAsset::ParakeetV3).unwrap();
        assert!(!models.is_installed(ModelAsset::ParakeetV3));
        assert!(
            models
                .speech
                .is_installed(&steno_speech::ModelAsset::silero_vad())
        );
    }

    /// The diarizer's row is installed only once both files are complete,
    /// and removing it deletes the folder.
    #[test]
    fn the_diarizer_row_reads_its_asset_in_the_onnx_store() {
        let dir = tempfile::tempdir().unwrap();
        let models = testing::models_in(dir.path());
        let asset = steno_diarize::models::asset();
        let folder = dir.path().join("onnx").join("diarization");
        assert_eq!(models.speech.directory(&asset), folder);
        assert!(!models.is_installed(ModelAsset::OfflineDiarizer));
        testing::install_onnx_diarizer(&models);
        assert!(models.is_installed(ModelAsset::OfflineDiarizer));
        assert_eq!(
            models.installed_size(ModelAsset::OfflineDiarizer),
            Some(i64::try_from(asset.total_size()).unwrap())
        );
        let embedding = &asset.files[1];
        std::fs::File::options()
            .write(true)
            .open(folder.join(&embedding.name))
            .unwrap()
            .set_len(embedding.size - 1)
            .unwrap();
        assert!(!models.is_installed(ModelAsset::OfflineDiarizer));
        assert_eq!(models.installed_size(ModelAsset::OfflineDiarizer), None);
        models.remove(ModelAsset::OfflineDiarizer).unwrap();
        assert!(!folder.exists());
    }

    /// The diarizer's download goes through the mirror and fails a junk file
    /// at its checksum, installing nothing; earlier `.part` files go first.
    #[test]
    fn the_diarizer_download_goes_through_the_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let asset = steno_diarize::models::asset();
        let first = &asset.files[0];
        let models = ModelStoreSpeechModels::new(&testing::setup(
            dir.path(),
            SpeechSettings {
                models_mirror: Some(junk_mirror(usize::try_from(first.size).unwrap())),
                ..SpeechSettings::default()
            },
        ));
        let folder = models.speech.directory(&asset);
        std::fs::create_dir_all(&folder).unwrap();
        let old_part = folder.join(format!("{}a1B2c3.part", first.name));
        std::fs::write(&old_part, b"old").unwrap();
        let mut reports = Vec::new();
        let error = models
            .download(ModelAsset::OfflineDiarizer, &mut |fraction, file| {
                reports.push((fraction, file.to_owned()));
            })
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("sha256") && error.contains(&first.name),
            "{error}"
        );
        assert!(
            !reports.is_empty()
                && reports
                    .iter()
                    .all(|(fraction, file)| *file == first.name && *fraction < 1.0),
            "{reports:?}"
        );
        assert!(!models.is_installed(ModelAsset::OfflineDiarizer));
        assert!(!old_part.exists(), "the earlier store's partial is deleted");
    }

    /// The pipelines' diarizer never downloads: over an empty directory its
    /// `prepare` is the models-missing refusal and the mirror gets no
    /// request.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_apps_diarizer_never_downloads_its_models() {
        let dir = tempfile::tempdir().unwrap();
        let (mirror, requests) = counting_junk_mirror(0);
        let engines = SpeechEngines::with_checks(
            testing::setup(
                dir.path(),
                SpeechSettings {
                    models_mirror: Some(mirror),
                    ..SpeechSettings::default()
                },
            ),
            Arc::new(|_| true),
            Arc::new(|| true),
        );
        let error = engines.diarizer().prepare().await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<steno_pipeline::PipelineFailure>(),
            Some(&steno_pipeline::PipelineFailure::models_missing(
                steno_core::PipelineStage::Diarize
            )),
            "{error}"
        );
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// The app's speech engine and diarizer share one sidecar child, over the
    /// real sidecar binary's fake engine.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_apps_speech_engine_and_diarizer_share_one_sidecar_child() {
        let dir = tempfile::tempdir().unwrap();
        let mut setup = testing::setup(dir.path(), SpeechSettings::default());
        // The binaries sit one folder above the test binary's `deps/`.
        let exe = std::env::current_exe().unwrap();
        setup.sidecar.program = exe
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join(steno_speech::sidecar::SIDECAR_BINARY);
        assert!(
            setup.sidecar.program.is_file(),
            "{} is missing: build it with `cargo build -p steno-speech-sidecar`",
            setup.sidecar.program.display()
        );
        setup.sidecar.args = vec!["--fake-engine".into()];
        let models = ModelStoreSpeechModels::new(&setup);
        for asset in steno_speech::ModelAsset::onnx() {
            testing::install_speech_asset(&models, &asset);
        }
        testing::install_onnx_diarizer(&models);
        let engines = SpeechEngines::with_checks(setup, Arc::new(|_| true), Arc::new(|| true));
        let sidecar = engines.sidecar.clone().unwrap();
        let speech = engines.engine(SpeechRuntime::OnnxSidecar);
        let diarizer = engines.diarizer();
        let audio = AudioBuffer16k::new(vec![0.25; 32_000]);

        speech.prepare().await.unwrap();
        let pid = sidecar.pid().expect("speech's child runs");
        let result = diarizer.diarize(&audio).await.unwrap();
        assert_eq!(result.clusters.len(), 1, "{result:?}");
        assert_eq!(sidecar.spawns(), 1, "the diarizer ran in speech's child");
        assert_eq!(sidecar.pid(), Some(pid));

        speech.release().await.unwrap();
        assert_eq!(sidecar.pid(), None);
        diarizer.diarize(&audio).await.unwrap();
        assert_eq!(
            sidecar.spawns(),
            2,
            "the diarizer's own child is this engine's"
        );
        assert_eq!(sidecar.pid(), None, "and stopped after the call");
    }

    /// `models_installed` holds only while both gates would let a run through.
    #[test]
    fn models_installed_needs_the_speech_models_and_the_diarizers() {
        let dir = tempfile::tempdir().unwrap();
        for (speech, diarizer) in [(false, false), (true, false), (false, true), (true, true)] {
            let engines = SpeechEngines::with_checks(
                testing::setup(dir.path(), SpeechSettings::default()),
                Arc::new(move |_| speech),
                Arc::new(move || diarizer),
            );
            assert_eq!(
                engines.models_installed(SpeechRuntime::OnnxSidecar),
                speech && diarizer,
                "speech {speech}, diarizer {diarizer}"
            );
        }
    }

    /// The diarizer's lines carry the licences `LICENCE` joins, and so does
    /// the host's default line.
    #[test]
    fn the_diarizer_notices_carry_the_models_licences() {
        let licences: Vec<&str> = ONNX_DIARIZER_NOTICES
            .iter()
            .map(|(_, licence, _)| *licence)
            .collect();
        assert_eq!(licences.join(" AND "), steno_diarize::models::LICENCE);
        assert_eq!(
            ModelAsset::OfflineDiarizer.licence(),
            steno_diarize::models::LICENCE
        );
    }

    /// A loopback mirror that answers every request with `len` junk bytes.
    fn junk_mirror(len: usize) -> String {
        counting_junk_mirror(len).0
    }

    fn counting_junk_mirror(len: usize) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&requests);
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Read the request head, so closing does not reset it.
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(&[head.as_bytes(), &vec![b'x'; len]].concat());
            }
        });
        (format!("http://{address}"), requests)
    }

    /// On the ONNX export, the Parakeet v3 download fetches Silero VAD too,
    /// counting both in its fraction.
    #[test]
    fn on_the_onnx_export_the_parakeet_v3_download_installs_the_vad_and_counts_it() {
        let dir = tempfile::tempdir().unwrap();
        let settings = |models_mirror| SpeechSettings {
            models_mirror,
            ..testing::sidecar_chosen()
        };
        let local = ModelStoreSpeechModels::new(&testing::setup(dir.path(), settings(None)));
        let with_mirror = |len| {
            ModelStoreSpeechModels::new(&testing::setup(
                dir.path(),
                settings(Some(junk_mirror(len))),
            ))
        };
        let download = |models: &ModelStoreSpeechModels| {
            let mut reports = Vec::new();
            let result = models.download(ModelAsset::ParakeetV3, &mut |fraction, file| {
                reports.push((fraction, file.to_owned()));
            });
            (result, reports)
        };
        let vad = steno_speech::ModelAsset::silero_vad();
        let export = steno_speech::ModelAsset::parakeet_v3_fp32();
        testing::install_speech_asset(&local, &export);

        let vad_size = usize::try_from(vad.total_size()).unwrap();
        let (result, _) = download(&with_mirror(vad_size));
        let error = result.unwrap_err().to_string();
        assert!(error.contains("silero_vad.onnx"), "{error}");

        testing::install_speech_asset(&local, &vad);
        let (result, reports) = download(&with_mirror(0));
        result.unwrap();
        assert_eq!(reports, [(1.0, "Installed".to_owned())]);

        let tokens = export
            .files
            .iter()
            .find(|f| f.name == "tokens.txt")
            .unwrap();
        std::fs::remove_file(local.speech.directory(&export).join("tokens.txt")).unwrap();
        let (result, reports) = download(&with_mirror(usize::try_from(tokens.size).unwrap()));
        let error = result.unwrap_err().to_string();
        assert!(error.contains("sha256"), "{error}");
        #[allow(clippy::cast_precision_loss)]
        let (total, rest) = (
            (vad.total_size() + export.total_size()) as f64,
            tokens.size as f64,
        );
        // The reports start at (total - tokens.txt) / total, below 1.0.
        assert!(
            reports.first().is_some_and(|(fraction, _)| *fraction < 1.0),
            "{reports:?}"
        );
        assert!(
            reports
                .iter()
                .all(|(fraction, file)| file == "tokens.txt" && *fraction >= (total - rest) / total),
            "{reports:?}"
        );
    }

    /// Settings downloads the `CoreML` Parakeet into `fluidaudio/` through the
    /// mirror, with progress; the junk file fails its checksum.
    #[test]
    fn the_coreml_parakeet_downloads_into_the_fluidaudio_folder_with_progress() {
        let dir = tempfile::tempdir().unwrap();
        let asset = steno_speech::ModelAsset::parakeet_v3_coreml();
        let first = &asset.files[0];
        let models = ModelStoreSpeechModels::new(&testing::setup(
            dir.path(),
            SpeechSettings {
                models_mirror: Some(junk_mirror(usize::try_from(first.size).unwrap())),
                ..SpeechSettings::default()
            },
        ));
        assert_eq!(
            models.coreml_store.directory(&asset),
            models.coreml_directory()
        );
        assert_eq!(
            models.coreml_directory(),
            dir.path().join("fluidaudio").join("parakeet-tdt-0.6b-v3")
        );
        let mut reports = Vec::new();
        let mut record = |fraction: f64, file: &str| reports.push((fraction, file.to_owned()));
        let error = if cfg!(target_os = "macos") {
            models.download(ModelAsset::ParakeetV3, &mut record)
        } else {
            install_with_progress(
                &models.coreml_store,
                std::slice::from_ref(&asset),
                &mut record,
            )
        }
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("sha256") && error.contains("coremldata.bin"),
            "{error}"
        );
        let bundle = first.name.split('/').next().unwrap();
        assert!(
            first.name.contains('/')
                && !reports.is_empty()
                && reports
                    .iter()
                    .all(|(fraction, file)| file == bundle && *fraction < 1.0),
            "the bundle, not the file inside it: {reports:?}"
        );
        assert!(!models.installed_on(SpeechRuntime::CoreMlInProcess));
        assert!(
            testing::files_under(&models.coreml_directory())
                .iter()
                .all(|path| path.extension().is_some_and(|ext| ext == "lock")),
            "only the lock of the file that failed"
        );
    }

    #[test]
    fn download_progress_goes_through_once_per_percent_or_file() {
        let mut throttle = ProgressThrottle::default();
        let forwarded: Vec<_> = [
            (0.0, "a"),
            (0.004, "a"),
            (0.009, "a"),
            (0.01, "a"),
            (0.011, "b"),
            (0.015, "b"),
            (0.5, "b"),
        ]
        .into_iter()
        .filter(|(fraction, file)| throttle.forwards(*fraction, file))
        .collect();
        assert_eq!(
            forwarded,
            [(0.0, "a"), (0.01, "a"), (0.011, "b"), (0.5, "b")]
        );
    }

    /// With the `CoreML` Parakeet on disk, only `parakeet-v3` on the Mac
    /// counts as installed for the warm-up.
    #[test]
    fn the_configured_engine_is_installed_only_when_the_model_it_loads_is() {
        let dir = tempfile::tempdir().unwrap();
        let models = testing::models_in(dir.path());
        assert!(!models.engine_installed("parakeet-v3"));
        testing::install_coreml_parakeet(&models);
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
        // The manifest holds every file the backend opens.
        let names: Vec<String> = steno_speech::ModelAsset::parakeet_v3_coreml()
            .files
            .into_iter()
            .map(|file| file.name)
            .collect();
        for bundle in [
            steno_speech_coreml::backend::PREPROCESSOR_FILE,
            steno_speech_coreml::backend::ENCODER_FILE,
            steno_speech_coreml::backend::DECODER_FILE,
            steno_speech_coreml::backend::JOINT_FILE,
        ] {
            let data = format!("{bundle}/coremldata.bin");
            assert!(names.contains(&data), "{data}");
        }
        let vocabulary = steno_speech_coreml::backend::VOCABULARY_FILE.to_owned();
        assert!(names.contains(&vocabulary), "{vocabulary}");
    }

    /// On the Mac `parakeet-v3` is `CoreML`, or the sidecar's with the
    /// fallback chosen; what `prepare` misses tells them apart. No network.
    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread")]
    async fn parakeet_v3_is_the_coreml_engine_on_the_mac_unless_the_sidecar_is_chosen() {
        let dir = tempfile::tempdir().unwrap();
        let unserved = |onnx_sidecar_on_mac| {
            testing::setup(
                dir.path(),
                SpeechSettings {
                    onnx_sidecar_on_mac,
                    models_mirror: Some("http://127.0.0.1:9/".to_owned()),
                    ..SpeechSettings::default()
                },
            )
        };
        let coreml = coreml_model_directory(dir.path()).display().to_string();
        let engine = speech_engine(PARAKEET_V3, &unserved(false));
        assert_eq!(engine.id(), steno_speech_coreml::ENGINE_ID);
        assert_eq!(engine.supported_languages().len(), 25);
        let error = engine.prepare().await.unwrap_err().to_string();
        assert!(error.contains(&coreml), "the CoreML engine: {error}");
        let error = speech_engine(PARAKEET_V3, &unserved(true))
            .prepare()
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains(&coreml), "the sidecar engine: {error}");
    }

    /// The platform policy per engine id.
    #[test]
    fn parakeet_runs_in_the_sidecar_except_on_the_mac_by_default() {
        let default = testing::setup(Path::new("/tmp/steno-models"), SpeechSettings::default());
        let fallback = testing::setup(Path::new("/tmp/steno-models"), testing::sidecar_chosen());
        let parakeet = if cfg!(target_os = "macos") {
            SpeechRuntime::CoreMlInProcess
        } else {
            SpeechRuntime::OnnxSidecar
        };
        assert_eq!(default.runtime("parakeet-v3"), parakeet);
        for id in ["whisperkit-large-v3-turbo", "parakeet-de", "anything"] {
            assert_eq!(default.runtime(id), SpeechRuntime::OnnxSidecar, "{id}");
        }
        for id in ["parakeet-v3", "whisperkit-large-v3-turbo"] {
            assert_eq!(fallback.runtime(id), SpeechRuntime::OnnxSidecar, "{id}");
        }
    }

    /// The sidecar engine installs into `onnx/` with the mirror and starts
    /// the binary the setup names.
    #[test]
    fn the_sidecar_engine_takes_its_store_mirror_and_binary_from_the_setup() {
        let models = Path::new("/tmp/steno-models");
        let setup = testing::setup(
            models,
            SpeechSettings {
                models_mirror: Some("http://mirror.example:8000/models/".to_owned()),
                ..SpeechSettings::default()
            },
        );
        let engine = sidecar_engine(&setup);
        assert_eq!(engine.store().root(), models.join("onnx"));
        assert_eq!(
            engine.store().mirror(),
            Some("http://mirror.example:8000/models")
        );
        assert_eq!(engine.config(), &setup.sidecar);
        assert_eq!(engine.id(), "parakeet-v3");
        assert_eq!(engine.pid(), None, "nothing starts before prepare");
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            SpeechSetup::in_models_directory(
                dir.path().join("Models"),
                &StenoPaths::new(dir.path())
            )
            .models_directory,
            dir.path().join("Models")
        );
    }

    #[test]
    fn the_directml_setting_reaches_the_sidecar_config() {
        for directml in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join(SPEECH_SETTINGS_FILE),
                format!(r#"{{"directmlOnWindows": {directml}}}"#),
            )
            .unwrap();
            let setup = SpeechSetup::in_models_directory(
                dir.path().join("Models"),
                &StenoPaths::new(dir.path()),
            );
            assert_eq!(setup.speech_settings.directml_on_windows, directml);
            assert_eq!(setup.sidecar.options.directml, directml);
        }
    }

    /// The sidecar's crash logs go into the support directory.
    #[test]
    fn the_sidecar_writes_its_crash_logs_into_the_support_directory() {
        let dir = tempfile::tempdir().unwrap();
        let setup = SpeechSetup::in_models_directory(
            dir.path().join("Models"),
            &StenoPaths::new(dir.path()),
        );
        assert_eq!(
            setup.sidecar.crash_log_directory.as_deref(),
            Some(dir.path())
        );
    }

    #[test]
    fn the_sidecar_binary_sits_beside_the_executable_with_the_onnx_threads() {
        let config = sidecar_config();
        assert_eq!(
            config.program,
            std::env::current_exe()
                .unwrap()
                .with_file_name(steno_speech::sidecar::SIDECAR_BINARY)
        );
        assert_eq!(config.options.intra_threads, ONNX_THREADS);
    }

    #[test]
    fn speech_settings_come_from_their_file_and_the_mirror_variable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(SPEECH_SETTINGS_FILE);
        assert_eq!(
            speech_settings_with(&file, None),
            SpeechSettings::default(),
            "no file: the defaults"
        );
        assert_eq!(
            speech_settings_with(&file, Some("http://env.example/m".to_owned())).models_mirror,
            Some("http://env.example/m".to_owned())
        );
        std::fs::write(
            &file,
            r#"{"directmlOnWindows":true,"onnxSidecarOnMac":true,"modelsMirror":"http://file.example/m"}"#,
        )
        .unwrap();
        let stored = SpeechSettings {
            onnx_sidecar_on_mac: true,
            directml_on_windows: true,
            models_mirror: Some("http://file.example/m".to_owned()),
        };
        assert_eq!(speech_settings_with(&file, None), stored);
        for empty in ["", " "] {
            assert_eq!(
                speech_settings_with(&file, Some(empty.to_owned())),
                stored,
                "an empty variable is unset"
            );
        }
        assert_eq!(
            speech_settings_with(&file, Some("http://env.example/m".to_owned())),
            SpeechSettings {
                models_mirror: Some("http://env.example/m".to_owned()),
                ..stored
            },
            "the variable wins over the file"
        );
        std::fs::write(&file, b"{not json").unwrap();
        assert_eq!(speech_settings_with(&file, None), SpeechSettings::default());
    }

    /// With the sidecar chosen, Parakeet v3 is the ONNX export: the `CoreML`
    /// model on disk does not count.
    #[test]
    fn with_the_sidecar_chosen_parakeet_v3_is_the_onnx_export() {
        let dir = tempfile::tempdir().unwrap();
        let models =
            ModelStoreSpeechModels::new(&testing::setup(dir.path(), testing::sidecar_chosen()));
        testing::install_coreml_parakeet(&models);
        assert!(!models.is_installed(ModelAsset::ParakeetV3));
        assert!(!models.engine_installed("parakeet-v3"));
        assert_eq!(
            models.display_name(ModelAsset::ParakeetV3),
            "Parakeet TDT 0.6B v3 (fp32)"
        );
        assert_eq!(
            models.source_repo(ModelAsset::ParakeetV3),
            "nvidia/parakeet-tdt-0.6b-v3"
        );
        assert_eq!(parakeet_size_text(&models), fp32_size_text());
    }

    /// A row this version does not offer is refused by Download and Remove,
    /// and nothing is touched.
    #[test]
    fn download_and_remove_refuse_a_retired_row_with_the_same_words() {
        let dir = tempfile::tempdir().unwrap();
        let models = testing::models_in(dir.path());
        for asset in [
            ModelAsset::ParakeetUltra,
            ModelAsset::ParakeetDe,
            ModelAsset::WhisperLargeV3Turbo,
        ] {
            let download = models.download(asset, &mut |_, _| {}).unwrap_err();
            let remove = models.remove(asset).unwrap_err();
            let words = format!("{} is not part of this version", asset.as_str());
            assert_eq!(download.to_string(), words);
            assert_eq!(remove.to_string(), words);
        }
    }

    /// The Parakeet v3 row's "Not downloaded" text with its size.
    fn parakeet_size_text(models: &ModelStoreSpeechModels) -> String {
        let speech = steno_host::settings::transcription::SpeechSettingsViewModel::new();
        let snapshot = steno_host::settings::snapshots::transcription(&speech, models, "");
        assert_eq!(snapshot.assets[0].id, ModelAsset::ParakeetV3.as_str());
        snapshot.assets[0].detail.clone()
    }

    fn fp32_size_text() -> String {
        let bytes = steno_speech::ModelAsset::parakeet_v3_fp32().total_size();
        format!(
            "Not downloaded · {}",
            steno_host::labels::file_size(i64::try_from(bytes).unwrap())
        )
    }

    /// Settings shows the size of the model the platform runs.
    #[test]
    fn the_expected_size_is_that_of_the_model_the_platform_runs() {
        let models = testing::models_in(Path::new("/tmp/steno-models"));
        let expected = if cfg!(target_os = "macos") {
            let bytes = steno_speech::ModelAsset::parakeet_v3_coreml().total_size();
            format!(
                "Not downloaded · {}",
                steno_host::labels::file_size(i64::try_from(bytes).unwrap())
            )
        } else {
            fp32_size_text()
        };
        assert_eq!(parakeet_size_text(&models), expected);
        assert_eq!(
            models.expected_bytes(ModelAsset::OfflineDiarizer),
            32_523_463
        );
    }

    /// The first acknowledgement row, Parakeet v3.
    fn parakeet_acknowledgement() -> (String, String) {
        let models = testing::models_in(Path::new("/tmp/steno-models"));
        let rows = steno_host::settings::snapshots::acknowledgements(&models);
        (rows[0].name.clone(), rows[0].source.clone())
    }

    /// Settings acknowledges Parakeet v3 and the diarizer's two models, each
    /// with its licence and author; `steno models` lists the ONNX models.
    #[test]
    fn the_diarizer_is_acknowledged_as_its_two_onnx_models() {
        let models = testing::models_in(Path::new("/tmp/steno-models"));
        let rows = steno_host::settings::snapshots::acknowledgements(&models);
        let speech: Vec<_> = rows
            .iter()
            .filter(|row| row.group == steno_bridge::GeneralAcknowledgementGroup::SpeechModels)
            .map(|row| (row.name.as_str(), row.licence.as_str(), row.source.as_str()))
            .collect();
        assert_eq!(speech.len(), 3, "{speech:?}");
        assert_eq!(
            &speech[1..],
            [
                (
                    "pyannote segmentation 3.0 by pyannote.audio, converted to ONNX",
                    "MIT",
                    "pyannote/segmentation-3.0",
                ),
                (
                    "WeSpeaker ResNet34-LM by WeSpeaker, trained on VoxCeleb, converted to ONNX",
                    "CC-BY-4.0",
                    "Wespeaker/wespeaker-voxceleb-resnet34-LM",
                ),
            ]
        );
        assert_eq!(
            models.display_name(ModelAsset::OfflineDiarizer),
            "Speaker diarization (pyannote segmentation 3.0, WeSpeaker ResNet34-LM)"
        );
        assert!(
            speech
                .iter()
                .all(|(name, _, source)| !name.contains("Whisper")
                    && !name.contains("Ultra")
                    && !source.contains("speaker-diarization-coreml")),
            "{speech:?}"
        );
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

    /// An engine that transcribes synchronously until `go` lets it, counting
    /// concurrent calls.
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

    /// Two transcriptions on one worker run one after the other, and the
    /// releasing task still runs while one blocks.
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

    /// Releasing the wrapper passes the call on to the inner engine.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_wrapper_passes_a_release_on_to_the_engine_it_wraps() {
        let inner = Arc::new(FakeSpeechEngine::default());
        let engine = OneCallAtATime::new(inner.clone());
        engine.release().await.unwrap();
        assert_eq!(inner.releases.count(), 1);
    }
}
