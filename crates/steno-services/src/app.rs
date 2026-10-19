//! [`build`] and [`build_with_import`]: turn the settings into the running
//! object graph, [`App`].

use std::path::Path;
use std::sync::Arc;

use chrono::{FixedOffset, Local, Offset as _};
use steno_adapters::DeliveryCoordinator;
use steno_audio::{CaptureSession, SymphoniaAudioCodec};
use steno_core::{
    DatabaseLock, DatabaseLockError, MeetingEvent, SecretKey, SecretStore, StenoPaths, Store,
    StoreError,
};
use steno_handover::HandoverService;
use steno_host::fakes::{FakeLoginItem, FakePermissions, FakeUpdater};
use steno_host::services::{
    LoginItem, LoginItemStatus, Opener, Permissions, Preferences, Services,
};
use steno_host::{Host, HostConfig};
use steno_llm::CodexCredentialStore;
use steno_pipeline::{MeetingEventBus, PipelineDependencies, RecordingIntake, RetentionSweep};

use crate::block_on;
use crate::calendar::Calendar;
use crate::detection::{DetectionController, DetectionOptions};
use crate::handover::{GatedHandover, IdentitySource, ListenerHandover, start_if_paired};
use crate::llm::{ClientLlmService, codex_store};
use crate::model_gate::ResumingSpeechModels;
use crate::permissions::SystemPermissions;
use crate::pipeline::{
    BuiltEngine, BuiltPipeline, CurrentPipeline, HostPipeline, MakeDependencies, run_sweep,
};
use crate::platform::{DiskFolderUsage, FilePreferences, PlatformAudioDevices, WallClock};
use crate::qr::PngQrEncoder;
use crate::recorder::{CaptureRecorder, DiskWatch, MakeCaptureSession};
use crate::recovery::{Interrupted, LiveRecordingCheck, adopt_orphans, reconcile_interrupted};
use crate::secrets::{KeepsApiKey, SecretsUnlocked, secret_store_with_unlock};
use crate::speech::{ModelStoreSpeechModels, SpeechEngines, SpeechSetup};
use crate::swift_import::{GraphImport, HandoverGate, ImportGate, ImportMarker, ImportStep};
use crate::updates::{IdleGate, ScheduleParts, UpdateSchedule, UpdateSource};

/// What stops the graph from being built: another process holds the
/// database ([`DatabaseLock`]), or the database could not be opened or
/// read.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error(transparent)]
    Lock(#[from] DatabaseLockError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("Could not create the database folder: {0}")]
    DatabaseFolder(#[source] std::io::Error),
    /// A stored setting cannot be read, with the database it is in
    /// ([`BuildError::unreadable_settings`]). The text names the key and the
    /// problem, never the value or the path. The app refuses to start rather
    /// than run on a default for it.
    #[error("{source}")]
    Setting {
        database: std::path::PathBuf,
        source: StoreError,
    },
}

impl BuildError {
    /// For unreadable stored settings: the store's error and the database
    /// they are in; `None` for every other failure.
    #[must_use]
    pub fn unreadable_settings(&self) -> Option<(&StoreError, &Path)> {
        match self {
            BuildError::Setting { database, source } => Some((source, database)),
            _ => None,
        }
    }
}

/// What differs between the shell, the CLI and the tests.
pub struct AppOptions {
    /// The support directory.
    pub paths: StenoPaths,
    /// `None` opens the database under `paths`.
    pub database_path: Option<std::path::PathBuf>,
    /// The platform keyring when true, else the 0600 secrets file.
    pub keyring: bool,
    pub opener: Arc<dyn Opener>,
    /// The login item; `None` for a fake that registers nothing.
    pub login_item: Option<Arc<dyn LoginItem>>,
    /// The updater the update schedule drives; `None` for a fake that never checks.
    pub update_source: Option<Arc<dyn UpdateSource>>,
    /// What the system says of each permission; `None` grants everything.
    pub permissions: Option<Arc<dyn Permissions>>,
    /// The calendar a recording is named from; `None` reads none.
    pub calendar: Option<Arc<dyn Calendar>>,
    pub runtime: tokio::runtime::Handle,
    pub version: String,
    pub make_capture_session: MakeCaptureSession,
    /// Meeting detection with the shell's prompt panel; `None` runs none.
    pub detection: Option<DetectionOptions>,
    /// How long [`build`] waits for the database's lock: [`LOCK_PATIENCE`]
    /// in the product, whose update relaunch starts the new process before
    /// the old one has exited.
    pub lock_patience: std::time::Duration,
    /// The shell runs from an installed app bundle.
    pub installed_bundle: bool,
    /// Where Pair again writes the Swift import's `swift-import-done` marker
    /// before it mints a new handover identity; `None` outside the shell.
    pub import_marker: Option<Arc<dyn ImportMarker>>,
}

/// The product's [`AppOptions::lock_patience`].
pub const LOCK_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

impl AppOptions {
    /// The product under the default support directory, with live capture.
    pub fn product(
        runtime: tokio::runtime::Handle,
        opener: Arc<dyn Opener>,
        version: &str,
    ) -> std::io::Result<Self> {
        Ok(AppOptions {
            paths: StenoPaths::create_default()?,
            database_path: None,
            keyring: true,
            opener,
            login_item: None,
            update_source: None,
            permissions: None,
            calendar: None,
            runtime,
            version: version.to_owned(),
            make_capture_session: Arc::new(|configuration| {
                CaptureSession::new(configuration).map_err(|error| error.to_string())
            }),
            detection: None,
            lock_patience: LOCK_PATIENCE,
            installed_bundle: false,
            import_marker: None,
        })
    }
}

/// The running graph.
pub struct App {
    pub paths: StenoPaths,
    pub store: Arc<Store>,
    pub secrets: Arc<dyn SecretStore>,
    pub events: MeetingEventBus,
    pub pipeline: Arc<CurrentPipeline>,
    pub sweep: RetentionSweep,
    pub services: Services,
    /// The update schedule, when the shell passed an update source.
    pub updates: Option<Arc<UpdateSchedule>>,
    pub handover: Option<Arc<ListenerHandover>>,
    /// The handover while the Swift import is pending: its listener comes
    /// once the import's gate opens.
    pub gated_handover: Option<Arc<GatedHandover>>,
    /// The Swift import's gate while it is pending: [`App::launch`] holds
    /// the resume, the re-exports, the sweep and the recovery until it opens.
    pub import_gate: Option<Arc<ImportGate>>,
    pub recorder: Arc<CaptureRecorder>,
    pub detection: Option<Arc<DetectionController>>,
    pub models_directory: std::path::PathBuf,
    pub zone: FixedOffset,
    pub runtime: tokio::runtime::Handle,
    pub version: String,
    pub installed_bundle: bool,
    /// What went wrong while building, for the shell's log.
    pub startup_warnings: Vec<String>,
    pub live_recording_check: LiveRecordingCheck,
    /// Where the Mac's system audio probe writes its tone, for
    /// [`App::launch`] to remove the files a crashed probe left; none off
    /// the Mac.
    pub tone_directory: Option<std::path::PathBuf>,
    pub(crate) launch_work: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Resolves once the secret store chose where the secrets are, true when
    /// it turned a read away while the keyring asked the user, so
    /// [`App::launch`] reads the API key and the identity again.
    pub(crate) secrets_unlocked: std::sync::Mutex<Option<SecretsUnlocked>>,
    /// Held while the graph lives, so no second process takes the database;
    /// `None` on a filesystem without locks. Declared last so it outlives
    /// what it guards.
    pub database_lock: Option<DatabaseLock>,
}

/// The viewer's zone, fixed at start.
#[must_use]
pub fn local_zone() -> FixedOffset {
    Local::now().offset().fix()
}

/// Opens (and migrates) the database at `path`, creating its folder.
pub fn open_store(path: &std::path::Path) -> Result<Arc<Store>, BuildError> {
    create_database_folder(path)?;
    Ok(Arc::new(Store::open(path)?))
}

/// Takes the lock of the database at `path`, creating its folder, and
/// waits up to `patience` while another process holds it.
pub fn lock_database(
    path: &std::path::Path,
    patience: std::time::Duration,
) -> Result<DatabaseLock, BuildError> {
    create_database_folder(path)?;
    Ok(DatabaseLock::acquire_within(path, patience)?)
}

fn create_database_folder(path: &std::path::Path) -> Result<(), BuildError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(BuildError::DatabaseFolder)?;
    }
    Ok(())
}

/// The LLM API key, or `None` with the reason when the secret store could
/// not be read: the app runs without summaries rather than not at all.
fn api_key(
    secrets: &dyn SecretStore,
    runtime: &tokio::runtime::Handle,
) -> Result<Option<String>, String> {
    block_on(runtime, secrets.secret(&SecretKey::llm_api_key()))
        .map_err(|error| format!("Could not read the LLM API key from the secret store: {error}"))
}

/// The graph's secret store, in three layers from the inside out: the
/// platform store; [`KeepsApiKey`], which keeps the API key last read or
/// written for a rebuild whose read fails; and, outermost, the Swift
/// import's gate while one stands. So a key the gate withholds is never
/// read from the keychain, and an empty key write the gate swallows never
/// reaches `KeepsApiKey`. Built only by `gated_secrets`.
#[derive(Clone)]
pub struct GraphSecrets {
    pub secrets: Arc<dyn SecretStore>,
    pub kept: Arc<KeepsApiKey>,
    /// The gate, which says whether the key is withheld
    /// ([`ImportGate::withholds_summaries`]).
    pub key_gate: Option<Arc<ImportGate>>,
}

/// The dependencies of one pipeline from the stored settings, the API key
/// and `engines`, shared by the first build and every reload. An
/// unreadable secret store is logged and the passes use the key
/// [`KeepsApiKey`] kept, or none. While the Swift import's gate withholds
/// the key for the stored endpoint there are no passes: the meeting
/// completes with its raw transcript instead of failing, and its summary
/// can run again once a key is saved.
pub fn pipeline_dependencies(
    store: &Arc<Store>,
    engines: &SpeechEngines,
    secrets: &GraphSecrets,
    codex: &Arc<CodexCredentialStore>,
    events: &MeetingEventBus,
    runtime: &tokio::runtime::Handle,
) -> Result<BuiltPipeline, BuildError> {
    let settings = store.settings()?;
    let api_key = api_key(secrets.secrets.as_ref(), runtime).unwrap_or_else(|warning| {
        let kept = secrets.kept.kept_api_key();
        if kept.is_some() {
            tracing::warn!("{warning}; the pipeline keeps the key it last had");
        } else {
            tracing::warn!("{warning}");
        }
        kept
    });
    let zone = steno_adapters::runtime::local_time_zone();
    let withheld = secrets
        .key_gate
        .as_deref()
        .is_some_and(|gate| gate.withholds_summaries(&settings));
    let passes = if withheld {
        None
    } else {
        crate::llm::passes(&settings, api_key.as_deref(), codex, zone)
    };
    let speech = engines.setup();
    let engine_runtime = speech.runtime(&settings.speech_engine_id);
    let speech_engine = engines.engine(engine_runtime);
    let dependencies = PipelineDependencies::new(
        Arc::new(SymphoniaAudioCodec::new()),
        speech_engine.engine().clone(),
        engines.diarizer(),
        Arc::new(steno_pipeline::StoreSpeakerMemory::new(store.clone())),
        Arc::new(DeliveryCoordinator::new(store.clone())),
        store.clone(),
        events.clone(),
    )
    .with_speech_engine(speech_engine)
    .with_speech_in_process(engine_runtime.runs_in_process());
    let dependencies = match passes {
        Some(passes) => dependencies.with_llm(Some(passes.cleaner), Some(passes.summarizer)),
        None => dependencies,
    };
    Ok(BuiltPipeline {
        dependencies,
        engine: BuiltEngine {
            runtime: engine_runtime,
            engine_id: settings.speech_engine_id,
        },
    })
}

/// [`pipeline_dependencies`] over clones of its inputs, for the reloads.
fn make_dependencies(
    store: &Arc<Store>,
    engines: &Arc<SpeechEngines>,
    secrets: &GraphSecrets,
    codex: &Arc<CodexCredentialStore>,
    events: &MeetingEventBus,
    runtime: &tokio::runtime::Handle,
) -> MakeDependencies {
    let (store, engines, secrets, codex, events, runtime) = (
        store.clone(),
        engines.clone(),
        secrets.clone(),
        codex.clone(),
        events.clone(),
        runtime.clone(),
    );
    Arc::new(move || pipeline_dependencies(&store, &engines, &secrets, &codex, &events, &runtime))
}

/// The phone intake over whichever pipeline is current when a recording
/// arrives, so a reload is not bypassed. Each copy's folder is noted
/// under `support_directory` until its admission commits, so the launch
/// adopts a copy left with no meeting.
pub fn handover_intake(
    store: Arc<Store>,
    pipeline: Arc<CurrentPipeline>,
    zone: FixedOffset,
    support_directory: &std::path::Path,
) -> RecordingIntake {
    let now = pipeline.current().dependencies().now.clone();
    RecordingIntake::new(
        store,
        Arc::new(move |meeting, asset| {
            let pipeline = pipeline.current();
            Box::pin(async move { pipeline.enqueue_saved(&meeting, &asset) })
        }),
        now,
        zone,
    )
    .noting_folders_in(Arc::new(crate::audio_folders::AdmissionRecord {
        support_directory: support_directory.to_path_buf(),
    }))
}

/// Where the handover reads its identity and mints one.
fn identity_source(
    store: &Arc<Store>,
    pipeline: &Arc<CurrentPipeline>,
    secrets: &Arc<dyn SecretStore>,
    paths: &StenoPaths,
    zone: FixedOffset,
    runtime: &tokio::runtime::Handle,
    marker: Option<Arc<dyn ImportMarker>>,
) -> Arc<IdentitySource> {
    let (listen_store, listen_pipeline, listen_paths) =
        (store.clone(), pipeline.clone(), paths.clone());
    Arc::new(IdentitySource {
        store: store.clone(),
        secrets: secrets.clone(),
        record: Arc::new(crate::handover::FingerprintFile::in_support_directory(
            &paths.support_directory,
        )),
        marker,
        common_name: format!(
            "Steno on {}",
            steno_handover::HandoverConfiguration::default_service_name()
        ),
        listen: Box::new(move |identity| {
            let intake = Arc::new(handover_intake(
                listen_store.clone(),
                listen_pipeline.clone(),
                zone,
                &listen_paths.support_directory,
            ));
            Arc::new(crate::handover::service(
                listener_configuration(&listen_paths, &listen_store),
                listen_store.clone(),
                intake,
                identity,
            ))
        }),
        runtime: runtime.clone(),
    })
}

/// The handover over the listener on the identity `source` loads or
/// mints, or one that waits with the reason when the identity cannot be
/// read or is not the one the paired phones pinned: it mints nothing
/// then. `None` when the launch checkpoint failed: the handover stays off
/// until the next launch.
fn handover_listener(source: &Arc<IdentitySource>) -> Result<Arc<ListenerHandover>, String> {
    HandoverService::checkpoint_store(&source.store).map_err(|error| error.to_string())?;
    Ok(Arc::new(ListenerHandover::read(source.clone())))
}

