//! What the unit tests share: a store in a temp directory, the
//! pipeline's dependencies over core's fakes, the app's graph over them,
//! the options of the product's graph under a temp directory, frames for a
//! recording writer, a live check that counts every master old, a paired
//! phone, a recording it delivered, a store that keeps the last phone, a
//! fingerprint record that refuses writes, the waits that fail a test
//! instead of hanging it ([`PATIENCE`] for the tests' own fakes and gates,
//! [`HANG_GUARD`] for real capture work), and
//! [`warnings`], what a call logs at `warn` and above.
//!
//! A test that writes models gives the app an explicit models directory in
//! its temp directory (the settings' one wins over `STENO_MODELS_DIR`), and
//! `speech::testing`'s writers panic outside the temp directory or inside
//! the directory `STENO_MODELS_DIR` names, so no test overwrites the
//! models a developer keeps for the real-model tests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use steno_adapters::DeliveryCoordinator;
use steno_audio::SymphoniaAudioCodec;
use steno_core::Store;
use steno_core::testing::{FakeDiarizer, FakeSpeechEngine, InMemorySpeakerMemory};
use steno_host::services::Recorder as _;
use steno_pipeline::{MeetingEventBus, PipelineDependencies};
use steno_speech::SpeechRuntime;

use crate::pipeline::{BuiltEngine, BuiltPipeline, CurrentPipeline, MakeDependencies};
use crate::recorder::MakeCaptureSession;

/// How long a test waits for work driven by its own fakes, gates and
/// manual clock before it fails. That work takes milliseconds, so this
/// bound only turns a hang into a failure; for real capture work, the
/// synthetic capture's first audio included, see [`HANG_GUARD`].
pub const PATIENCE: Duration = Duration::from_secs(5);

/// How long a test waits for real work over the synthetic capture before
/// it fails as hung: a start, a stop and save, a quit, a rebuild or
/// restart, a recording that ends on its own, and the first audio once
/// it records (a level, frames in the files), which passes the producer,
/// the processing and the writer threads at wall-clock pace. A loaded
/// runner stretches that work past [`PATIENCE`]. A gate that waits while
/// such work runs, before the test can open it, takes this bound too,
/// and so does a call that does the work only when the behaviour under
/// test is broken (a refused start accepted after all), so the break
/// fails on its assertion. A gate the test opens after only its own
/// checks keeps [`PATIENCE`], so a claim can be timed against it. No
/// verdict rests on how long the work took: a test about how often
/// something happens counts it, and a product patience a test passes in
/// takes this bound unless that patience is what it checks. An outage
/// that can end a wait's condition (a device change whose rebuild
/// silences the meter) is held by the test until that wait is over
/// ([`HeldOutages`]): once the outage began, no bound would help. The
/// value is steno-audio's, which its capture tests and
/// `ManualClock::wait_for_sleepers` wait too.
pub const HANG_GUARD: Duration = steno_audio::testing::HANG_GUARD;

/// The synthetic backends a test's capture built with their outages held
/// (`SyntheticOptions::hold_outages`), so that the test lets each outage
/// go once it saw what comes before it.
#[derive(Clone, Default)]
pub struct HeldOutages(Arc<Mutex<Vec<Arc<steno_audio::testing::SyntheticCaptureBackend>>>>);

impl HeldOutages {
    /// A backend over `options` with its outages held, kept for
    /// [`release`](Self::release).
    pub fn backend(
        &self,
        options: steno_audio::testing::synthetic::SyntheticOptions,
    ) -> Arc<steno_audio::testing::SyntheticCaptureBackend> {
        let backend = Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(
            options.hold_outages(),
        ));
        self.0.lock().unwrap().push(backend.clone());
        backend
    }

    /// Lets the next held outage of every backend kept go. Call it once
    /// the start built the capture: a release saves nothing for a backend
    /// built later.
    pub fn release(&self) {
        let backends = self.0.lock().unwrap();
        assert!(
            !backends.is_empty(),
            "released before the start built the capture"
        );
        for backend in backends.iter() {
            backend.release_outage();
        }
    }
}

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
    synthetic_capture_through(|writer| Box::new(writer))
}

