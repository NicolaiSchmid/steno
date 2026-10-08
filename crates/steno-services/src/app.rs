//! [`build`]: the one function that turns the settings into the running
//! object graph, and [`App`], what it hands back. Swift: `AppEnvironment.live`
//! and `AppController.launch`.

use std::sync::Arc;

use chrono::{FixedOffset, Local, Offset as _};
use steno_adapters::DeliveryCoordinator;
use steno_audio::{CaptureSession, SymphoniaAudioCodec};
use steno_core::{
    DatabaseLock, DatabaseLockError, MeetingEvent, SecretKey, SecretStore, StenoPaths, Store,
    StoreError,
};
use steno_handover::{HandoverService, IdentityError, Unavailability};
use steno_host::fakes::{
    FakeClipPlayer, FakeFileSystem, FakeLoginItem, FakePermissions, FakeUpdater,
};
use steno_host::services::{LoginItem, LoginItemStatus, Opener, Preferences, Services};
use steno_host::{Host, HostConfig};
use steno_llm::CodexCredentialStore;
use steno_pipeline::{
    ExportRetries, MeetingEventBus, PipelineDependencies, RecordingIntake, RetentionSweep,
};

use crate::block_on;
use crate::handover::{ListenerHandover, start_if_paired};
use crate::llm::{ClientLlmService, codex_store};
use crate::pipeline::{
    BuiltEngine, BuiltPipeline, CurrentPipeline, HostPipeline, MakeDependencies, run_sweep,
};
use crate::platform::{DiskFolderUsage, FilePreferences, PlatformAudioDevices, WallClock};
use crate::qr::PngQrEncoder;
use crate::recorder::{CaptureRecorder, DiskWatch, MakeCaptureSession};
use crate::recovery::{Interrupted, LiveRecordingCheck, adopt_orphans, reconcile_interrupted};
use crate::secrets::{KeepsApiKey, KeyringUnavailable, SecretsUnlocked, secret_store_with_unlock};
use crate::speech::{ModelStoreSpeechModels, SpeechEngines, SpeechSetup};
use crate::updates::{InstallGate, NeverIdle, ScheduleParts, UpdateSchedule, UpdateSource};

