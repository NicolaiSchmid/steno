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
use steno_handover::HandoverService;
use steno_host::fakes::{
    FakeClipPlayer, FakeFileSystem, FakeLoginItem, FakePermissions, FakeQrEncoder, FakeUpdater,
};
use steno_host::services::{LoginItem, LoginItemStatus, Opener, Services};
use steno_host::{Host, HostConfig};
use steno_llm::CodexCredentialStore;
use steno_pipeline::{
    ExportRetries, MeetingEventBus, PipelineDependencies, RecordingIntake, RetentionSweep,
};

use crate::block_on;
use crate::handover::ListenerHandover;
use crate::llm::{ClientLlmService, codex_store};
use crate::pipeline::{
    BuiltEngine, BuiltPipeline, CurrentPipeline, HostPipeline, MakeDependencies, run_sweep,
};
use crate::platform::{DiskFolderUsage, FilePreferences, PlatformAudioDevices, WallClock};
use crate::recorder::{CaptureRecorder, DiskWatch, MakeCaptureSession};
use crate::recovery::{Interrupted, LiveRecordingCheck, reconcile_interrupted};
use crate::secrets::secret_store;
use crate::speech::{ModelStoreSpeechModels, SpeechEngines, SpeechSetup};

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
    /// and headless machines). Linux always uses the file; see the crate doc.
    pub keyring: bool,
    /// The window opener and URL opener; the shell's, a no-op for the CLI.
    pub opener: Arc<dyn Opener>,
    /// The login item; the shell's (`autostart.rs`), `None` for a fake
    /// that registers nothing (the CLI, the tests).
    pub login_item: Option<Arc<dyn LoginItem>>,
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
    pub handover: Option<Arc<HandoverService>>,
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
    secrets: &Arc<dyn SecretStore>,
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
/// is logged and the passes are built without a key.
pub fn pipeline_dependencies(
    store: &Arc<Store>,
    engines: &SpeechEngines,
    secrets: &Arc<dyn SecretStore>,
    codex: &Arc<CodexCredentialStore>,
    events: &MeetingEventBus,
    runtime: &tokio::runtime::Handle,
) -> Result<BuiltPipeline, BuildError> {
    let settings = store.settings()?;
    let api_key = api_key(secrets, runtime).unwrap_or_else(|warning| {
        tracing::warn!("{warning}");
        None
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
    secrets: &Arc<dyn SecretStore>,
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

/// The handover listener over a loaded or minted identity, with the mac id
/// the Phones settings show; `None`, with the reason, when the launch
/// checkpoint failed ([`HandoverService::checkpoint_store`], which says why
/// it comes first) or the identity could not be read or stored. A failed
/// checkpoint keeps the handover off until the next launch, and the rest of
/// the app runs. Swift: `AppEnvironment.makeHandover`.
fn handover_listener(
    store: &Arc<Store>,
    pipeline: &Arc<CurrentPipeline>,
    secrets: &Arc<dyn SecretStore>,
    zone: FixedOffset,
    runtime: &tokio::runtime::Handle,
) -> Result<(Arc<HandoverService>, uuid::Uuid), String> {
    HandoverService::checkpoint_store(store).map_err(|error| error.to_string())?;
    let identity = block_on(
        runtime,
        steno_handover::HandoverIdentity::load_or_create(
            secrets.as_ref(),
            &format!(
                "Steno on {}",
                steno_handover::HandoverConfiguration::default_service_name()
            ),
            chrono::Utc::now(),
        ),
    )
    .map_err(|error| error.to_string())?;
    let intake = Arc::new(handover_intake(store.clone(), pipeline.clone(), zone));
    let mac_id = identity.mac_id();
    let service = Arc::new(crate::handover::service(store.clone(), intake, identity));
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
/// passes its own ([`AppOptions::login_item`]). Fakes where no platform
/// side exists yet (the plan's "Pipeline and services (WP6b)" list says why
/// for each): permissions (all granted), updater, clip player, QR encoder;
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
    let secrets = secret_store(options.keyring, &paths);
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
    let make = make_dependencies(&store, &engines, &secrets, &codex, &events, &runtime);
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

    let handover = match handover_listener(&store, &pipeline, &secrets, zone, &runtime) {
        Ok(pair) => Some(pair),
        Err(error) => {
            warnings.push(format!("Phone handover is unavailable: {error}"));
            None
        }
    };

    let services = Services {
        clock: Arc::new(WallClock),
        login_item: options
            .login_item
            .unwrap_or_else(|| Arc::new(FakeLoginItem::new(LoginItemStatus::NotRegistered))),
        permissions,
        updater: Arc::new(FakeUpdater::default()),
        recorder: recorder.clone(),
        pipeline: Arc::new(HostPipeline {
            pipeline: pipeline.clone(),
            sweep: sweep.clone(),
            export_retries: export_retries.clone(),
        }),
        speech_models,
        llm: Arc::new(ClientLlmService { codex }),
        export_validator: Arc::new(crate::export::ObsidianExportValidator),
        handover: handover.as_ref().map(|(service, mac_id)| {
            Arc::new(ListenerHandover {
                service: service.clone(),
                mac_id: *mac_id,
                runtime: runtime.clone(),
            }) as Arc<dyn steno_host::services::Handover>
        }),
        qr: Arc::new(FakeQrEncoder::new("")),
        audio_devices: Arc::new(PlatformAudioDevices),
        folder_usage: Arc::new(DiskFolderUsage),
        file_system: Arc::new(steno_host::services::RealFileSystem),
        clip_player: Arc::new(FakeClipPlayer::new(Arc::new(FakeFileSystem::default()))),
        opener: options.opener,
        preferences: Arc::new(FilePreferences::new(
            paths.support_directory.join("preferences.json"),
        )),
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
        handover: handover.map(|(service, _)| service),
        recorder,
        models_directory: speech.models_directory.clone(),
        zone,
        runtime,
        version: options.version,
        startup_warnings: warnings,
        live_recording_check: LiveRecordingCheck::default(),
        launch_work: std::sync::Mutex::default(),
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
/// rows it had not reached stay `recording` for the next launch. Blocks, so
/// [`App::launch`] runs it on a blocking task.
pub(crate) fn reconcile_at_launch(
    store: &Arc<Store>,
    pipeline: &CurrentPipeline,
    interrupted: &Interrupted,
    check: &LiveRecordingCheck,
    zone: FixedOffset,
    runtime: &tokio::runtime::Handle,
) {
    let intake =
        steno_pipeline::LocalRecordingIntake::over(store.clone(), pipeline.current(), zone);
    let reconciled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reconcile_interrupted(store, &intake, interrupted, check, runtime)
    }));
    if reconciled.is_err() {
        tracing::warn!(
            "the recovery of interrupted recordings panicked; the next launch tries again"
        );
    }
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
    /// The host over this graph, with the viewer's zone and the version.
    pub fn host(&self) -> Result<Host, steno_host::host::HostError> {
        Host::new(
            self.store.clone(),
            self.services.clone(),
            HostConfig {
                version: self.version.clone(),
                zone: self.zone,
                platform: steno_bridge::Platform::CURRENT,
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
    /// left to flush. Swift: `AppController.shutdown`, whose pipeline died
    /// with the app, so the next launch resumed its job.
    pub fn shutdown(&self) {
        self.pipeline.quit();
        self.recorder.stop_for_quit();
        if let Some(handover) = &self.handover {
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
    ///    recovered, left alone or failed, and the list is refreshed.
    /// 5. Meanwhile the login item is registered the first time, and the
    ///    handover listener starts when a phone is already paired.
    ///
    /// Swift: `AppController.launch`, which failed every interrupted
    /// recording instead of recovering it.
    pub fn launch(&self, host: &Arc<Host>) {
        let recorder_host = host.clone();
        self.recorder
            .on_change(Arc::new(move || recorder_host.recorder_changed()));

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
        let pipeline = self.pipeline.current();
        match pipeline.resume_unfinished() {
            Ok(resumed) if !resumed.is_empty() => {
                tracing::info!(count = resumed.len(), "resumed unfinished meetings");
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "unfinished meetings could not be resumed"),
        }
        // After `resume_unfinished`, so a meeting it resumed is skipped:
        // its run exports it.
        match pipeline.redeliver_unfinished(&self.export_retries) {
            Ok(owed) if !owed.is_empty() => {
                tracing::info!(count = owed.len(), "re-exporting unfinished exports");
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "unfinished exports could not be re-exported"),
        }
        run_sweep(&self.sweep);
        let work = {
            let (store, pipeline) = (self.store.clone(), self.pipeline.clone());
            let (check, zone, runtime) = (
                self.live_recording_check.clone(),
                self.zone,
                self.runtime.clone(),
            );
            let host = host.clone();
            tokio::task::spawn_blocking(move || {
                reconcile_at_launch(&store, &pipeline, &interrupted, &check, zone, &runtime);
                host.store_changed();
            })
        };
        *self
            .launch_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(work);
        host.register_login_item_on_first_launch();
        if let Some(handover) = &self.handover {
            let handover = handover.clone();
            tokio::spawn(async move {
                let paired = handover.paired_devices().await.unwrap_or_default();
                if !paired.is_empty()
                    && let Err(error) = handover.start().await
                {
                    tracing::warn!(%error, "handover listener did not start");
                }
            });
        }
        host.store_changed();
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

    use steno_core::{
        AudioFormat, HandoverIntake as _, PairedDevice, RecordingMetadata, SecretKey, SecretStore,
        async_trait, paths::file_url, protocols::BoundaryResult,
    };
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
            .snapshot(steno_bridge::BridgeTopic::Onboarding)
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

    /// The options of the product's graph under `support`, without a
    /// capture backend or a keyring.
    fn options_under(support: &std::path::Path) -> AppOptions {
        AppOptions {
            paths: StenoPaths::new(support),
            database_path: None,
            keyring: false,
            opener: Arc::new(steno_host::fakes::FakeOpener::default()),
            login_item: None,
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
        let listener = || handover_listener(&store, &pipeline, &secrets, local_zone(), &runtime);
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
        let secrets: Arc<dyn SecretStore> = Arc::new(BrokenSecrets);
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
}