/// [`lock_database`], or `None` with a startup warning without locks.
fn lock_or_run_without(
    database_path: &std::path::Path,
    patience: std::time::Duration,
    warnings: &mut Vec<String>,
) -> Result<Option<DatabaseLock>, BuildError> {
    match lock_database(database_path, patience) {
        Ok(lock) => Ok(Some(lock)),
        Err(BuildError::Lock(error @ DatabaseLockError::Unsupported { .. })) => {
            warnings.push(format!(
                "Running without the database lock, so a second Steno on this database is not kept out: {error}"
            ));
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// The listener's configuration, with the inbox under the support
/// directory; in tests loopback only, unadvertised, on any port.
fn listener_configuration(
    paths: &StenoPaths,
    store: &Store,
) -> steno_handover::HandoverConfiguration {
    let inbox_directory = paths
        .support_directory
        .join(steno_handover::HandoverConfiguration::INBOX_DIRECTORY_NAME);
    if cfg!(test) {
        return steno_handover::HandoverConfiguration {
            advertise: false,
            inbox_directory,
            ..steno_handover::HandoverConfiguration::default()
        };
    }
    let environment = std::env::var_os(crate::handover::PORT_VARIABLE)
        .map(|value| value.to_string_lossy().into_owned());
    steno_handover::HandoverConfiguration {
        inbox_directory,
        ..crate::handover::configuration(store, environment.as_deref())
    }
}

/// The graph's [`GraphSecrets`] over `platform`: [`KeepsApiKey`], then
/// the import's gate while an import is pending, else the gate of a
/// refused keychain read, if any. Over the secrets file (`file`) the key
/// stays readable, so `STENO_LLM_API_KEY` still works.
pub(crate) fn gated_secrets(
    pending: Option<crate::swift_import::PendingImport>,
    preferences: &Arc<FilePreferences>,
    platform: Arc<dyn SecretStore>,
    paths: &StenoPaths,
    file: bool,
) -> (Option<GraphImport>, GraphSecrets) {
    let kept = Arc::new(KeepsApiKey::new(platform));
    let (import, secrets, key_gate) = match pending {
        Some(pending) => {
            let record =
                crate::handover::FingerprintFile::in_support_directory(&paths.support_directory);
            let import = GraphImport::new(pending, kept.clone(), Arc::new(record));
            let (gated, gate) = (import.secrets.clone(), import.gate.clone());
            (Some(import), gated, Some(gate))
        }
        None if file => (None, kept.clone() as Arc<dyn SecretStore>, None),
        None => {
            let (gated, gate) = crate::swift_import::key_denied_secrets(preferences, kept.clone());
            (None, gated, gate)
        }
    };
    (
        import,
        GraphSecrets {
            secrets,
            kept,
            key_gate,
        },
    )
}

/// The handover while an import is pending: its listener comes once the
/// import's gate opens; Try again runs `step` again and Pair again ends
/// the import with an identity `source` mints.
fn gated_handover(
    import: &GraphImport,
    step: &Arc<ImportStep>,
    source: &Arc<IdentitySource>,
) -> Arc<GatedHandover> {
    let (make_source, mint_source) = (source.clone(), source.clone());
    Arc::new(GatedHandover::new(
        source.store.clone(),
        import.gate.subscribe(),
        Box::new(move || handover_listener(&make_source)),
        step.clone(),
        Box::new(move || mint_source.mint_anew().map(drop)),
    ))
}

fn host_handover(
    gated: Option<&Arc<GatedHandover>>,
    listener: Option<&Arc<ListenerHandover>>,
) -> Option<Arc<dyn steno_host::services::Handover>> {
    if let Some(gated) = gated {
        return Some(gated.clone());
    }
    listener.map(|listener| listener.clone() as Arc<dyn steno_host::services::Handover>)
}

/// The import's onboarding step, which reloads the pipeline whenever the
/// key gate opens.
fn import_step(
    import: &GraphImport,
    pipeline: &Arc<CurrentPipeline>,
    store: &Arc<Store>,
    runtime: &tokio::runtime::Handle,
) -> Arc<ImportStep> {
    let pipeline = pipeline.clone();
    let choice = crate::swift_import::CertificateChoice {
        store: store.clone(),
        now: chrono::Utc::now,
    };
    Arc::new(import.step(
        runtime.clone(),
        choice,
        Box::new(move || {
            if let Err(error) = pipeline.reload() {
                tracing::warn!(%error, "the pipeline did not reload with the imported key");
            }
        }),
    ))
}

/// Turns a stored engine id other than Parakeet v3 into Parakeet v3,
/// before anything reads the engine, with the notice the main window
/// shows once; the notice's flag is written to `preferences` before the
/// database changes. A failed update is a warning.
fn retire_the_engine(store: &Store, preferences: &FilePreferences, warnings: &mut Vec<String>) {
    let retired = store.retire_speech_engine(|| {
        preferences.set_flag(steno_host::setup::engine_notice::PENDING_KEY, true);
    });
    if let Err(error) = retired {
        warnings.push(format!("Steno could not switch to Parakeet v3: {error}"));
    }
}

/// Builds the graph. First the database's lock: while another process
/// holds it past [`AppOptions::lock_patience`] the build fails with
/// [`BuildError::Lock`] before the database is opened. Fakes stand in
/// where the shell passes no platform side: the permissions (all
/// granted) and the updater.
pub fn build(options: AppOptions) -> Result<App, BuildError> {
    build_with_import(options, |_| None)
}

/// [`build`] with the Swift import's launch half: `import` runs once the
/// database's lock is held and before anything reads a preference or a
/// secret. With a pending import the graph reads its secrets through the
/// import's gate, so no API key is read and no handover listener is built
/// until the onboarding step ran.
#[allow(clippy::too_many_lines)]
pub fn build_with_import(
    options: AppOptions,
    import: impl FnOnce(Arc<FilePreferences>) -> Option<crate::swift_import::PendingImport>,
) -> Result<App, BuildError> {
    let mut warnings = Vec::new();
    let paths = options.paths;
    let database_path = options
        .database_path
        .unwrap_or_else(|| paths.database_path());
    let database_lock = lock_or_run_without(&database_path, options.lock_patience, &mut warnings)?;
    let preferences = Arc::new(FilePreferences::in_support_directory(
        &paths.support_directory,
    ));
    let pending = import(preferences.clone());
    let store = open_store(&database_path)?;
    let (platform, secrets_unlocked) = secret_store_with_unlock(options.keyring, &paths, &store);
    let (import, graph_secrets) =
        gated_secrets(pending, &preferences, platform, &paths, !options.keyring);
    let secrets = graph_secrets.secrets.clone();
    let codex = codex_store();
    let events = MeetingEventBus::new();
    let runtime = options.runtime;
    let zone = local_zone();

    retire_the_engine(&store, &preferences, &mut warnings);
    // Read once, here: the pipeline (and every reload) and the model
    // service share them.
    let settings = store.settings().map_err(|error| match error {
        StoreError::UndecodableSetting { .. } | StoreError::UndecodableSettings => {
            BuildError::Setting {
                database: database_path.clone(),
                source: error,
            }
        }
        other => BuildError::Store(other),
    })?;
    let engines = Arc::new(SpeechEngines::new(SpeechSetup::new(&settings, &paths)));
    let speech = engines.setup();
    let withheld_api_key = graph_secrets
        .key_gate
        .clone()
        .map(|gate| gate as Arc<dyn steno_host::services::WithheldApiKey>);
    let make = make_dependencies(&store, &engines, &graph_secrets, &codex, &events, &runtime);
    let pipeline = Arc::new(
        CurrentPipeline::new(make()?, make, runtime.clone()).resuming_when({
            let engines = engines.clone();
            Arc::new(move |on| engines.models_installed(on))
        }),
    );
    let sweep = RetentionSweep::new(store.clone());

    let system_permissions = options
        .permissions
        .map(|system| Arc::new(SystemPermissions::new(system, preferences.clone())));
    let permissions: Arc<dyn Permissions> = match &system_permissions {
        Some(system) => system.clone(),
        None => Arc::new(FakePermissions::all_granted()),
    };
    // Settings and onboarding install the models, so an install resumes
    // the meetings waiting for it.
    let speech_models = Arc::new(ResumingSpeechModels::resuming(
        ModelStoreSpeechModels::new(speech),
        pipeline.clone(),
    ));
    let recorder = CaptureRecorder::new(
        store.clone(),
        pipeline.clone(),
        options.make_capture_session,
        permissions.clone(),
        speech_models.clone(),
        zone,
        runtime.clone(),
        paths.support_directory.clone(),
    );
    // The save writes to the database's volume too.
    recorder.watch_disk_with(DiskWatch::system(database_path.parent()));
    if let Some(system_permissions) = &system_permissions {
        system_permissions.keep_out_of_recordings(&recorder);
    }
    if let Some(calendar) = options.calendar {
        recorder.name_meetings_from(calendar);
    }
    let detection = options
        .detection
        .map(|detection| detection.controller(recorder.clone(), store.clone()));

    let source = identity_source(
        &store,
        &pipeline,
        &secrets,
        &paths,
        zone,
        &runtime,
        options.import_marker,
    );
    let step = import
        .as_ref()
        .map(|import| import_step(import, &pipeline, &store, &runtime));
    let gated_handover = import
        .as_ref()
        .zip(step.as_ref())
        .map(|(import, step)| gated_handover(import, step, &source));
    let handover = if gated_handover.is_some() {
        None
    } else {
        handover_listener(&source)
            .inspect_err(|error| warnings.push(format!("Phone handover is unavailable: {error}")))
            .ok()
    };
    let swift_import = step.map(|step| step as Arc<dyn steno_host::services::SwiftImport>);

    let clock = Arc::new(WallClock);
    let preferences: Arc<dyn Preferences> = preferences;
    let updates = options.update_source.map(|source| {
        UpdateSchedule::new(ScheduleParts {
            source,
            preferences: preferences.clone(),
            clock: clock.clone(),
            // No update installs over a recording or a processing job.
            gate: Arc::new(IdleGate::new(recorder.clone(), pipeline.clone())),
            recorder: recorder.clone(),
            support_directory: paths.support_directory.clone(),
            managed: crate::updates::updates_are_managed(),
            runtime: runtime.clone(),
        })
    });

    let services = Services {
        clock,
        login_item: options
            .login_item
            .unwrap_or_else(|| Arc::new(FakeLoginItem::new(LoginItemStatus::NotRegistered))),
        permissions,
        updater: match &updates {
            Some(updates) => updates.clone(),
            None => Arc::new(FakeUpdater::default()),
        },
        recorder: recorder.clone(),
        pipeline: Arc::new(HostPipeline {
            pipeline: pipeline.clone(),
            sweep: sweep.clone(),
        }),
        speech_models,
        llm: Arc::new(ClientLlmService { codex }),
        export_validator: Arc::new(crate::export::ObsidianExportValidator),
        handover: host_handover(gated_handover.as_ref(), handover.as_ref()),
        qr: Arc::new(PngQrEncoder),
        audio_devices: Arc::new(PlatformAudioDevices),
        folder_usage: Arc::new(DiskFolderUsage),
        file_system: Arc::new(steno_host::services::RealFileSystem),
        clip_player: Arc::new(crate::clip_player::ProcessClipPlayer::new()),
        opener: options.opener,
        preferences,
        secrets: secrets.clone(),
        swift_import,
        withheld_api_key,
    };

    Ok(App {
        paths,
        store,
        secrets,
        events,
        pipeline,
        sweep,
        services,
        updates,
        handover,
        gated_handover,
        import_gate: import.map(|import| import.gate),
        recorder,
        detection,
        models_directory: speech.models_directory.clone(),
        zone,
        runtime,
        version: options.version,
        installed_bundle: options.installed_bundle,
        startup_warnings: warnings,
        live_recording_check: LiveRecordingCheck::default(),
        tone_directory: cfg!(target_os = "macos")
            .then(steno_audio::capture::permission::tone_directory),
        launch_work: std::sync::Mutex::default(),
        secrets_unlocked: std::sync::Mutex::new(secrets_unlocked),
        database_lock,
    })
}

/// The launch's last step on the meetings a previous process left
/// unfinished: the interrupted recordings are recovered, left alone or
/// failed ([`reconcile_interrupted`]), then the recordings with no meeting
/// are adopted ([`adopt_orphans`]); their ids are returned. Last, so a
/// slow folder delays nothing else. A panic is caught and logged.
/// Blocks.
pub(crate) fn reconcile_at_launch(
    store: &Arc<Store>,
    pipeline: &CurrentPipeline,
    interrupted: &Interrupted,
    check: &LiveRecordingCheck,
    zone: FixedOffset,
    runtime: &tokio::runtime::Handle,
) -> Vec<uuid::Uuid> {
    let current = pipeline.current();
    let intake = steno_pipeline::LocalRecordingIntake::over(store.clone(), current.clone(), zone);
    let reconciled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reconcile_interrupted(store, &intake, interrupted, check, runtime)
    }));
    if reconciled.is_err() {
        tracing::warn!(
            "the recovery of interrupted recordings panicked; the next launch tries again"
        );
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        adopt_orphans(store, &current, interrupted, check, zone, runtime)
    }))
    .unwrap_or_else(|_| {
        tracing::warn!(
            "the recovery of recordings with no meeting panicked; the next launch tries again"
        );
        Vec::new()
    })
}

/// How long an exit waits for [`App::shutdown`] before the process ends
/// anyway.
pub const SHUTDOWN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// The shell's exits around [`App::shutdown`], which runs once, for at
/// most `patience`: an exit request the gate can hold goes through
/// [`ExitGate::exit_requested`], one it cannot (a logout, an update's
/// relaunch, a system shutdown) through [`ExitGate::exiting`]. Clones
/// share one gate.
///
/// ```
/// use std::time::Duration;
///
/// use steno_services::app::ExitGate;
///
/// let gate = ExitGate::default();
/// let (exited, exit) = std::sync::mpsc::channel();
/// // The first request is held: the shutdown runs, then the exit.
/// let held = !gate.exit_requested(
///     Duration::from_secs(10),
///     || { /* stop and save the recording */ },
///     move || exited.send(()).unwrap(),
/// );
/// assert!(held);
/// exit.recv_timeout(Duration::from_secs(10)).unwrap();
/// // Every request after it goes ahead.
/// assert!(gate.exit_requested(
///     Duration::from_secs(10),
///     || unreachable!("the shutdown runs once"),
///     || unreachable!("the gate's exit runs once"),
/// ));
/// ```
#[derive(Debug, Clone, Default)]
pub struct ExitGate {
    shared: Arc<(std::sync::Mutex<ExitStage>, std::sync::Condvar)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ExitStage {
    /// No exit yet.
    #[default]
    Open,
    /// The shutdown runs for a held request, whose exit the gate raises
    /// once it ends.
    ShuttingDown,
    /// The shutdown runs, and the process ends without the gate's exit
    /// ([`ExitGate::exiting`]).
    Ending,
    /// The shutdown ended or ran out of patience: exits go ahead.
    Released,
}

impl ExitGate {
    /// Whether this exit request may go ahead now. The first returns false,
    /// runs `shutdown` on its own thread for at most `patience`, then calls
    /// `exit`, which should raise the request again; that and every later
    /// one return true. Requests while the shutdown runs are held.
    #[must_use]
    pub fn exit_requested(
        &self,
        patience: std::time::Duration,
        shutdown: impl FnOnce() + Send + 'static,
        exit: impl FnOnce() + Send + 'static,
    ) -> bool {
        match self.advance(ExitStage::ShuttingDown) {
            ExitStage::Released => return true,
            ExitStage::ShuttingDown | ExitStage::Ending => return false,
            ExitStage::Open => {}
        }
        let gate = self.clone();
        std::thread::spawn(move || {
            run_for_at_most(patience, shutdown);
            // `exiting` took over: the process ends without the gate's exit.
            if gate.release() == ExitStage::ShuttingDown {
                exit();
            }
        });
        false
    }

    /// The process is about to end without a held request: runs `shutdown`,
    /// or waits for the one already running, and returns once it ended, at
    /// most `patience` later.
    pub fn exiting(&self, patience: std::time::Duration, shutdown: impl FnOnce() + Send + 'static) {
        match self.advance(ExitStage::Ending) {
            ExitStage::Open => {
                run_for_at_most(patience, shutdown);
                self.release();
            }
            ExitStage::ShuttingDown | ExitStage::Ending => {
                let (stage, released) = &*self.shared;
                let _ = released
                    .wait_timeout_while(lock_stage(stage), patience, |stage| {
                        *stage != ExitStage::Released
                    })
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            ExitStage::Released => {}
        }
    }

    /// Whether the shutdown has run or ran out of patience.
    #[must_use]
    pub fn released(&self) -> bool {
        *lock_stage(&self.shared.0) == ExitStage::Released
    }

    /// Moves an open gate to `next`; returns the stage it found.
    fn advance(&self, next: ExitStage) -> ExitStage {
        let mut stage = lock_stage(&self.shared.0);
        let found = *stage;
        if found == ExitStage::Open
            || (found == ExitStage::ShuttingDown && next == ExitStage::Ending)
        {
            *stage = next;
        }
        found
    }

    /// Releases the gate; the stage it left.
    fn release(&self) -> ExitStage {
        let (stage, released) = &*self.shared;
        let left = std::mem::replace(&mut *lock_stage(stage), ExitStage::Released);
        released.notify_all();
        left
    }
}

fn lock_stage(stage: &std::sync::Mutex<ExitStage>) -> std::sync::MutexGuard<'_, ExitStage> {
    stage
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn run_for_at_most(patience: std::time::Duration, shutdown: impl FnOnce() + Send + 'static) {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        shutdown();
        let _ = done.send(());
    });
    if finished.recv_timeout(patience).is_err() {
        tracing::warn!(
            seconds = patience.as_secs(),
            "exiting before the shutdown finished; a recording in progress may not be saved"
        );
    }
}

/// Logs an `OperationFailed`: warn names the meeting, the operation and
/// the stage; the failure text, which can quote the server, goes to debug.
fn log_operation_failure(event: &MeetingEvent) {
    if let MeetingEvent::OperationFailed {
        meeting_id,
        operation,
        stage,
        failure,
    } = event
    {
        tracing::warn!(
            %meeting_id,
            operation = operation.label(),
            stage = stage.as_str(),
            "a background operation failed"
        );
        tracing::debug!(%meeting_id, %failure, "background operation failure");
    }
}

/// How often the reload owner's pass runs when nothing asked for one.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// The event follower's handling of one bus event, on the blocking pool:
/// the host applies it, then asks the reload owner for a pass where the
/// event changed rows or the pipeline's flags. `None` is a follower that
/// fell behind and missed events: it asks for a reload too.
fn follow_event(host: &Host, sweep: &RetentionSweep, event: Option<&MeetingEvent>) {
    let Some(event) = event else {
        host.request_reload();
        return;
    };
    host.apply_meeting_event(event);
    match event {
        MeetingEvent::Progress { .. } | MeetingEvent::WaitingForRecording { .. } => {}
        MeetingEvent::RetentionApplied { .. } => {
            run_sweep(sweep);
            host.request_reload();
        }
        MeetingEvent::SpeakersNeedReview { .. }
        | MeetingEvent::Deleted { .. }
        | MeetingEvent::ModelsMissing { .. }
        | MeetingEvent::StoreBusy { .. } => host.request_reload(),
        MeetingEvent::OperationFailed { .. } => {
            log_operation_failure(event);
            host.request_reload();
        }
    }
}

