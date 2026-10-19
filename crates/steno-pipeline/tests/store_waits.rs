//! A run that meets the store locked by another process past its busy
//! timeout: the meeting waits `queued` for the store, never `failed`, and
//! the store's retry processes it once the lock goes. Every test runs on one
//! worker, since store calls wait on the blocking pool and must leave the
//! worker free.

mod common;

use std::convert::identity;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use steno_core::paths::file_url_path;
use steno_core::protocols::{BoundaryResult, BoxError};
use steno_core::testing::{
    FakeSpeechEngine, InMemorySpeakerMemory, PassthroughCleaner, WriteLockHold, sample_data,
};
use steno_core::{
    AudioBuffer16k, AudioRetention, CleanupInput, CleanupOutput, Embedding, LanguageTag,
    MeetingEvent, MeetingState, RawSegment, SpeakerMatch, SpeakerMemory, SpeechEngine, Store,
    StoreError, TranscriptCleaner, async_trait,
};
use steno_pipeline::{EventReceiver, PipelineDependencies, ProcessingPipeline, ReprocessError};
use uuid::Uuid;

/// How long a test waits for a gate, or for a run that waits out the
/// store's five-second busy timeout.
const PATIENCE: Duration = Duration::from_secs(30);

/// A gate a fake waits at once: `entered` when reached, then it waits for
/// `open`.
#[derive(Default)]
struct Gate {
    used: AtomicBool,
    entered: tokio::sync::Notify,
    open: tokio::sync::Notify,
}

impl Gate {
    async fn pass(&self) {
        if !self.used.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.open.notified().await;
        }
    }

    async fn reached(&self) {
        tokio::time::timeout(PATIENCE, self.entered.notified())
            .await
            .expect("the gate was reached");
    }
}

/// The fake engine, its first warm-up held at `gate`, right before the
/// run's first write.
#[derive(Default)]
struct HeldWarmUp {
    gate: Gate,
    inner: FakeSpeechEngine,
}

#[async_trait]
impl SpeechEngine for HeldWarmUp {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn supported_languages(&self) -> &std::collections::BTreeSet<LanguageTag> {
        self.inner.supported_languages()
    }

    async fn prepare(&self) -> BoundaryResult<()> {
        self.gate.pass().await;
        self.inner.prepare().await
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        self.inner.transcribe(audio, hint).await
    }
}

/// The passthrough cleaner, its first cleanup held at `gate`, after the
/// merge committed.
#[derive(Default)]
struct HeldCleaner {
    gate: Gate,
    inner: PassthroughCleaner,
}

#[async_trait]
impl TranscriptCleaner for HeldCleaner {
    async fn clean(&self, input: &CleanupInput) -> BoundaryResult<CleanupOutput> {
        self.gate.pass().await;
        self.inner.clean(input).await
    }
}

fn sqlite_error(code: std::ffi::c_int, message: &str) -> StoreError {
    StoreError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(code),
        Some(message.to_owned()),
    ))
}

fn full() -> StoreError {
    sqlite_error(rusqlite::ffi::SQLITE_FULL, "database or disk is full")
}

/// A speaker memory over the store whose first match meets `error`.
struct FailsOnce {
    error: fn() -> StoreError,
    failed: AtomicBool,
    inner: InMemorySpeakerMemory,
}

#[async_trait]
impl SpeakerMemory for FailsOnce {
    async fn candidates(
        &self,
        embedding: &Embedding,
        limit: usize,
    ) -> BoundaryResult<Vec<SpeakerMatch>> {
        if !self.failed.swap(true, Ordering::SeqCst) {
            let boxed: BoxError = Box::new((self.error)());
            return Err(boxed);
        }
        self.inner.candidates(embedding, limit).await
    }
}

fn memory_failing_once(
    error: fn() -> StoreError,
) -> impl FnOnce(PipelineDependencies) -> PipelineDependencies {
    move |mut dependencies| {
        dependencies.speaker_memory = Arc::new(FailsOnce {
            error,
            failed: AtomicBool::new(false),
            inner: InMemorySpeakerMemory::default(),
        });
        dependencies
    }
}

