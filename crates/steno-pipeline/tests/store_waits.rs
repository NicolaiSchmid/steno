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

use common::Uninstalled;
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

/// The store's busy error, as for a lock held past the busy timeout.
fn busy() -> StoreError {
    sqlite_error(rusqlite::ffi::SQLITE_BUSY, "database is locked")
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

fn sorted(ids: impl IntoIterator<Item = Uuid>) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = ids.into_iter().collect();
    ids.sort();
    ids
}

/// Two meetings waiting for their models, the older one's asset row gone,
/// then the models installed and the lock taken: an install's resume meets
/// the lock before it starts the other.
async fn waiting_for_models_under_the_lock() -> (World, Uuid, Uuid, WriteLockHold) {
    let engine = Arc::new(Uninstalled::default());
    let world = world(engine.clone(), identity);
    let gone = world.enqueue_call(0);
    let kept = world.enqueue_call(1);
    world.pipeline.wait_until_idle().await;
    assert_eq!(
        world.pipeline.dependencies().model_waits.waiting(),
        sorted([gone, kept])
    );
    world
        .store
        .write(|transaction| {
            transaction.execute(
                "DELETE FROM audioAsset WHERE meetingID = ?1",
                [steno_core::json::uuid_string(gone)],
            )?;
            Ok(())
        })
        .unwrap();
    engine.installed.store(true, Ordering::SeqCst);
    world.without_busy_timeout();
    let lock = world.lock();
    (world, gone, kept, lock)
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

/// A speaker match the store refuses makes the run wait for the store, and
/// the retry matches the speakers.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_speaker_match_the_store_refuses_waits_instead_of_dropping_the_match() {
    let world = world(
        Arc::new(FakeSpeechEngine::default()),
        memory_failing_once(busy),
    );
    let pipeline = &world.pipeline;
    let meeting = world.enqueue_call(0);
    world.until_parked(meeting).await;
    assert_eq!(world.state(meeting), MeetingState::Queued);
    assert!(store_retry_woken(pipeline).await, "the park woke the retry");

    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [meeting]);
    assert!(
        !store_retry_woken(pipeline).await,
        "a retry that started every meeting does not wake the next"
    );
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
}

/// A meeting waiting for its models is not taken by the store's retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_store_retry_leaves_a_meeting_waiting_for_models_alone() {
    let engine = Arc::new(Uninstalled::default());
    let world = world(engine.clone(), identity);
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    let meeting = world.enqueue_call(0);
    pipeline.wait_until_idle().await;
    assert_eq!(waits.waiting(), [meeting]);
    let mut events = pipeline.dependencies().events.subscribe();

    assert_eq!(
        pipeline.resume_waiting_for_store().unwrap(),
        Vec::<Uuid>::new()
    );
    pipeline.wait_until_idle().await;
    assert_eq!(drain(&mut events), Vec::<MeetingEvent>::new());
    assert_eq!(waits.waiting(), [meeting]);
    assert_eq!(waits.waiting_for_store(), Vec::<Uuid>::new());
    assert_eq!(
        pipeline.process_again(meeting),
        Err(ReprocessError::WaitingForModels(meeting))
    );
}

/// An install's resume the store refuses leaves its meetings waiting for
/// the store, and the retry starts them once the lock goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_resume_the_store_refuses_waits_for_the_store_retry() {
    let (world, gone, kept, lock) = waiting_for_models_under_the_lock().await;
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    let mut events = pipeline.dependencies().events.subscribe();

    let refused = pipeline.resume_waiting().unwrap_err();
    assert!(refused.is_busy(), "{refused}");
    assert_eq!(waits.waiting(), Vec::<Uuid>::new());
    assert_eq!(waits.waiting_for_store(), sorted([gone, kept]));
    assert!(
        store_retry_woken(pipeline).await,
        "the store's retry was woken"
    );
    let posted = drain(&mut events);
    for meeting_id in [gone, kept] {
        assert!(
            posted.contains(&MeetingEvent::StoreBusy { meeting_id }),
            "{posted:?}"
        );
    }
    assert_eq!(world.state(kept), MeetingState::Queued);
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());

    drop(lock);
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [kept]);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(kept), MeetingState::Ready);
    assert!(matches!(world.state(gone), MeetingState::Failed { .. }));
    assert_eq!(waits.waiting_for_store(), Vec::<Uuid>::new());
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