/// The reload owner: one [`Host::reload_pass`] on each tick of `every`
/// and on each request ([`Host::request_reload`]), on the blocking pool.
/// A late tick is followed by the next a full interval later, never a
/// burst; requests during a pass fold into one more pass. The tick finds
/// stage writes and other processes' commits, which post no events.
pub(crate) async fn own_the_reloads(host: Arc<Host>, every: std::time::Duration) {
    let mut interval = tokio::time::interval(every);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            () = host.reload_requested() => {}
        }
        let pass = host.clone();
        if !off_the_workers(move || pass.reload_pass()).await {
            return;
        }
    }
}

/// Runs `body` on the blocking pool and waits for it there, so a wait on
/// the store holds no runtime worker. A panic resumes here. False when
/// the runtime is shutting down and did not run it.
async fn off_the_workers(body: impl FnOnce() + Send + 'static) -> bool {
    match tokio::task::spawn_blocking(body).await {
        Ok(()) => true,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_) => false,
    }
}

/// The update schedule's change hook: the host's `updates_changed` on the
/// blocking pool, since the host's lock may be held through a busy wait.
fn updates_hook(host: &Arc<Host>, runtime: &tokio::runtime::Handle) -> Arc<dyn Fn() + Send + Sync> {
    let (host, runtime) = (host.clone(), runtime.clone());
    Arc::new(move || {
        let host = host.clone();
        notify_on_the_blocking_pool(&runtime, "updates_changed", move || host.updates_changed());
    })
}

fn phones_hook(
    host: &Arc<Host>,
    runtime: &tokio::runtime::Handle,
) -> impl FnOnce() + Send + 'static {
    let (host, runtime) = (host.clone(), runtime.clone());
    move || notify_on_the_blocking_pool(&runtime, "phones_changed", move || host.phones_changed())
}

/// Runs `notify`, a host notification nothing waits for, on the blocking
/// pool; a panic is logged at `error` under `name`, without its message.
fn notify_on_the_blocking_pool(
    runtime: &tokio::runtime::Handle,
    notification: &'static str,
    notify: impl FnOnce() + Send + 'static,
) {
    runtime.spawn_blocking(move || {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(notify)).is_err() {
            tracing::error!(notification, "a host notification panicked");
        }
    });
}

impl App {
    /// The host over this graph.
    pub fn host(&self) -> Result<Host, steno_host::host::HostError> {
        Host::new(
            self.store.clone(),
            self.services.clone(),
            HostConfig {
                version: self.version.clone(),
                zone: self.zone,
                platform: steno_bridge::Platform::CURRENT,
                updates_managed: crate::updates::updates_are_managed(),
                installed_bundle: self.installed_bundle,
            },
        )
    }

    /// What every exit does first ([`ExitGate`]), in order: the pipelines
    /// quit, so a job the exit ends stays `processing` for the next launch;
    /// a start or stop in progress settles; a recording is stopped with
    /// `quit` and saved; the handover listener stops; detection stops. A
    /// phone admission in flight may still finish or be cut as by a crash:
    /// the next launch adopts the copy.
    pub fn shutdown(&self) {
        self.pipeline.quit();
        self.recorder.stop_for_quit();
        if let Some(handover) = self.handover.as_deref().and_then(ListenerHandover::close) {
            block_on(&self.runtime, handover.stop());
        }
        if let Some(handover) = self.gated_handover.as_ref().and_then(|gated| gated.close()) {
            block_on(&self.runtime, handover.stop());
        }
        // Last: the quit save must not wait for a detector start.
        if let Some(detection) = &self.detection {
            detection.stop();
        }
    }

    /// Everything that happens once at launch, in order:
    ///
    /// 1. The pipeline's and the recorder's events are routed into the host,
    ///    the reload owner starts, and meeting detection starts.
    /// 2. The meetings left `recording` and the known audio folders are
    ///    listed, before anything here can start a recording.
    /// 3. Meetings left queued or processing are processed again, unfinished
    ///    exports are re-exported, and the retention sweep runs.
    /// 4. On a blocking task (`reconcile_at_launch`): those recordings are
    ///    recovered, left alone or failed, and a master with no meeting is
    ///    adopted.
    /// 5. Meanwhile the login item is registered the first time, the
    ///    handover listener starts when a phone is paired, and the update
    ///    schedule starts.
    ///
    /// Steps 3 and 4 wait for the keyring's answer where the secret store can
    /// ask, and for the Swift import's step while it is pending, so nothing
    /// runs on a pipeline built without the key. Keyring prompts may stay up
    /// for two minutes, so a meeting can show as processing that long.
    /// [`App::launch_finished`] waits for that task.
    ///
    /// When the keyring answers after a failed read, the pipeline is rebuilt
    /// and the API key and the handover identity are read again.
    pub fn launch(&self, host: &Arc<Host>) {
        let recorder_host = host.clone();
        let detection = self.detection.as_ref().map(Arc::downgrade);
        self.recorder.on_change(Arc::new(move || {
            recorder_host.recorder_changed();
            if let Some(detection) = detection.as_ref().and_then(std::sync::Weak::upgrade) {
                detection.recorder_changed();
            }
        }));
        if let Some(detection) = &self.detection {
            detection.run();
        }
        self.report_update_checks(host);

        // Reloads and the sweep may wait behind another process's write lock,
        // so both tasks run on the blocking pool, one event at a time in the
        // bus's order, and no wait holds a runtime worker.
        let mut receiver = self.events.subscribe();
        let sweep = self.sweep.clone();
        let event_host = host.clone();
        tokio::spawn(async move {
            loop {
                let event = match receiver.recv().await {
                    Ok(event) => Some(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                let (host, sweep) = (event_host.clone(), sweep.clone());
                if !off_the_workers(move || follow_event(&host, &sweep, event.as_ref())).await {
                    return;
                }
            }
        });
        tokio::spawn(own_the_reloads(host.clone(), POLL_INTERVAL));

        let reconcile = self.reconcile_listed(host);
        let work = self.recover_at_launch(host, reconcile);
        *self
            .launch_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(work);
        self.start_alongside(host);
        if let Some(directory) = self.tone_directory.clone() {
            tokio::task::spawn_blocking(move || {
                steno_audio::capture::permission::remove_leftover_tones(
                    &directory,
                    std::time::SystemTime::now(),
                );
            });
        }
        host.store_changed();
    }

    /// The host hears of every update check from here on.
    fn report_update_checks(&self, host: &Arc<Host>) {
        if let Some(updates) = &self.updates {
            updates.on_change(updates_hook(host, &self.runtime));
        }
    }

    /// The launch's step 5, without the update schedule.
    fn start_alongside(&self, host: &Arc<Host>) {
        host.register_login_item_on_first_launch();
        if let Some(handover) = self
            .handover
            .as_deref()
            .and_then(ListenerHandover::listener)
            .cloned()
        {
            tokio::spawn(async move { start_if_paired(&handover).await });
        }
        if let Some(gated) = &self.gated_handover {
            tokio::spawn(gated.clone().follow(phones_hook(host, &self.runtime)));
        }
    }

    /// Steps 3 and 4 of [`App::launch`]: at once, or on a task of their own
    /// once the keyring answered and the import's step ran.
    fn recover_at_launch(
        &self,
        host: &Arc<Host>,
        reconcile: impl FnOnce() + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let unlocked = self
            .secrets_unlocked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let held = self
            .import_gate
            .clone()
            .filter(|gate| gate.handover() == HandoverGate::Pending);
        let recover = self.recover_unfinished();
        let updates = self.updates.clone();
        if unlocked.is_none() && held.is_none() {
            recover();
            run_sweep(&self.sweep);
            if let Some(updates) = updates {
                updates.start();
            }
            tokio::task::spawn_blocking(reconcile)
        } else {
            // The pipeline built while the keyring asked or the import was pending
            // has no API key, so the recovery waits. Behind the import the sweep
            // waits too, so it runs after the resume.
            let sweep_now = held.is_none();
            if sweep_now {
                run_sweep(&self.sweep);
            }
            let (reread, sweep) = (self.reread_after_unlock(host), self.sweep.clone());
            tokio::spawn(async move {
                let read_again = match unlocked {
                    Some(unlocked) => unlocked.await,
                    None => false,
                };
                if let Some(updates) = updates {
                    updates.start();
                }
                if let Some(gate) = held {
                    gate.step_over().await;
                }
                let finished = tokio::task::spawn_blocking(move || {
                    if read_again {
                        reread();
                    }
                    recover();
                    if !sweep_now {
                        run_sweep(&sweep);
                    }
                    reconcile();
                })
                .await;
                if finished.is_err() {
                    tracing::warn!("the launch's held work did not finish");
                }
            })
        }
    }

    /// The launch's crash recovery: queued or processing meetings processed
    /// again, then unfinished exports re-exported.
    fn recover_unfinished(&self) -> impl FnOnce() + Send + 'static {
        let pipeline = self.pipeline.clone();
        move || {
            let pipeline = pipeline.current();
            match pipeline.resume_unfinished() {
                Ok(resumed) if !resumed.is_empty() => {
                    tracing::info!(count = resumed.len(), "resumed unfinished meetings");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "unfinished meetings could not be resumed"),
            }
            // After `resume_unfinished`, whose runs export what they resumed.
            match pipeline.redeliver_unfinished() {
                Ok(owed) if !owed.is_empty() => {
                    tracing::info!(count = owed.len(), "re-exporting unfinished exports");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "unfinished exports could not be re-exported");
                }
            }
        }
    }