/// A store with an audio folder in `dir`, and a pipeline over it with
/// `engine`.
struct World {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    audio: std::path::PathBuf,
    store: Arc<Store>,
    pipeline: ProcessingPipeline,
}

fn world(
    engine: Arc<dyn SpeechEngine>,
    dependencies: impl FnOnce(PipelineDependencies) -> PipelineDependencies,
) -> World {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steno.sqlite");
    let (store, audio) = common::store_with_audio(&path);
    let pipeline = ProcessingPipeline::new(dependencies(common::dependencies(&store, engine)));
    World {
        _dir: dir,
        path,
        audio,
        store,
        pipeline,
    }
}

impl World {
    /// Enqueues a two-lane call starting `minutes` after the sample start.
    fn enqueue_call(&self, minutes: i64) -> Uuid {
        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        meeting.state = MeetingState::Recording;
        meeting.started_at += chrono::Duration::minutes(minutes);
        let asset = steno_pipeline::fixtures::two_lane_call(
            &self.audio,
            meeting.id,
            AudioRetention::KeepForever,
        )
        .unwrap();
        self.pipeline.enqueue(&meeting, &asset).unwrap();
        meeting.id
    }

    /// Saves what the last process left `queued`: a meeting without its
    /// asset row, and a two-lane call a minute later; a launch's resume
    /// fails the first at a write before it starts the other.
    fn left_queued(&self) -> (Uuid, Uuid) {
        let mut gone = sample_data::meeting();
        gone.id = Uuid::new_v4();
        gone.state = MeetingState::Queued;
        self.store.save_meeting(&gone).unwrap();
        let mut queued = sample_data::meeting();
        queued.id = Uuid::new_v4();
        queued.state = MeetingState::Queued;
        queued.started_at += chrono::Duration::minutes(1);
        let asset = steno_pipeline::fixtures::two_lane_call(
            &self.audio,
            queued.id,
            AudioRetention::KeepForever,
        )
        .unwrap();
        self.store.save_meeting_with_asset(&queued, &asset).unwrap();
        (gone.id, queued.id)
    }

    /// A second connection that holds the write lock until dropped.
    fn lock(&self) -> WriteLockHold {
        WriteLockHold::new(&self.path)
    }

    /// Takes the lock once `gate` is reached, then opens the gate.
    async fn lock_at(&self, gate: &Gate) -> WriteLockHold {
        gate.reached().await;
        let lock = self.lock();
        gate.open.notify_one();
        lock
    }

    /// The pipeline's connection gives up on a lock at once instead of after
    /// five seconds.
    fn without_busy_timeout(&self) {
        self.store
            .on_writer(|connection| connection.busy_timeout(Duration::ZERO))
            .unwrap();
    }

    fn state(&self, meeting_id: Uuid) -> MeetingState {
        self.store.meeting(meeting_id).unwrap().unwrap().state
    }

    async fn until_parked(&self, meeting_id: Uuid) {
        until_parked(&self.pipeline, meeting_id).await;
    }

    /// The next launch: a fresh pipeline over the same store.
    fn launch(&self) -> ProcessingPipeline {
        ProcessingPipeline::new(common::dependencies(
            &self.store,
            Arc::new(FakeSpeechEngine::default()),
        ))
    }
}