/// [`synthetic_capture`], each session's files written through what
/// `wrap` makes of the production writer: a writer that dies, or one held
/// at a gate.
pub fn synthetic_capture_through(
    wrap: impl Fn(steno_audio::RecordingWriter) -> Box<dyn steno_audio::writer::RecordingWriting>
    + Send
    + Sync
    + 'static,
) -> MakeCaptureSession {
    synthetic_capture_writing(Arc::new(move |layout, lanes, keep_raw| {
        Ok(wrap(steno_audio::RecordingWriter::new(
            layout, lanes, keep_raw,
        )?))
    }))
}

/// [`synthetic_capture`] whose sessions make their writer with
/// `make_writer` when they start: a writer that cannot be created fails
/// the start.
pub fn synthetic_capture_writing(
    make_writer: steno_audio::capture::RecordingWriterFactory,
) -> MakeCaptureSession {
    Arc::new(move |configuration: steno_audio::CaptureConfiguration| {
        let options = synthetic_tone(&configuration);
        steno_audio::CaptureSession::with_writer_factory(
            configuration,
            Arc::new(steno_audio::testing::SyntheticCaptureBackend::new(options)),
            None,
            steno_audio::CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
            Arc::new(steno_audio::SystemClock::new()),
            make_writer.clone(),
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

/// A capture recorder over [`synthetic_capture`] and fakes, recording
/// into a temp audio folder, for the tests that drive it from outside
/// (the auto-stop, the detection prompt). Dropped after a test that
/// passed, it stops a recording still running; after a failed one it
/// leaves the recording, since a stop stuck on the recorder's lock would
/// hang the test binary. Either way it then waits for the pipeline's
/// runs, so no file of a test's processing is left behind; that needs a
/// multi-threaded runtime. The temp directory is dropped last, after the
/// store, the pipeline and the recorder.
pub struct SyntheticRecorder {
    pub store: Arc<Store>,
    pipeline: Arc<CurrentPipeline>,
    pub recorder: Arc<crate::recorder::CaptureRecorder>,
    _dir: tempfile::TempDir,
}

/// A [`SyntheticRecorder`] on the current runtime.
pub fn synthetic_recorder() -> SyntheticRecorder {
    synthetic_recorder_over(synthetic_capture())
}

/// A [`SyntheticRecorder`] whose sessions `make_session` builds: one whose
/// device changes, say.
pub fn synthetic_recorder_over(make_session: MakeCaptureSession) -> SyntheticRecorder {
    let (dir, store) = temp_store();
    let mut settings = store.settings().unwrap();
    settings.audio_folder = steno_core::paths::file_url(&dir.path().join("audio"), true);
    store.save_settings(&settings).unwrap();
    let fakes = steno_host::fakes::FakeServices::new(chrono::Utc::now());
    let pipeline = current_pipeline(fake_dependencies(&store, "fake-engine"));
    let recorder = crate::recorder::CaptureRecorder::new(
        store.clone(),
        pipeline.clone(),
        make_session,
        fakes.permissions.clone(),
        fakes.speech_models.clone(),
        chrono::FixedOffset::east_opt(0).unwrap(),
        tokio::runtime::Handle::current(),
        dir.path().join("support"),
    );
    SyntheticRecorder {
        store,
        pipeline,
        recorder,
        _dir: dir,
    }
}

impl SyntheticRecorder {
    /// Starts a recording in `mode` for `call_app` on a thread of its own,
    /// as the host calls it, and returns once the start returned, refused
    /// or not, failing the test as hung after [`HANG_GUARD`].
    pub fn start(&self, mode: steno_bridge::CaptureMode, call_app: Option<&str>) {
        let (recorder, call_app) = (self.recorder.clone(), call_app.map(str::to_owned));
        on_own_thread(HANG_GUARD, "the start returned", move || {
            recorder.start(mode, call_app.as_deref());
        });
    }

    /// Stops the recording on a thread of its own, as the host calls it,
    /// and returns once the stop saved it, failing the test as hung after
    /// [`HANG_GUARD`].
    pub fn stop(&self) {
        let recorder = self.recorder.clone();
        on_own_thread(HANG_GUARD, "the stop returned", move || recorder.stop());
    }

    /// Waits until the recorder is idle again: a stop it began on its own
    /// (a grace that ran out, say) saved the recording.
    pub async fn idle(&self, what: &str) {
        eventually_within(HANG_GUARD, what, || {
            self.recorder.status().state == steno_bridge::RecordingState::Idle
        })
        .await;
    }
}

impl Drop for SyntheticRecorder {
    fn drop(&mut self) {
        // Panicking first: `is_busy` takes the recorder's lock, which a
        // stop that failed the test may still hold.
        if !std::thread::panicking() && self.recorder.is_busy() {
            self.stop();
        }
        let pipeline = self.pipeline.current();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(pipeline.wait_until_idle());
        });
    }
}

/// The options of the product's graph under `support`, without a
/// capture backend, a keyring, an updater, permissions, a calendar,
/// detection or the Swift import's marker.
pub fn options_under(support: &std::path::Path) -> crate::AppOptions {
    crate::AppOptions {
        paths: steno_core::StenoPaths::new(support),
        database_path: None,
        keyring: false,
        opener: Arc::new(steno_host::fakes::FakeOpener::default()),
        login_item: None,
        update_source: None,
        permissions: None,
        calendar: None,
        runtime: tokio::runtime::Handle::current(),
        version: "0.0.0".to_owned(),
        make_capture_session: Arc::new(|_| Err("no capture in this test".to_owned())),
        detection: None,
        lock_patience: Duration::ZERO,
        installed_bundle: false,
        import_marker: None,
    }
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
        root.join("support"),
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
        updates: None,
        handover: None,
        gated_handover: None,
        import_gate: None,
        recorder,
        detection: None,
        models_directory: root.join("models"),
        zone,
        runtime: tokio::runtime::Handle::current(),
        version: "0.0.0".to_owned(),
        installed_bundle: false,
        startup_warnings: Vec::new(),
        live_recording_check: crate::recovery::LiveRecordingCheck::default(),
        launch_work: std::sync::Mutex::default(),
        secrets_unlocked: std::sync::Mutex::new(None),
        database_lock: None,
    }
}

/// A live check whose clock reads an hour after now: every master is a
/// crash's, and none is waited for.
pub fn an_hour_later() -> crate::recovery::LiveRecordingCheck {
    crate::recovery::LiveRecordingCheck {
        now: Arc::new(|| std::time::SystemTime::now() + Duration::from_secs(3_600)),
        wait: Arc::new(|_| panic!("an old master is not waited for")),
        ..crate::recovery::LiveRecordingCheck::default()
    }
}

/// A fingerprint record over the app's file that refuses every record of
/// a fingerprint while `refuse` is set, as a full disk does, so a Pair
/// again stops at its last step; the next fingerprint's note is written.
pub struct RefusingRecord {
    pub file: Arc<crate::handover::FingerprintFile>,
    pub refuse: std::sync::atomic::AtomicBool,
}

impl steno_handover::FingerprintRecord for RefusingRecord {
    fn recorded(&self) -> steno_core::BoundaryResult<Option<String>> {
        self.file.recorded()
    }

    fn record(&self, fingerprint: &str) -> steno_core::BoundaryResult<()> {
        if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("No space left on device".into());
        }
        self.file.record(fingerprint)
    }

    fn next(&self) -> steno_core::BoundaryResult<Option<String>> {
        self.file.next()
    }

    fn note_next(&self, fingerprint: Option<&str>) -> steno_core::BoundaryResult<()> {
        self.file.note_next(fingerprint)
    }
}