    /// The launch's reconcile ([`reconcile_at_launch`]), then the list is
    /// refreshed.
    fn reconcile_listed(&self, host: &Arc<Host>) -> impl FnOnce() + Send + 'static {
        let interrupted = Interrupted::list(&self.store, &self.paths.support_directory);
        let (store, pipeline) = (self.store.clone(), self.pipeline.clone());
        let (check, zone, runtime) = (
            self.live_recording_check.clone(),
            self.zone,
            self.runtime.clone(),
        );
        let (host, recorder) = (host.clone(), self.recorder.clone());
        move || {
            let adopted =
                reconcile_at_launch(&store, &pipeline, &interrupted, &check, zone, &runtime);
            recorder.note_adopted(adopted.len());
            host.store_changed();
        }
    }

    /// What [`App::launch`] runs once the keyring answered: the pipeline
    /// rebuilt, the API key and a waiting handover's identity read again.
    fn reread_after_unlock(&self, host: &Arc<Host>) -> impl FnOnce() + Send + 'static {
        let (pipeline, host) = (self.pipeline.clone(), host.clone());
        let waiting = self
            .handover
            .clone()
            .filter(|handover| handover.listener().is_none());
        move || {
            if let Err(error) = pipeline.reload() {
                tracing::warn!(%error, "the pipeline was not rebuilt after the unlock");
            }
            host.secrets_changed();
            let Some(waiting) = waiting else {
                return;
            };
            if let Err(error) = waiting.read_again() {
                tracing::warn!(%error, "the handover identity could not be read after the unlock");
            }
            host.phones_changed();
        }
    }

    /// Waits until the launch's background half has run; at once when
    /// [`App::launch`] was not called or this was already awaited.
    pub async fn launch_finished(&self) {
        let work = self
            .launch_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(work) = work {
            let _ = work.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Utc;

    use steno_bridge::BridgeTopic;
    use steno_core::{
        AudioFormat, HandoverIntake as _, PairedDevice, RecordingMetadata, SecretKey, SecretStore,
        SettingProblem, async_trait, paths::file_url, protocols::BoundaryResult,
    };
    use steno_host::services::ListenerState;
    use steno_pipeline::MeetingEventBus;
    use steno_speech::SpeechRuntime;

    use super::*;
    use crate::secrets::KeyringUnavailable;
    use crate::testing::{
        GatedFiles, HANG_GUARD, LIVE, PATIENCE, TimedSink, built, fake_dependencies, found_within,
        on_own_thread, options_under, shown_meeting, temp_store,
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_phone_intake_commits_durably_and_enqueues_on_the_current_pipeline() {
        let (dir, store) = temp_store();
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&dir.path().join("audio"), true);
        store.save_settings(&settings).unwrap();
        let reloads = Arc::new(AtomicUsize::new(0));
        let make: MakeDependencies = {
            let (store, reloads) = (store.clone(), reloads.clone());
            Arc::new(move || {
                let n = reloads.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(built(fake_dependencies(&store, &format!("engine-{n}"))))
            })
        };
        let current = Arc::new(CurrentPipeline::new(
            built(fake_dependencies(&store, "engine-0")),
            make,
            tokio::runtime::Handle::current(),
        ));
        let support = dir.path().join("support");
        let intake = handover_intake(store.clone(), current.clone(), local_zone(), &support);

        current.reload().unwrap();
        assert_eq!(
            current.current().dependencies().speech_engine.id(),
            "engine-1"
        );

        let upload = dir.path().join("upload.wav");
        std::fs::write(
            &upload,
            steno_pipeline::fixtures::wav_data(&vec![0; 16_000], 16_000, 1),
        )
        .unwrap();
        let device = PairedDevice {
            id: uuid::Uuid::new_v4(),
            name: "Phone".to_owned(),
            paired_at: Utc::now(),
            last_seen_at: None,
        };
        store.save_paired_device(&device, &[1; 32]).unwrap();
        let metadata = RecordingMetadata {
            recording_id: uuid::Uuid::new_v4(),
            started_at: Utc::now(),
            duration_seconds: 1.0,
            byte_count: 32_044,
            sha256: vec![0; 32],
            chunk_size: 32_044,
            format: AudioFormat::Wav16kInt16,
            device_name: "Phone".to_owned(),
        };
        // The level, the meeting's state and the folders noted, per commit that
        // holds a phone meeting.
        let commits: Arc<std::sync::Mutex<Vec<(i64, String, usize)>>> = Arc::default();
        let (seen, noted) = (commits.clone(), support.clone());
        store.probe_commits(move |connection| {
            let state: Option<String> = connection
                .query_row(
                    "SELECT (SELECT state FROM meeting WHERE source = 'phone')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if let Some(state) = state {
                let level = connection
                    .query_row("PRAGMA synchronous", [], |row| row.get(0))
                    .unwrap();
                let noted = crate::audio_folders::recorded(&noted).unwrap().len();
                seen.lock().unwrap().push((level, state, noted));
            }
        });
        let meeting_id = intake.admit(&upload, &metadata, &device).await.unwrap();
        assert!(
            crate::audio_folders::recorded(&support).unwrap().is_empty(),
            "the copy's folder is forgotten once its admission committed"
        );
        // The retired pipeline never saw the meeting; the current one did.
        let current_pipeline = current.current();
        current_pipeline.wait_until_idle().await;
        let meeting = store.meeting(meeting_id).unwrap().unwrap();
        assert_ne!(meeting.state.kind(), steno_core::MeetingStateKind::Queued);
        let commits = commits.lock().unwrap().clone();
        let queued: Vec<_> = commits
            .iter()
            .filter(|(_, state, _)| state == "queued")
            .collect();
        assert_eq!(
            queued,
            [&(2, "queued".to_owned(), 1)],
            "the intake's commit under FULL is the only one with the meeting queued, its folder noted: {commits:?}"
        );
        assert_eq!(
            store
                .stage_rates()
                .unwrap()
                .iter()
                .filter(|row| row.key == "engine-1")
                .count(),
            1,
            "the transcribe rate was learned under the current engine's id"
        );
    }

    /// One unreadable setting row stops the build: the error names the key
    /// and the problem, not the value or the path; the database keeps the row.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_setting_names_its_key_and_the_database_and_not_its_value() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        let database = StenoPaths::new(&support).database_path();
        let value = "https://user:sk-secret@example.com";
        {
            let store = open_store(&database).unwrap();
            store
                .write(|transaction| {
                    transaction.execute(
                        "INSERT INTO setting (key, value) VALUES ('llmBaseURL', ?1)",
                        [value],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let Err(error) = build(options_under(&support)) else {
            panic!("the build went ahead over an unreadable setting");
        };
        let text = error.to_string();
        assert_eq!(
            text,
            "the stored setting llmBaseURL is not in a form Steno can read"
        );
        assert!(!text.contains(&support.display().to_string()), "{text}");
        assert!(!format!("{error:?}").contains("sk-secret"), "{error:?}");
        let Some((
            StoreError::UndecodableSetting {
                key,
                problem: SettingProblem::NotJson,
            },
            in_database,
        )) = error.unreadable_settings()
        else {
            panic!("not an unreadable setting: {error:?}");
        };
        assert_eq!(
            (key.as_str(), in_database),
            ("llmBaseURL", database.as_path())
        );
        let kept: String = open_store(&database)
            .unwrap()
            .read(|connection| {
                Ok(connection.query_row(
                    "SELECT value FROM setting WHERE key = 'llmBaseURL'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(kept, value);
        assert!(
            BuildError::Store(StoreError::BlankPersonName)
                .unreadable_settings()
                .is_none()
        );
    }

    /// The shell's host lists the permissions of the OS it was built for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_host_runs_on_the_platform_the_app_was_built_for() {
        let dir = tempfile::tempdir().unwrap();
        let app = build(options_under(&dir.path().join("support"))).unwrap();
        let onboarding = app
            .host()
            .unwrap()
            .snapshot(BridgeTopic::Onboarding)
            .unwrap();
        let listed: Vec<&str> = onboarding["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["kind"].as_str().unwrap())
            .collect();
        let expected: Vec<&str> =
            steno_bridge::PermissionKind::for_platform(steno_bridge::Platform::CURRENT)
                .iter()
                .map(|kind| kind.as_str())
                .collect();
        assert_eq!(listed, expected);
    }

    const DISTRIBUTION_CHILD: &str = "STENO_DISTRIBUTION_TEST_CHILD";

    /// The host says a package manager delivers the updates exactly when
    /// [`updates_are_managed`](crate::updates::updates_are_managed) does, here
    /// and in a child told the opposite.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_host_says_whether_a_package_manager_delivers_the_updates() {
        let managed = crate::updates::updates_are_managed();
        let dir = tempfile::tempdir().unwrap();
        let app = build(options_under(&dir.path().join("support"))).unwrap();
        let general = app
            .host()
            .unwrap()
            .snapshot(BridgeTopic::SettingsGeneral)
            .unwrap();
        assert_eq!(general["updates"]["managedNote"].is_string(), managed);
        if std::env::var_os(DISTRIBUTION_CHILD).is_some() {
            return;
        }
        let name = "app::tests::the_host_says_whether_a_package_manager_delivers_the_updates";
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--test-threads=1"])
            .env(DISTRIBUTION_CHILD, "1")
            .env(
                crate::updates::DISTRIBUTION_VARIABLE,
                if managed { "appimage" } else { "nix" },
            )
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The recorder's disk watch reads the database's volume too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_recorder_watches_the_database_folder_s_volume_too() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("elsewhere").join("steno.sqlite");
        let mut options = options_under(&dir.path().join("support"));
        options.database_path = Some(database.clone());
        let app = build(options).unwrap();
        let disk = app.recorder.disk();
        assert_eq!(disk.database_folder.as_deref(), database.parent());
    }

    /// The launch removes a tone file a crashed probe left, and leaves a file
    /// whose name only resembles it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn launch_removes_the_tone_files_a_probe_left() {
        let dir = tempfile::tempdir().unwrap();
        let tones = dir.path().join("tmp");
        std::fs::create_dir_all(&tones).unwrap();
        let leftover = tones.join(format!(
            "steno-permission-4242-{}.wav",
            uuid::Uuid::new_v4()
        ));
        let resembling = tones.join("steno-permission-4242.wav");
        let written =
            std::time::SystemTime::now() - steno_audio::capture::permission::LEFTOVER_TONE_AGE;
        for path in [&leftover, &resembling] {
            std::fs::write(path, b"RIFF").unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(written)
                .unwrap();
        }
        let mut app = build(options_under(&dir.path().join("support"))).unwrap();
        app.tone_directory = Some(tones);
        let host = Arc::new(app.host().unwrap());
        app.launch(&host);
        crate::testing::eventually("the leftover tone file is gone", || !leftover.exists()).await;
        assert!(resembling.exists());
        app.launch_finished().await;
    }

    /// The launch fails a meeting left recording with no audio on disk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn launch_fails_a_recording_the_last_process_left_with_the_swift_reason() {
        let dir = tempfile::tempdir().unwrap();
        let app = build(options_under(&dir.path().join("support"))).unwrap();
        let audio_folder = app.store.settings().unwrap().audio_folder;
        std::fs::create_dir_all(steno_core::paths::file_url_path(&audio_folder).unwrap()).unwrap();
        let mut meeting = steno_core::testing::sample_data::meeting();
        meeting.state = steno_core::MeetingState::Recording;
        app.store.save_meeting(&meeting).unwrap();
        let host = Arc::new(app.host().unwrap());
        app.launch(&host);
        app.launch_finished().await;
        assert_eq!(
            app.store.meeting(meeting.id).unwrap().unwrap().state,
            steno_core::MeetingState::Failed {
                reason: "Recording was interrupted before it finished.".to_owned()
            }
        );
    }

    /// The launch re-exports a ready meeting whose export was left `pending`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn launch_re_exports_a_meeting_whose_export_was_left_pending() {
        let dir = tempfile::tempdir().unwrap();
        let app = build(options_under(&dir.path().join("support"))).unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let mut settings = app.store.settings().unwrap();
        settings.obsidian = Some(steno_core::ObsidianSettings {
            vault_path: vault.to_string_lossy().into_owned(),
            people_folder: None,
            include_audio: false,
            task_tag: None,
            extra: serde_json::Map::new(),
        });
        app.store.save_settings(&settings).unwrap();
        let mut meeting = steno_core::testing::sample_data::meeting();
        meeting.state = steno_core::MeetingState::Ready;
        app.store.save_meeting(&meeting).unwrap();
        let destination = DeliveryCoordinator::destinations_for(&settings).remove(0);
        app.store
            .save_delivery(&steno_core::Delivery {
                id: steno_core::Delivery::id_for(meeting.id, destination.id()),
                meeting_id: meeting.id,
                destination_id: destination.id().to_owned(),
                status: steno_core::DeliveryStatus::Pending,
                last_attempt_at: None,
                receipt: None,
            })
            .unwrap();

        let host = Arc::new(app.host().unwrap());
        app.launch(&host);
        app.pipeline.current().wait_until_idle().await;
        let delivery = app.store.deliveries(meeting.id).unwrap().remove(0);
        assert_eq!(delivery.status, steno_core::DeliveryStatus::Delivered);
        assert!(delivery.receipt.is_some());
    }

    /// The launch recovers a call recording whose process died, queued with
    /// the `failed` end reason and processed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn launch_recovers_a_recording_the_last_process_left() {
        let (dir, store) = crate::testing::temp_store();
        let mut app = recording_app(&dir, &store);
        app.live_recording_check = crate::testing::an_hour_later();
        let meeting = steno_pipeline::LocalRecordingIntake::over(
            store.clone(),
            app.pipeline.current(),
            app.zone,
        )
        .begin(
            uuid::Uuid::new_v4(),
            steno_core::MeetingSource::MacCall,
            None,
            None,
            &[],
            Utc::now(),
        )
        .unwrap();
        let layout = steno_core::RecordingLayout::new(&dir.path().join("audio"), meeting.id);
        let lanes = [steno_core::AudioLane::Mic, steno_core::AudioLane::System];
        let mut writer = steno_audio::RecordingWriter::new(&layout, &lanes, false).unwrap();
        crate::testing::write_frames(&mut writer, 100);
        drop(writer);

        let host = Arc::new(app.host().unwrap());
        app.launch(&host);
        app.launch_finished().await;
        app.pipeline.current().wait_until_idle().await;
        let recovered = store.meeting(meeting.id).unwrap().unwrap();
        assert_eq!(recovered.state, steno_core::MeetingState::Ready);
        assert_eq!(
            recovered.end_reason,
            Some(steno_core::RecordingEndReason::Failed)
        );
        assert_eq!(recovered.duration, 1.0);
        assert_eq!(store.asset(meeting.id).unwrap().unwrap().lanes, lanes);
    }

    /// The launch adopts a call master with no meeting, processes it, and the
    /// main window says so until dismissed; its folder stays recorded until a
    /// later launch finds the row durable.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn launch_adopts_a_recording_with_no_meeting_and_says_so() {
        use steno_host::services::Recorder as _;

        let (dir, store) = crate::testing::temp_store();
        let mut app = recording_app(&dir, &store);
        app.live_recording_check = crate::testing::an_hour_later();
        let meeting_id = uuid::Uuid::new_v4();
        let audio = dir.path().join("audio");
        crate::audio_folders::record(&app.paths.support_directory, meeting_id, &audio).unwrap();
        let layout = steno_core::RecordingLayout::new(&audio, meeting_id);
        let lanes = [steno_core::AudioLane::Mic, steno_core::AudioLane::System];
        let mut writer = steno_audio::RecordingWriter::new(&layout, &lanes, false).unwrap();
        crate::testing::write_frames(&mut writer, 100);
        drop(writer);

        let host = Arc::new(app.host().unwrap());
        app.launch(&host);
        app.launch_finished().await;
        app.pipeline.current().wait_until_idle().await;
        let adopted = store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(adopted.state, steno_core::MeetingState::Ready);
        assert_eq!(adopted.source, steno_core::MeetingSource::MacCall);
        assert_eq!(adopted.duration, 1.0);
        assert_eq!(
            app.recorder.status().warning.as_deref(),
            Some("Recovered a recording that was missing from your list. It is being processed.")
        );
        assert!(
            crate::audio_folders::recorded(&app.paths.support_directory)
                .unwrap()
                .contains_key(&meeting_id)
        );
        app.recorder.clear_messages();
        assert_eq!(app.recorder.status().warning, None);
    }

    /// A meeting waiting for models stays waiting through reloads while they
    /// are missing, and the first reload once they are on disk starts it. The
    /// settings' models directory wins over `STENO_MODELS_DIR`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_resumes_the_waiting_meetings_once_the_apps_gates_pass() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        std::fs::create_dir_all(&support).unwrap();
        std::fs::write(
            support.join("speech.json"),
            r#"{"onnxSidecarOnMac":true,"modelsMirror":"http://127.0.0.1:9"}"#,
        )
        .unwrap();
        let models = dir.path().join("models");
        let store = open_store(&StenoPaths::new(&support).database_path()).unwrap();
        let mut settings = store.settings().unwrap();
        settings.models_directory = Some(file_url(&models, true));
        store.save_settings(&settings).unwrap();
        drop(store);
        let app = build(options_under(&support)).unwrap();
        assert_eq!(app.models_directory, models);
        let mut meeting = steno_core::testing::sample_data::meeting();
        meeting.id = uuid::Uuid::new_v4();
        let asset = steno_pipeline::fixtures::two_lane_call(
            &dir.path().join("audio"),
            meeting.id,
            steno_core::AudioRetention::KeepForever,
        )
        .unwrap();
        app.pipeline.current().enqueue(&meeting, &asset).unwrap();
        app.pipeline.current().wait_until_idle().await;
        let waiting = || app.pipeline.current().dependencies().model_waits.waiting();
        assert_eq!(waiting(), [meeting.id]);
        let mut events = app.events.subscribe();
        let decodes = |events: &mut steno_pipeline::EventReceiver| {
            std::iter::from_fn(|| events.try_recv().ok())
                .filter(|event| {
                    matches!(event, steno_core::MeetingEvent::Progress { progress, .. }
                        if progress.stage == steno_core::PipelineStage::Decode)
                })
                .count()
        };

        app.pipeline.reload().unwrap();
        app.pipeline.current().wait_until_idle().await;
        assert_eq!(decodes(&mut events), 0, "no run started");
        assert_eq!(waiting(), [meeting.id]);

        crate::speech::testing::install_every_model(&crate::speech::testing::models_in(
            &app.models_directory,
        ));
        app.pipeline.reload().unwrap();
        assert_eq!(waiting(), Vec::<uuid::Uuid>::new());
        assert!(decodes(&mut events) > 0, "the meeting started");
        app.pipeline.quit();
        app.pipeline.current().wait_until_idle().await;
    }

    /// The reload test above, as a child with `STENO_MODELS_DIR` naming a
    /// directory with a known file: that file is left alone. A child, because
    /// setting the variable here would reach every test beside this one.
    #[test]
    fn the_reload_test_leaves_the_models_directory_the_environment_names_alone() {
        const RELOAD_TEST: &str =
            "app::tests::a_reload_resumes_the_waiting_meetings_once_the_apps_gates_pass";
        let home = tempfile::tempdir().unwrap();
        let named = tempfile::tempdir().unwrap();
        let asset = steno_diarize::models::asset();
        let sentinel = crate::speech::testing::models_in(named.path())
            .speech
            .directory(&asset)
            .join(&asset.files[0].name);
        std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
        let bytes = b"a developer's downloaded model";
        std::fs::write(&sentinel, bytes).unwrap();

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", RELOAD_TEST, "--test-threads=1"])
            .env("HOME", home.path())
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("APPDATA", home.path().join("appdata"))
            .env("CODEX_HOME", home.path().join("codex"))
            .env(steno_speech::ModelStore::ENVIRONMENT_VARIABLE, named.path())
            .env_remove(steno_speech::ModelStore::MIRROR_ENVIRONMENT_VARIABLE)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{output:?}");
        assert!(
            stdout.contains("1 passed"),
            "the child ran the test: {stdout}"
        );
        let after = std::fs::read(&sentinel).unwrap();
        assert!(
            after == bytes,
            "the model file was overwritten: {} bytes now",
            after.len()
        );
        assert_eq!(
            crate::speech::testing::files_under(named.path()),
            std::collections::BTreeSet::from([sentinel])
        );
    }

    /// A stored engine the app does not run is Parakeet v3 once the app is
    /// built, with the notice's pending flag in `preferences.json`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_build_retires_a_stored_whisper_engine_with_the_notice_pending() {
        use steno_host::setup::engine_notice::PENDING_KEY;

        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        let paths = StenoPaths::new(&support);
        let store = open_store(&paths.database_path()).unwrap();
        let mut settings = store.settings().unwrap();
        "whisperkit-large-v3-turbo".clone_into(&mut settings.speech_engine_id);
        store.save_settings(&settings).unwrap();
        drop(store);
        let preferences = support.join("preferences.json");

        let app = build(options_under(&support)).unwrap();
        assert_eq!(
            app.store.settings().unwrap().speech_engine_id,
            "parakeet-v3"
        );
        assert!(FilePreferences::new(&preferences).flag(PENDING_KEY));
        assert!(app.services.preferences.flag(PENDING_KEY));
        app.pipeline.quit();
    }

    /// The retirement writes the notice's flag through the launch half's
    /// `preferences.json`, so a later write there keeps it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_engine_notice_and_the_import_share_one_preferences_file() {
        use steno_host::setup::engine_notice::PENDING_KEY;

        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        let paths = StenoPaths::new(&support);
        let store = open_store(&paths.database_path()).unwrap();
        let mut settings = store.settings().unwrap();
        "whisperkit-large-v3-turbo".clone_into(&mut settings.speech_engine_id);
        store.save_settings(&settings).unwrap();
        drop(store);

        let mut imported = None;
        let app = build_with_import(options_under(&support), |preferences| {
            preferences.set_flag("steno.onboardingCompleted", true);
            imported = Some(preferences);
            None
        })
        .unwrap();
        let imported = imported.unwrap();
        imported.set_flag(crate::swift_import::IMPORT_RAN_KEY, true);

        let on_disk = FilePreferences::new(support.join("preferences.json"));
        for key in [
            PENDING_KEY,
            "steno.onboardingCompleted",
            crate::swift_import::IMPORT_RAN_KEY,
        ] {
            assert!(on_disk.flag(key), "{key}");
            assert!(app.services.preferences.flag(key), "{key}");
        }
        app.pipeline.quit();
    }

    /// The models directory is decided once, at build: a reload after the
    /// settings name another directory keeps the first, so `prepare` is still
    /// refused for missing models.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_keeps_the_models_directory_the_app_was_built_with() {
        use crate::speech::testing::{install_every_model, models_in};
        let dir = tempfile::tempdir().unwrap();
        let paths = StenoPaths::new(dir.path().join("support"));
        let (first, reloaded) = (
            dir.path().join("built-models"),
            dir.path().join("reloaded-models"),
        );
        std::fs::create_dir_all(&first).unwrap();
        install_every_model(&models_in(&reloaded));
        let store = open_store(&paths.database_path()).unwrap();
        let mut settings = store.settings().unwrap();
        settings.models_directory = Some(file_url(&first, true));
        store.save_settings(&settings).unwrap();
        drop(store);

        let app = build(options_under(&dir.path().join("support"))).unwrap();
        let refused = async || {
            let engine = app.pipeline.current().dependencies().speech_engine.clone();
            let error = engine.prepare().await.unwrap_err();
            error
                .downcast_ref::<steno_pipeline::PipelineFailure>()
                .is_some_and(steno_pipeline::PipelineFailure::is_models_missing)
        };
        assert!(refused().await, "the empty directory");

        settings.models_directory = Some(file_url(&reloaded, true));
        app.store.save_settings(&settings).unwrap();
        app.pipeline.reload().unwrap();
        assert!(
            refused().await,
            "still the empty directory after the reload"
        );
    }

    /// Reloads keep the speech engine while the stored engine id runs where
    /// the last one did; on the Mac a switch to a sidecar id builds a new
    /// engine and the switch back finds the first. Every reload keeps the
    /// diarizer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_keeps_the_speech_engine_while_the_engine_id_runs_where_it_did() {
        let dir = tempfile::tempdir().unwrap();
        let app = build(options_under(&dir.path().join("support"))).unwrap();
        let engine = || app.pipeline.current().dependencies().speech_engine.clone();
        let reload_with = |engine_id: &str| {
            let mut settings = app.store.settings().unwrap();
            engine_id.clone_into(&mut settings.speech_engine_id);
            app.store.save_settings(&settings).unwrap();
            app.pipeline.reload().unwrap();
            app.pipeline.current_with_engine().1.runtime
        };
        let diarizer = || app.pipeline.current().dependencies().diarizer.clone();
        let first = engine();
        let first_diarizer = diarizer();
        let first_runtime = app.pipeline.current_with_engine().1.runtime;
        app.pipeline.reload().unwrap();
        assert!(engine().ptr_eq(&first), "unchanged settings, same engine");

        let other_runtime = reload_with("whisperkit-large-v3-turbo");
        assert_eq!(other_runtime, SpeechRuntime::OnnxSidecar);
        let in_process = first_runtime == SpeechRuntime::CoreMlInProcess;
        assert_eq!(engine().ptr_eq(&first), !in_process, "{first_runtime:?}");
        let other = engine();
        assert_eq!(reload_with("parakeet-ultra"), SpeechRuntime::OnnxSidecar);
        assert!(engine().ptr_eq(&other), "both ids run in the sidecar");

        assert_eq!(reload_with("parakeet-v3"), first_runtime);
        assert!(engine().ptr_eq(&first), "back where the first ran");
        assert!(Arc::ptr_eq(&diarizer(), &first_diarizer));
    }

    /// The shell's login item is the host's; without one a fake registers
    /// nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_login_item_the_shell_passes_is_the_hosts() {
        let options = |login_item| {
            let dir = tempfile::tempdir().unwrap();
            let options = AppOptions {
                login_item,
                ..options_under(&dir.path().join("support"))
            };
            (dir, options)
        };
        let shells = Arc::new(FakeLoginItem::new(LoginItemStatus::Enabled));
        let (_dir, with) = options(Some(shells.clone() as Arc<dyn LoginItem>));
        let app = build(with).unwrap();
        assert_eq!(app.services.login_item.status(), LoginItemStatus::Enabled);
        app.services.login_item.set_enabled(false).unwrap();
        assert_eq!(*shells.changes.lock().unwrap(), [false]);

        let (_dir, without) = options(None);
        let app = build(without).unwrap();
        assert_eq!(
            app.services.login_item.status(),
            LoginItemStatus::NotRegistered
        );
    }

    /// The shell's permissions are the host's: a denied microphone opens
    /// onboarding, and on the Mac system audio is the last probe's result.
    /// Without the shell's every permission is granted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_permissions_the_shell_passes_are_the_hosts() {
        use steno_bridge::{PermissionKind, PermissionState};

        let dir = tempfile::tempdir().unwrap();
        let shells = Arc::new(FakePermissions::all(PermissionState::Granted));
        shells.set_state(PermissionKind::Microphone, PermissionState::Denied);
        let app = build(AppOptions {
            permissions: Some(shells.clone() as Arc<dyn Permissions>),
            ..options_under(&dir.path().join("support"))
        })
        .unwrap();
        app.services.preferences.set_flag(
            steno_host::onboarding::OnboardingViewModel::COMPLETED_KEY,
            true,
        );
        let permissions = &app.services.permissions;
        assert_eq!(
            permissions.state(PermissionKind::Microphone),
            PermissionState::Denied
        );
        assert!(app.host().unwrap().should_open_onboarding());
        let system_audio = if cfg!(target_os = "macos") {
            PermissionState::Unknown
        } else {
            PermissionState::Granted
        };
        assert_eq!(permissions.state(PermissionKind::SystemAudio), system_audio);
        app.services
            .preferences
            .set_flag(crate::permissions::SYSTEM_AUDIO_GRANTED_KEY, true);
        assert_eq!(
            permissions.state(PermissionKind::SystemAudio),
            PermissionState::Granted
        );
        drop(app);

        let dir = tempfile::tempdir().unwrap();
        let app = build(options_under(&dir.path().join("support"))).unwrap();
        for &kind in PermissionKind::ALL {
            assert_eq!(
                app.services.permissions.state(kind),
                PermissionState::Granted
            );
        }
    }

    /// A recording started during an event takes its title and attendee.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_calendar_the_shell_passes_names_the_graphs_recordings() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        let calendar =
            crate::calendar::testing::FakeCalendar::with(vec![steno_core::CalendarEvent {
                id: "event-7".to_owned(),
                title: "Planning".to_owned(),
                start: now - chrono::TimeDelta::minutes(1),
                end: now + chrono::TimeDelta::minutes(30),
                attendees: vec![steno_core::CalendarAttendee {
                    name: "Ada".to_owned(),
                    email: None,
                    is_current_user: false,
                }],
            }]);
        let app = build(AppOptions {
            calendar: Some(calendar.clone()),
            make_capture_session: crate::testing::synthetic_capture(),
            ..options_under(&dir.path().join("support"))
        })
        .unwrap();
        let mut settings = app.store.settings().unwrap();
        settings.audio_folder = steno_core::paths::file_url(&dir.path().join("audio"), true);
        app.store.save_settings(&settings).unwrap();
        let recorder = app.recorder.clone();
        crate::testing::on_own_thread(HANG_GUARD, "the start returned", move || {
            steno_host::services::Recorder::start(
                &*recorder,
                steno_bridge::CaptureMode::InPerson,
                None,
            );
        });
        let meeting_id = steno_host::services::Recorder::status(&*app.recorder)
            .meeting_id
            .expect("recording");
        crate::testing::eventually("the event named the recording", || {
            app.store
                .meeting(meeting_id)
                .unwrap()
                .is_some_and(|meeting| meeting.title == "Planning")
        })
        .await;
        let participants = app.store.participants(meeting_id).unwrap();
        assert_eq!(participants.len(), 1);
        assert_eq!(participants[0].display_name, "Ada");
        let recorder = app.recorder.clone();
        crate::testing::on_own_thread(HANG_GUARD, "the stop returned", move || {
            steno_host::services::Recorder::stop(&*recorder);
        });
        let pipeline = app.pipeline.current();
        pipeline.wait_until_idle().await;
    }

    /// A recording's start reads the calendar but asks for no permission.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_never_asks_for_a_permission() {
        use steno_bridge::PermissionState;

        let dir = tempfile::tempdir().unwrap();
        let shells = Arc::new(FakePermissions::all(PermissionState::Unknown));
        let now = chrono::Utc::now();
        let calendar =
            crate::calendar::testing::FakeCalendar::with(vec![steno_core::CalendarEvent {
                id: "event-7".to_owned(),
                title: "Planning".to_owned(),
                start: now - chrono::TimeDelta::minutes(1),
                end: now + chrono::TimeDelta::minutes(30),
                attendees: Vec::new(),
            }]);
        let app = build(AppOptions {
            permissions: Some(shells.clone() as Arc<dyn Permissions>),
            calendar: Some(calendar.clone()),
            make_capture_session: crate::testing::synthetic_capture(),
            ..options_under(&dir.path().join("support"))
        })
        .unwrap();
        let mut settings = app.store.settings().unwrap();
        settings.audio_folder = steno_core::paths::file_url(&dir.path().join("audio"), true);
        app.store.save_settings(&settings).unwrap();
        let recorder = app.recorder.clone();
        crate::testing::on_own_thread(HANG_GUARD, "the start returned", move || {
            steno_host::services::Recorder::start(
                &*recorder,
                steno_bridge::CaptureMode::InPerson,
                None,
            );
        });
        let meeting_id = steno_host::services::Recorder::status(&*app.recorder)
            .meeting_id
            .expect("recording");
        crate::testing::eventually("the event named the recording", || {
            app.store
                .meeting(meeting_id)
                .unwrap()
                .is_some_and(|meeting| meeting.title == "Planning")
        })
        .await;
        let recorder = app.recorder.clone();
        crate::testing::on_own_thread(HANG_GUARD, "the stop returned", move || {
            steno_host::services::Recorder::stop(&*recorder);
        });
        assert_eq!(*shells.requests.lock().unwrap(), Vec::new(), "no prompt");
        app.pipeline.current().wait_until_idle().await;
    }

    /// On the Mac only a graph built for an installed bundle registers the
    /// login item at first launch; off the Mac both do.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_first_launch_follows_the_shells_installed_bundle() {
        for installed_bundle in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let login_item = Arc::new(FakeLoginItem::new(LoginItemStatus::NotRegistered));
            let app = build(AppOptions {
                login_item: Some(login_item.clone() as Arc<dyn LoginItem>),
                installed_bundle,
                ..options_under(&dir.path().join("support"))
            })
            .unwrap();
            let mut settings = app.store.settings().unwrap();
            settings.launch_at_login = true;
            app.store.save_settings(&settings).unwrap();
            app.host().unwrap().register_login_item_on_first_launch();
            let registers = installed_bundle || !cfg!(target_os = "macos");
            let changes = login_item.changes.lock().unwrap().clone();
            assert_eq!(
                changes == [true],
                registers,
                "{installed_bundle}: {changes:?}"
            );
        }
    }

    fn recording_app(dir: &tempfile::TempDir, store: &Arc<Store>) -> App {
        crate::testing::app_over_fakes(dir.path(), store, crate::testing::synthetic_capture())
    }

    /// `app`'s host with the hook `App::launch` wires, without the launch's
    /// tasks, which could hang the runtime's shutdown.
    fn wired_host(app: &App) -> Host {
        let host = app.host().unwrap();
        let changed = host.clone();
        app.recorder
            .on_change(Arc::new(move || changed.recorder_changed()));
        host
    }

    /// Calls `method` through `host` on its own thread and asserts success,
    /// failing as hung after [`HANG_GUARD`].
    fn call(host: &Host, method: steno_bridge::BridgeMethod, params: Option<serde_json::Value>) {
        let dispatcher = steno_bridge::Dispatcher::new(host.clone());
        let reply = on_own_thread(HANG_GUARD, &format!("{method} returned"), move || {
            dispatcher.call(method, params)
        });
        assert!(reply.is_ok(), "{method}: {reply:?}");
    }

    /// A recording that cannot start: every recorder command through the host
    /// returns, though each recorder change re-enters the host, and the
    /// error is shown.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_recorder_command_returns_when_the_recording_cannot_start() {
        use steno_bridge::BridgeMethod;
        use steno_host::services::Recorder as _;
        let (dir, store) = temp_store();
        let app = crate::testing::app_over_fakes(
            dir.path(),
            &store,
            Arc::new(|_| Err("no capture device".to_owned())),
        );
        let host = wired_host(&app);
        let start = serde_json::json!({ "mode": "inPerson" });
        let failed =
            Some("Recording could not start: Steno could not open the audio devices.".to_owned());
        for (method, params, error) in [
            (BridgeMethod::RecordingStart, Some(start), failed.clone()),
            (BridgeMethod::RecordingToggle, None, failed.clone()),
            (BridgeMethod::RecordingKeepGoing, None, failed.clone()),
            (BridgeMethod::RecordingStop, None, failed),
            (BridgeMethod::RecordingClearMessages, None, None),
        ] {
            call(&host, method, params);
            let status = app.recorder.status();
            assert_eq!(status.state, steno_bridge::RecordingState::Idle, "{method}");
            assert_eq!(status.error, error, "{method}");
        }
        assert_eq!(store.all_meetings().unwrap(), []);
    }

    /// Record while a Quit refuses starts: each returns, records nothing,
    /// and the `recording` snapshot says why.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_pages_record_during_a_menu_quit_is_refused_and_says_why() {
        use steno_bridge::BridgeMethod;
        use steno_host::services::{QUITTING_REFUSAL, Recorder as _};
        let (dir, store) = temp_store();
        let app = recording_app(&dir, &store);
        let host = wired_host(&app);
        app.recorder.refuse_starts();
        let start = serde_json::json!({ "mode": "inPerson" });
        for (method, params) in [
            (BridgeMethod::RecordingStart, Some(start)),
            (BridgeMethod::RecordingToggle, None),
        ] {
            call(&host, method, params);
            let status = app.recorder.status();
            assert_eq!(status.state, steno_bridge::RecordingState::Idle, "{method}");
            let shown = host.snapshot(steno_bridge::BridgeTopic::Recording).unwrap();
            assert_eq!(shown["error"], QUITTING_REFUSAL, "{method}");
        }
        assert_eq!(store.all_meetings().unwrap(), []);
    }

    /// [`recording_app`] with the real pipeline over a failed meeting with a
    /// call on disk, selected on a ready page; the meeting's id.
    fn failed_meeting_selected(
        dir: &tempfile::TempDir,
        store: &Arc<Store>,
        reason: &str,
    ) -> (App, Host, uuid::Uuid) {
        use steno_bridge::BridgeMethod;
        let mut app = recording_app(dir, store);
        app.services.pipeline = Arc::new(HostPipeline {
            pipeline: app.pipeline.clone(),
            sweep: RetentionSweep::new(store.clone()),
        });
        app.services.file_system = Arc::new(steno_host::services::RealFileSystem);
        let mut meeting = steno_core::testing::sample_data::meeting();
        meeting.state = steno_core::MeetingState::Failed {
            reason: reason.to_owned(),
        };
        let asset = steno_pipeline::fixtures::two_lane_call(
            &dir.path().join("audio"),
            meeting.id,
            steno_core::AudioRetention::KeepForever,
        )
        .unwrap();
        store.save_meeting_with_asset(&meeting, &asset).unwrap();
        let host = wired_host(&app);
        call(&host, BridgeMethod::PageReady, None);
        call(
            &host,
            BridgeMethod::MeetingsSelect,
            Some(serde_json::json!({ "meetingID": steno_core::json::uuid_string(meeting.id) })),
        );
        (app, host, meeting.id)
    }

    /// "Process again" on a failed meeting shows it queued at once, and the
    /// run takes it to ready.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_through_the_host_runs_a_failed_meeting_to_ready() {
        use steno_bridge::BridgeMethod;
        let (dir, store) = temp_store();
        let (app, host, id) =
            failed_meeting_selected(&dir, &store, "transcribe: the model is not installed");
        let detail = host.snapshot(BridgeTopic::MeetingDetail).unwrap();
        assert_eq!(detail["state"], "failed");
        assert_eq!(detail["retention"]["filesExist"], true);

        call(&host, BridgeMethod::MeetingProcessAgain, None);
        let detail = host.snapshot(BridgeTopic::MeetingDetail).unwrap();
        assert_ne!(detail["state"], "failed", "{detail}");
        assert_eq!(detail.get("error"), None, "{detail}");
        let list = host.snapshot(BridgeTopic::MeetingsList).unwrap();
        assert_eq!(list["counts"]["failed"], 0, "{list}");

        app.pipeline.current().wait_until_idle().await;
        assert_eq!(
            store.meeting(id).unwrap().unwrap().state,
            steno_core::MeetingState::Ready
        );
    }

    /// A ready but incomplete meeting is offered "Process again" too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_through_the_host_runs_a_ready_meeting_kept_incomplete() {
        use steno_bridge::BridgeMethod;
        let (dir, store) = temp_store();
        let (app, host, id) =
            failed_meeting_selected(&dir, &store, "transcribe: the model is not installed");
        store
            .set_state(id, steno_core::MeetingState::Ready, chrono::Utc::now())
            .unwrap();
        host.store_changed();
        let detail = host.snapshot(BridgeTopic::MeetingDetail).unwrap();
        assert_eq!(detail["state"], "ready", "{detail}");
        assert_eq!(detail["canProcessAgain"], true, "{detail}");

        call(&host, BridgeMethod::MeetingProcessAgain, None);
        let detail = host.snapshot(BridgeTopic::MeetingDetail).unwrap();
        assert_eq!(detail.get("error"), None, "{detail}");
        app.pipeline.current().wait_until_idle().await;
        assert_eq!(
            store.meeting(id).unwrap().unwrap().state,
            steno_core::MeetingState::Ready
        );
        assert_ne!(
            store.segments(id).unwrap(),
            Vec::new(),
            "the run transcribed it"
        );
    }

    /// A recording stopped from the sidebar or the tray: each command returns
    /// and each recording is saved.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopping_a_recording_through_the_host_returns_and_saves_it() {
        use steno_bridge::BridgeMethod;
        use steno_host::services::Recorder as _;
        let (dir, store) = temp_store();
        let app = recording_app(&dir, &store);
        let host = wired_host(&app);
        for stop in [BridgeMethod::RecordingStop, BridgeMethod::RecordingToggle] {
            call(
                &host,
                BridgeMethod::RecordingStart,
                Some(serde_json::json!({ "mode": "inPerson" })),
            );
            let status = app.recorder.status();
            assert_eq!(
                status.state,
                steno_bridge::RecordingState::Recording,
                "{stop}"
            );
            let meeting_id = status.meeting_id.expect("recording");
            call(&host, stop, None);
            let status = app.recorder.status();
            assert_eq!(status.state, steno_bridge::RecordingState::Idle, "{stop}");
            assert_eq!(status.error, None, "{stop}");
            let meeting = store.meeting(meeting_id).unwrap().unwrap();
            assert_eq!(
                meeting.end_reason,
                Some(steno_core::RecordingEndReason::Manual),
                "{stop}"
            );
            assert!(
                store.asset(meeting_id).unwrap().is_some(),
                "{stop}: the asset"
            );
        }
    }

    /// The shutdown stops and saves a recording with `quit`; it stays
    /// `queued` for the next launch, through a reload too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutting_down_stops_and_saves_a_recording_in_progress() {
        use steno_host::services::Recorder as _;
        let (dir, store) = temp_store();
        let app = Arc::new(recording_app(&dir, &store));
        let starting = app.clone();
        tokio::task::spawn_blocking(move || {
            starting
                .recorder
                .start(steno_bridge::CaptureMode::InPerson, None);
        })
        .await
        .unwrap();
        let meeting_id = app.recorder.status().meeting_id.expect("recording");
        let quitting = app.clone();
        tokio::task::spawn_blocking(move || quitting.shutdown())
            .await
            .unwrap();
        assert_eq!(
            app.recorder.status().state,
            steno_bridge::RecordingState::Idle
        );
        let meeting = store.meeting(meeting_id).unwrap().unwrap();
        assert_eq!(
            meeting.end_reason,
            Some(steno_core::RecordingEndReason::Quit)
        );
        assert!(store.asset(meeting_id).unwrap().is_some(), "the asset");
        app.pipeline.current().wait_until_idle().await;
        assert_eq!(
            store.meeting(meeting_id).unwrap().unwrap().state,
            steno_core::MeetingState::Queued,
            "no job ran in the exit"
        );
        app.pipeline.reload().unwrap();
        assert_eq!(
            app.pipeline.current().resume_unfinished().unwrap(),
            Vec::<uuid::Uuid>::new(),
            "a reload's pipeline has quit too"
        );
    }

    /// An idle shutdown saves nothing and returns at once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutting_down_with_nothing_recorded_saves_nothing() {
        let (dir, store) = temp_store();
        let app = recording_app(&dir, &store);
        on_own_thread(PATIENCE, "an idle shutdown returned", move || {
            app.shutdown();
        });
        assert_eq!(store.all_meetings().unwrap(), []);
    }

    /// The first exit request is held and runs the shutdown once; requests
    /// meanwhile are held; then every request goes ahead.
    #[test]
    fn the_first_exit_runs_the_shutdown_once_and_holds_the_others_until_it_ends() {
        use std::sync::Mutex;
        use std::time::Duration;
        let gate = ExitGate::default();
        let steps = Arc::new(Mutex::new(Vec::<&str>::new()));
        let (exited, exit_seen) = std::sync::mpsc::channel();
        let shutdowns = Arc::new(AtomicUsize::new(0));

        let held = gate.exit_requested(
            PATIENCE,
            {
                let (steps, shutdowns) = (steps.clone(), shutdowns.clone());
                move || {
                    shutdowns.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(100));
                    steps.lock().unwrap().push("saved");
                }
            },
            {
                let (gate, steps) = (gate.clone(), steps.clone());
                move || {
                    steps.lock().unwrap().push("exit");
                    let _ = exited.send(gate.exit_requested(
                        PATIENCE,
                        || panic!("no second shutdown"),
                        || panic!("no second exit"),
                    ));
                }
            },
        );
        assert!(!held, "the first request is held");
        assert!(
            !gate.exit_requested(
                PATIENCE,
                || panic!("no second shutdown"),
                || panic!("no second exit"),
            ),
            "a request while the shutdown runs is held too"
        );
        assert!(
            exit_seen.recv_timeout(PATIENCE).unwrap(),
            "the exit after the shutdown goes ahead"
        );
        assert_eq!(*steps.lock().unwrap(), ["saved", "exit"]);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
        assert!(gate.exit_requested(
            PATIENCE,
            || panic!("no second shutdown"),
            || panic!("no second exit"),
        ));
    }

    /// A shutdown past the patience does not keep the app from quitting.
    #[test]
    fn an_exit_waits_at_most_the_patience() {
        let gate = ExitGate::default();
        let (exited, exit_seen) = std::sync::mpsc::channel();
        let (_never, stuck) = std::sync::mpsc::channel::<()>();
        assert!(!gate.exit_requested(
            std::time::Duration::from_millis(100),
            move || {
                let _ = stuck.recv();
            },
            move || exited.send(()).unwrap(),
        ));
        exit_seen
            .recv_timeout(PATIENCE)
            .expect("exited after the patience");
    }

    /// An exit no request held runs the shutdown and returns once it ended or
    /// the patience ran out; a second one runs nothing.
    #[test]
    fn an_exit_without_a_request_runs_the_shutdown_before_it_returns() {
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;
        let gate = ExitGate::default();
        let saved = Arc::new(AtomicBool::new(false));
        let saving = saved.clone();
        gate.exiting(PATIENCE, move || {
            std::thread::sleep(Duration::from_millis(100));
            saving.store(true, Ordering::SeqCst);
        });
        assert!(saved.load(Ordering::SeqCst), "saved before it returned");
        gate.exiting(PATIENCE, || panic!("no second shutdown"));
        assert!(gate.exit_requested(
            PATIENCE,
            || panic!("no second shutdown"),
            || panic!("no exit of the gate's"),
        ));

        let stuck_gate = ExitGate::default();
        let (_never, stuck) = std::sync::mpsc::channel::<()>();
        on_own_thread(
            PATIENCE,
            "the exit waited at most the patience",
            move || {
                stuck_gate.exiting(Duration::from_millis(100), move || {
                    let _ = stuck.recv();
                });
            },
        );
    }

    /// `exiting` during a held request's shutdown waits for it instead of
    /// running another, and the gate's exit is not raised.
    #[test]
    fn an_exit_during_a_held_shutdown_waits_for_it_and_takes_its_exit_over() {
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;
        let gate = ExitGate::default();
        let saved = Arc::new(AtomicBool::new(false));
        let (begun, shutdown_begun) = std::sync::mpsc::channel();
        let saving = saved.clone();
        let (raised, exit_raised) = std::sync::mpsc::channel();
        assert!(!gate.exit_requested(
            PATIENCE,
            move || {
                begun.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(200));
                saving.store(true, Ordering::SeqCst);
            },
            move || {
                let _ = raised.send(());
            },
        ));
        shutdown_begun
            .recv_timeout(PATIENCE)
            .expect("the shutdown began");
        gate.exiting(PATIENCE, || panic!("a second shutdown"));
        assert!(saved.load(Ordering::SeqCst), "waited for the shutdown");
        assert!(
            exit_raised
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "no exit of the gate's after the process took the exit over"
        );
    }

    /// A second unheld exit waits for the first's shutdown.
    #[test]
    fn a_second_exit_without_a_request_waits_for_the_first_ones_shutdown() {
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;
        let gate = ExitGate::default();
        let saved = Arc::new(AtomicBool::new(false));
        let (begun, shutdown_begun) = std::sync::mpsc::channel();
        let first = {
            let (gate, saved) = (gate.clone(), saved.clone());
            std::thread::spawn(move || {
                gate.exiting(PATIENCE, move || {
                    begun.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(200));
                    saved.store(true, Ordering::SeqCst);
                });
            })
        };
        shutdown_begun
            .recv_timeout(PATIENCE)
            .expect("the first shutdown began");
        let second = gate.clone();
        on_own_thread(PATIENCE, "the second exit returned", move || {
            second.exiting(PATIENCE, || panic!("a second shutdown"));
        });
        assert!(
            saved.load(Ordering::SeqCst),
            "waited for the first shutdown"
        );
        on_own_thread(PATIENCE, "the first exit returned", move || {
            first.join().unwrap();
        });
    }

    #[test]
    fn a_background_failure_warns_without_its_text() {
        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        let meeting_id = uuid::Uuid::new_v4();
        log_operation_failure(&MeetingEvent::OperationFailed {
            meeting_id,
            operation: steno_core::MeetingOperation::SummaryRerun,
            stage: steno_core::PipelineStage::Summarize,
            failure: "summarize: the model said: I cannot summarise this".to_owned(),
        });
        let text = log.text();
        let line = text
            .lines()
            .find(|line| line.contains(&meeting_id.to_string()))
            .unwrap_or_else(|| panic!("no line for the meeting: {text}"));
        assert!(line.contains("Summary re-run"), "{line}");
        assert!(line.contains("stage=\"summarize\""), "{line}");
        assert!(!text.contains("the model said"), "{text}");
    }

    /// The shell's update source makes the schedule the host's updater;
    /// without one the updater is the fake.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_update_source_the_shell_passes_drives_the_hosts_updater() {
        struct NoUpdates;
        #[async_trait::async_trait]
        impl UpdateSource for NoUpdates {
            async fn check(&self) -> Result<Option<String>, String> {
                Ok(None)
            }
            async fn download(&self, _version: &str) -> Result<Vec<u8>, String> {
                Ok(Vec::new())
            }
            async fn install(&self, _version: &str, _package: Vec<u8>) -> Result<(), String> {
                Ok(())
            }
            async fn relaunch(&self) {}
            async fn ask(&self, _question: crate::updates::Question<'_>) -> bool {
                false
            }
            fn installer(&self) -> crate::updates::Installer {
                crate::updates::Installer::InPlace
            }
            fn tell_install_failed(&self, _message: &str) {}
            fn tell_relaunch_waits(&self, _version: &str, _busy: crate::updates::Busy) {}
            fn announce(&self, _version: &str) {}
        }
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        let app = build(AppOptions {
            update_source: Some(Arc::new(NoUpdates)),
            ..options_under(&support)
        })
        .unwrap();
        let updates = app.updates.clone().expect("the schedule");
        assert!(app.services.updater.can_check_for_updates());
        app.services.updater.set_automatically_downloads(true);
        assert!(
            app.services
                .preferences
                .flag(crate::updates::AUTOMATIC_DOWNLOAD_KEY)
        );
        assert_eq!(updates.check_on_request().await, Ok(None));
        assert!(app.services.updater.last_check_at().is_some());
        assert!(support.join(crate::updates::LAST_CHECK_FILE).is_file());
        assert!(app.services.qr.png_base64("steno://pair/v1").is_some());
        drop(app);

        let dir = tempfile::tempdir().unwrap();
        let app = build(options_under(&dir.path().join("support"))).unwrap();
        assert!(app.updates.is_none());
        assert!(app.services.updater.last_check_at().is_none());
    }

    /// After a refused keychain read the keychain answers no key, but the
    /// secrets file's key still answers.
    #[tokio::test]
    async fn a_refused_key_read_gates_the_keychain_and_not_the_secrets_file() {
        use steno_host::services::Preferences as _;

        let dir = tempfile::tempdir().unwrap();
        let preferences = Arc::new(FilePreferences::in_support_directory(dir.path()));
        preferences.set_flag(crate::swift_import::KEY_DENIED_KEY, true);
        let paths = StenoPaths::new(dir.path().to_path_buf());
        let secrets = Arc::new(steno_core::testing::InMemorySecretStore::new());
        let key = SecretKey::llm_api_key();
        secrets.set_secret(&key, Some("sk-stored")).await.unwrap();
        for (file, expected) in [(false, None), (true, Some("sk-stored"))] {
            let (import, gated) = gated_secrets(None, &preferences, secrets.clone(), &paths, file);
            assert!(import.is_none());
            assert_eq!(
                gated.secrets.secret(&key).await.unwrap().as_deref(),
                expected,
                "file: {file}"
            );
        }
    }

    /// A paired phone with its identity gone: the launch builds no listener
    /// and mints nothing; the handover waits for Pair again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_launch_mints_no_identity_over_paired_phones() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        let first = build(options_under(&support)).unwrap();
        assert!(first.handover.is_some());
        crate::testing::pair_a_phone(&first.store);
        first.shutdown();
        drop(first);
        let secrets = support.join("secrets.json");
        std::fs::remove_file(&secrets).unwrap();

        let again = build(options_under(&support)).unwrap();
        let handover = again.handover.clone().unwrap();
        assert!(handover.listener().is_none());
        assert_eq!(
            steno_host::services::Handover::waiting(handover.as_ref()),
            Some(steno_bridge::PhoneWaitingReason::Missing)
        );
        assert!(!secrets.exists(), "an identity was minted");
    }

    /// A second app on one database is refused before it opens it; once the
    /// first is gone, the next one builds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_app_on_one_database_is_refused_until_the_first_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        let first = build(options_under(&support)).unwrap();
        let mut recording = steno_core::testing::sample_data::meeting();
        recording.state = steno_core::MeetingState::Recording;
        first.store.save_meeting(&recording).unwrap();

        let mut second = options_under(&support);
        second.lock_patience = std::time::Duration::ZERO;
        let refused = build(second).err().unwrap();
        assert!(
            matches!(&refused, BuildError::Lock(DatabaseLockError::Held(path)) if path == &support.join("steno.lock")),
            "{refused:?}"
        );
        assert_eq!(
            first.store.meeting(recording.id).unwrap().unwrap().state,
            steno_core::MeetingState::Recording,
            "the refused app left the first one's recording alone"
        );

        drop(first);
        build(options_under(&support)).unwrap();
    }

    /// The lock comes before the database: a second app over a non-database
    /// file fails with `Held`, not with the store's error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_lock_is_taken_before_the_database_is_opened() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        std::fs::create_dir_all(&support).unwrap();
        let garbage = support.join("garbage.sqlite");
        std::fs::write(&garbage, b"not a database, not even close").unwrap();
        let _first = DatabaseLock::acquire(&garbage).unwrap();
        let mut options = options_under(&support);
        options.database_path = Some(garbage.clone());
        options.lock_patience = std::time::Duration::ZERO;
        let refused = build(options).err().unwrap();
        assert!(
            matches!(&refused, BuildError::Lock(DatabaseLockError::Held(_))),
            "{refused:?}"
        );
    }

    /// An update's relaunch waits out the old app's lock within its patience,
    /// and is refused once it has passed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_relaunched_app_waits_for_the_old_one_within_its_patience() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        std::fs::create_dir_all(&support).unwrap();
        let database = support.join("steno.sqlite");

        let old = DatabaseLock::acquire(&database).unwrap();
        let exit = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(old);
        });
        let mut options = options_under(&support);
        options.lock_patience = LOCK_PATIENCE;
        let relaunched = build(options).unwrap();
        exit.join().unwrap();
        assert!(relaunched.database_lock.is_some());
        drop(relaunched);

        let _never_exits = DatabaseLock::acquire(&database).unwrap();
        let mut options = options_under(&support);
        options.lock_patience = std::time::Duration::from_millis(300);
        let started = std::time::Instant::now();
        let refused = build(options).err().unwrap();
        assert!(
            matches!(&refused, BuildError::Lock(DatabaseLockError::Held(_))),
            "{refused:?}"
        );
        assert!(started.elapsed() >= std::time::Duration::from_millis(300));
    }

    #[test]
    fn a_database_folder_that_cannot_be_created_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"").unwrap();
        let error = open_store(&file.join("steno.sqlite")).err().unwrap();
        assert!(matches!(error, BuildError::DatabaseFolder(_)));
        assert!(
            error
                .to_string()
                .starts_with("Could not create the database folder: "),
            "{error}"
        );
    }

    /// A failing checkpoint keeps the handover off before an identity is
    /// minted; once it succeeds the listener is built.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_store_that_cannot_sync_keeps_the_handover_off() {
        let (dir, store) = temp_store();
        let pipeline = crate::testing::current_pipeline(fake_dependencies(&store, "fake-engine"));
        let memory = Arc::new(steno_core::testing::InMemorySecretStore::new());
        let secrets: Arc<dyn SecretStore> = memory.clone();
        let runtime = tokio::runtime::Handle::current();
        let paths = StenoPaths::new(dir.path().join("support"));
        let source = identity_source(
            &store,
            &pipeline,
            &secrets,
            &paths,
            local_zone(),
            &runtime,
            None,
        );
        let listener = || handover_listener(&source);
        store
            .on_writer(|connection| connection.busy_timeout(std::time::Duration::ZERO))
            .unwrap();
        let writer = rusqlite::Connection::open(dir.path().join("steno.sqlite")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();

        let error = listener().err().unwrap();
        assert!(
            error.starts_with("the database could not be synced to the disk: "),
            "{error}"
        );
        assert!(memory.keys().is_empty(), "no identity is minted");

        writer.execute_batch("ROLLBACK").unwrap();
        assert!(listener().is_ok());
    }

    /// A secret store whose reads fail with `failure` while it is set.
    #[derive(Default)]
    struct UnavailableSecrets {
        failure: std::sync::Mutex<Option<KeyringUnavailable>>,
        inner: steno_core::testing::InMemorySecretStore,
    }

    impl UnavailableSecrets {
        fn fail_with(&self, failure: Option<KeyringUnavailable>) {
            *self.failure.lock().unwrap() = failure;
        }
    }

    #[async_trait]
    impl SecretStore for UnavailableSecrets {
        async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
            if let Some(failure) = self.failure.lock().unwrap().clone() {
                return Err(Box::new(failure));
            }
            self.inner.secret(key).await
        }

        async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
            self.inner.set_secret(key, value).await
        }

        /// Undecided while the keyring asks, as the Linux store.
        fn place(&self) -> Option<steno_core::SecretPlace> {
            let asking = *self.failure.lock().unwrap() == Some(KeyringUnavailable::Unlocking);
            (!asking).then_some(steno_core::SecretPlace::Keyring)
        }
    }

    /// An app launched while the keyring asks the user: reads are turned
    /// away, a queued meeting waits for the recovery, and the handover waits
    /// for its listener. `answer` makes the keyring answer.
    struct AskedAtLaunch {
        _dir: tempfile::TempDir,
        app: App,
        host: Arc<Host>,
        secrets: Arc<UnavailableSecrets>,
        handover: Arc<ListenerHandover>,
        keys_read: Arc<std::sync::Mutex<Vec<Option<String>>>>,
        queued: uuid::Uuid,
        /// A meeting the last process left recording, with no audio.
        interrupted: uuid::Uuid,
        paired: Vec<PairedDevice>,
        answered: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl AskedAtLaunch {
        async fn new(paired: bool) -> Self {
            let (dir, store) = temp_store();
            let mut app = recording_app(&dir, &store);
            let secrets = Arc::new(UnavailableSecrets::default());
            let key = SecretKey::llm_api_key();
            secrets.inner.set_secret(&key, Some("sk-1")).await.unwrap();
            let mut phones = Vec::new();
            if paired {
                let identity = steno_handover::HandoverIdentity::mint("x", Utc::now()).unwrap();
                let record = crate::handover::FingerprintFile::in_support_directory(
                    &app.paths.support_directory,
                );
                identity.store(&secrets.inner, &record).await.unwrap();
                let phone = PairedDevice {
                    id: uuid::Uuid::new_v4(),
                    name: "Phone".to_owned(),
                    paired_at: Utc::now(),
                    last_seen_at: None,
                };
                store.save_paired_device(&phone, &[1; 32]).unwrap();
                phones = store.paired_devices().unwrap();
            }
            secrets.fail_with(Some(KeyringUnavailable::Unlocking));
            app.secrets = secrets.clone();
            app.services.secrets = secrets.clone();

            let keys_read = Arc::new(std::sync::Mutex::new(Vec::new()));
            let make: MakeDependencies = {
                let (store, secrets, keys_read) =
                    (store.clone(), app.secrets.clone(), keys_read.clone());
                Arc::new(move || {
                    let read = api_key(secrets.as_ref(), &tokio::runtime::Handle::current());
                    keys_read.lock().unwrap().push(read.ok().flatten());
                    Ok(built(fake_dependencies(&store, "fake-engine")))
                })
            };
            app.pipeline = Arc::new(CurrentPipeline::new(
                make().unwrap(),
                make,
                app.runtime.clone(),
            ));
            let mut meeting = steno_core::testing::sample_data::meeting();
            meeting.state = steno_core::MeetingState::Queued;
            store.save_meeting(&meeting).unwrap();
            let audio_folder = store.settings().unwrap().audio_folder;
            std::fs::create_dir_all(steno_core::paths::file_url_path(&audio_folder).unwrap())
                .unwrap();
            let mut interrupted = steno_core::testing::sample_data::meeting();
            interrupted.id = uuid::Uuid::new_v4();
            interrupted.state = steno_core::MeetingState::Recording;
            store.save_meeting(&interrupted).unwrap();

            let handover = handover_listener(&identity_source(
                &app.store,
                &app.pipeline,
                &app.secrets,
                &app.paths,
                app.zone,
                &app.runtime,
                None,
            ))
            .unwrap();
            app.handover = Some(handover.clone());
            app.services.handover =
                Some(handover.clone() as Arc<dyn steno_host::services::Handover>);
            let (answered, unlocked) = tokio::sync::oneshot::channel::<()>();
            *app.secrets_unlocked.get_mut().unwrap() =
                Some(Box::pin(async move { unlocked.await.is_ok() }));
            let host = Arc::new(app.host().unwrap());
            app.launch(&host);
            AskedAtLaunch {
                _dir: dir,
                app,
                host,
                secrets,
                handover,
                keys_read,
                queued: meeting.id,
                interrupted: interrupted.id,
                paired: phones,
                answered: Some(answered),
            }
        }

        /// The keyring answers; reads fail with `failure` from now on.
        fn answer(&mut self, failure: Option<KeyringUnavailable>) {
            self.secrets.fail_with(failure);
            self.answered.take().unwrap().send(()).unwrap();
        }

        fn queued_state(&self) -> steno_core::MeetingState {
            self.app.store.meeting(self.queued).unwrap().unwrap().state
        }

        fn interrupted_state(&self) -> steno_core::MeetingState {
            self.app
                .store
                .meeting(self.interrupted)
                .unwrap()
                .unwrap()
                .state
        }

        /// Waits until the crash recovery ran, which ends the reread.
        async fn until_recovered(&self) {
            tokio::time::timeout(HANG_GUARD, async {
                while self.queued_state() == steno_core::MeetingState::Queued {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the meetings were recovered after the answer");
        }

        fn snapshot(&self, topic: BridgeTopic) -> serde_json::Value {
            let host = self.host.clone();
            on_own_thread(PATIENCE, "the snapshot was built", move || {
                host.snapshot(topic).unwrap()
            })
        }

        fn handover_state(&self) -> ListenerState {
            steno_host::services::Handover::state(self.handover.as_ref())
        }
    }

    impl Drop for AskedAtLaunch {
        fn drop(&mut self) {
            if let Some(listener) = self.handover.listener() {
                block_on(&self.app.runtime, listener.stop());
            }
        }
    }

    /// Once the keyring answered, the launch rebuilds the pipeline with the
    /// key, reads the identity again, hands the listener over, and only then
    /// recovers the meetings.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_handover_read_while_the_keyring_asked_gets_its_listener_once_it_answers() {
        let mut asked = AskedAtLaunch::new(false).await;
        let handover = asked.handover.clone();
        assert!(handover.listener().is_none());
        let ListenerState::Failed(reason) = asked.handover_state() else {
            panic!("a waiting handover reads as failed");
        };
        assert!(reason.contains("waiting for an answer"), "{reason}");
        assert!(steno_host::services::Handover::start(handover.as_ref()).is_err());
        assert_eq!(
            asked.secrets.inner.keys(),
            [SecretKey::llm_api_key()],
            "nothing minted"
        );
        assert_eq!(*asked.keys_read.lock().unwrap(), [None]);
        let summaries = asked.snapshot(BridgeTopic::SettingsSummaries);
        assert_eq!(summaries["hasAPIKey"], false);
        assert!(summaries.get("keyStore").is_none(), "{summaries}");
        // No speech model is installed, so onboarding waits on page 1.
        let host = asked.host.clone();
        on_own_thread(PATIENCE, "onboarding moved on", move || {
            steno_bridge::BridgeHost::onboarding_advance(&host).unwrap();
        });
        let onboarding = asked.snapshot(BridgeTopic::Onboarding);
        assert_eq!(
            onboarding["summaries"]["error"],
            steno_host::settings::KeyRead::UNREADABLE,
            "{onboarding}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            asked.queued_state(),
            steno_core::MeetingState::Queued,
            "no recovery on the pipeline built without the key"
        );
        assert_eq!(
            asked.interrupted_state(),
            steno_core::MeetingState::Recording,
            "nor of an interrupted recording"
        );

        asked.answer(None);
        asked.until_recovered().await;
        asked.app.launch_finished().await;
        assert!(
            matches!(
                asked.interrupted_state(),
                steno_core::MeetingState::Failed { .. }
            ),
            "the interrupted recording is settled after the answer"
        );
        assert!(handover.listener().is_some());
        assert_ne!(
            steno_host::services::Handover::mac_id(handover.as_ref()),
            ""
        );
        assert_eq!(
            asked.secrets.inner.keys(),
            [
                steno_handover::HandoverIdentity::secret_key(),
                SecretKey::llm_api_key()
            ],
            "minted once the store answered, with no phone paired"
        );
        assert_eq!(
            *asked.keys_read.lock().unwrap(),
            [None, Some("sk-1".to_owned())],
            "the pipeline was built again, with the key"
        );
        let summaries = asked.snapshot(BridgeTopic::SettingsSummaries);
        assert_eq!(summaries["hasAPIKey"], true);
        assert_eq!(summaries["keyStore"], "keyring", "{summaries}");
        let onboarding = asked.snapshot(BridgeTopic::Onboarding);
        assert!(
            onboarding["summaries"].get("error").is_none(),
            "onboarding read the key again: {onboarding}"
        );
        assert_eq!(onboarding["summaries"]["keyStore"], "keyring");
        assert_eq!(asked.handover_state(), ListenerState::Stopped);
    }

    /// A paired phone's listener starts once the keyring answered.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_paired_phones_listener_starts_once_the_keyring_answered() {
        let mut asked = AskedAtLaunch::new(true).await;
        assert_eq!(
            steno_host::services::Handover::paired_devices(asked.handover.as_ref()).unwrap(),
            asked.paired,
            "the paired phones come from the database meanwhile"
        );
        assert_eq!(asked.paired.len(), 1);
        asked.answer(None);
        asked.until_recovered().await;
        assert!(
            matches!(asked.handover_state(), ListenerState::Listening(_)),
            "{:?}",
            asked.handover_state()
        );
        let phone = asked.snapshot(BridgeTopic::SettingsPhone);
        assert_eq!(phone["listener"]["state"], "listening", "{phone}");
    }

    /// A quit after the answer stops the listener the reread started.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quit_stops_the_listener_started_after_the_answer() {
        let mut asked = AskedAtLaunch::new(true).await;
        asked.answer(None);
        asked.until_recovered().await;
        assert!(matches!(
            asked.handover_state(),
            ListenerState::Listening(_)
        ));
        let app = &asked.app;
        tokio::task::block_in_place(|| app.shutdown());
        assert_eq!(asked.handover_state(), ListenerState::Stopped);
    }

    /// A store that chose without turning a read away recovers with no reread.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_choice_that_turned_nothing_away_still_recovers_the_meetings() {
        let mut asked = AskedAtLaunch::new(false).await;
        asked.secrets.fail_with(None);
        drop(asked.answered.take());
        asked.until_recovered().await;
        assert_eq!(*asked.keys_read.lock().unwrap(), [None], "no reread");
    }

    /// A quit before the answer leaves nothing listening.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quit_before_the_keyring_answered_leaves_no_listener_running() {
        let mut asked = AskedAtLaunch::new(true).await;
        let app = &asked.app;
        tokio::task::block_in_place(|| app.shutdown());
        asked.answer(None);
        tokio::time::timeout(PATIENCE, async {
            while asked.keys_read.lock().unwrap().len() < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the reread ran");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(asked.handover.listener().is_none());
    }

    /// A reread that still fails keeps the handover waiting, with the new
    /// reason, and recovers the meetings anyway.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reread_that_fails_again_says_why_and_recovers_the_meetings() {
        let mut asked = AskedAtLaunch::new(true).await;
        asked.answer(Some(KeyringUnavailable::Locked));
        asked.until_recovered().await;
        assert!(asked.handover.listener().is_none());
        let ListenerState::Failed(reason) = asked.handover_state() else {
            panic!("still waiting");
        };
        assert!(reason.contains("locked"), "{reason}");
        let phone = asked.snapshot(BridgeTopic::SettingsPhone);
        assert!(
            phone["listener"]["failure"]
                .as_str()
                .is_some_and(|failure| failure.contains("locked")),
            "{phone}"
        );
    }

    /// Any other identity read failure leaves the handover waiting for Try
    /// again, with the reason, and mints nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_identity_read_that_fails_otherwise_waits_for_try_again() {
        use steno_host::services::Handover as _;

        let (dir, store) = temp_store();
        let pipeline = crate::testing::current_pipeline(fake_dependencies(&store, "fake-engine"));
        let paths = StenoPaths::new(dir.path().join("support"));
        for failure in [
            KeyringUnavailable::Locked,
            KeyringUnavailable::NotOpened(steno_handover::HandoverIdentity::SECRET_KEY.to_owned()),
        ] {
            let unavailable = Arc::new(UnavailableSecrets::default());
            unavailable.fail_with(Some(failure.clone()));
            let secrets: Arc<dyn SecretStore> = unavailable.clone();
            let handover = handover_listener(&identity_source(
                &store,
                &pipeline,
                &secrets,
                &paths,
                local_zone(),
                &tokio::runtime::Handle::current(),
                None,
            ))
            .unwrap();
            let expected = match failure {
                // Its own sentence names the pairing.
                KeyringUnavailable::NotOpened(_) => failure.to_string(),
                _ => format!("this computer's phone pairing could not be read ({failure})"),
            };
            assert_eq!(
                handover.state(),
                ListenerState::Failed(format!("Phone handover is unavailable: {expected}"))
            );
            assert_eq!(
                handover.waiting(),
                Some(steno_bridge::PhoneWaitingReason::Unreadable)
            );
            let error = tokio::task::block_in_place(|| handover.try_again()).unwrap_err();
            assert_eq!(error.to_string(), expected, "still waiting");
            assert!(unavailable.inner.keys().is_empty(), "nothing minted");
        }
    }

    /// Counts the reads of the handover identity.
    #[derive(Default)]
    struct CountingIdentityReads {
        inner: steno_core::testing::InMemorySecretStore,
        reads: AtomicUsize,
    }

    #[async_trait]
    impl SecretStore for CountingIdentityReads {
        async fn secret(&self, key: &SecretKey) -> BoundaryResult<Option<String>> {
            if key.as_str() == steno_handover::HandoverIdentity::SECRET_KEY {
                self.reads.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.secret(key).await
        }

        async fn set_secret(&self, key: &SecretKey, value: Option<&str>) -> BoundaryResult<()> {
            self.inner.set_secret(key, value).await
        }
    }

    /// The listener reads the identity once: a stored one, a minted one while
    /// no phone is paired, and none once a phone is paired and it is gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_listener_reads_the_identity_once_and_mints_none_over_paired_phones() {
        let dir = tempfile::tempdir().unwrap();
        let app = build(options_under(&dir.path().join("support"))).unwrap();
        let runtime = tokio::runtime::Handle::current();
        // A record of its own: the app's build minted and recorded one.
        let paths = StenoPaths::new(dir.path().join("listener"));
        let listener = |secrets: &Arc<CountingIdentityReads>| {
            let secrets: Arc<dyn SecretStore> = secrets.clone();
            let (store, pipeline, paths, zone) = (
                app.store.clone(),
                app.pipeline.clone(),
                paths.clone(),
                app.zone,
            );
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                let source =
                    identity_source(&store, &pipeline, &secrets, &paths, zone, &runtime, None);
                let handover = handover_listener(&source).unwrap();
                match steno_host::services::Handover::state(handover.as_ref()) {
                    ListenerState::Failed(reason) => Err(reason),
                    _ => Ok(steno_host::services::Handover::mac_id(handover.as_ref())),
                }
            })
            .join()
            .unwrap()
        };
        let minting = Arc::new(CountingIdentityReads::default());
        let minted = listener(&minting).unwrap();
        assert_eq!(minting.reads.load(Ordering::SeqCst), 1, "minted");
        assert_eq!(listener(&minting).unwrap(), minted, "the stored one");
        assert_eq!(minting.reads.load(Ordering::SeqCst), 2, "one read each");

        crate::testing::pair_a_phone(&app.store);
        let lost = Arc::new(CountingIdentityReads::default());
        let refused = listener(&lost).unwrap_err();
        assert!(refused.contains("no longer saved"), "{refused}");
        assert_eq!(lost.reads.load(Ordering::SeqCst), 1);
        assert_eq!(
            block_on(
                &runtime,
                lost.inner
                    .secret(&steno_handover::HandoverIdentity::secret_key())
            )
            .unwrap(),
            None,
            "an identity was minted over a paired phone"
        );
        app.shutdown();
    }

    /// A secret store whose reads fail.
    struct BrokenSecrets;

    #[async_trait]
    impl SecretStore for BrokenSecrets {
        async fn secret(&self, _key: &SecretKey) -> BoundaryResult<Option<String>> {
            Err("no default keychain".into())
        }

        async fn set_secret(&self, _key: &SecretKey, _value: Option<&str>) -> BoundaryResult<()> {
            Err("no default keychain".into())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_secret_store_that_cannot_be_read_leaves_the_pipeline_without_a_key() {
        let (dir, store) = temp_store();
        let secrets = KeepsApiKey::new(Arc::new(BrokenSecrets));
        let paths = StenoPaths::new(dir.path().join("support"));
        let built = pipeline_dependencies(
            &store,
            &SpeechEngines::new(SpeechSetup::new(&store.settings().unwrap(), &paths)),
            &ungated(Arc::new(BrokenSecrets)),
            &codex_store(),
            &MeetingEventBus::new(),
            &tokio::runtime::Handle::current(),
        )
        .expect("the graph builds without the key");
        assert!(built.dependencies.cleaner.is_none());
        assert_eq!(
            api_key(&secrets, &tokio::runtime::Handle::current()).unwrap_err(),
            "Could not read the LLM API key from the secret store: no default keychain"
        );
    }

    /// `platform` behind the app's [`KeepsApiKey`] and no gate.
    fn ungated(platform: Arc<dyn SecretStore>) -> GraphSecrets {
        let kept = Arc::new(KeepsApiKey::new(platform));
        GraphSecrets {
            secrets: kept.clone(),
            kept,
            key_gate: None,
        }
    }

    /// The Authorization header a reload's pipeline over `secrets` sends
    /// to `server`.
    async fn authorization_sent(
        store: &Arc<Store>,
        paths: &StenoPaths,
        secrets: &GraphSecrets,
        server: &steno_llm::testing::StubChatServer,
    ) -> Option<String> {
        let built = pipeline_dependencies(
            store,
            &SpeechEngines::new(SpeechSetup::new(&store.settings().unwrap(), paths)),
            secrets,
            &codex_store(),
            &MeetingEventBus::new(),
            &tokio::runtime::Handle::current(),
        )
        .unwrap();
        let cleaner = built.dependencies.cleaner.expect("an endpoint is set");
        let before = server.request_count();
        let segment = steno_core::TranscriptSegment {
            id: uuid::Uuid::from_u128(1),
            meeting_id: uuid::Uuid::from_u128(2),
            start: 0.0,
            end: 2.0,
            speaker_id: None,
            lane: steno_core::AudioLane::Mic,
            text: "hello there".to_owned(),
            raw_text: "hello there".to_owned(),
        };
        let input = steno_core::CleanupInput {
            segments: vec![segment],
            language: None,
            participants: Vec::new(),
            speakers: Vec::new(),
            known_people: Vec::new(),
        };
        let _ = cleaner.clean(&input).await;
        server.requests()[before].authorization().map(str::to_owned)
    }

    /// A keyring locked again fails a rebuild's read: the pipeline keeps its
    /// key. A key removed or changed is never the one kept.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pipeline_rebuilt_while_the_keyring_is_locked_keeps_the_key_it_had() {
        let (dir, store) = temp_store();
        let paths = StenoPaths::new(dir.path().join("support"));
        let server = steno_llm::testing::StubChatServer::start().await.unwrap();
        let mut settings = store.settings().unwrap();
        settings.llm_provider = steno_core::LlmProvider::Endpoint;
        settings.llm_base_url = Some(server.base_url().to_string());
        settings.llm_model = Some("model".to_owned());
        store.save_settings(&settings).unwrap();
        let key = SecretKey::llm_api_key();
        let memory = Arc::new(steno_core::testing::InMemorySecretStore::with([(
            key.clone(),
            "sk-1".to_owned(),
        )]));
        let secrets = ungated(memory.clone());
        let kept = secrets.kept.clone();
        let sent = || authorization_sent(&store, &paths, &secrets, &server);
        assert_eq!(sent().await.as_deref(), Some("Bearer sk-1"));

        memory.fail_reads(Some("the keyring is locked"));
        assert_eq!(sent().await.as_deref(), Some("Bearer sk-1"), "kept");
        kept.set_secret(&key, Some("sk-2")).await.unwrap();
        assert_eq!(sent().await.as_deref(), Some("Bearer sk-2"), "changed");
        kept.set_secret(&key, None).await.unwrap();
        assert_eq!(sent().await, None, "a removal while locked keeps no key");

        memory.fail_reads(None);
        memory.set_secret(&key, Some("sk-3")).await.unwrap();
        assert_eq!(sent().await.as_deref(), Some("Bearer sk-3"));
        memory.set_secret(&key, None).await.unwrap();
        assert_eq!(sent().await, None, "a read of no key drops it");
        memory.fail_reads(Some("the keyring is locked"));
        assert_eq!(sent().await, None);
        server.stop();
    }

    /// Fails unless the runtime's one worker runs tasks spawned while a
    /// reload is held at the gate.
    fn assert_the_worker_runs(runtime: &tokio::runtime::Runtime, what: &str) {
        for _ in 0..3 {
            let (ran, runs) = std::sync::mpsc::channel();
            runtime.spawn(async move {
                let _ = ran.send(());
            });
            runs.recv_timeout(HANG_GUARD)
                .unwrap_or_else(|_| panic!("{what}: the runtime's one worker is blocked"));
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// The event follower and the reload owner reach the host on the blocking
    /// pool: while a reload is held at a gate, a task on the one-worker
    /// services runtime still runs.
    #[test]
    fn the_poll_and_the_events_task_never_block_a_worker() {
        let runtime = one_worker_runtime();
        let dir = tempfile::tempdir().unwrap();
        let (app, pipeline, host) = launched_over_a_gated_pipeline(&runtime, dir.path());
        let gate = &pipeline.gate;

        let armed = gate.arm();
        app.events.post(MeetingEvent::Deleted {
            meeting_id: uuid::Uuid::new_v4(),
        });
        assert!(
            gate.wait_for(HANG_GUARD, |state| state.entered == 1),
            "the event's reload reaches the gate"
        );
        app.events.post(MeetingEvent::Deleted {
            meeting_id: uuid::Uuid::new_v4(),
        });
        assert_the_worker_runs(&runtime, "the event follower");
        drop(armed);
        assert!(
            gate.wait_for(HANG_GUARD, |state| state.held == 0),
            "the event's reload goes on"
        );
        settled(&host);

        // No event now: the reload the gate holds is the one asked for.
        let armed = gate.arm();
        host.request_reload();
        assert!(
            gate.wait_for(HANG_GUARD, |state| state.entered == 2),
            "the poll's reload reaches the gate"
        );
        assert_the_worker_runs(&runtime, "the poll");
        drop(armed);
        runtime.shutdown_background();
    }

    /// Waits until the reload owner has nothing left to reload.
    fn settled(host: &Host) {
        host.store_changed();
        std::thread::sleep(std::time::Duration::from_millis(300));
        host.store_changed();
    }

    /// The update and phone hooks reach the host on the blocking pool: called
    /// while the host's lock is held through a busy save, they return at
    /// once, and the phone section is published once the lock goes.
    #[test]
    fn the_update_and_phone_hooks_never_wait_on_the_hosts_lock() {
        use steno_bridge::BridgeHost as _;
        let runtime = one_worker_runtime();
        let (dir, store) = temp_store();
        let services = steno_host::fakes::FakeServices::new(Utc::now()).services();
        let host = Arc::new(Host::new(store, services, steno_host::HostConfig::default()).unwrap());
        let sink = Arc::new(crate::testing::TimedSink::default());
        host.attach(sink.clone());
        host.page_ready().unwrap();
        let mut held =
            crate::testing::HeldStore::lock_only(&crate::testing::database_in(dir.path()));
        let saving = std::thread::spawn({
            let host = host.clone();
            move || {
                host.settings_general_set_detection(steno_bridge::SetBoolParams { value: false })
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(200));

        let (updates, phones) = (
            updates_hook(&host, runtime.handle()),
            phones_hook(&host, runtime.handle()),
        );
        let (returned, hooks_returned) = std::sync::mpsc::channel();
        let since = std::time::Instant::now();
        runtime.spawn(async move {
            updates();
            phones();
            let _ = returned.send(std::time::Instant::now());
        });
        let took = hooks_returned
            .recv_timeout(HANG_GUARD)
            .expect("the hooks returned")
            .duration_since(since);
        assert!(
            took < crate::testing::LIVE,
            "the hooks held the runtime's worker for {took:?}"
        );
        assert!(!saving.is_finished(), "the save still waits");

        held.release();
        let _ = saving.join().unwrap();
        crate::testing::found_within(HANG_GUARD, "the phone section was published", || {
            sink.since(BridgeTopic::SettingsPhone, since).pop()
        });
        runtime.shutdown_background();
    }

    /// A panicking host notification is logged at `error` under its name,
    /// without the panic's message.
    #[test]
    fn a_notification_that_panics_is_logged_without_its_message() {
        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        let runtime = one_worker_runtime();
        notify_on_the_blocking_pool(runtime.handle(), "the_panicking_test_notification", || {
            panic!("at /a/private/folder")
        });
        let line =
            crate::testing::found_within(crate::testing::PATIENCE, "the panic was logged", || {
                log.text()
                    .lines()
                    .find(|line| line.contains("the_panicking_test_notification"))
                    .map(str::to_owned)
            });
        assert!(line.contains("ERROR"), "{line}");
        assert!(line.contains("a host notification panicked"), "{line}");
        assert!(!log.text().contains("/a/private/folder"), "{line}");
        runtime.shutdown_background();
    }

    /// A services runtime with one worker, so a wait on it shows.
    fn one_worker_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    /// The launch on `runtime` over [`GatedFiles`], with a detail shown so
    /// each reload asks the gate, its own reloads over.
    fn launched_over_a_gated_pipeline(
        runtime: &tokio::runtime::Runtime,
        dir: &std::path::Path,
    ) -> (App, Arc<GatedFiles>, Arc<Host>) {
        let mut app =
            runtime.block_on(async { build(options_under(&dir.join("support"))).unwrap() });
        let pipeline = Arc::new(GatedFiles::default());
        app.services.file_system = pipeline.clone();
        let meeting = shown_meeting(&app.store);
        let host = Arc::new(app.host().unwrap());
        host.request_meeting(meeting.id);
        assert!(pipeline.gate.reloads() > 0, "the meeting's detail is shown");
        {
            let _inside = runtime.enter();
            app.launch(&host);
        }
        runtime.block_on(app.launch_finished());
        settled(&host);
        (app, pipeline, host)
    }

    /// A host over `store` and the fakes, with `pipeline` if given, the page
    /// ready and a detail shown, and its [`TimedSink`].
    fn idle_host(
        store: &Arc<Store>,
        fakes: &steno_host::fakes::FakeServices,
        pipeline: Option<Arc<GatedFiles>>,
    ) -> (Arc<Host>, Arc<TimedSink>) {
        use steno_bridge::BridgeHost as _;
        let meeting = shown_meeting(store);
        let mut services = fakes.services();
        if let Some(pipeline) = pipeline {
            services.file_system = pipeline;
        }
        let host = Arc::new(
            Host::new(store.clone(), services, steno_host::HostConfig::default()).unwrap(),
        );
        let sink = Arc::new(TimedSink::default());
        host.attach(sink.clone());
        host.page_ready().unwrap();
        host.request_meeting(meeting.id);
        (host, sink)
    }

    /// Waits until `n` reload owner passes ran since `since`.
    fn passes(sink: &TimedSink, since: std::time::Instant, n: usize) {
        found_within(PATIENCE, "the reload owner's passes", || {
            (sink.since(BridgeTopic::SettingsPhone, since).len() >= n).then_some(())
        });
    }

    /// With nothing committed, ten passes send no `meetings.list`,
    /// `meeting.detail` or `progress` snapshot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn an_idle_app_sends_no_list_detail_or_progress() {
        let (_dir, store) = temp_store();
        let fakes = steno_host::fakes::FakeServices::new(Utc::now());
        let (host, sink) = idle_host(&store, &fakes, None);
        let since = std::time::Instant::now();
        let owner = tokio::spawn(own_the_reloads(
            host.clone(),
            std::time::Duration::from_millis(50),
        ));
        passes(&sink, since, 10);
        owner.abort();
        for topic in [
            BridgeTopic::MeetingsList,
            BridgeTopic::MeetingDetail,
            BridgeTopic::Progress,
        ] {
            assert_eq!(
                sink.since(topic, since).len(),
                0,
                "{topic:?} sent by an idle app"
            );
        }
    }

    /// `writer` retitles the sample meeting, and the list shows it within one
    /// poll interval and [`LIVE`].
    fn a_retitle_shows_within_one_poll(store: &Arc<Store>, writer: &Store) {
        let fakes = steno_host::fakes::FakeServices::new(Utc::now());
        let (host, sink) = idle_host(store, &fakes, None);
        let owner = tokio::spawn(own_the_reloads(host.clone(), POLL_INTERVAL));
        passes(&sink, std::time::Instant::now(), 1);
        let since = std::time::Instant::now();
        let meeting = steno_core::testing::sample_data::meeting();
        writer
            .update_meeting(meeting.id, Utc::now(), |meeting| {
                "Retitled".clone_into(&mut meeting.title);
                meeting.title_origin = steno_core::TitleOrigin::User;
                Ok(())
            })
            .unwrap();
        let shown = found_within(POLL_INTERVAL + LIVE, "the retitled meeting listed", || {
            sink.since(BridgeTopic::MeetingsList, since)
                .into_iter()
                .find(|(_, list)| list.to_string().contains("Retitled"))
                .map(|(at, _)| at)
        });
        let took = shown.duration_since(since);
        assert!(
            took < POLL_INTERVAL + LIVE,
            "the list carried the title {took:?} after the commit"
        );
        owner.abort();
    }

    /// A retitle shows within one poll interval, from another connection
    /// (through the reader's `data_version`) and from an in-memory store's
    /// own writer (through its commit count).
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_commit_shows_within_one_poll() {
        let (dir, store) = temp_store();
        let other = Store::open(crate::testing::database_in(dir.path())).unwrap();
        a_retitle_shows_within_one_poll(&store, &other);
        let store = Arc::new(Store::in_memory().unwrap());
        a_retitle_shows_within_one_poll(&store, &store);
    }

    /// An event that commits nothing (`StoreBusy`, `OperationFailed`,
    /// `ModelsMissing`) still reloads the detail and the progress.
    #[test]
    fn an_event_without_a_commit_still_reloads() {
        use steno_bridge::BridgeHost as _;
        let runtime = one_worker_runtime();
        let dir = tempfile::tempdir().unwrap();
        let (app, pipeline, host) = launched_over_a_gated_pipeline(&runtime, dir.path());
        let sink = Arc::new(TimedSink::default());
        host.attach(sink.clone());
        host.page_ready().unwrap();
        let other = uuid::Uuid::new_v4();
        for event in [
            MeetingEvent::StoreBusy { meeting_id: other },
            MeetingEvent::OperationFailed {
                meeting_id: other,
                operation: steno_core::MeetingOperation::Reexport,
                stage: steno_core::PipelineStage::Deliver,
                failure: "deliver: the vault is gone".to_owned(),
            },
            MeetingEvent::ModelsMissing { meeting_id: other },
        ] {
            let name = format!("{event:?}");
            let before = pipeline.gate.reloads();
            let since = std::time::Instant::now();
            app.events.post(event);
            for topic in [BridgeTopic::MeetingDetail, BridgeTopic::Progress] {
                let at = found_within(PATIENCE, "the event's snapshots", || {
                    sink.since(topic, since).first().map(|(at, _)| *at)
                });
                let took = at.duration_since(since);
                assert!(
                    took < LIVE,
                    "{name}: {topic:?} came {took:?} after the event"
                );
            }
            assert!(
                pipeline.gate.reloads() > before,
                "{name}: the detail reloaded"
            );
        }
        runtime.shutdown_background();
    }

    /// A microphone permission granted in the system shows within one poll
    /// with nothing committed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_permission_granted_outside_the_app_shows_within_one_poll() {
        use steno_bridge::{PermissionKind, PermissionState};
        let (_dir, store) = temp_store();
        let fakes = steno_host::fakes::FakeServices::new(Utc::now());
        fakes
            .permissions
            .states
            .lock()
            .unwrap()
            .insert(PermissionKind::Microphone, PermissionState::Denied);
        let (host, sink) = idle_host(&store, &fakes, None);
        let denied = host.snapshot(BridgeTopic::SettingsRecording).unwrap()["subtitle"].clone();
        let owner = tokio::spawn(own_the_reloads(host.clone(), POLL_INTERVAL));
        passes(&sink, std::time::Instant::now(), 1);
        let since = std::time::Instant::now();
        fakes
            .permissions
            .states
            .lock()
            .unwrap()
            .insert(PermissionKind::Microphone, PermissionState::Granted);
        let shown = found_within(POLL_INTERVAL + LIVE, "the granted microphone", || {
            sink.since(BridgeTopic::SettingsRecording, since)
                .into_iter()
                .find(|(_, section)| section["subtitle"] != denied)
                .map(|(at, _)| at)
        });
        let took = shown.duration_since(since);
        assert!(
            took < POLL_INTERVAL + LIVE,
            "shown {took:?} after the grant"
        );
        owner.abort();
    }

    /// Ten requests during a held pass run exactly one more forced reload.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn reload_requests_during_a_pass_run_one_more_pass() {
        let (_dir, store) = temp_store();
        let fakes = steno_host::fakes::FakeServices::new(Utc::now());
        let pipeline = Arc::new(GatedFiles::default());
        let (host, sink) = idle_host(&store, &fakes, Some(pipeline.clone()));
        let gate = &pipeline.gate;
        let owner = tokio::spawn(own_the_reloads(
            host.clone(),
            std::time::Duration::from_secs(3600),
        ));
        passes(&sink, std::time::Instant::now(), 1);

        let before = gate.reloads();
        host.request_reload();
        assert!(gate.wait_for(PATIENCE, |state| state.reloads > before));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let per_reload = gate.reloads() - before;

        let base = gate.reloads();
        let armed = gate.arm();
        host.request_reload();
        assert!(
            gate.wait_for(PATIENCE, |state| state.entered == 1),
            "the pass is held"
        );
        for _ in 0..10 {
            host.request_reload();
        }
        drop(armed);
        let expected = base + 2 * per_reload;
        assert!(
            gate.wait_for(PATIENCE, |state| state.reloads >= expected),
            "one more reload ran"
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            gate.reloads(),
            expected,
            "the ten requests ran one more reload, {per_reload} gate calls each"
        );
        owner.abort();
    }
}