async fn until_waiting_for_store(pipeline: &ProcessingPipeline, meeting_id: Uuid) {
    let waits = pipeline.dependencies().model_waits.clone();
    tokio::time::timeout(PATIENCE, async {
        while !waits.waiting_for_store().contains(&meeting_id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the meeting waits for the store");
}

/// Waits until `meeting_id` waits for the store on `pipeline` and every run
/// there ended.
async fn until_parked(pipeline: &ProcessingPipeline, meeting_id: Uuid) {
    until_waiting_for_store(pipeline, meeting_id).await;
    pipeline.wait_until_idle().await;
}

/// Whether the store's retry is woken within a moment (a wake-up is kept
/// until awaited).
async fn store_retry_woken(pipeline: &ProcessingPipeline) -> bool {
    let waits = pipeline.dependencies().model_waits.clone();
    tokio::time::timeout(Duration::from_millis(200), waits.until_store_retry())
        .await
        .is_ok()
}

fn drain(receiver: &mut EventReceiver) -> Vec<MeetingEvent> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}

/// A first write that meets the lock leaves the meeting `queued` waiting
/// for the store, and the store's retry processes it to `ready`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_run_the_store_keeps_locked_past_its_busy_timeout_waits_queued_and_then_runs() {
    let engine = Arc::new(HeldWarmUp::default());
    let world = world(engine.clone(), identity);
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    let mut events = pipeline.dependencies().events.subscribe();
    let meeting = world.enqueue_call(0);
    let lock = world.lock_at(&engine.gate).await;
    let held_at = std::time::Instant::now();

    until_waiting_for_store(pipeline, meeting).await;
    let waited = held_at.elapsed();
    assert!(
        waited >= Duration::from_secs(5),
        "the write waited out the busy timeout"
    );
    // The park writes nothing over a meeting still `queued`.
    assert!(
        waited < Duration::from_secs(8),
        "the park waited on the lock again: {waited:?}"
    );
    let row = world.store.meeting(meeting).unwrap().unwrap();
    assert_eq!(row.state, MeetingState::Queued);
    assert_eq!(row.state.failure_reason(), None);
    let posted = drain(&mut events);
    assert!(
        posted.contains(&MeetingEvent::StoreBusy {
            meeting_id: meeting
        }),
        "{posted:?}"
    );
    assert!(
        !posted.contains(&MeetingEvent::ModelsMissing {
            meeting_id: meeting
        }),
        "{posted:?}"
    );
    assert_eq!(waits.waiting(), Vec::<Uuid>::new(), "no models are missing");
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
    assert_eq!(
        pipeline.resume_waiting().unwrap(),
        Vec::<Uuid>::new(),
        "an install's resume leaves a meeting waiting for the store"
    );
    assert_eq!(
        pipeline.process_again(meeting),
        Err(ReprocessError::WaitingForStore(meeting))
    );
    assert_eq!(waits.waiting_for_store(), [meeting]);

    drop(lock);
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [meeting]);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
    assert_eq!(waits.waiting_for_store(), Vec::<Uuid>::new());
}

/// A lock after the merge leaves the meeting `processing` with its
/// transcript and audio, and the retry processes it to `ready`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_run_the_store_stops_after_the_merge_keeps_its_transcript_and_its_audio() {
    let cleaner = Arc::new(HeldCleaner::default());
    let world = world(Arc::new(FakeSpeechEngine::default()), |dependencies| {
        dependencies.with_llm(Some(cleaner.clone()), None)
    });
    let pipeline = &world.pipeline;
    world.without_busy_timeout();
    let meeting = world.enqueue_call(0);
    cleaner.gate.reached().await;
    let segments = world.store.segments(meeting).unwrap();
    assert!(!segments.is_empty(), "the merge committed the transcript");
    let asset = world.store.asset(meeting).unwrap().unwrap();
    let lock = world.lock();
    cleaner.gate.open.notify_one();

    world.until_parked(meeting).await;
    assert_eq!(
        world.state(meeting),
        MeetingState::Processing,
        "the park's write met the lock too"
    );
    assert_eq!(world.store.segments(meeting).unwrap(), segments);
    assert_eq!(world.store.asset(meeting).unwrap().unwrap(), asset);
    assert!(file_url_path(&asset.url).unwrap().exists());
    for lane in asset.sidecars_16k.values() {
        assert!(file_url_path(lane).unwrap().exists());
    }

    drop(lock);
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [meeting]);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
}

