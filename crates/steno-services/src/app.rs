//! [`build`]: the one function that turns the settings into the running
//! object graph, and [`App`], what it hands back. Swift: `AppEnvironment.live`
//! and `AppController.launch`.

use std::sync::Arc;

use chrono::{FixedOffset, Local, Offset as _, Utc};
use steno_adapters::DeliveryCoordinator;
use steno_audio::{CaptureSession, SymphoniaAudioCodec};
use steno_core::{MeetingEvent, SecretKey, SecretStore, StenoPaths, Store, StoreError};
use steno_handover::HandoverService;
use steno_host::fakes::{
    FakeClipPlayer, FakeFileSystem, FakeLoginItem, FakePermissions, FakeQrEncoder, FakeUpdater,
};
use steno_host::services::{LoginItemStatus, Opener, Services};
use steno_host::{Host, HostConfig};
use steno_llm::CodexCredentialStore;
use steno_pipeline::{
    MeetingEventBus, PipelineDependencies, ProcessingPipeline, RecordingIntake, RetentionSweep,
};

use crate::block_on;
use crate::handover::ListenerHandover;
use crate::llm::{ClientLlmService, codex_store};
use crate::pipeline::{CurrentPipeline, HostPipeline, MakeDependencies, run_sweep};
use crate::platform::{DiskFolderUsage, FilePreferences, PlatformAudioDevices, WallClock};
use crate::recorder::{CaptureRecorder, MakeCaptureSession};
use crate::secrets::secret_store;
use crate::speech::ModelStoreSpeechModels;

/// What stops the graph from being built: the database could not be
/// opened or read. A secret store that cannot be read and a handover
/// identity that cannot be loaded are warnings, not errors; the graph
/// runs without them.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
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
    /// The runtime the host's synchronous service calls block on.
    pub runtime: tokio::runtime::Handle,
    /// `CFBundleShortVersionString`'s equivalent.
    pub version: String,
    /// Builds a capture session; `CaptureSession::new` in the product.
    pub make_capture_session: MakeCaptureSession,
}

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
            runtime,
            version: version.to_owned(),
            make_capture_session: Arc::new(|configuration| {
                CaptureSession::new(configuration).map_err(|error| error.to_string())
            }),
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
}

/// The viewer's zone, fixed at start.
#[must_use]
pub fn local_zone() -> FixedOffset {
    Local::now().offset().fix()
}

/// Opens (and migrates) the database at `path`, creating its folder.
pub fn open_store(path: &std::path::Path) -> Result<Arc<Store>, BuildError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(BuildError::DatabaseFolder)?;
    }
    Ok(Arc::new(Store::open(path)?))
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

/// The dependencies of one pipeline from the stored settings and the API
/// key, shared by the first build and every reload. A secret store that
/// cannot be read is logged and the passes are built without a key.
pub fn pipeline_dependencies(
    store: &Arc<Store>,
    paths: &StenoPaths,
    secrets: &Arc<dyn SecretStore>,
    codex: &Arc<CodexCredentialStore>,
    events: &MeetingEventBus,
    runtime: &tokio::runtime::Handle,
) -> Result<PipelineDependencies, BuildError> {
    let settings = store.settings()?;
    let api_key = api_key(secrets, runtime).unwrap_or_else(|warning| {
        tracing::warn!("{warning}");
        None
    });
    let models_directory = crate::speech::models_directory(&settings, paths);
    let zone = steno_adapters::runtime::local_time_zone();
    let passes = crate::llm::passes(&settings, api_key.as_deref(), codex, zone);
    let dependencies = PipelineDependencies::new(
        Arc::new(SymphoniaAudioCodec::new()),
        crate::speech::speech_engine(&settings, &models_directory),
        crate::speech::diarizer(&models_directory),
        Arc::new(steno_pipeline::StoreSpeakerMemory::new(store.clone())),
        Arc::new(DeliveryCoordinator::new(store.clone())),
        store.clone(),
        events.clone(),
    );
    Ok(match passes {
        Some(passes) => dependencies.with_llm(Some(passes.cleaner), Some(passes.summarizer)),
        None => dependencies,
    })
}