/// A retry the lock refuses again keeps the meetings waiting and wakes the
/// next retry; one that fails otherwise leaves them `queued` for the next
/// launch.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_retry_the_lock_refuses_keeps_its_meetings_and_another_failure_leaves_them_queued() {
    let (world, gone, kept, lock) = waiting_for_models_under_the_lock().await;
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    assert!(pipeline.resume_waiting().unwrap_err().is_busy());
    let both = sorted([gone, kept]);
    assert_eq!(waits.waiting_for_store(), both);
    assert!(store_retry_woken(pipeline).await);

    assert!(pipeline.resume_waiting_for_store().unwrap_err().is_busy());
    assert_eq!(
        waits.waiting_for_store(),
        both,
        "the refused retry kept them"
    );
    assert_eq!(
        waits.waiting(),
        Vec::<Uuid>::new(),
        "and not for models too"
    );
    assert!(
        store_retry_woken(pipeline).await,
        "the refused retry woke the next one"
    );

    drop(lock);
    world
        .store
        .on_writer(|connection| connection.pragma_update(None, "query_only", true))
        .unwrap();
    let refused = pipeline.resume_waiting_for_store().unwrap_err();
    assert!(!refused.is_busy(), "{refused}");
    assert_eq!(waits.waiting_for_store(), Vec::<Uuid>::new());
    assert_eq!(waits.waiting(), Vec::<Uuid>::new());
    assert!(!store_retry_woken(pipeline).await);
    for meeting in [gone, kept] {
        assert_eq!(world.state(meeting), MeetingState::Queued);
    }

    world
        .store
        .on_writer(|connection| connection.pragma_update(None, "query_only", false))
        .unwrap();
    pipeline.quit();
    let launch = world.launch();
    assert_eq!(launch.resume_unfinished().unwrap(), [kept]);
    launch.wait_until_idle().await;
    assert_eq!(world.state(kept), MeetingState::Ready);
    assert!(matches!(world.state(gone), MeetingState::Failed { .. }));
}

/// A retry of a launch's resume puts back only the held meeting that waited
/// for the store; others stay `queued` for the next launch.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_owed_launch_resume_puts_back_the_held_meeting_that_waits_for_the_store() {
    let world = world(
        Arc::new(FakeSpeechEngine::default()),
        memory_failing_once(busy),
    );
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    let parked = world.enqueue_call(2);
    world.until_parked(parked).await;
    // Older than `parked`, so the launch's resume meets the lock at the
    // write that fails `gone`.
    let (gone, other) = world.left_queued();
    world.without_busy_timeout();
    let lock = world.lock();
    assert!(pipeline.resume_unfinished().unwrap_err().is_busy());
    drop(lock);

    let held = [
        pipeline.claim_redeliver(parked).unwrap(),
        pipeline.claim_redeliver(other).unwrap(),
    ];
    assert_eq!(
        pipeline.resume_waiting_for_store().unwrap(),
        Vec::<Uuid>::new()
    );
    assert!(matches!(world.state(gone), MeetingState::Failed { .. }));
    assert_eq!(waits.waiting_for_store(), [parked]);
    assert_eq!(
        waits.waiting(),
        Vec::<Uuid>::new(),
        "neither waits for models"
    );
    drop(held);

    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [parked]);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(parked), MeetingState::Ready);
    assert_eq!(world.state(other), MeetingState::Queued);
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

/// A quit, then the lock goes, and the next launch resumes the meeting to
/// `ready` without doubling its transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_meeting_the_lock_left_processing_runs_at_the_next_launch() {
    let (world, meeting, lock, segments) = left_processing_by_the_lock().await;
    world.pipeline.quit();
    drop(lock);
    let launch = world.launch();
    assert_eq!(launch.resume_unfinished().unwrap(), [meeting]);
    launch.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
    assert_eq!(world.store.segments(meeting).unwrap().len(), segments);
}

