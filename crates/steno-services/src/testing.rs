//! What the unit tests share: temp stores, fakes and the app's graph over
//! them, recording frames, paired phones, and the waits that fail a test
//! instead of hanging it. A test that writes models gives the app a models
//! directory in its temp directory, so no test overwrites a developer's
//! real models.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
/// manual clock before it fails; real capture work takes [`HANG_GUARD`].
pub const PATIENCE: Duration = Duration::from_secs(5);

/// How long a test waits for real work (the synthetic capture, a real
/// handover listener's build) before it fails as hung. A loaded runner
/// stretches that work past [`PATIENCE`]; no verdict rests on how long it
/// took.
pub const HANG_GUARD: Duration = steno_audio::testing::HANG_GUARD;

/// What "live" means in the held-store checks: one call or one hop that
/// must not wait on the store.
pub const LIVE: Duration = Duration::from_secs(1);

/// The bound of a held-store check from the user's side: well under the
/// five-second freeze a wait on the store would show.
pub const END_TO_END: Duration = Duration::from_secs(2);

/// The synthetic backends a test's capture built with their outages held,
/// so the test lets each outage go once it saw what comes before it.
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

    /// Lets the next held outage of every kept backend go. Call it once the
    /// start built the capture.
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
    let store = Arc::new(Store::open(database_in(dir.path())).unwrap());
    (dir, store)
}

/// The database [`temp_store`] opens in `dir`.
pub fn database_in(dir: &Path) -> PathBuf {
    dir.join("steno.sqlite")
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

/// `dependencies` as a build of `parakeet-v3` in the speech sidecar.
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
    current_pipeline_of(built(dependencies))
}

/// [`current_pipeline`] over the build `first`; every reload hands back
/// that same build.
fn current_pipeline_of(first: BuiltPipeline) -> Arc<CurrentPipeline> {
    let make: MakeDependencies = {
        let (dependencies, engine) = (first.dependencies.clone(), first.engine.clone());
        Arc::new(move || {
            Ok(BuiltPipeline {
                dependencies: dependencies.clone(),
                engine: engine.clone(),
            })
        })
    };
    Arc::new(CurrentPipeline::new(
        first,
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
/// `wrap` makes of the production writer.
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
/// `make_writer` when they start.
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

/// A capture recorder over [`synthetic_capture`] and fakes, recording into
/// a temp audio folder. Dropped after a passing test, it stops a recording
/// still running, then waits for the pipeline's runs (needs a
/// multi-threaded runtime).
pub struct SyntheticRecorder {
    pub store: Arc<Store>,
    pipeline: Arc<CurrentPipeline>,
    pub recorder: Arc<crate::recorder::CaptureRecorder>,
    dir: tempfile::TempDir,
}

/// A [`SyntheticRecorder`] on the current runtime.
pub fn synthetic_recorder() -> SyntheticRecorder {
    synthetic_recorder_over(synthetic_capture())
}

/// A [`SyntheticRecorder`] whose sessions `make_session` builds.
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
        dir,
    }
}

impl SyntheticRecorder {
    /// The database file the store is over.
    pub fn database(&self) -> PathBuf {
        database_in(self.dir.path())
    }

    /// Starts a recording on a thread of its own and returns once the start
    /// returned, failing the test as hung after [`HANG_GUARD`].
    pub fn start(&self, mode: steno_bridge::CaptureMode, call_app: Option<&str>) {
        let (recorder, call_app) = (self.recorder.clone(), call_app.map(str::to_owned));
        on_own_thread(HANG_GUARD, "the start returned", move || {
            recorder.start(mode, call_app.as_deref());
        });
    }

    /// Stops the recording on a thread of its own and returns once it saved,
    /// failing the test as hung after [`HANG_GUARD`].
    pub fn stop(&self) {
        let recorder = self.recorder.clone();
        on_own_thread(HANG_GUARD, "the stop returned", move || recorder.stop());
    }

    /// Waits until the recorder is idle again.
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

/// The options of the product's graph under `support`, without a capture
/// backend, keyring, updater, permissions, calendar, detection or import
/// marker. The database's lock waits up to [`HANG_GUARD`]: a child another
/// test forked can hold a copy of the lock's file until it execs.
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
        lock_patience: HANG_GUARD,
        installed_bundle: false,
        import_marker: None,
    }
}