/// [`pipeline_dependencies`] over clones of its inputs, for the reloads.
fn make_dependencies(
    store: &Arc<Store>,
    paths: &StenoPaths,
    secrets: &Arc<dyn SecretStore>,
    codex: &Arc<CodexCredentialStore>,
    events: &MeetingEventBus,
    runtime: &tokio::runtime::Handle,
) -> MakeDependencies {
    let (store, paths, secrets, codex, events, runtime) = (
        store.clone(),
        paths.clone(),
        secrets.clone(),
        codex.clone(),
        events.clone(),
        runtime.clone(),
    );
    Arc::new(move || pipeline_dependencies(&store, &paths, &secrets, &codex, &events, &runtime))
}

/// The phone intake over whichever pipeline is current when a recording
/// arrives, so a reload is not bypassed. Swift: `AppEnvironment.makeIntake`.
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
            Box::pin(async move { pipeline.enqueue(&meeting, &asset) })
        }),
        now,
        zone,
    )
}

/// The handover listener over a loaded or minted identity, with the mac id
/// the Phones settings show; `None`, with the reason, when the identity
/// could not be read or stored.
fn handover_listener(
    store: &Arc<Store>,
    pipeline: &Arc<CurrentPipeline>,
    secrets: &Arc<dyn SecretStore>,
    zone: FixedOffset,
    runtime: &tokio::runtime::Handle,
) -> Result<(Arc<HandoverService>, uuid::Uuid), String> {
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

/// Builds the graph. Real: store, settings, secret store, speech engine
/// (`CoreML` on the Mac, ONNX elsewhere, in this process until the speech
/// sidecar lands), ONNX diarizer, cosine speaker memory over the store,
/// LLM passes, delivery coordinator, handover listener, capture session,
/// recorder, the speech models, folder usage, preferences. Fakes until the shell
/// draws their platform side (plan: `WP8`): permissions (all granted), login
/// item, updater, clip player, QR encoder; the audio device list is empty
/// off the Mac until the `PipeWire` and WASAPI backends enumerate devices.
pub fn build(options: AppOptions) -> Result<App, BuildError> {
    let mut warnings = Vec::new();
    let paths = options.paths;
    let store = open_store(
        &options
            .database_path
            .unwrap_or_else(|| paths.database_path()),
    )?;
    let secrets = secret_store(options.keyring, &paths);
    let codex = codex_store();
    let events = MeetingEventBus::new();
    let runtime = options.runtime;
    let zone = local_zone();

    // An unreadable API key is logged by the first build of the
    // dependencies below, once.
    let make = make_dependencies(&store, &paths, &secrets, &codex, &events, &runtime);
    let pipeline = Arc::new(CurrentPipeline::new(
        ProcessingPipeline::new(make()?),
        make,
        runtime.clone(),
    ));
    let sweep = RetentionSweep::new(store.clone());
    let settings = store.settings()?;
    let models_directory = crate::speech::models_directory(&settings, &paths);

    let permissions = Arc::new(FakePermissions::all_granted());
    let speech_models = Arc::new(ModelStoreSpeechModels::new(&models_directory));
    let recorder = Arc::new(CaptureRecorder::new(
        store.clone(),
        pipeline.clone(),
        options.make_capture_session,
        permissions.clone(),
        speech_models.clone(),
        zone,
        runtime.clone(),
    ));

    let handover = match handover_listener(&store, &pipeline, &secrets, zone, &runtime) {
        Ok(pair) => Some(pair),
        Err(error) => {
            warnings.push(format!("Phone handover is unavailable: {error}"));
            None
        }
    };

    let services = Services {
        clock: Arc::new(WallClock),
        login_item: Arc::new(FakeLoginItem::new(LoginItemStatus::NotRegistered)),
        permissions,
        updater: Arc::new(FakeUpdater::default()),
        recorder: recorder.clone(),
        pipeline: Arc::new(HostPipeline {
            pipeline: pipeline.clone(),
            sweep: sweep.clone(),
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
        services,
        handover: handover.map(|(service, _)| service),
        recorder,
        models_directory,
        zone,
        runtime,
        version: options.version,
        startup_warnings: warnings,
    })
}

/// How long quitting waits for a recording to be saved before it exits
/// anyway.
pub const SHUTDOWN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// The shell's exit requests around [`App::shutdown`]. The first request
/// that finds a recording in progress is held while the shutdown runs on
/// a thread of its own, for at most a patience; then `exit` is called.
/// Every later request, the exit that `exit` itself causes included, goes
/// ahead. Swift: `applicationShouldTerminate` answered `.terminateLater`,
/// awaited `AppController.shutdown()` and then replied.
#[derive(Debug, Default)]
pub struct ExitGate {
    shutting_down: std::sync::atomic::AtomicBool,
}

impl ExitGate {
    /// Whether this exit request may go ahead now. `recording` is the
    /// meeting being recorded, if any; with one, the first request returns
    /// false and runs `shutdown`, then `exit`, as described on the type.
    pub fn exit_requested(
        &self,
        recording: Option<uuid::Uuid>,
        patience: std::time::Duration,
        shutdown: impl FnOnce() + Send + 'static,
        exit: impl FnOnce() + Send + 'static,
    ) -> bool {
        let Some(meeting_id) = recording else {
            return true;
        };
        if self
            .shutting_down
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return true;
        }
        std::thread::spawn(move || {
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                shutdown();
                let _ = done.send(());
            });
            if finished.recv_timeout(patience).is_err() {
                tracing::warn!(
                    %meeting_id,
                    seconds = patience.as_secs(),
                    "quitting before the recording was saved"
                );
            }
            exit();
        });
        false
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
            },
        )
    }

    /// The meeting being recorded, if any: what quitting has to save first.
    #[must_use]
    pub fn recording(&self) -> Option<uuid::Uuid> {
        use steno_host::services::Recorder as _;
        let status = self.recorder.status();
        (status.state != steno_bridge::RecordingState::Idle)
            .then_some(status.meeting_id)
            .flatten()
    }

    /// What quitting does first, in order: a recording in progress is
    /// stopped with the `quit` end reason and saved (its asset written,
    /// the meeting queued, so the next launch processes it), and the phone
    /// listener stops. Every write is a committed transaction by then, so
    /// the store has nothing left to flush. Swift: `AppController.shutdown`.
    pub fn shutdown(&self) {
        self.recorder.stop_for_quit();
        if let Some(handover) = &self.handover {
            block_on(&self.runtime, handover.stop());
        }
    }

    /// Everything that happens once at launch, in order: the pipeline's
    /// events are subscribed and routed into the host, interrupted
    /// recordings become failed, meetings left queued or processing are
    /// processed again, the retention sweep runs, the login item is
    /// registered the first time, and the handover listener starts when a
    /// phone is already paired. Swift: `AppController.launch`.
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

        if let Err(error) = self
            .store
            .fail_interrupted_recordings(Store::INTERRUPTED_RECORDING_REASON, Utc::now())
        {
            tracing::warn!(%error, "interrupted recordings could not be marked");
        }
        match self.pipeline.current().resume_unfinished() {
            Ok(resumed) if !resumed.is_empty() => {
                tracing::info!(count = resumed.len(), "resumed unfinished meetings");
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "unfinished meetings could not be resumed"),
        }
        run_sweep(&self.sweep);
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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use steno_core::{
        AudioFormat, HandoverIntake as _, PairedDevice, RecordingMetadata, SecretKey, SecretStore,
        async_trait, paths::file_url, protocols::BoundaryResult,
    };
    use steno_pipeline::{MeetingEventBus, ProcessingPipeline};

    use super::*;
    use crate::testing::{fake_dependencies, temp_store};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_phone_intake_enqueues_through_the_pipeline_current_at_admission() {
        let (dir, store) = temp_store();
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&dir.path().join("audio"), true);
        store.save_settings(&settings).unwrap();
        let reloads = Arc::new(AtomicUsize::new(0));
        let make: MakeDependencies = {
            let (store, reloads) = (store.clone(), reloads.clone());
            Arc::new(move || {
                let n = reloads.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(fake_dependencies(&store, &format!("engine-{n}")))
            })
        };
        let current = Arc::new(CurrentPipeline::new(
            ProcessingPipeline::new(fake_dependencies(&store, "engine-0")),
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
        let meeting_id = intake.admit(&upload, &metadata, &device).await.unwrap();
        // The retired pipeline never saw the meeting; the current one did.
        let current_pipeline = current.current();
        current_pipeline.wait_until_idle().await;
        let meeting = store.meeting(meeting_id).unwrap().unwrap();
        assert_ne!(meeting.state.kind(), steno_core::MeetingStateKind::Queued);
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

    /// What `App::launch` does to a meeting a previous process left
    /// recording: it fails with Swift's reason.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn launch_fails_a_recording_the_last_process_left_with_the_swift_reason() {
        let dir = tempfile::tempdir().unwrap();
        let app = build(AppOptions {
            paths: StenoPaths::new(dir.path().join("support")),
            database_path: None,
            keyring: false,
            opener: Arc::new(steno_host::fakes::FakeOpener::default()),
            runtime: tokio::runtime::Handle::current(),
            version: "0.0.0".to_owned(),
            make_capture_session: Arc::new(|_| Err("no capture in this test".to_owned())),
        })
        .unwrap();
        let mut meeting = steno_core::testing::sample_data::meeting();
        meeting.state = steno_core::MeetingState::Recording;
        app.store.save_meeting(&meeting).unwrap();
        let host = Arc::new(app.host().unwrap());
        app.launch(&host);
        assert_eq!(
            app.store.meeting(meeting.id).unwrap().unwrap().state,
            steno_core::MeetingState::Failed {
                reason: "Recording was interrupted before it finished.".to_owned()
            }
        );
    }

    /// An exit request during a recording is held: the shutdown runs once,
    /// to the end, and only then the exit; the exit that causes, and any
    /// other request meanwhile, goes ahead. With nothing recording every
    /// request goes ahead.
    #[test]
    fn an_exit_while_recording_waits_for_the_save_then_exits_once() {
        use std::sync::Mutex;
        use std::time::Duration;
        let gate = Arc::new(ExitGate::default());
        let steps = Arc::new(Mutex::new(Vec::<&str>::new()));
        let (exited, exit_seen) = std::sync::mpsc::channel();
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let meeting = uuid::Uuid::new_v4();
        assert!(gate.exit_requested(None, Duration::from_secs(1), || {}, || {}));

        let held = gate.exit_requested(
            Some(meeting),
            Duration::from_secs(5),
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
                        Some(meeting),
                        Duration::from_secs(5),
                        || panic!("no second shutdown"),
                        || {},
                    ));
                }
            },
        );
        assert!(!held, "the first request waits");
        assert!(
            gate.exit_requested(Some(meeting), Duration::from_secs(5), || {}, || {}),
            "a request while the shutdown runs goes ahead"
        );
        assert!(
            exit_seen.recv_timeout(Duration::from_secs(5)).unwrap(),
            "the exit after the shutdown goes ahead"
        );
        assert_eq!(*steps.lock().unwrap(), ["saved", "exit"]);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }

    /// A save that does not finish within the patience does not keep the
    /// app from quitting.
    #[test]
    fn an_exit_waits_at_most_the_patience() {
        let gate = ExitGate::default();
        let (exited, exit_seen) = std::sync::mpsc::channel();
        let (_never, stuck) = std::sync::mpsc::channel::<()>();
        assert!(!gate.exit_requested(
            Some(uuid::Uuid::new_v4()),
            std::time::Duration::from_millis(100),
            move || {
                let _ = stuck.recv();
            },
            move || exited.send(()).unwrap(),
        ));
        exit_seen
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("exited after the patience");
    }

    #[test]
    fn a_background_failure_warns_without_its_text() {
        let log = crate::testing::CapturedLog::warnings();
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
        let dependencies = pipeline_dependencies(
            &store,
            &paths,
            &secrets,
            &codex_store(),
            &MeetingEventBus::new(),
            &tokio::runtime::Handle::current(),
        )
        .expect("the graph builds without the key");
        assert!(dependencies.cleaner.is_none());
        assert_eq!(
            api_key(&secrets, &tokio::runtime::Handle::current()).unwrap_err(),
            "Could not read the LLM API key from the secret store: no default keychain"
        );
    }
}