/// The launch's resume the store refuses stays owed to the store's retry,
/// which runs it once the lock goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_launch_resume_the_store_refuses_runs_again_on_the_store_retry() {
    let world = world(Arc::new(FakeSpeechEngine::default()), identity);
    let pipeline = &world.pipeline;
    // The resume's write that fails `gone` meets the lock.
    let (gone, queued) = world.left_queued();
    world.without_busy_timeout();
    let lock = world.lock();

    let refused = pipeline.resume_unfinished().unwrap_err();
    assert!(refused.is_busy(), "{refused}");
    assert!(
        store_retry_woken(pipeline).await,
        "the store's retry was woken"
    );
    assert_eq!(world.state(queued), MeetingState::Queued);
    // A retry while the lock is still held is refused too.
    assert!(pipeline.resume_waiting_for_store().unwrap_err().is_busy());
    assert_eq!(world.state(queued), MeetingState::Queued);

    drop(lock);
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [queued]);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(queued), MeetingState::Ready);
    assert!(matches!(world.state(gone), MeetingState::Failed { .. }));
}

/// A store error that is not the lock costs the match only, and the meeting
/// runs to `ready`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_store_error_that_is_not_the_lock_does_not_wait_for_the_store() {
    assert!(!full().is_busy());
    let world = world(
        Arc::new(FakeSpeechEngine::default()),
        memory_failing_once(full),
    );
    let pipeline = &world.pipeline;
    let meeting = world.enqueue_call(0);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
    assert_eq!(
        pipeline.dependencies().model_waits.waiting_for_store(),
        Vec::<Uuid>::new()
    );
}

/// A meeting left `processing` by a park whose `queued` write met the lock:
/// the world, the meeting, the held lock and the transcript's segment count.
async fn left_processing_by_the_lock() -> (World, Uuid, WriteLockHold, usize) {
    let cleaner = Arc::new(HeldCleaner::default());
    let world = world(Arc::new(FakeSpeechEngine::default()), |dependencies| {
        dependencies.with_llm(Some(cleaner.clone()), None)
    });
    world.without_busy_timeout();
    let meeting = world.enqueue_call(0);
    cleaner.gate.reached().await;
    let segments = world.store.segments(meeting).unwrap().len();
    let lock = world.lock();
    cleaner.gate.open.notify_one();
    world.until_parked(meeting).await;
    assert_eq!(world.state(meeting), MeetingState::Processing);
    (world, meeting, lock, segments)
}

/// The lock goes, then a quit saves the meeting `queued` again, and the
/// next launch resumes it to `ready` without doubling its transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_meeting_the_lock_left_processing_runs_at_the_next_launch() {
    let (world, meeting, lock, segments) = left_processing_by_the_lock().await;
    drop(lock);
    world.pipeline.quit();
    assert_eq!(world.state(meeting), MeetingState::Queued);
    let launch = world.launch();
    assert_eq!(launch.resume_unfinished().unwrap(), [meeting]);
    launch.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
    assert_eq!(world.store.segments(meeting).unwrap().len(), segments);
}

/// After a quit a retry starts nothing, and the next launch processes the
/// meeting.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_quit_while_a_meeting_waits_for_the_store_leaves_it_for_the_next_launch() {
    let engine = Arc::new(HeldWarmUp::default());
    let world = world(engine.clone(), identity);
    world.without_busy_timeout();
    let meeting = world.enqueue_call(0);
    let lock = world.lock_at(&engine.gate).await;
    world.until_parked(meeting).await;
    assert_eq!(world.state(meeting), MeetingState::Queued);

    world.pipeline.quit();
    drop(lock);
    assert_eq!(
        world.pipeline.resume_waiting_for_store().unwrap(),
        Vec::<Uuid>::new(),
        "no retry after the quit"
    );
    world.pipeline.wait_until_idle().await;
    let row = world.store.meeting(meeting).unwrap().unwrap();
    assert_eq!(row.state, MeetingState::Queued);
    assert_eq!(row.state.failure_reason(), None);

    let launch = world.launch();
    assert_eq!(launch.resume_unfinished().unwrap(), [meeting]);
    launch.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
}