/// The graph `build` assembles, over fakes, with a recorder over
/// `make_session` that records into `root/audio`.
pub fn app_over_fakes(
    root: &std::path::Path,
    store: &Arc<Store>,
    make_session: MakeCaptureSession,
) -> crate::App {
    let fakes = steno_host::fakes::FakeServices::new(chrono::Utc::now());
    app_over_build(
        root,
        store,
        make_session,
        built(fake_dependencies(store, "fake-engine")),
        &fakes,
    )
}

/// [`app_over_fakes`] reading every recording through `decoder`.
pub fn app_decoding(
    root: &std::path::Path,
    store: &Arc<Store>,
    decoder: Arc<dyn steno_core::AudioDecoder>,
) -> crate::App {
    let mut dependencies = fake_dependencies(store, "fake-engine");
    dependencies.decoder = decoder;
    let fakes = steno_host::fakes::FakeServices::new(chrono::Utc::now());
    app_over_build(
        root,
        store,
        synthetic_capture(),
        built(dependencies),
        &fakes,
    )
}

/// [`app_over_fakes`] over the build `first` and the host's `fakes`.
fn app_over_build(
    root: &std::path::Path,
    store: &Arc<Store>,
    make_session: MakeCaptureSession,
    first: BuiltPipeline,
    fakes: &steno_host::fakes::FakeServices,
) -> crate::App {
    app_over_pipeline(
        root,
        store,
        make_session,
        current_pipeline_of(first),
        fakes.speech_models.clone(),
        fakes,
    )
}

