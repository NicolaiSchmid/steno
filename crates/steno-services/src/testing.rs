//! What the unit tests share: a store in a temp directory, the
//! pipeline's dependencies over core's fakes, the app's graph over them,
//! the options of the product's graph under a temp directory, frames for a
//! recording writer, a live check that counts every master old, a paired
//! phone, a recording it delivered, a store that keeps the last phone, a
//! fingerprint record that refuses writes, the waits that fail a test
//! instead of hanging it ([`PATIENCE`] for the tests' own fakes and gates,
//! [`HANG_GUARD`] for real capture work and a real listener's build),
//! [`warnings`], what a call logs at `warn` and above, and what the checks
//! of `.plans/2026-10-12-store-off-the-workers.md` run under: a store held
//! by another connection ([`HeldStore`]) and a host whose sink keeps the
//! time of each snapshot ([`hosted`]).
//!
//! A test that writes models gives the app an explicit models directory in
//! its temp directory (the settings' one wins over `STENO_MODELS_DIR`), and
//! `speech::testing`'s writers panic outside the temp directory or inside
//! the directory `STENO_MODELS_DIR` names, so no test overwrites the
//! models a developer keeps for the real-model tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use steno_adapters::DeliveryCoordinator;
use steno_audio::SymphoniaAudioCodec;
use steno_core::Store;
use steno_core::testing::{FakeDiarizer, FakeSpeechEngine, InMemorySpeakerMemory};
use steno_host::services::Recorder as _;
use steno_host::speech::ModelAsset;
use steno_pipeline::{MeetingEventBus, PipelineDependencies};
use steno_speech::SpeechRuntime;

use crate::pipeline::{BuiltEngine, BuiltPipeline, CurrentPipeline, MakeDependencies};
use crate::recorder::MakeCaptureSession;

/// How long a test waits for work driven by its own fakes, gates and
/// manual clock before it fails. That work takes milliseconds, so this
/// bound only turns a hang into a failure; for real capture work, the
/// synthetic capture's first audio included, and a real listener's
/// build, see [`HANG_GUARD`].
pub const PATIENCE: Duration = Duration::from_secs(5);

/// How long a test waits for real work (over the synthetic capture, or a
/// real handover listener's build) before it fails as hung: a start, a stop and save, a quit, a rebuild or
/// restart, a recording that ends on its own, and the first audio once
/// it records (a level, frames in the files), which passes the producer,
/// the processing and the writer threads at wall-clock pace; and the
/// build of a real handover listener (its identity read or minted and,
/// with a phone paired, its TLS setup and its socket). A loaded runner
/// stretches that work past [`PATIENCE`]. A gate that waits while
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

/// What "live" means in the held-store checks of
/// `.plans/2026-10-12-store-off-the-workers.md`: one call or one hop that
/// must not wait on the store. A fifth of the busy timeout that every
/// regression waits out, and loose enough for a loaded machine at
/// `nice 19`.
pub const LIVE: Duration = Duration::from_secs(1);

/// The coarse bound of a held-store check from the user's side, the
/// action to what the host shows: generous for a loaded machine (a load
/// of 40 to 70), and still well under the five-second freeze a wait on
/// the store would show.
pub const END_TO_END: Duration = Duration::from_secs(2);

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
    dir: tempfile::TempDir,
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
        dir,
    }
}