/// What stops the graph from being built: another process holds the
/// database ([`DatabaseLock`]), or the database could not be opened or
/// read. A secret store that cannot be read and a handover identity that
/// cannot be loaded are warnings, not errors; the graph runs without them.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error(transparent)]
    Lock(#[from] DatabaseLockError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("Could not create the database folder: {0}")]
    DatabaseFolder(#[source] std::io::Error),
}

/// What differs between the shell, the CLI and the tests.
pub struct AppOptions {
    /// The support directory; the database, the preferences and, by
    /// default, the models and the audio live under it.
    pub paths: StenoPaths,
    /// `None` opens the database under `paths`.
    pub database_path: Option<std::path::PathBuf>,
    /// The platform keyring when true, else the 0600 secrets file (the CLI
    /// and headless machines). On Linux the keyring is the Secret Service
    /// when a provider runs, else the file; see the crate doc.
    pub keyring: bool,
    /// The window opener and URL opener; the shell's, a no-op for the CLI.
    pub opener: Arc<dyn Opener>,
    /// The login item; the shell's (`autostart.rs`), `None` for a fake
    /// that registers nothing (the CLI, the tests).
    pub login_item: Option<Arc<dyn LoginItem>>,
    /// The updater the update schedule drives; the shell's (`updater.rs`),
    /// `None` for a fake that never checks (the CLI, the tests).
    pub update_source: Option<Arc<dyn UpdateSource>>,
    /// Whether an update may install now (stable plan P25); [`NeverIdle`]
    /// until the shell supplies the real gate, and through it nothing
    /// downloads or installs by itself.
    pub install_gate: Arc<dyn InstallGate>,
    /// The runtime the host's synchronous service calls block on.
    pub runtime: tokio::runtime::Handle,
    /// `CFBundleShortVersionString`'s equivalent.
    pub version: String,
    /// Builds a capture session; `CaptureSession::new` in the product.
    pub make_capture_session: MakeCaptureSession,
    /// How long [`build`] waits for the database's lock while another
    /// process holds it ([`DatabaseLock::acquire_within`]):
    /// [`LOCK_PATIENCE`] in the product, whose update relaunch starts the
    /// new process before the old one has exited; zero in the tests.
    pub lock_patience: std::time::Duration,
}

/// The product's [`AppOptions::lock_patience`].
pub const LOCK_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

impl AppOptions {
    /// The product under the default support directory on the given
    /// runtime, with the live capture backend.
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
            install_gate: Arc::new(NeverIdle),
            runtime,
            version: version.to_owned(),
            make_capture_session: Arc::new(|configuration| {
                CaptureSession::new(configuration).map_err(|error| error.to_string())
            }),
            lock_patience: LOCK_PATIENCE,
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
    /// The launch re-exports in a row per meeting that did not deliver
    /// every row, which the launch counts and any re-export the user causes
    /// resets.
    pub export_retries: Arc<ExportRetries>,
    pub services: Services,
    /// The update schedule behind `services.updater`, when the shell
    /// passed an update source; [`App::launch`] starts it.
    pub updates: Option<Arc<UpdateSchedule>>,
    pub handover: Option<Arc<ListenerHandover>>,
    pub recorder: Arc<CaptureRecorder>,
    /// Where the speech and diarization models live.
    pub models_directory: std::path::PathBuf,
    pub zone: FixedOffset,
    /// The runtime the graph's async calls block on.
    pub runtime: tokio::runtime::Handle,
    pub version: String,
    /// What went wrong while building, for the shell's log; an unreadable
    /// API key is logged where it is read.
    pub startup_warnings: Vec<String>,
    /// When the launch counts an interrupted recording's master as still
    /// being written ([`crate::recovery`]).
    pub live_recording_check: LiveRecordingCheck,
    /// The launch's background half, for [`App::launch_finished`].
    pub(crate) launch_work: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Resolves once the secret store chose where the secrets are, true
    /// when it turned a read away while the keyring asked the user, for
    /// [`App::launch`] to read the API key and the handover identity again
    /// before it recovers the meetings
    /// ([`crate::secrets::secret_store_with_unlock`]).
    pub(crate) secrets_unlocked: std::sync::Mutex<Option<SecretsUnlocked>>,
    /// Held while the graph lives, so no second app or CLI command takes
    /// the same database; `None` on a filesystem without locks
    /// ([`DatabaseLockError::Unsupported`], a startup warning). Declared
    /// last so it outlives what it guards: fields drop in order.
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

/// Takes the lock of the database at `path` ([`DatabaseLock`]), creating
/// its folder, and waits up to `patience` while another process holds it.
/// Rust only: the Swift app and CLI took no lock.
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
/// not be read: the app runs without summaries rather than not at all, as
/// `AppEnvironment.live` did when the keychain was unreachable.
fn api_key(
    secrets: &dyn SecretStore,
    runtime: &tokio::runtime::Handle,
) -> Result<Option<String>, String> {
    block_on(runtime, secrets.secret(&SecretKey::llm_api_key()))
        .map_err(|error| format!("Could not read the LLM API key from the secret store: {error}"))
}

/// The dependencies of one pipeline from the stored settings, the API key
/// and `engines`, shared by the first build and every reload, with the
/// speech engine and the diarizer `engines` keeps: the engine for the
/// runtime of the stored engine id ([`SpeechEngines`]). The setup of
/// `engines` is read once by [`build`] and also backs the model service,
/// so the two agree on where each engine runs; the recorder's warm-up
/// reads the engine from the pipeline. A secret store that cannot be read
/// is logged, and the passes are built with the key last read or written
/// through `secrets` ([`KeepsApiKey`]), or without one: a keyring locked
/// again while the app runs keeps the key a rebuild had, and a key the user
/// removed or changed is never the one kept.
pub fn pipeline_dependencies(
    store: &Arc<Store>,
    engines: &SpeechEngines,
    secrets: &KeepsApiKey,
    codex: &Arc<CodexCredentialStore>,
    events: &MeetingEventBus,
    runtime: &tokio::runtime::Handle,
) -> Result<BuiltPipeline, BuildError> {
    let settings = store.settings()?;
    let api_key = api_key(secrets, runtime).unwrap_or_else(|warning| {
        let kept = secrets.kept_api_key();
        if kept.is_some() {
            tracing::warn!("{warning}; the pipeline keeps the key it last had");
        } else {
            tracing::warn!("{warning}");
        }
        kept
    });
    let zone = steno_adapters::runtime::local_time_zone();
    let passes = crate::llm::passes(&settings, api_key.as_deref(), codex, zone);
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
    .with_speech_engine(speech_engine);
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
    secrets: &Arc<KeepsApiKey>,
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
/// arrives, so a reload is not bypassed. The intake commits the meeting
/// with its receipt and the pipeline only processes it
/// ([`steno_pipeline::ProcessingPipeline::enqueue_saved`]), as in
/// `RecordingIntake::over`. Swift: `AppEnvironment.makeIntake`.
pub fn handover_intake(
    store: Arc<Store>,
    pipeline: Arc<CurrentPipeline>,
    zone: FixedOffset,
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
}

/// The handover over the listener on a loaded or minted identity, whose
/// mac id the Phones settings show; `None`, with the reason, when the
/// launch checkpoint failed ([`HandoverService::checkpoint_store`], which
/// says why it comes first), the identity could not be read or stored, or
/// it is not the one the paired phones pinned
/// (`HandoverIdentity::load_or_create`). A failed checkpoint keeps the
/// handover off until the next launch, and the rest of the app runs. An
/// identity read while the keyring was asking the user
/// ([`KeyringUnavailable::Unlocking`]) gives a handover that waits for its
/// listener, which [`App::launch`] reads again once the keyring answered.
/// Swift: `AppEnvironment.makeHandover`.
fn handover_listener(
    store: &Arc<Store>,
    pipeline: &Arc<CurrentPipeline>,
    secrets: &Arc<dyn SecretStore>,
    paths: &StenoPaths,
    zone: FixedOffset,
    runtime: &tokio::runtime::Handle,
) -> Result<Arc<ListenerHandover>, String> {
    HandoverService::checkpoint_store(store).map_err(|error| error.to_string())?;
    let handover = match listener_over_identity(store, pipeline, secrets, paths, zone, runtime) {
        Ok((service, mac_id)) => {
            ListenerHandover::over(service, mac_id, store.clone(), runtime.clone())
        }
        Err(error) if waits_on_the_keyring(&error) => {
            ListenerHandover::waiting(identity_failure(&error), store.clone(), runtime.clone())
        }
        Err(error) => return Err(identity_failure(&error)),
    };
    Ok(Arc::new(handover))
}

/// The listener over the identity the secret store holds, or one minted
/// and stored, and the identity's mac id.
fn listener_over_identity(
    store: &Arc<Store>,
    pipeline: &Arc<CurrentPipeline>,
    secrets: &Arc<dyn SecretStore>,
    paths: &StenoPaths,
    zone: FixedOffset,
    runtime: &tokio::runtime::Handle,
) -> Result<(Arc<HandoverService>, uuid::Uuid), IdentityError> {
    let record = crate::handover::FingerprintFile::in_support_directory(&paths.support_directory);
    let identity = block_on(
        runtime,
        steno_handover::HandoverIdentity::load_or_create(
            secrets.as_ref(),
            &record,
            store,
            &format!(
                "Steno on {}",
                steno_handover::HandoverConfiguration::default_service_name()
            ),
            chrono::Utc::now(),
        ),
    )?;
    let intake = Arc::new(handover_intake(store.clone(), pipeline.clone(), zone));
    let mac_id = identity.mac_id();
    let service = Arc::new(crate::handover::service(
        listener_configuration(paths),
        store.clone(),
        intake,
        identity,
    ));
    Ok((service, mac_id))
}

/// [`lock_database`], or `None` with a startup warning on a filesystem
/// without locks.
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

/// The listener's configuration: the default, and in this crate's tests
/// loopback only, unadvertised, with the inbox under the test's support
/// directory.
fn listener_configuration(paths: &StenoPaths) -> steno_handover::HandoverConfiguration {
    let configuration = steno_handover::HandoverConfiguration::default();
    if !cfg!(test) {
        return configuration;
    }
    steno_handover::HandoverConfiguration {
        advertise: false,
        inbox_directory: paths.support_directory.join("handover-inbox"),
        ..configuration
    }
}

/// Why the identity is not available, for the Phones settings: the
/// keyring's own sentence when it was not opened at start, as that one
/// already names the pairing.
fn identity_failure(error: &IdentityError) -> String {
    match keyring_failure(error) {
        Some(not_opened @ KeyringUnavailable::NotOpened(_)) => not_opened.to_string(),
        _ => error.to_string(),
    }
}

/// Whether the identity could not be read because the keyring was asking
/// the user at that moment.
fn waits_on_the_keyring(error: &IdentityError) -> bool {
    keyring_failure(error) == Some(&KeyringUnavailable::Unlocking)
}

/// The keyring's reason when the identity could not be read from it.
fn keyring_failure(error: &IdentityError) -> Option<&KeyringUnavailable> {
    match error {
        IdentityError::Unavailable(Unavailability::Unreadable(source)) => source.downcast_ref(),
        _ => None,
    }
}

/// Builds the graph. First the database's lock ([`DatabaseLock`]): while
/// another process holds it past [`AppOptions::lock_patience`] the build
/// fails with [`BuildError::Lock`] before the database is opened; on a
/// filesystem without locks it runs without one and says so in
/// [`App::startup_warnings`]. Real: store, settings, secret store, speech
/// engine (`CoreML` in this process on the Mac, the ONNX speech sidecar
/// elsewhere and as the Mac's fallback; [`SpeechSetup::runtime`]), ONNX
/// diarizer, cosine speaker memory over the store, LLM passes, delivery
/// coordinator, handover listener, capture session, recorder, the speech
/// models, folder usage, preferences, and the login item when the shell
/// passes its own ([`AppOptions::login_item`]), the update schedule over
/// the shell's update source ([`AppOptions::update_source`]), the QR
/// encoder. Fakes where no platform side exists yet (the plan's "Pipeline
/// and services (WP6b)" list says why for each): permissions (all
/// granted), clip player, the updater when the shell passes no source;
/// the audio device list is empty off the Mac until the `PipeWire` and
/// WASAPI backends enumerate devices.
#[allow(clippy::too_many_lines)]
pub fn build(options: AppOptions) -> Result<App, BuildError> {
    let mut warnings = Vec::new();
    let paths = options.paths;
    let database_path = options
        .database_path
        .unwrap_or_else(|| paths.database_path());
    let database_lock = lock_or_run_without(&database_path, options.lock_patience, &mut warnings)?;
    let store = open_store(&database_path)?;
    let (secrets, secrets_unlocked) = secret_store_with_unlock(options.keyring, &paths);
    let kept = Arc::new(KeepsApiKey::new(secrets));
    let secrets: Arc<dyn SecretStore> = kept.clone();
    let codex = codex_store();
    let events = MeetingEventBus::new();
    let runtime = options.runtime;
    let zone = local_zone();

    // The speech settings and the models directory are read once, here:
    // the pipeline (and every reload, which keeps its engine when it runs
    // where the last one did) and the model service share them.
    let engines = Arc::new(SpeechEngines::new(SpeechSetup::new(
        &store.settings()?,
        &paths,
    )));
    let speech = engines.setup();
    // An unreadable API key is logged by the first build of the
    // dependencies below, once.
    let make = make_dependencies(&store, &engines, &kept, &codex, &events, &runtime);
    let pipeline = Arc::new(CurrentPipeline::new(make()?, make, runtime.clone()));
    let sweep = RetentionSweep::new(store.clone());
    let export_retries = Arc::new(ExportRetries::in_directory(&paths.support_directory));

    let permissions = Arc::new(FakePermissions::all_granted());
    let speech_models = Arc::new(ModelStoreSpeechModels::new(speech));
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

    let handover = handover_listener(&store, &pipeline, &secrets, &paths, zone, &runtime)
        .inspect_err(|error| warnings.push(format!("Phone handover is unavailable: {error}")))
        .ok();

    let clock = Arc::new(WallClock);
    let preferences: Arc<dyn Preferences> = Arc::new(FilePreferences::new(
        paths.support_directory.join("preferences.json"),
    ));
    let updates = options.update_source.map(|source| {
        UpdateSchedule::new(ScheduleParts {
            source,
            preferences: preferences.clone(),
            clock: clock.clone(),
            gate: options.install_gate,
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
            export_retries: export_retries.clone(),
        }),
        speech_models,
        llm: Arc::new(ClientLlmService { codex }),
        export_validator: Arc::new(crate::export::ObsidianExportValidator),
        handover: handover
            .clone()
            .map(|handover| handover as Arc<dyn steno_host::services::Handover>),
        qr: Arc::new(PngQrEncoder),
        audio_devices: Arc::new(PlatformAudioDevices),
        folder_usage: Arc::new(DiskFolderUsage),
        file_system: Arc::new(steno_host::services::RealFileSystem),
        clip_player: Arc::new(FakeClipPlayer::new(Arc::new(FakeFileSystem::default()))),
        opener: options.opener,
        preferences,
        secrets: secrets.clone(),
    };

    Ok(App {
        paths,
        store,
        secrets,
        events,
        pipeline,
        sweep,
        export_retries,
        services,
        updates,
        handover,
        recorder,
        models_directory: speech.models_directory.clone(),
        zone,
        runtime,
        version: options.version,
        startup_warnings: warnings,
        live_recording_check: LiveRecordingCheck::default(),
        launch_work: std::sync::Mutex::default(),
        secrets_unlocked: std::sync::Mutex::new(secrets_unlocked),
        database_lock,
    })
}

/// The launch's last step on the meetings a previous process left
/// unfinished, after the resume, the re-exports and the sweep: the
/// interrupted recordings in `interrupted` are recovered, left alone while
/// another process still writes them or while a folder they may be in
/// cannot be read, or failed ([`reconcile_interrupted`]). It comes last, so
/// a folder that is slow to answer (a network volume) or a fresh master it
/// watches delays nothing else. A panic in it is caught and logged, and the
/// rows it had not reached stay `recording` for the next launch. Then the
/// recordings in the audio folders that have no meeting at all are adopted
/// ([`adopt_orphans`]), also after such a panic; their ids are returned.
/// Blocks, so [`App::launch`] runs it on a blocking task.
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
/// most `patience` (the shell passes [`SHUTDOWN_PATIENCE`]), before the
/// process ends: an exit request the gate can hold goes through
/// [`ExitGate::exit_requested`], an exit it cannot (the run loop's last
/// event after the Dock's Quit or a logout on macOS and a logoff on
/// Windows, an update's relaunch, the Windows installer's exit, a logout
/// or a system shutdown on Linux, from the shell's `session_end`)
/// through [`ExitGate::exiting`]. Clones share one gate. Swift:
/// `applicationShouldTerminate` answered `.terminateLater`, awaited
/// `AppController.shutdown()` without a bound and then replied.
///
/// ```
/// use std::time::Duration;
///
/// use steno_services::app::ExitGate;
///
/// let gate = ExitGate::default();
/// let (exited, exit) = std::sync::mpsc::channel();
/// // The first request is held: the shutdown runs, then the exit, which
/// // the shell's run loop answers by raising the request again.
/// let held = !gate.exit_requested(
///     Duration::from_secs(10),
///     || { /* stop and save the recording */ },
///     move || exited.send(()).unwrap(),
/// );
/// assert!(held);
/// exit.recv_timeout(Duration::from_secs(10)).unwrap();
/// // That request, and every one after it, goes ahead.
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

/// Where the gate stands.
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
    /// Whether this exit request may go ahead now. The first one returns
    /// false, runs `shutdown` on a thread of its own for at most
    /// `patience` and then calls `exit`, which is expected to raise the
    /// request again; that one, and every one after it, returns true. A
    /// request while the shutdown runs returns false and is held: the
    /// exit is coming.
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
            // `exiting` took over while the shutdown ran: the process is
            // ending without the gate's exit.
            if gate.release() == ExitStage::ShuttingDown {
                exit();
            }
        });
        false
    }

    /// The process is about to end without an exit request the gate
    /// holds: runs `shutdown` on this thread's behalf, or waits for the
    /// one already running (a held request's, or an earlier `exiting`'s),
    /// and returns once it ended, at most `patience` later; at once when it
    /// already ran. Every exit request goes ahead afterwards.
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

    /// Whether the shutdown has run, or ran out of patience: every exit
    /// goes ahead without one.
    #[must_use]
    pub fn released(&self) -> bool {
        *lock_stage(&self.shared.0) == ExitStage::Released
    }

    /// Moves an open gate to `next`, and a held request's shutdown to
    /// `Ending` when `next` is `Ending`; returns the stage it found.
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

/// Runs `shutdown` on a thread of its own and waits for it, at most
/// `patience`.
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
/// the stage; the failure text, which can quote the model or the server,
/// goes to debug only.
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

impl App {
    /// The host over this graph, with the viewer's zone, the version and
    /// whether a package manager delivers the updates
    /// ([`updates_are_managed`](crate::updates::updates_are_managed)).
    pub fn host(&self) -> Result<Host, steno_host::host::HostError> {
        Host::new(
            self.store.clone(),
            self.services.clone(),
            HostConfig {
                version: self.version.clone(),
                zone: self.zone,
                platform: steno_bridge::Platform::CURRENT,
                updates_managed: crate::updates::updates_are_managed(),
            },
        )
    }

    /// What every exit does first ([`ExitGate`]), in order: the pipelines
    /// quit ([`CurrentPipeline::quit`]), so no job starts and a job the
    /// exit ends stays `processing` for the next launch rather than
    /// `failed`; a start or a stop in progress settles; a recording in
    /// progress is stopped with the `quit` end reason and saved (its asset
    /// written, the meeting enqueued, where it stays `queued` until the
    /// next launch processes it); and the handover listener stops. Every
    /// write is a committed transaction by then, so the store has nothing
    /// left to flush. A handover still waiting for its listener gets none
    /// after this ([`ListenerHandover::close`]). Swift:
    /// `AppController.shutdown`, whose pipeline died with the app, so the
    /// next launch resumed its job.
    pub fn shutdown(&self) {
        self.pipeline.quit();
        self.recorder.stop_for_quit();
        if let Some(handover) = self.handover.as_deref().and_then(ListenerHandover::close) {
            block_on(&self.runtime, handover.stop());
        }
    }

    /// Everything that happens once at launch, in order:
    ///
    /// 1. The pipeline's events are subscribed and routed into the host.
    /// 2. The meetings left `recording`, the folder each was recorded into
    ///    and the known audio folders ([`crate::audio_folders`]) are
    ///    listed, before anything here can start a recording.
    /// 3. Meetings left queued or processing are processed again, exports
    ///    left unfinished are re-exported
    ///    ([`ProcessingPipeline::redeliver_unfinished`](steno_pipeline::ProcessingPipeline::redeliver_unfinished)),
    ///    and the retention sweep runs.
    /// 4. On a blocking task (`reconcile_at_launch`, which may wait up to
    ///    10 s for a master that is still written): those recordings are
    ///    recovered, left alone or failed; a master in an audio folder with
    ///    no meeting at all is adopted as one, and the main window says
    ///    "Recovered a recording." under the Record control
    ///    (`CaptureRecorder::note_adopted`); the list is refreshed.
    /// 5. Meanwhile the login item is registered the first time, the
    ///    handover listener starts when a phone is already paired, and the
    ///    update schedule starts with its launch tick
    ///    ([`UpdateSchedule::start`]).
    ///
    /// Where the secret store can ask (the Secret Service), steps 3 and 4
    /// (all but the sweep) wait until it chose and the reread below ran, so
    /// no meeting, export or recovered recording runs on a pipeline built
    /// without the key, and the update schedule waits until it chose, so
    /// its launch tick's alert does not come up beside the keyring's
    /// prompt. The choice includes the unlock and the first
    /// launch's move or a later launch's tidy; on a locked keyring or
    /// `KeePassXC` each of their prompts may stay up for two minutes, so a
    /// meeting a crash left processing can show as processing that long (it
    /// is not a hang). [`build`] itself may wait on the user once: on the
    /// first launch over an open keyring with no identity in it, the
    /// handover identity is minted into it on the calling thread, before
    /// any window shows, and that write waits up to two minutes when the
    /// provider asks to confirm the new item (`KeePassXC` answers every new
    /// item with a prompt, which may show a window).
    ///
    /// When the keyring answers later, after a read failed while it asked
    /// the user, the pipeline is built again, the host reads the API key
    /// again, and a handover that waits for its listener reads the identity
    /// again and starts it when a phone is paired.
    ///
    /// Swift: `AppController.launch`, which failed every interrupted
    /// recording instead of recovering it.
    pub fn launch(&self, host: &Arc<Host>) {
        let recorder_host = host.clone();
        self.recorder
            .on_change(Arc::new(move || recorder_host.recorder_changed()));
        self.report_update_checks(host);

        let mut receiver = self.events.subscribe();
        let sweep = self.sweep.clone();
        let event_host = host.clone();
        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        event_host.apply_meeting_event(&event);
                        match &event {
                            MeetingEvent::Progress { .. } => {}
                            MeetingEvent::RetentionApplied { .. } => {
                                run_sweep(&sweep);
                                event_host.store_changed();
                            }
                            MeetingEvent::SpeakersNeedReview { .. }
                            | MeetingEvent::Deleted { .. } => {
                                event_host.store_changed();
                            }
                            MeetingEvent::OperationFailed { .. } => {
                                log_operation_failure(&event);
                                event_host.store_changed();
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        event_host.store_changed();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        // Stage writes do not post events of their own; the list follows
        // the meeting rows through a slow poll until the store observes
        // itself (the Swift app had GRDB observation). The same tick is
        // the pairing poll Swift's Phones pane ran every two seconds: a
        // phone that paired closes the code, a code that ran out closes.
        let poll_host = host.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                interval.tick().await;
                poll_host.store_changed();
                poll_host.refresh_pairing();
            }
        });

        let interrupted = Interrupted::list(&self.store, &self.paths.support_directory);
        let recover = self.recover_unfinished();
        let reconcile = {
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
        };
        let unlocked = self
            .secrets_unlocked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let updates = self.updates.clone();
        let work = match unlocked {
            None => {
                recover();
                run_sweep(&self.sweep);
                if let Some(updates) = updates {
                    updates.start();
                }
                tokio::task::spawn_blocking(reconcile)
            }
            // The pipeline built while the keyring asked has no API key, so
            // the meetings and the interrupted recordings wait for the one
            // built after the answer. The update schedule waits for the
            // answer too.
            Some(unlocked) => {
                run_sweep(&self.sweep);
                let reread = self.reread_after_unlock(host);
                tokio::spawn(async move {
                    let read_again = unlocked.await;
                    if let Some(updates) = updates {
                        updates.start();
                    }
                    let _ = tokio::task::spawn_blocking(move || {
                        if read_again {
                            reread();
                        }
                        recover();
                        reconcile();
                    })
                    .await;
                })
            }
        };
        *self
            .launch_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(work);
        self.start_alongside(host);
        host.store_changed();
    }

    /// The host hears of every update check from here on, the launch
    /// tick's included.
    fn report_update_checks(&self, host: &Arc<Host>) {
        if let Some(updates) = &self.updates {
            let updates_host = host.clone();
            updates.on_change(Arc::new(move || updates_host.updates_changed()));
        }
    }

    /// The launch's step 5, but the update schedule: the login item
    /// registered the first time, and the handover listener started when a
    /// phone is already paired.
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
    }

    /// The launch's crash recovery: meetings left queued or processing
    /// processed again on the current pipeline, then exports left
    /// unfinished re-exported.
    fn recover_unfinished(&self) -> impl FnOnce() + Send + 'static {
        let (pipeline, export_retries) = (self.pipeline.clone(), self.export_retries.clone());
        move || {
            let pipeline = pipeline.current();
            match pipeline.resume_unfinished() {
                Ok(resumed) if !resumed.is_empty() => {
                    tracing::info!(count = resumed.len(), "resumed unfinished meetings");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "unfinished meetings could not be resumed"),
            }
            // After `resume_unfinished`, so a meeting it resumed is skipped:
            // its run exports it.
            match pipeline.redeliver_unfinished(&export_retries) {
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

    /// What [`App::launch`] runs once the keyring answered: the pipeline
    /// built again, the API key read again, and the listener of a waiting
    /// handover made and started.
    fn reread_after_unlock(&self, host: &Arc<Host>) -> impl FnOnce() + Send + 'static {
        let (pipeline, host) = (self.pipeline.clone(), host.clone());
        let waiting = self
            .handover
            .clone()
            .filter(|handover| handover.listener().is_none());
        let (store, secrets, paths, zone, runtime) = (
            self.store.clone(),
            self.secrets.clone(),
            self.paths.clone(),
            self.zone,
            self.runtime.clone(),
        );
        move || {
            if let Err(error) = pipeline.reload() {
                tracing::warn!(%error, "the pipeline was not rebuilt after the unlock");
            }
            host.secrets_changed();
            let Some(waiting) = waiting else {
                return;
            };
            match listener_over_identity(&store, &pipeline, &secrets, &paths, zone, &runtime) {
                Ok((service, mac_id)) => {
                    waiting.set(service, mac_id);
                }
                Err(error) => {
                    tracing::warn!(%error, "the handover identity could not be read after the unlock");
                    waiting.still_waiting(identity_failure(&error));
                }
            }
            host.phones_changed();
        }
    }

    /// Waits until the launch's background half (`reconcile_at_launch`)
    /// has run; at once when [`App::launch`] was not called or this was
    /// already awaited.
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
        async_trait, paths::file_url, protocols::BoundaryResult,
    };
    use steno_host::services::ListenerState;
    use steno_pipeline::MeetingEventBus;
    use steno_speech::SpeechRuntime;

    use super::*;
    use crate::testing::{PATIENCE, built, fake_dependencies, on_own_thread, temp_store};

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
        let intake = handover_intake(store.clone(), current.clone(), local_zone());

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
        // The level and the meeting's state of each commit that holds a
        // phone meeting. The intake's commit is the first, and the only one
        // with the meeting `queued`: the enqueue writes nothing.
        let commits: Arc<std::sync::Mutex<Vec<(i64, String)>>> = Arc::default();
        let seen = commits.clone();
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
                seen.lock().unwrap().push((level, state));
            }
        });
        let meeting_id = intake.admit(&upload, &metadata, &device).await.unwrap();
        // The retired pipeline never saw the meeting; the current one did.
        let current_pipeline = current.current();
        current_pipeline.wait_until_idle().await;
        let meeting = store.meeting(meeting_id).unwrap().unwrap();
        assert_ne!(meeting.state.kind(), steno_core::MeetingStateKind::Queued);
        let commits = commits.lock().unwrap().clone();
        let queued: Vec<_> = commits
            .iter()
            .filter(|(_, state)| state == "queued")
            .collect();
        assert_eq!(
            queued,
            [&(2, "queued".to_owned())],
            "the intake's commit under FULL is the only one with the meeting queued: {commits:?}"
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

    /// The shell's host lists the permissions of the OS it was built for,
    /// so a host wired to the Mac fails here on Windows and Linux.
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

    /// Set in the child [`the_host_says_whether_a_package_manager_delivers_the_updates`]
    /// runs.
    const DISTRIBUTION_CHILD: &str = "STENO_DISTRIBUTION_TEST_CHILD";

    /// The shell's host says a package manager delivers the updates exactly
    /// when [`updates_are_managed`](crate::updates::updates_are_managed)
    /// does: here, and in a child of this test binary told the opposite
    /// through the environment (stable plan X5).
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

    /// The recorder's disk watch reads the volume of the database's folder
    /// beside the recordings folder's.
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

    /// What `App::launch` does to a meeting a previous process left
    /// recording with no audio in the audio folder, which is there: it
    /// fails with Swift's reason.
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

    /// What `App::launch` does to a ready meeting whose export a previous
    /// process left `pending`: it re-exports it into the vault.
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

    /// `App::launch` itself, over fakes: a call recording whose process
    /// died (its writer dropped without `finish`) is recovered on the
    /// launch's blocking task, queued with the `failed` end reason and
    /// processed.
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

    /// `App::launch` itself: a call master in the audio folder with no
    /// meeting (a row lost with the database) is adopted on the launch's
    /// blocking task and processed, and the main window says "Recovered a
    /// recording." under the Record control until the user dismisses it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn launch_adopts_a_recording_with_no_meeting_and_says_so() {
        use steno_host::services::Recorder as _;

        let (dir, store) = crate::testing::temp_store();
        let mut app = recording_app(&dir, &store);
        app.live_recording_check = crate::testing::an_hour_later();
        let meeting_id = uuid::Uuid::new_v4();
        let layout = steno_core::RecordingLayout::new(&dir.path().join("audio"), meeting_id);
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
            Some("Recovered a recording.")
        );
        app.recorder.clear_messages();
        assert_eq!(app.recorder.status().warning, None);
    }

    /// The models directory is decided once, when the app is built: a
    /// reload after the settings name another directory keeps the first,
    /// so the pipeline and the model service agree. Both directories are
    /// plain files, so the engine's `prepare` fails naming the one it uses
    /// without touching the network (`CoreML` misses its bundles under it
    /// on the Mac; elsewhere the sidecar's store cannot create its folder
    /// in it).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_keeps_the_models_directory_the_app_was_built_with() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StenoPaths::new(dir.path().join("support"));
        let (first, reloaded) = (
            dir.path().join("built-models"),
            dir.path().join("reloaded-models"),
        );
        for file in [&first, &reloaded] {
            std::fs::write(file, b"not a directory").unwrap();
        }
        let store = open_store(&paths.database_path()).unwrap();
        let mut settings = store.settings().unwrap();
        settings.models_directory = Some(file_url(&first, true));
        store.save_settings(&settings).unwrap();
        drop(store);

        let app = build(options_under(&dir.path().join("support"))).unwrap();
        let prepare = async || {
            let engine = app.pipeline.current().dependencies().speech_engine.clone();
            engine.prepare().await.unwrap_err().to_string()
        };
        let named = first.display().to_string();
        let error = prepare().await;
        assert!(error.contains(&named), "{named} in {error}");

        settings.models_directory = Some(file_url(&reloaded, true));
        app.store.save_settings(&settings).unwrap();
        app.pipeline.reload().unwrap();
        let error = prepare().await;
        assert!(
            error.contains(&named) && !error.contains("reloaded-models"),
            "{named} in {error}"
        );
    }

    /// The app's reloads keep the speech engine (the same engine and
    /// claims) while the stored engine id runs where the last one did: a
    /// reload with the settings unchanged, and on Linux and Windows a
    /// switch to an engine id the sidecar runs too. On the Mac, where
    /// `parakeet-v3` runs on `CoreML` in this process and other ids in the
    /// sidecar, the switch builds the sidecar engine, and the switch back
    /// finds the first `CoreML` one, which the test still holds as a
    /// retired pipeline would. Every reload keeps the diarizer. Nothing
    /// here starts a sidecar: no job runs.
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

    /// The shell's login item is the one the host reads and switches; the
    /// CLI and the tests, which pass none, get a fake that registers
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

    /// [`crate::testing::app_over_fakes`] under `dir` over a synthetic
    /// tone.
    fn recording_app(dir: &tempfile::TempDir, store: &Arc<Store>) -> App {
        crate::testing::app_over_fakes(dir.path(), store, crate::testing::synthetic_capture())
    }

    /// `app`'s host with the hook `App::launch` wires, without the launch's
    /// tasks: one stuck on the host's lock would hang the runtime's
    /// shutdown, and with it the test.
    fn wired_host(app: &App) -> Host {
        let host = app.host().unwrap();
        let changed = host.clone();
        app.recorder
            .on_change(Arc::new(move || changed.recorder_changed()));
        host
    }

    /// Calls `method` through `host` on a thread of its own, failing the
    /// test unless it returns, and returns success, within [`PATIENCE`].
    fn call(host: &Host, method: steno_bridge::BridgeMethod, params: Option<serde_json::Value>) {
        let dispatcher = steno_bridge::Dispatcher::new(host.clone());
        let reply = on_own_thread(PATIENCE, &format!("{method} returned"), move || {
            dispatcher.call(method, params)
        });
        assert!(reply.is_ok(), "{method}: {reply:?}");
    }

    /// A recording that cannot start (no capture device), from the sidebar
    /// and the tray's Record and Stop: every recorder command through the
    /// host returns, though each change the recorder reports re-enters the
    /// host (`Host::recorder_changed`), and the start's error is shown.
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

    /// [`recording_app`] whose host runs the app's own pipeline and the
    /// real file system, over a failed meeting (`reason`) with a two-lane
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
            export_retries: app.export_retries.clone(),
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

    /// "Process again" through the host on the selected failed meeting
    /// whose master is on disk: the detail and the list show it queued or
    /// further on once the call returns, and the run takes it to ready.
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

    /// The detail is loaded while the meeting is failed; the store then
    /// marks it ready behind the host's back (as a Try again run does
    /// before the host's `store_changed` reload). The click is refused
    /// under the pipeline's own read: the ready meeting is not queued
    /// again, and the error line says why.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_on_a_stale_failed_detail_refuses_a_ready_meeting() {
        use steno_bridge::BridgeMethod;
        let (dir, store) = temp_store();
        let (app, host, id) =
            failed_meeting_selected(&dir, &store, "summarize: the endpoint did not answer");
        assert_eq!(
            host.snapshot(BridgeTopic::MeetingDetail).unwrap()["state"],
            "failed"
        );

        store
            .set_state(id, steno_core::MeetingState::Ready, chrono::Utc::now())
            .unwrap();
        call(&host, BridgeMethod::MeetingProcessAgain, None);
        assert_eq!(
            store.meeting(id).unwrap().unwrap().state,
            steno_core::MeetingState::Ready,
            "a ready meeting is not queued again"
        );
        assert_eq!(
            host.snapshot(BridgeTopic::MeetingDetail).unwrap()["error"],
            "Only a failed meeting can be processed again."
        );
        app.pipeline.current().wait_until_idle().await;
    }

    /// A recording in progress, stopped from the sidebar or the tray (Stop,
    /// then Toggle): each command through the host returns, though the
    /// recorder's `Stopping` and `Idle` re-enter the host
    /// (`Host::recorder_changed`), and each recording is saved.
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

    /// What every exit runs first: a recording in progress is stopped with
    /// `quit` and saved by the time the shutdown returns. The pipelines
    /// quit before the stop, so the saved meeting stays `queued` for the
    /// next launch, through a reload too, and no job starts in the exit.
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

    /// An idle exit runs the same shutdown, which saves nothing and
    /// returns at once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutting_down_with_nothing_recorded_saves_nothing() {
        let (dir, store) = temp_store();
        let app = recording_app(&dir, &store);
        on_own_thread(PATIENCE, "an idle shutdown returned", move || {
            app.shutdown();
        });
        assert_eq!(store.all_meetings().unwrap(), []);
    }

    /// The first exit request is held and runs the shutdown once, to the
    /// end, recording or not; a request meanwhile is held too; then the
    /// gate's exit runs, and the request it raises goes ahead, as does
    /// every one after it.
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
                    // The shell's own exit raises a second request.
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

    /// A shutdown that does not finish within the patience does not keep
    /// the app from quitting.
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

    /// An exit no request held (the Dock's Quit, a relaunch) runs the
    /// shutdown on the caller's behalf and returns once it ended, or once
    /// the patience ran out; exits go ahead afterwards, and a second one
    /// runs nothing.
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

    /// The process ends while a held request's shutdown runs (a Quit, then
    /// the Dock's Quit): `exiting` waits for that shutdown instead of
    /// running another, and the gate's exit is not raised on top.
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

    /// Two exits no request held (an update's relaunch, then the Dock's
    /// Quit or a logout): the second waits for the first's shutdown instead
    /// of running another, or returning while it still runs.
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

    /// The update source the shell passes makes the update schedule the
    /// host's updater, over the graph's `preferences.json`; without one the
    /// updater is the fake. The QR encoder draws real codes either way.
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
            fn tell_install_failed(&self, _message: &str) {}
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

    /// The options of the product's graph under `support`, without a
    /// capture backend or a keyring.
    fn options_under(support: &std::path::Path) -> AppOptions {
        AppOptions {
            paths: StenoPaths::new(support),
            database_path: None,
            keyring: false,
            opener: Arc::new(steno_host::fakes::FakeOpener::default()),
            login_item: None,
            update_source: None,
            install_gate: Arc::new(NeverIdle),
            runtime: tokio::runtime::Handle::current(),
            version: "0.0.0".to_owned(),
            make_capture_session: Arc::new(|_| Err("no capture in this test".to_owned())),
            lock_patience: std::time::Duration::ZERO,
        }
    }

    /// A second app on one database is refused before it opens the
    /// database, so it neither migrates it nor fails the first one's
    /// recording at launch; once the first is gone, the next one builds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_app_on_one_database_is_refused_until_the_first_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let support = dir.path().join("support");
        let first = build(options_under(&support)).unwrap();
        let mut recording = steno_core::testing::sample_data::meeting();
        recording.state = steno_core::MeetingState::Recording;
        first.store.save_meeting(&recording).unwrap();

        let refused = build(options_under(&support)).err().unwrap();
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

    /// The lock comes before the database: a second app over a file that
    /// is no database fails with `Held`, not with the store's error, so it
    /// never opened (or migrated) what the first one holds.
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
        let refused = build(options).err().unwrap();
        assert!(
            matches!(&refused, BuildError::Lock(DatabaseLockError::Held(_))),
            "{refused:?}"
        );
    }

    /// An update's relaunch starts the new app before the old one has
    /// exited: the new one waits out the old one's lock within its
    /// patience, and is refused once the patience has passed.
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

    /// The handover listener is built only over a store whose commits are on
    /// the disk: a checkpoint that fails (here one another connection blocks,
    /// with no busy timeout so the test does not wait) keeps the handover off,
    /// before an identity is minted; once the checkpoint succeeds the listener
    /// is built. Swift: `testAStoreThatCannotSyncKeepsTheHandoverOff`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_store_that_cannot_sync_keeps_the_handover_off() {
        let (dir, store) = temp_store();
        let pipeline = crate::testing::current_pipeline(fake_dependencies(&store, "fake-engine"));
        let memory = Arc::new(steno_core::testing::InMemorySecretStore::new());
        let secrets: Arc<dyn SecretStore> = memory.clone();
        let runtime = tokio::runtime::Handle::current();
        let paths = StenoPaths::new(dir.path().join("support"));
        let listener =
            || handover_listener(&store, &pipeline, &secrets, &paths, local_zone(), &runtime);
        store
            .read(|connection| Ok(connection.busy_timeout(std::time::Duration::ZERO)?))
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

    /// A secret store whose reads fail with `failure` while it is set:
    /// [`KeyringUnavailable::Unlocking`] as the Linux store's while the
    /// keyring's prompt is up, `Locked` or `NotOpened` after it.
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

    /// An app launched while the keyring asks the user: its secret store
    /// holds the API key (and, with `paired`, an identity and a paired
    /// phone) but turns every read away, its pipeline notes the key each
    /// build read, a meeting the last process left queued without its
    /// asset waits for the crash recovery, and its handover waits for its
    /// listener. `answer` makes the keyring answer.
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

            let handover = handover_listener(
                &app.store,
                &app.pipeline,
                &app.secrets,
                &app.paths,
                app.zone,
                &app.runtime,
            )
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
            tokio::time::timeout(PATIENCE, async {
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

    /// The identity read while the keyring asked the user: the handover
    /// waits for its listener and says why, and once the keyring answered
    /// `App::launch` rebuilds the pipeline with the key, shows the key in
    /// Settings, reads the identity again and hands the listener over, and
    /// only then recovers the meetings the last process left.
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

    /// A phone paired: its listener starts once the keyring answered, the
    /// Phones settings show it, and the paired phones come from the
    /// database while the handover waits.
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

    /// The app quits after the keyring answered: the shutdown stops the
    /// listener the reread started.
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

    /// The store chose without turning a read away (the commonest Linux
    /// launch): the meetings are recovered all the same, with no reread.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_choice_that_turned_nothing_away_still_recovers_the_meetings() {
        let mut asked = AskedAtLaunch::new(false).await;
        asked.secrets.fail_with(None);
        drop(asked.answered.take());
        asked.until_recovered().await;
        assert_eq!(*asked.keys_read.lock().unwrap(), [None], "no reread");
    }

    /// The app quits before the keyring answered: the reread hands no
    /// listener over, so nothing listens after the shutdown.
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

    /// A reread that still cannot read the identity keeps the handover
    /// waiting, with the new reason, and recovers the meetings anyway.
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

    /// An identity read that fails for any reason but the keyring asking
    /// (locked again, or not opened at start) turns the handover off for
    /// the run instead of waiting for a reread that never comes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_identity_read_that_fails_otherwise_turns_the_handover_off() {
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
            let error = handover_listener(
                &store,
                &pipeline,
                &secrets,
                &paths,
                local_zone(),
                &tokio::runtime::Handle::current(),
            )
            .err()
            .unwrap();
            let expected = match failure {
                // Its own sentence names the pairing.
                KeyringUnavailable::NotOpened(_) => failure.to_string(),
                _ => format!("this computer's phone pairing could not be read ({failure})"),
            };
            assert_eq!(error, expected);
            assert!(unavailable.inner.keys().is_empty(), "nothing minted");
        }
    }

    /// A secret store whose reads fail, as the keyring does without a
    /// default keychain.
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
            &secrets,
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

    /// The Authorization header one build of the pipeline sends: builds it
    /// as a reload does over `secrets` and runs its cleanup once against
    /// `server`.
    async fn authorization_sent(
        store: &Arc<Store>,
        paths: &StenoPaths,
        secrets: &KeepsApiKey,
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

    /// A keyring locked again while the app runs fails a rebuild's read
    /// (a model-only save, a speech engine change): the pipeline keeps the
    /// key it had. A key removed or changed meanwhile is never the one
    /// kept, and a reread of no key drops it.
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
        let secrets = KeepsApiKey::new(memory.clone());
        let sent = || authorization_sent(&store, &paths, &secrets, &server);
        assert_eq!(sent().await.as_deref(), Some("Bearer sk-1"));

        memory.fail_reads(Some("the keyring is locked"));
        assert_eq!(sent().await.as_deref(), Some("Bearer sk-1"), "kept");
        secrets.set_secret(&key, Some("sk-2")).await.unwrap();
        assert_eq!(sent().await.as_deref(), Some("Bearer sk-2"), "changed");
        secrets.set_secret(&key, None).await.unwrap();
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
}