/// Saves a paired phone in `store`, the evidence that an identity existed;
/// each call another phone, with a token of its own.
pub fn pair_a_phone(store: &Store) {
    pair_a_phone_at(store, chrono::Utc::now());
}

/// [`pair_a_phone`], paired at `paired_at`.
pub fn pair_a_phone_at(store: &Store, paired_at: chrono::DateTime<chrono::Utc>) {
    let id = uuid::Uuid::new_v4();
    let token = [id.as_bytes().as_slice(), id.as_bytes()].concat();
    store
        .save_paired_device(
            &steno_core::PairedDevice {
                id,
                name: "Phone".to_owned(),
                paired_at,
                last_seen_at: None,
            },
            &token,
        )
        .unwrap();
}

/// Keeps the last paired phone in `store` whatever deletes it, as a disk
/// that refuses the write would, until [`let_the_last_phone_go`]: Pair
/// again stops right after the write with one phone paired, and after the
/// first phone with two.
pub fn keep_the_last_phone(store: &Store) {
    execute(
        store,
        "CREATE TRIGGER keep_the_last_phone BEFORE DELETE ON pairedDevice \
         WHEN (SELECT COUNT(*) FROM pairedDevice) = 1 \
         BEGIN SELECT RAISE(ABORT, 'the disk refused the write'); END;",
    );
}