/// A next launch while the lock is still held waits for the store again and
/// its retry processes the meeting.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_meeting_the_lock_left_processing_waits_again_at_a_launch_under_the_lock() {
    let (world, meeting, lock, segments) = left_processing_by_the_lock().await;
    world.pipeline.quit();
    let launch = world.launch();
    assert_eq!(launch.resume_unfinished().unwrap(), [meeting]);
    until_parked(&launch, meeting).await;
    let state = world.state(meeting);
    assert!(
        matches!(state, MeetingState::Processing | MeetingState::Queued),
        "{state:?}"
    );
    drop(lock);
    assert_eq!(launch.resume_waiting_for_store().unwrap(), [meeting]);
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

/// A quit during the busy timeout neither fails nor records the meeting as
/// waiting; the next launch processes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_quit_while_a_run_waits_on_the_lock_leaves_the_meeting_for_the_next_launch() {
    let engine = Arc::new(HeldWarmUp::default());
    let world = world(engine.clone(), identity);
    let meeting = world.enqueue_call(0);
    let lock = world.lock_at(&engine.gate).await;
    let held_at = std::time::Instant::now();
    tokio::time::sleep(Duration::from_secs(1)).await;
    world.pipeline.quit();
    world.pipeline.wait_until_idle().await;
    assert!(held_at.elapsed() >= Duration::from_secs(5));
    let row = world.store.meeting(meeting).unwrap().unwrap();
    assert!(
        matches!(row.state, MeetingState::Queued | MeetingState::Processing),
        "{:?}",
        row.state
    );
    assert_eq!(row.state.failure_reason(), None);
    assert_eq!(
        world
            .pipeline
            .dependencies()
            .model_waits
            .waiting_for_store(),
        Vec::<Uuid>::new()
    );
    drop(lock);

    let launch = world.launch();
    assert_eq!(launch.resume_unfinished().unwrap(), [meeting]);
    launch.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
}

/// A models install while the lock is held takes only the meeting waiting
/// for models; the store's retry later processes both.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_models_install_under_the_lock_leaves_both_meetings_waiting_for_the_store() {
    let engine = Arc::new(Uninstalled::default());
    engine.installed.store(true, Ordering::SeqCst);
    let world = world(engine.clone(), memory_failing_once(busy));
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    let parked = world.enqueue_call(0);
    world.until_parked(parked).await;
    engine.installed.store(false, Ordering::SeqCst);
    let models = world.enqueue_call(1);
    pipeline.wait_until_idle().await;
    assert_eq!(waits.waiting(), [models]);
    assert_eq!(waits.waiting_for_store(), [parked]);

    world.without_busy_timeout();
    let lock = world.lock();
    engine.installed.store(true, Ordering::SeqCst);
    let resumed = pipeline.resume_waiting();
    world.until_parked(models).await;
    assert!(
        matches!(&resumed, Ok(started) if started == &[models])
            || matches!(&resumed, Err(failure) if failure.is_busy()),
        "{resumed:?}"
    );
    assert_eq!(waits.waiting(), Vec::<Uuid>::new());
    let both = sorted([parked, models]);
    assert_eq!(waits.waiting_for_store(), both);
    for meeting in both.iter().copied() {
        assert_eq!(world.state(meeting).failure_reason(), None);
    }

    drop(lock);
    assert_eq!(sorted(pipeline.resume_waiting_for_store().unwrap()), both);
    pipeline.wait_until_idle().await;
    for meeting in both {
        assert_eq!(world.state(meeting), MeetingState::Ready);
    }
}

/// A meeting waiting for the store does not keep an update's install out;
/// after the install's quit the next launch processes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_update_hold_during_a_store_wait_runs_nothing_and_the_relaunch_processes_it() {
    let world = world(
        Arc::new(FakeSpeechEngine::default()),
        memory_failing_once(busy),
    );
    let pipeline = &world.pipeline;
    let meeting = world.enqueue_call(0);
    world.until_parked(meeting).await;
    assert_eq!(world.state(meeting), MeetingState::Queued);

    let hold = pipeline
        .dependencies()
        .in_flight
        .try_hold()
        .expect("a meeting waiting for the store does not keep an install out");
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [meeting]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        world.state(meeting),
        MeetingState::Queued,
        "nothing ran under the hold"
    );
    pipeline.quit();
    drop(hold);
    pipeline.wait_until_idle().await;
    let row = world.store.meeting(meeting).unwrap().unwrap();
    assert_eq!(row.state, MeetingState::Queued);
    assert_eq!(row.state.failure_reason(), None);

    let launch = world.launch();
    assert_eq!(launch.resume_unfinished().unwrap(), [meeting]);
    launch.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
}

