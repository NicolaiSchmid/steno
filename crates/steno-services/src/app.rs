//! [`build`]: the one function that turns the settings into the running
//! object graph, and [`App`], what it hands back. Swift: `AppEnvironment.live`
//! and `AppController.launch`.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{FixedOffset, Local, Offset as _, Utc};
use steno_adapters::DeliveryCoordinator;
use steno_audio::{CaptureSession, SymphoniaAudioCodec};
use steno_core::{MeetingEvent, SecretKey, SecretStore, StenoPaths, Store};
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
use steno_speech::ModelStore;

use crate::handover::RealHandover;
use crate::llm::RealLlmService;
use crate::misc::{DiskFolderUsage, FilePreferences, PlatformAudioDevices, WallClock};
use crate::pipeline_service::{MakeDependencies, PipelineHandle, RealPipeline, run_sweep};
use crate::recorder::{MakeCaptureSession, RealRecorder};
use crate::secrets::{FileSecretStore, KeyringSecretStore};
use crate::speech::RealSpeechModels;

/// What differs between the shell, the CLI and the tests.
pub struct AppOptions {
    /// The support directory; the database, the preferences and, by
    /// default, the models and the audio live under it.
    pub paths: StenoPaths,
    /// `None` opens the database under `paths`.
    pub database_path: Option<std::path::PathBuf>,
    /// The platform keyring when true, else the 0600 secrets file (the CLI
    /// and headless machines).
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

/// A no-op opener for the CLI and the tests.
#[derive(Debug, Default)]
pub struct NoOpener;

impl Opener for NoOpener {
    fn reveal(&self, _path: &std::path::Path) {}
    fn open_url(&self, _url: &str) {}
    fn open_window(&self, _window: steno_bridge::BridgeWindow) {}
    fn close_window(&self, _window: steno_bridge::BridgeWindow) {}
}

impl AppOptions {
    /// The product under the default support directory on the given
    /// runtime, with the live capture backend.
    pub fn product(runtime: tokio::runtime::Handle, opener: Arc<dyn Opener>, version: &str) -> std::io::Result<Self> {
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
    pub pipeline: Arc<PipelineHandle>,
    pub sweep: RetentionSweep,
    pub services: Services,
    pub handover: Option<Arc<HandoverService>>,
    pub recorder: Arc<RealRecorder>,
    pub speech_store: ModelStore,
    pub zone: FixedOffset,
    pub version: String,
    /// What went wrong while building, for the shell's log.
    pub startup_warnings: Vec<String>,
}

/// The viewer's zone, fixed at start.
#[must_use]
pub fn local_zone() -> FixedOffset {
    Local::now().offset().fix()
}

/// The dependencies of one pipeline from the stored settings and the API
/// key, shared by the first build and every reload.
pub fn pipeline_dependencies(
    store: &Arc<Store>,
    paths: &StenoPaths,
    secrets: &Arc<dyn SecretStore>,
    codex: &Arc<CodexCredentialStore>,
    events: &MeetingEventBus,
    runtime: &tokio::runtime::Handle,
) -> Result<PipelineDependencies, String> {
    let settings = store.settings().map_err(|error| error.to_string())?;
    let api_key = tokio::task::block_in_place(|| {
        runtime.block_on(secrets.secret(&SecretKey::llm_api_key()))
    })
    .map_err(|error| error.to_string())?;
    let speech_store = crate::speech::speech_store(&settings, paths);
    let zone = steno_adapters::runtime::local_time_zone();
    let passes = crate::llm::passes(&settings, api_key.as_deref(), codex, zone);
    let dependencies = PipelineDependencies::new(
        Arc::new(SymphoniaAudioCodec::new()),
        crate::speech::speech_engine(&settings, &speech_store),
        crate::speech::diarizer(&speech_store),
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


/// The host's service table over the graph's parts.
#[allow(clippy::too_many_arguments)]
fn services(
    permissions: Arc<FakePermissions>,
    recorder: Arc<RealRecorder>,
    pipeline: &Arc<PipelineHandle>,
    sweep: &RetentionSweep,
    speech_store: &ModelStore,
    codex: Arc<CodexCredentialStore>,
    handover: Option<(&Arc<HandoverService>, uuid::Uuid)>,
    opener: Arc<dyn Opener>,
    preferences_path: std::path::PathBuf,
    secrets: Arc<dyn SecretStore>,
    runtime: &tokio::runtime::Handle,
) -> Services {
    Services {
        clock: Arc::new(WallClock),
        login_item: Arc::new(FakeLoginItem::new(LoginItemStatus::NotRegistered)),
        permissions,
        updater: Arc::new(FakeUpdater::default()),
        recorder,
        pipeline: Arc::new(RealPipeline {
            handle: pipeline.clone(),
            sweep: sweep.clone(),
        }),
        speech_models: Arc::new(RealSpeechModels {
            speech: speech_store.clone(),
        }),
        llm: Arc::new(RealLlmService {
            codex,
            runtime: runtime.clone(),
        }),
        export_validator: Arc::new(crate::delivery::ObsidianValidator),
        handover: handover.map(|(service, mac_id)| {
            Arc::new(RealHandover {
                service: service.clone(),
                mac_id,
                runtime: runtime.clone(),
            }) as Arc<dyn steno_host::services::Handover>
        }),
        qr: Arc::new(FakeQrEncoder::new("")),
        audio_devices: Arc::new(PlatformAudioDevices),
        folder_usage: Arc::new(DiskFolderUsage),
        file_system: Arc::new(steno_host::services::RealFileSystem),
        clip_player: Arc::new(FakeClipPlayer::new(Arc::new(FakeFileSystem::default()))),
        opener,
        preferences: Arc::new(FilePreferences::new(preferences_path)),
        secrets,
    }
}

/// Builds the graph. Real: store, settings, secret store, speech engine
/// (`CoreML` on the Mac, ONNX elsewhere, in-process until the `WP4c`
/// sidecar), ONNX diarizer, cosine speaker memory over the store, LLM
/// passes, delivery coordinator, handover listener, capture session,
/// recorder, model store, folder usage, preferences. Fakes until `WP8`:
/// permissions (all granted), login item, updater, clip player, QR
/// encoder; the audio device list is empty off the Mac until `WP5b`.
pub fn build(options: AppOptions) -> Result<App, String> {
    let mut warnings = Vec::new();
    let paths = options.paths;
    let database = options
        .database_path
        .unwrap_or_else(|| paths.database_path());
    if let Some(parent) = database.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let store = Arc::new(Store::open(&database).map_err(|error| error.to_string())?);
    let secrets: Arc<dyn SecretStore> = if options.keyring {
        Arc::new(KeyringSecretStore)
    } else {
        Arc::new(FileSecretStore::in_support_directory(&paths.support_directory))
    };
    let codex = Arc::new(CodexCredentialStore::new(CodexCredentialStore::default_home(
        &std::env::vars().collect(),
    )));
    let events = MeetingEventBus::new();
    let runtime = options.runtime;
    let zone = local_zone();

    let make: MakeDependencies = {
        let (store, paths, secrets, codex, events, runtime) = (
            store.clone(),
            paths.clone(),
            secrets.clone(),
            codex.clone(),
            events.clone(),
            runtime.clone(),
        );
        Arc::new(move || pipeline_dependencies(&store, &paths, &secrets, &codex, &events, &runtime))
    };
    let codex = codex.clone();
    let pipeline = Arc::new(PipelineHandle::new(
        ProcessingPipeline::new(make()?),
        make,
        runtime.clone(),
    ));
    let sweep = RetentionSweep::new(store.clone());
    let settings = store.settings().map_err(|error| error.to_string())?;
    let speech_store = crate::speech::speech_store(&settings, &paths);

    let permissions = Arc::new(FakePermissions::all_granted());
    let recorder = Arc::new(RealRecorder::new(
        store.clone(),
        pipeline.clone(),
        options.make_capture_session,
        permissions.clone(),
        zone,
        runtime.clone(),
    ));

    let handover = match tokio::task::block_in_place(|| {
        runtime.block_on(crate::handover::load_or_mint_identity(
            secrets.as_ref(),
            &format!("Steno on {}", steno_handover::HandoverConfiguration::default_service_name()),
        ))
    }) {
        Ok(identity) => {
            let intake = Arc::new(RecordingIntake::over(store.clone(), pipeline.current(), zone));
            let mac_id = identity.mac_id();
            let service = Arc::new(crate::handover::service(store.clone(), intake, identity));
            Some((service, mac_id))
        }
        Err(error) => {
            warnings.push(format!("Phone handover is unavailable: {error}"));
            None
        }
    };

    let services = services(
        permissions,
        recorder.clone(),
        &pipeline,
        &sweep,
        &speech_store,
        codex,
        handover.as_ref().map(|(service, mac_id)| (service, *mac_id)),
        options.opener,
        paths.support_directory.join("preferences.json"),
        secrets.clone(),
        &runtime,
    );

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
        speech_store,
        zone,
        version: options.version,
        startup_warnings: warnings,
    })
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
                            MeetingEvent::SpeakersNeedReview { .. } | MeetingEvent::Deleted { .. } => {
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
        // itself (the Swift app had GRDB observation).
        let poll_host = host.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                interval.tick().await;
                poll_host.store_changed();
            }
        });

        if let Err(error) = self.store.fail_interrupted_recordings(
            "Steno quit before this recording ended.",
            Utc::now(),
        ) {
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
        let _ = BTreeMap::<String, String>::new();
    }
}