/// Lets the phone [`keep_the_last_phone`] kept go again.
pub fn let_the_last_phone_go(store: &Store) {
    execute(store, "DROP TRIGGER keep_the_last_phone;");
}

fn execute(store: &Store, sql: &str) {
    store
        .write(|transaction| {
            transaction.execute_batch(sql)?;
            Ok(())
        })
        .unwrap();
}

/// A recording [`admit_a_recording`] delivered into a meeting.
pub struct Admitted {
    pub recording: uuid::Uuid,
    pub meeting: uuid::Uuid,
}

impl Admitted {
    /// The recording still reads as delivered into its meeting, which is
    /// still there.
    pub fn is_unchanged(&self, store: &Store) -> bool {
        store.admitted_meeting(self.recording, 4, &[7; 32]).unwrap() == Some(self.meeting)
            && store.meeting(self.meeting).unwrap().is_some()
    }
}

/// Admits a recording of the first paired phone in `store` as a meeting,
/// with its `complete` receipt and its row in the admission ledger (4
/// bytes, SHA-256 `[7; 32]`).
pub fn admit_a_recording(store: &Store) -> Admitted {
    let now = chrono::Utc::now();
    let meeting = steno_core::Meeting {
        id: uuid::Uuid::new_v4(),
        ..steno_core::testing::sample_data::meeting()
    };
    let asset = steno_core::AudioAsset {
        id: uuid::Uuid::new_v4(),
        meeting_id: meeting.id,
        url: format!("file:///tmp/Audio/{}/master.m4a", meeting.id),
        format: steno_core::AudioFormat::M4aAac,
        lanes: vec![steno_core::AudioLane::Mixed],
        sidecars_16k: std::collections::BTreeMap::new(),
        mixdown_url: None,
        retention: steno_core::AudioRetention::KeepForever,
        expires_at: None,
    };
    let recording = uuid::Uuid::new_v4();
    let receipt = steno_core::HandoverReceipt {
        recording_id: recording,
        device_id: store.paired_devices().unwrap()[0].id,
        state: steno_core::HandoverState::Complete {
            meeting_id: meeting.id,
        },
        byte_count: 4,
        sha256: vec![7; 32],
        chunk_size: 4,
        received_chunks: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    store
        .save_admission_durably(&receipt, &meeting, &asset)
        .unwrap();
    Admitted {
        recording,
        meeting: meeting.id,
    }
}

/// Waits until `done` holds, failing the test with `what` after
/// [`PATIENCE`].
pub async fn eventually(what: &str, done: impl FnMut() -> bool) {
    eventually_within(PATIENCE, what, done).await;
}

/// As [`eventually`], failing the test only after `patience`.
pub async fn eventually_within(patience: Duration, what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(patience, async {
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

/// A `Write` into a shared buffer, for a test's log subscriber.
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logged {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// What `run` returns, and the warnings it logged on this thread.
pub fn warnings<T>(run: impl FnOnce() -> T) -> (T, String) {
    let logged = Logged::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let logged = logged.clone();
            move || logged.clone()
        })
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    let value = tracing::subscriber::with_default(subscriber, run);
    let logged = String::from_utf8(logged.0.lock().unwrap().clone()).unwrap();
    (value, logged)
}