/// The same install given up: the run the retry started goes on to
/// `ready`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_retry_under_an_update_hold_runs_once_the_hold_is_given_up() {
    let world = world(
        Arc::new(FakeSpeechEngine::default()),
        memory_failing_once(busy),
    );
    let pipeline = &world.pipeline;
    let meeting = world.enqueue_call(0);
    world.until_parked(meeting).await;
    let hold = pipeline.dependencies().in_flight.try_hold().expect("idle");
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [meeting]);
    drop(hold);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
}

/// Two meetings left `queued` whose launch resume the lock refused, then a
/// newer meeting waiting for its models; the launch's resume is owed.
async fn an_owed_launch_resume_and_a_meeting_waiting_for_models()
-> (World, Arc<Uninstalled>, Uuid, Uuid, Uuid) {
    let engine = Arc::new(Uninstalled::default());
    let world = world(engine.clone(), identity);
    let pipeline = &world.pipeline;
    let (gone, queued) = world.left_queued();
    world.without_busy_timeout();
    let lock = world.lock();
    assert!(pipeline.resume_unfinished().unwrap_err().is_busy());
    assert!(store_retry_woken(pipeline).await);
    drop(lock);
    let models = world.enqueue_call(5);
    pipeline.wait_until_idle().await;
    assert_eq!(pipeline.dependencies().model_waits.waiting(), [models]);
    (world, engine, gone, queued, models)
}

/// The retry of a launch's resume runs the meeting waiting for models once
/// they are in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_owed_launch_resume_takes_the_meetings_waiting_for_models() {
    let (world, engine, gone, queued, models) =
        an_owed_launch_resume_and_a_meeting_waiting_for_models().await;
    let pipeline = &world.pipeline;
    engine.installed.store(true, Ordering::SeqCst);

    assert_eq!(
        sorted(pipeline.resume_waiting_for_store().unwrap()),
        sorted([queued, models])
    );
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(queued), MeetingState::Ready);
    assert_eq!(world.state(models), MeetingState::Ready);
    assert!(matches!(world.state(gone), MeetingState::Failed { .. }));
    assert_eq!(
        pipeline.dependencies().model_waits.waiting(),
        Vec::<Uuid>::new()
    );
}

/// The retry of a launch's resume puts a held meeting waiting for models
/// back to wait for them, so the models install starts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_owed_launch_resume_keeps_a_held_meeting_waiting_for_its_models() {
    let (world, engine, gone, queued, models) =
        an_owed_launch_resume_and_a_meeting_waiting_for_models().await;
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();

    let held = pipeline.claim_redeliver(models).unwrap();
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [queued]);
    pipeline.wait_until_idle().await;
    assert!(matches!(world.state(gone), MeetingState::Failed { .. }));
    // `queued` met the missing models too.
    assert_eq!(waits.waiting(), sorted([queued, models]));
    assert_eq!(waits.waiting_for_store(), Vec::<Uuid>::new());
    drop(held);

    engine.installed.store(true, Ordering::SeqCst);
    assert_eq!(
        sorted(pipeline.resume_waiting().unwrap()),
        sorted([queued, models])
    );
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(models), MeetingState::Ready);
    assert_eq!(world.state(queued), MeetingState::Ready);
}

/// A retry of a launch's resume that meets the lock again stays owed, keeps
/// the models wait, and the next retry runs every queued meeting.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_owed_launch_resume_the_lock_refuses_again_keeps_the_meeting_waiting_for_models() {
    let (world, engine, gone, queued, models) =
        an_owed_launch_resume_and_a_meeting_waiting_for_models().await;
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();

    let lock = world.lock();
    assert!(pipeline.resume_waiting_for_store().unwrap_err().is_busy());
    assert!(store_retry_woken(pipeline).await, "owed again");
    assert_eq!(waits.waiting(), [models]);
    drop(lock);

    engine.installed.store(true, Ordering::SeqCst);
    assert_eq!(
        sorted(pipeline.resume_waiting_for_store().unwrap()),
        sorted([queued, models])
    );
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(queued), MeetingState::Ready);
    assert_eq!(world.state(models), MeetingState::Ready);
    assert!(matches!(world.state(gone), MeetingState::Failed { .. }));
    assert_eq!(waits.waiting(), Vec::<Uuid>::new());
}