impl SyntheticRecorder {
    /// The database file the store is over.
    pub fn database(&self) -> PathBuf {
        database_in(self.dir.path())
    }

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
/// detection or the Swift import's marker. The database's lock waits up
/// to [`HANG_GUARD`]: once a test dropped its app, a child that another
/// test forked before the drop holds a copy of the lock's file until it
/// execs, so a relaunch that does not wait can find the lock still held.
/// A test of the refusal itself sets its own patience.
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
/// `make_session` that records into `root/audio`: what `App::shutdown`
/// drives, the host's recorder, and the launch. On the current runtime.
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

/// [`app_over_fakes`] with `engine` run in this process, as `CoreML` is on
/// the Mac (its load and transcriptions marked as reads, and run only
/// while no recording is under way), its models and the diarizer's
/// installed, and the warm-up mark of `root/support` read as a launch
/// reads it: a recording warms `engine` up, unless a mark left there turns
/// the warm-up off.
pub fn app_warming_up(
    root: &std::path::Path,
    store: &Arc<Store>,
    make_session: MakeCaptureSession,
    engine: Arc<dyn steno_core::SpeechEngine>,
) -> crate::App {
    let dependencies = fake_dependencies(store, "parakeet-v3")
        .with_speech_engine(steno_pipeline::SharedSpeechEngine::new(engine))
        .with_warm_up_mark(Arc::new(steno_pipeline::WarmUpMark::at_launch(
            &root.join("support"),
        )))
        .with_speech_in_process(true);
    let in_process = BuiltPipeline {
        dependencies,
        engine: BuiltEngine {
            engine_id: "parakeet-v3".to_owned(),
            runtime: SpeechRuntime::CoreMlInProcess,
        },
    };
    let fakes = steno_host::fakes::FakeServices::new(chrono::Utc::now());
    for asset in [ModelAsset::ParakeetV3, ModelAsset::OfflineDiarizer] {
        fakes.speech_models.set_installed(asset, None);
    }
    app_over_build(root, store, make_session, in_process, &fakes)
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

/// The app's launch over `root/support` with two speech engines: `coreml`
/// in this process, as `CoreML` is on the Mac, and `sidecar` in its
/// place, as the speech sidecar. The launch reads the warm-up mark and the
/// reading marks there, and the fallback over them
/// (`steno_pipeline::SpeechFallback`), as `build_with_import` does; every
/// build runs `coreml`, the Mac's default on every platform, unless the
/// fallback is on, and the model service is the app's over
/// `root/models`, every model installed. The fallback ends through the
/// host's model service (`SpeechModels::end_speech_fallback`) and a
/// reload.
pub fn app_falling_back(
    root: &std::path::Path,
    store: &Arc<Store>,
    coreml: Arc<dyn steno_core::SpeechEngine>,
    sidecar: Arc<dyn steno_core::SpeechEngine>,
) -> crate::App {
    let support = root.join("support");
    let warm_up_mark = Arc::new(steno_pipeline::WarmUpMark::at_launch(&support));
    let reading_marks = Arc::new(steno_pipeline::ReadingMarks::at_launch(
        &support,
        store,
        chrono::Utc::now(),
    ));
    let fallback = Arc::new(steno_pipeline::SpeechFallback::at_launch(
        &support,
        &warm_up_mark,
        &reading_marks,
    ));
    let setup = crate::speech::testing::setup(
        &root.join("models"),
        steno_speech::SpeechSettings::default(),
    )
    .with_fallback(fallback);
    let models = crate::speech::ModelStoreSpeechModels::new(&setup);
    crate::speech::testing::install_every_model(&models);
    let engines = Arc::new(crate::speech::SpeechEngines::with_builder(
        setup,
        Box::new(move |runtime| match runtime {
            SpeechRuntime::CoreMlInProcess => coreml.clone(),
            SpeechRuntime::OnnxSidecar => sidecar.clone(),
        }),
    ));
    let make: MakeDependencies = {
        let store = store.clone();
        Arc::new(move || {
            let runtime = crate::speech::unless_falling_back(
                SpeechRuntime::CoreMlInProcess,
                &engines.setup().fallback,
            );
            Ok(BuiltPipeline {
                dependencies: fake_dependencies(&store, "parakeet-v3")
                    .with_speech_engine(engines.engine(runtime))
                    .with_warm_up_mark(warm_up_mark.clone())
                    .with_reading_marks(reading_marks.clone())
                    .with_speech_in_process(runtime.runs_in_process()),
                engine: BuiltEngine {
                    engine_id: "parakeet-v3".to_owned(),
                    runtime,
                },
            })
        })
    };
    let pipeline = Arc::new(CurrentPipeline::new(
        make().unwrap(),
        make,
        tokio::runtime::Handle::current(),
    ));
    let fakes = steno_host::fakes::FakeServices::new(chrono::Utc::now());
    app_over_pipeline(
        root,
        store,
        synthetic_capture(),
        pipeline,
        Arc::new(models),
        &fakes,
    )
}

/// [`app_over_fakes`] over the build `first` (every reload builds it again)
/// and the host's `fakes`, its reads marked in `root/support`, which the
/// launch check reads first (`steno_pipeline::ReadingMarks`).
fn app_over_build(
    root: &std::path::Path,
    store: &Arc<Store>,
    make_session: MakeCaptureSession,
    mut first: BuiltPipeline,
    fakes: &steno_host::fakes::FakeServices,
) -> crate::App {
    first.dependencies =
        first
            .dependencies
            .with_reading_marks(Arc::new(steno_pipeline::ReadingMarks::at_launch(
                &root.join("support"),
                store,
                chrono::Utc::now(),
            )));
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

/// What `found` returns once it returns something, polled on this thread,
/// failing the test with `what` after `patience`.
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

/// What the checks of `.plans/2026-10-12-store-off-the-workers.md` run
/// under ("Acceptance"): another connection holds the database's write
/// lock, as another process does, for [`HeldStore::HOLD`]. With
/// [`HeldStore::with_a_write_waiting`] a write of this process waits out
/// its busy timeout behind it meanwhile, as a pipeline write does, and so
/// holds the store's writer. Dropped, it lets the lock go, and the waiting
/// write commits.
pub struct HeldStore {
    since: Instant,
    released: Arc<AtomicBool>,
    hold: Option<steno_core::testing::WriteLockHold>,
    waiting: Option<std::thread::JoinHandle<()>>,
}

impl HeldStore {
    /// How long a check holds the lock: past the five-second busy timeout,
    /// so a write that waits for it fails once and tries again.
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

    /// The lock held on the database at `path`, and a write through
    /// `store`, which is over it, waiting behind it, tried again whenever
    /// its busy timeout runs out, until the lock goes. Returns once that
    /// write is in its wait.
    pub fn with_a_write_waiting(store: &Arc<Store>, path: &Path) -> HeldStore {
        let store = store.clone();
        HeldStore::with_waiting(path, move || {
            let _ = store.write(|_| Ok(()));
        })
    }

    /// The lock held on the database at `path`, and `write`, which waits
    /// behind it, run on a thread of its own again and again (each time
    /// its busy timeout runs out) until the lock goes. Returns once the
    /// first is in its wait.
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

    /// Waits until [`Self::HOLD`] has passed since the lock was taken, then
    /// lets it go; returns when it did.
    pub fn release_after_hold(&mut self) -> Instant {
        std::thread::sleep((self.since + Self::HOLD).saturating_duration_since(Instant::now()));
        self.release()
    }

    /// Lets the lock go, and waits for the waiting write to commit; returns
    /// when the lock went.
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

/// Waits up to `within` of `released` until `meeting_id` was saved: out
/// of `recording` and not failed.
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
    /// The snapshots of `topic` that arrived at or after `since`, with
    /// when each did.
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

    /// When the first `recording` snapshot in `state` (`"stopping"`, say)
    /// that arrived at or after `since` did, waiting up to `patience` for
    /// it.
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

/// One `recorder_changed` the recorder's hook made: when it began, how
/// long it took, on which thread, and the recorder's state and whether its
/// auto-stop was armed once it returned.
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

/// A host over `store` and the fakes, whose recorder is `recorder`, with
/// the recorder's changes routed into it as `App::launch` routes them, and
/// a [`TimedSink`]. The page is ready, so every publish reaches the sink.
pub fn hosted(store: &Arc<Store>, recorder: &Arc<crate::recorder::CaptureRecorder>) -> Hosted {
    hosted_over(store, recorder, None)
}

/// [`hosted`], over `pipeline` in place of the fake's when there is one.
pub fn hosted_over(
    store: &Arc<Store>,
    recorder: &Arc<crate::recorder::CaptureRecorder>,
    pipeline: Option<Arc<dyn steno_host::services::Pipeline>>,
) -> Hosted {
    use steno_bridge::BridgeHost as _;
    let mut services = steno_host::fakes::FakeServices::new(chrono::Utc::now()).services();
    services.recorder = recorder.clone();
    if let Some(pipeline) = pipeline {
        services.pipeline = pipeline;
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
    /// A forced reload in flight, as the reload owner runs one, on a
    /// thread of its own.
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

/// A pipeline whose `export_keeps_failing`, which the detail's reload
/// asks, waits at a gate while the gate is armed: a reload held there
/// stands for one that waits on the store behind another process's
/// write lock. Set to a store handle ([`Self::gate_inside_reads_on`]), it
/// waits inside a read on that handle's reader, so the reader stays taken
/// while it waits. Everything else is the host's fake.
#[derive(Default)]
pub struct GatedPipeline {
    fake: steno_host::fakes::FakePipeline,
    pub gate: ReloadGate,
    reader: std::sync::OnceLock<Arc<Store>>,
}

impl GatedPipeline {
    /// From now on the gate is passed inside a read on `store`'s reader;
    /// the first call wins.
    pub fn gate_inside_reads_on(&self, store: Arc<Store>) {
        let _ = self.reader.set(store);
    }
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

    /// Arms the gate; the next reload waits there until the guard
    /// drops, also when the test fails.
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

impl steno_host::services::Pipeline for GatedPipeline {
    fn rerun_summary(
        &self,
        meeting_id: uuid::Uuid,
        template_id: &str,
    ) -> steno_core::protocols::BoundaryResult<()> {
        self.fake.rerun_summary(meeting_id, template_id)
    }

    fn redeliver(&self, meeting_id: uuid::Uuid) -> steno_core::protocols::BoundaryResult<()> {
        self.fake.redeliver(meeting_id)
    }

    fn export_keeps_failing(&self, meeting_id: uuid::Uuid) -> bool {
        match self.reader.get() {
            Some(store) => store
                .read(|_| {
                    self.gate.pass();
                    Ok(())
                })
                .unwrap(),
            None => self.gate.pass(),
        }
        self.fake.export_keeps_failing(meeting_id)
    }

    fn process_again(
        &self,
        meeting_id: uuid::Uuid,
    ) -> Result<(), steno_host::services::ProcessAgainRefusal> {
        self.fake.process_again(meeting_id)
    }

    fn damaged_audio(&self, meeting_id: uuid::Uuid) -> steno_core::AudioDamage {
        self.fake.damaged_audio(meeting_id)
    }

    fn audio_may_be_damaged(&self, meeting_id: uuid::Uuid) -> bool {
        self.fake.audio_may_be_damaged(meeting_id)
    }

    fn apply_retention(
        &self,
        meeting_id: uuid::Uuid,
        rule: steno_core::AudioRetention,
    ) -> steno_core::protocols::BoundaryResult<()> {
        self.fake.apply_retention(meeting_id, rule)
    }

    fn reload(&self) -> steno_core::protocols::BoundaryResult<()> {
        self.fake.reload()
    }

    fn keep_all_recordings(&self) -> steno_core::protocols::BoundaryResult<i64> {
        self.fake.keep_all_recordings()
    }
}
