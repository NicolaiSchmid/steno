//! What the unit tests share: a store in a temp directory, the
//! pipeline's dependencies over core's fakes, the app's graph over them,
//! frames for a recording writer, and the waits that fail a test instead
//! of hanging it.

use std::sync::Arc;
use std::time::Duration;

use steno_adapters::DeliveryCoordinator;
use steno_audio::SymphoniaAudioCodec;
use steno_core::Store;
use steno_core::testing::{FakeDiarizer, FakeSpeechEngine, InMemorySpeakerMemory};
use steno_pipeline::{MeetingEventBus, PipelineDependencies};
use steno_speech::SpeechRuntime;

use crate::pipeline::{BuiltEngine, BuiltPipeline, CurrentPipeline, MakeDependencies};
use crate::recorder::MakeCaptureSession;

/// How long a test waits for background work before it fails.
pub const PATIENCE: Duration = Duration::from_secs(5);

/// A fresh store; the directory lives as long as the guard.
pub fn temp_store() -> (tempfile::TempDir, Arc<Store>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    (dir, store)
}

/// Fakes over `store`, the engine named `engine_id` so a run shows which
/// pipeline it went through.
pub fn fake_dependencies(store: &Arc<Store>, engine_id: &str) -> PipelineDependencies {
    PipelineDependencies::new(
        Arc::new(SymphoniaAudioCodec::new()),
        Arc::new(FakeSpeechEngine {
            id: engine_id.to_owned(),
            ..FakeSpeechEngine::default()
        }),
        Arc::new(FakeDiarizer::default()),
        Arc::new(InMemorySpeakerMemory::new(Vec::new())),
        Arc::new(DeliveryCoordinator::new(store.clone())),
        store.clone(),
        MeetingEventBus::new(),
    )
}

/// `dependencies` as a build of `parakeet-v3` in the speech sidecar; only
/// the recorder's warm-up reads the engine, and its tests build their own
/// (`recorder::tests::harness_over`).
pub fn built(dependencies: PipelineDependencies) -> BuiltPipeline {
    BuiltPipeline {
        dependencies,
        engine: BuiltEngine {
            engine_id: "parakeet-v3".to_owned(),
            runtime: SpeechRuntime::OnnxSidecar,
        },
    }
}

/// A pipeline on the current runtime whose reload builds `dependencies`
/// again.
pub fn current_pipeline(dependencies: PipelineDependencies) -> Arc<CurrentPipeline> {
    let make: MakeDependencies = {
        let dependencies = dependencies.clone();
        Arc::new(move || Ok(built(dependencies.clone())))
    };
    Arc::new(CurrentPipeline::new(
        built(dependencies),
        make,
        tokio::runtime::Handle::current(),
    ))
}

/// Capture sessions over a synthetic backend that plays a tone on the
/// microphone lane in real time, for up to ten minutes.
pub fn synthetic_capture() -> MakeCaptureSession {
    Arc::new(|configuration: steno_audio::CaptureConfiguration| {
        let options = synthetic_tone(&configuration);
        steno_audio::CaptureSession::with_backend(
            configuration,
            Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
            None,
            steno_audio::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(steno_audio::SystemClock::new()),
        )
        .map_err(|error| error.to_string())
    })
}

/// What [`synthetic_capture`] plays for `configuration`: a tone on the
/// microphone lane in real time, for up to ten minutes.
pub fn synthetic_tone(
    configuration: &steno_audio::CaptureConfiguration,
) -> steno_audio::testing::synthetic::SyntheticOptions {
    let mut options = steno_audio::testing::synthetic::SyntheticOptions::tones(
        &configuration.lanes(),
        &[(steno_core::AudioLane::Mic, 440.0)],
        600.0,
    );
    options.real_time = true;
    options
}

/// The graph `build` assembles, over fakes, with a recorder over
/// `make_session` that records into `root/audio`: what `App::shutdown`
/// drives, the host's recorder, and the launch. On the current runtime.
pub fn app_over_fakes(
    root: &std::path::Path,
    store: &Arc<Store>,
    make_session: MakeCaptureSession,
) -> crate::App {
    let mut settings = store.settings().unwrap();
    settings.audio_folder = steno_core::paths::file_url(&root.join("audio"), true);
    store.save_settings(&settings).unwrap();
    let pipeline = current_pipeline(fake_dependencies(store, "fake-engine"));
    let zone = chrono::FixedOffset::east_opt(0).unwrap();
    let fakes = steno_host::fakes::FakeServices::new(chrono::Utc::now());
    let recorder = crate::recorder::CaptureRecorder::new(
        store.clone(),
        pipeline.clone(),
        make_session,
        fakes.permissions.clone(),
        fakes.speech_models.clone(),
        zone,
        tokio::runtime::Handle::current(),
    );
    let mut services = fakes.services();
    services.recorder = recorder.clone();
    crate::App {
        paths: steno_core::StenoPaths::new(root.join("support")),
        store: store.clone(),
        secrets: Arc::new(steno_core::testing::InMemorySecretStore::new()),
        events: MeetingEventBus::new(),
        pipeline,
        sweep: steno_pipeline::RetentionSweep::new(store.clone()),
        export_retries: Arc::new(steno_pipeline::ExportRetries::in_memory()),
        services,
        handover: None,
        recorder,
        models_directory: root.join("models"),
        zone,
        runtime: tokio::runtime::Handle::current(),
        version: "0.0.0".to_owned(),
        startup_warnings: Vec::new(),
        live_recording_check: crate::recovery::LiveRecordingCheck::default(),
        launch_work: std::sync::Mutex::default(),
        database_lock: None,
    }
}

/// Waits until `done` holds, failing the test with `what` after
/// [`PATIENCE`].
pub async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(PATIENCE, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(what);
}

/// Calls `f` on a thread of its own, as the host calls its services,
/// failing the test with `what` unless it returns within `patience`. Not
/// a `spawn_blocking` task: the runtime waits for those when it shuts
/// down, so a call stuck on the work would hang the test instead of
/// failing it.
pub fn on_own_thread<T: Send + 'static>(
    patience: Duration,
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(f());
    });
    match receiver.recv_timeout(patience) {
        Ok(value) => value,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("{what}"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!("the call panicked"),
    }
}

/// `frames` frames of a tone on every lane of `writer`, as a capture
/// hands them over.
pub fn write_frames(writer: &mut steno_audio::RecordingWriter, frames: usize) {
    use steno_audio::FRAME_SIZE;
    use steno_audio::writer::{LaneFrames, RecordingWriting as _};

    let lanes = writer.lanes().len();
    let tone = steno_audio::testing::AudioFixtures::tone(440.0, 0.01, 0.5);
    let slices: Vec<&[f32]> = (0..lanes).map(|_| &tone[..FRAME_SIZE]).collect();
    for _ in 0..frames {
        writer
            .write(&LaneFrames {
                frame_count: FRAME_SIZE,
                lanes: &slices,
                raw_mic: None,
            })
            .unwrap();
    }
}