/// A retry of a launch's resume that fails otherwise is not owed again; the
/// meeting waiting for models still waits for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_owed_launch_resume_another_failure_ends_keeps_the_meetings_waiting_for_models() {
    let (world, engine, gone, queued, models) =
        an_owed_launch_resume_and_a_meeting_waiting_for_models().await;
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();

    world
        .store
        .on_writer(|connection| connection.pragma_update(None, "query_only", true))
        .unwrap();
    let refused = pipeline.resume_waiting_for_store().unwrap_err();
    assert!(!refused.is_busy(), "{refused}");
    assert!(!store_retry_woken(pipeline).await, "not woken again");
    assert_eq!(waits.waiting(), [models]);
    world
        .store
        .on_writer(|connection| connection.pragma_update(None, "query_only", false))
        .unwrap();
    assert_eq!(
        pipeline.resume_waiting_for_store().unwrap(),
        Vec::<Uuid>::new(),
        "the failed owed resume is not owed again"
    );
    for meeting in [gone, queued] {
        assert_eq!(world.state(meeting), MeetingState::Queued);
    }

    engine.installed.store(true, Ordering::SeqCst);
    assert_eq!(pipeline.resume_waiting().unwrap(), [models]);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(models), MeetingState::Ready);
}

/// A phone admission that meets the lock waits on the blocking pool, so a
/// timer on the one worker keeps ticking (stalls of at most 600 ms), and
/// the admission commits once the lock goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_admit_under_a_held_lock_leaves_the_worker_free() {
    use steno_core::{AudioFormat, HandoverIntake as _, PairedDevice, RecordingMetadata};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steno.sqlite");
    let store = Arc::new(Store::open(&path).unwrap());
    let mut settings = store.settings().unwrap();
    settings.audio_folder = steno_core::paths::file_url(&dir.path().join("audio"), true);
    store.save_settings(&settings).unwrap();
    let now = chrono::Utc::now();
    let device = PairedDevice {
        id: Uuid::new_v4(),
        name: "Phone".to_owned(),
        paired_at: now,
        last_seen_at: None,
    };
    store.save_paired_device(&device, &[1; 32]).unwrap();
    let metadata = RecordingMetadata {
        recording_id: Uuid::new_v4(),
        started_at: now,
        duration_seconds: 12.0,
        byte_count: 9,
        sha256: vec![0; 32],
        chunk_size: 9,
        format: AudioFormat::M4aAac,
        device_name: "Phone".to_owned(),
    };
    let upload = dir.path().join("upload.m4a");
    std::fs::write(&upload, b"aac bytes").unwrap();
    let intake = steno_pipeline::RecordingIntake::new(
        store.clone(),
        Arc::new(|_, _| Box::pin(async { Ok(()) })),
        Arc::new(move || now),
        chrono::FixedOffset::east_opt(0).unwrap(),
    );
    let ticked = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let timer = tokio::spawn({
        let ticked = ticked.clone();
        async move {
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                *ticked.lock().unwrap() = std::time::Instant::now();
            }
        }
    });

    let hold = WriteLockHold::new(&path);
    let admit = tokio::spawn({
        let metadata = metadata.clone();
        async move { intake.admit(&upload, &metadata, &device).await }
    });
    let held_until = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < held_until {
        std::thread::sleep(Duration::from_millis(50));
        let since = ticked.lock().unwrap().elapsed();
        assert!(
            since < Duration::from_millis(600),
            "the timer stood still for {since:?} while the admission waited"
        );
    }
    assert!(!admit.is_finished(), "the admission waits for the lock");
    drop(hold);
    let meeting_id = admit.await.unwrap().unwrap();
    timer.abort();
    let receipt = store
        .handover_receipt(metadata.recording_id)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.state.meeting_id(), Some(meeting_id));
    assert_eq!(
        store.meeting(meeting_id).unwrap().unwrap().state,
        MeetingState::Queued
    );
}