/// [`app_over_fakes`] over `pipeline` and `speech_models`, the rest of the
/// host's services from `fakes`, recording into `root/audio`.
fn app_over_pipeline(
    root: &std::path::Path,
    store: &Arc<Store>,
    make_session: MakeCaptureSession,
    pipeline: Arc<CurrentPipeline>,
    speech_models: Arc<dyn steno_host::services::SpeechModels>,
    fakes: &steno_host::fakes::FakeServices,
) -> crate::App {
    let mut settings = store.settings().unwrap();
    settings.audio_folder = steno_core::paths::file_url(&root.join("audio"), true);
    store.save_settings(&settings).unwrap();
    let zone = chrono::FixedOffset::east_opt(0).unwrap();
    let recorder = crate::recorder::CaptureRecorder::new(
        store.clone(),
        pipeline.clone(),
        make_session,
        fakes.permissions.clone(),
        speech_models.clone(),
        zone,
        tokio::runtime::Handle::current(),
        root.join("support"),
    );
    let mut services = fakes.services();
    services.recorder = recorder.clone();
    services.speech_models = speech_models;
    crate::App {
        paths: steno_core::StenoPaths::new(root.join("support")),
        store: store.clone(),
        secrets: Arc::new(steno_core::testing::InMemorySecretStore::new()),
        events: MeetingEventBus::new(),
        pipeline,
        sweep: steno_pipeline::RetentionSweep::new(store.clone()),
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
        tone_directory: None,
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

/// A fingerprint record that refuses every write while `refuse` is set, as
/// a full disk does.
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

/// Saves another paired phone in `store`, with a token of its own.
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
/// that refuses the write would, until [`let_the_last_phone_go`].
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
    /// The recording still reads as delivered into its still existing meeting.
    pub fn is_unchanged(&self, store: &Store) -> bool {
        store.admitted_meeting(self.recording, 4, &[7; 32]).unwrap() == Some(self.meeting)
            && store.meeting(self.meeting).unwrap().is_some()
    }
}

/// Admits a recording of the first paired phone in `store` as a meeting,
/// with its `complete` receipt and its admission ledger row.
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

/// Waits until `done` holds, failing the test after [`PATIENCE`].
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

/// What `found` returns once it returns something, failing the test after
/// `patience`.
pub fn found_within<T>(patience: Duration, what: &str, mut found: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + patience;
    loop {
        if let Some(value) = found() {
            return value;
        }
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Calls `f` on a thread of its own, failing the test unless it returns
/// within `patience`. Not `spawn_blocking`: the runtime waits for those at
/// shutdown, so a stuck call would hang the test.
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

/// `frames` frames of a tone on every lane of `writer`.
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

/// Another connection holding the database's write lock for
/// [`HeldStore::HOLD`], as another process does. Dropped, it lets the lock
/// go.
pub struct HeldStore {
    since: Instant,
    released: Arc<AtomicBool>,
    hold: Option<steno_core::testing::WriteLockHold>,
    waiting: Option<std::thread::JoinHandle<()>>,
}

impl HeldStore {
    /// Past the five-second busy timeout, so a waiting write fails once and
    /// tries again.
    pub const HOLD: Duration = Duration::from_secs(7);

    /// The lock held on the database at `path`, and no write waiting.
    pub fn lock_only(path: &Path) -> HeldStore {
        HeldStore {
            since: Instant::now(),
            released: Arc::new(AtomicBool::new(false)),
            hold: Some(steno_core::testing::WriteLockHold::new(path)),
            waiting: None,
        }
    }

    /// The lock held on `path`, and a write through `store` waiting behind it
    /// until the lock goes. Returns once that write is in its wait.
    pub fn with_a_write_waiting(store: &Arc<Store>, path: &Path) -> HeldStore {
        let store = store.clone();
        HeldStore::with_waiting(path, move || {
            let _ = store.write(|_| Ok(()));
        })
    }

    /// The lock held on `path`, and `write` run again and again behind it until
    /// the lock goes. Returns once the first is in its wait.
    pub fn with_waiting(path: &Path, mut write: impl FnMut() + Send + 'static) -> HeldStore {
        let mut held = HeldStore::lock_only(path);
        let released = held.released.clone();
        held.waiting = Some(std::thread::spawn(move || {
            while !released.load(Ordering::SeqCst) {
                write();
            }
        }));
        // The write's busy wait has begun by now; nothing reports it.
        std::thread::sleep(Duration::from_millis(100));
        held
    }

    /// Lets the lock go once [`Self::HOLD`] has passed; returns when it did.
    pub fn release_after_hold(&mut self) -> Instant {
        std::thread::sleep((self.since + Self::HOLD).saturating_duration_since(Instant::now()));
        self.release()
    }

    /// Lets the lock go and waits for the waiting write to commit.
    pub fn release(&mut self) -> Instant {
        self.released.store(true, Ordering::SeqCst);
        drop(self.hold.take());
        let released = Instant::now();
        if let Some(waiting) = self.waiting.take() {
            waiting.join().unwrap();
        }
        released
    }
}

impl Drop for HeldStore {
    fn drop(&mut self) {
        self.release();
    }
}

/// Waits up to `within` of `released` until `meeting_id` was saved.
pub fn saved_within(store: &Store, meeting_id: uuid::Uuid, released: Instant, within: Duration) {
    let deadline = released + within;
    loop {
        let state = store.meeting(meeting_id).unwrap().unwrap().state;
        if state != steno_core::MeetingState::Recording {
            assert!(
                !matches!(state, steno_core::MeetingState::Failed { .. }),
                "handed to processing, got {state:?}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "saved within {within:?} of the lock going"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The time a snapshot arrived, its topic, its payload and the name of
/// the thread that emitted it.
type Arrival = (
    Instant,
    steno_bridge::BridgeTopic,
    serde_json::Value,
    Option<String>,
);

/// A sink that keeps every snapshot with the time it arrived and the
/// thread that sent it.
#[derive(Default)]
pub struct TimedSink(Mutex<Vec<Arrival>>);

impl steno_bridge::EventSink for TimedSink {
    fn emit(&self, event: steno_bridge::BridgeEvent) {
        let thread = std::thread::current().name().map(str::to_owned);
        self.0
            .lock()
            .unwrap()
            .push((Instant::now(), event.topic, event.payload, thread));
    }
}

impl TimedSink {
    /// The snapshots of `topic` that arrived at or after `since`.
    pub fn since(
        &self,
        topic: steno_bridge::BridgeTopic,
        since: Instant,
    ) -> Vec<(Instant, serde_json::Value)> {
        self.since_on(topic, since)
            .into_iter()
            .map(|(at, payload, _)| (at, payload))
            .collect()
    }

    /// [`Self::since`] with the name of the thread that sent each.
    pub fn since_on(
        &self,
        topic: steno_bridge::BridgeTopic,
        since: Instant,
    ) -> Vec<(Instant, serde_json::Value, Option<String>)> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(at, arrived, _, _)| *arrived == topic && *at >= since)
            .map(|(at, _, payload, thread)| (*at, payload.clone(), thread.clone()))
            .collect()
    }

    /// When the first `recording` snapshot in `state` at or after `since`
    /// arrived, waiting up to `patience`.
    pub fn recording_in(&self, state: &str, since: Instant, patience: Duration) -> Option<Instant> {
        let deadline = Instant::now() + patience;
        loop {
            let reached = self
                .since(steno_bridge::BridgeTopic::Recording, since)
                .into_iter()
                .find(|(_, payload)| payload["state"] == state)
                .map(|(at, _)| at);
            if reached.is_some() || Instant::now() >= deadline {
                return reached;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// One `recorder_changed` call the recorder's hook made.
#[derive(Debug, Clone)]
pub struct HookCall {
    pub began: Instant,
    pub took: Duration,
    pub thread: Option<String>,
    pub state: steno_bridge::RecordingState,
    pub armed: bool,
}

/// A host over `store`, as [`hosted`] builds it.
pub struct Hosted {
    pub host: Arc<steno_host::Host>,
    pub sink: Arc<TimedSink>,
    hooks: Arc<Mutex<Vec<HookCall>>>,
}

/// A host over `store` and the fakes with `recorder`'s changes routed into
/// it as `App::launch` routes them, and a [`TimedSink`].
pub fn hosted(store: &Arc<Store>, recorder: &Arc<crate::recorder::CaptureRecorder>) -> Hosted {
    hosted_over(store, recorder, None)
}

/// [`hosted`], over `files` in place of the fake's when there is one.
pub fn hosted_over(
    store: &Arc<Store>,
    recorder: &Arc<crate::recorder::CaptureRecorder>,
    files: Option<Arc<dyn steno_host::services::FileSystem>>,
) -> Hosted {
    use steno_bridge::BridgeHost as _;
    let mut services = steno_host::fakes::FakeServices::new(chrono::Utc::now()).services();
    services.recorder = recorder.clone();
    if let Some(files) = files {
        services.file_system = files;
    }
    let host = Arc::new(
        steno_host::Host::new(store.clone(), services, steno_host::HostConfig::default()).unwrap(),
    );
    let sink = Arc::new(TimedSink::default());
    host.attach(sink.clone());
    host.page_ready().unwrap();
    let hooks = Arc::new(Mutex::new(Vec::new()));
    let (hooked, watched, calls) = (
        Arc::downgrade(&host),
        Arc::downgrade(recorder),
        hooks.clone(),
    );
    recorder.on_change(Arc::new(move || {
        let (Some(host), Some(recorder)) = (hooked.upgrade(), watched.upgrade()) else {
            return;
        };
        let began = Instant::now();
        host.recorder_changed();
        let took = began.elapsed();
        let status = recorder.status();
        calls.lock().unwrap().push(HookCall {
            began,
            took,
            thread: std::thread::current().name().map(str::to_owned),
            state: status.state,
            armed: status.auto_stop.is_some(),
        });
    }));
    Hosted { host, sink, hooks }
}

impl Hosted {
    /// A forced reload in flight on a thread of its own.
    pub fn reload_in_flight(&self) -> std::thread::JoinHandle<()> {
        let host = self.host.clone();
        std::thread::spawn(move || host.store_changed())
    }

    /// The hook's calls that began at or after `since`.
    pub fn hooks_since(&self, since: Instant) -> Vec<HookCall> {
        self.hooks
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.began >= since)
            .cloned()
            .collect()
    }
}

/// A file system whose `exists`, which a shown detail's reload asks about
/// its recording ([`shown_meeting`]), waits at a gate while it is armed,
/// standing for a reload that waits on the store. Set with
/// [`Self::gate_inside_reads_on`], it waits inside a read on that store.
#[derive(Default)]
pub struct GatedFiles {
    pub gate: ReloadGate,
    reader: std::sync::OnceLock<Arc<Store>>,
}

impl GatedFiles {
    /// From now on the gate is passed inside a read on `store`; the first
    /// call wins.
    pub fn gate_inside_reads_on(&self, store: Arc<Store>) {
        let _ = self.reader.set(store);
    }
}

impl steno_host::services::FileSystem for GatedFiles {
    fn exists(&self, path: &std::path::Path) -> bool {
        match self.reader.get() {
            Some(store) => store
                .read(|_| {
                    self.gate.pass();
                    Ok(())
                })
                .unwrap(),
            None => self.gate.pass(),
        }
        path.exists()
    }

    fn remove(&self, path: &std::path::Path) -> steno_core::protocols::BoundaryResult<()> {
        steno_host::services::RealFileSystem.remove(path)
    }
}

/// The sample meeting saved ready in `store` with an asset, so its shown detail asks
/// the file system about the recording at every reload.
pub fn shown_meeting(store: &Store) -> steno_core::Meeting {
    let mut meeting = steno_core::testing::sample_data::meeting();
    meeting.state = steno_core::MeetingState::Ready;
    let asset = steno_core::AudioAsset {
        id: uuid::Uuid::new_v4(),
        meeting_id: meeting.id,
        url: steno_core::paths::file_url(std::path::Path::new("/no-such-folder/master.wav"), false),
        format: steno_core::AudioFormat::Wav16kInt16,
        lanes: vec![steno_core::AudioLane::Mixed],
        sidecars_16k: std::collections::BTreeMap::new(),
        mixdown_url: None,
        retention: steno_core::AudioRetention::KeepForever,
        expires_at: None,
    };
    store.save_meeting_with_asset(&meeting, &asset).unwrap();
    meeting
}

#[derive(Default)]
pub struct ReloadGate {
    state: std::sync::Mutex<GateState>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
pub struct GateState {
    armed: bool,
    /// Reloads waiting at the gate now.
    pub held: usize,
    /// Reloads the gate has held, in all.
    pub entered: usize,
    /// Reloads that passed the gate or reached it, in all.
    pub reloads: usize,
}

impl ReloadGate {
    fn state(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap()
    }

    fn pass(&self) {
        let mut state = self.state();
        state.reloads += 1;
        if state.armed {
            state.held += 1;
            state.entered += 1;
            self.changed.notify_all();
            while state.armed {
                state = self.changed.wait(state).unwrap();
            }
            state.held -= 1;
        }
        self.changed.notify_all();
    }

    pub fn reloads(&self) -> usize {
        self.state().reloads
    }

    /// Waits up to `patience` for `reached`; whether it was.
    pub fn wait_for(
        &self,
        patience: std::time::Duration,
        reached: impl Fn(&GateState) -> bool,
    ) -> bool {
        let (state, _) = self
            .changed
            .wait_timeout_while(self.state(), patience, |state| !reached(state))
            .unwrap();
        reached(&state)
    }

    /// Arms the gate until the guard drops.
    pub fn arm(&self) -> ArmedGate<'_> {
        self.state().armed = true;
        ArmedGate(self)
    }
}

pub struct ArmedGate<'a>(&'a ReloadGate);

impl Drop for ArmedGate<'_> {
    fn drop(&mut self) {
        self.0.state().armed = false;
        self.0.changed.notify_all();
    }
}
