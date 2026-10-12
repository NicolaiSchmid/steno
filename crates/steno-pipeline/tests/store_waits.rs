//! A run that meets the store locked by another process past its busy
//! timeout: the meeting waits `queued` for the store with what the run
//! committed, never `failed`, and the store's retry
//! (`resume_waiting_for_store`) processes it once the lock is let go; a
//! resume the store refuses is owed to that retry. The lock is a second
//! connection's `BEGIN IMMEDIATE`, as a `steno` command's write holds it.

mod common;

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

/// A gate a fake waits at once: `entered` when it is reached, then it
/// waits for `open`.
#[derive(Default)]
struct Gate {
    used: AtomicBool,
    entered: tokio::sync::Notify,
    open: tokio::sync::Notify,
}

impl Gate {
    /// The first call waits here.
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

/// The fake engine, its first warm-up held at `gate`: the run's first
/// write (`processing`) comes right after.
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

/// The passthrough cleaner, its first cleanup held at `gate`: the merge
/// has committed the transcript by then, and the cleanup's write follows.
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

/// The store's busy error, as SQLite reports a lock another connection
/// held past the busy timeout.
fn busy() -> StoreError {
    StoreError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("database is locked".to_owned()),
    ))
}

/// A speaker memory over the store whose first match meets the store
/// locked: boxed, as `StoreSpeakerMemory` returns it.
#[derive(Default)]
struct LockedOnce {
    refused: AtomicBool,
    inner: InMemorySpeakerMemory,
}

#[async_trait]
impl SpeakerMemory for LockedOnce {
    async fn candidates(
        &self,
        embedding: &Embedding,
        limit: usize,
    ) -> BoundaryResult<Vec<SpeakerMatch>> {
        if !self.refused.swap(true, Ordering::SeqCst) {
            let boxed: BoxError = Box::new(busy());
            return Err(boxed);
        }
        self.inner.candidates(embedding, limit).await
    }
}

/// A store with an audio folder in `dir`, and a pipeline over it with
/// `engine`; `dependencies` changes the rest.
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
    /// Enqueues a two-lane call starting `minutes` after the sample start;
    /// its meeting id.
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

    /// A second connection that holds the write lock until it is dropped,
    /// as another process's write transaction does.
    fn lock(&self) -> WriteLockHold {
        WriteLockHold::new(&self.path)
    }

    /// The pipeline's connection gives up on a lock at once instead of
    /// after five seconds, for the tests that are not about the wait.
    fn without_busy_timeout(&self) {
        self.store
            .read(|connection| Ok(connection.busy_timeout(Duration::ZERO)?))
            .unwrap();
    }

    fn state(&self, meeting_id: Uuid) -> MeetingState {
        self.store.meeting(meeting_id).unwrap().unwrap().state
    }

    /// Waits until `meeting_id` waits for the store.
    async fn until_waiting_for_store(&self, meeting_id: Uuid) {
        until_waiting_for_store(&self.pipeline, meeting_id).await;
    }

    /// The next launch: a fresh pipeline (its own waits, quit latch and
    /// in-flight set) over the same store.
    fn launch(&self) -> ProcessingPipeline {
        ProcessingPipeline::new(common::dependencies(
            &self.store,
            Arc::new(FakeSpeechEngine::default()),
        ))
    }
}

/// Waits until `meeting_id` waits for the store on `pipeline`.
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

/// Whether the store's retry is woken within a moment: a wake-up is kept
/// until it is awaited, so this also takes one an earlier step left.
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

/// The run's first write meets the lock and waits out the whole
/// five-second busy timeout: the meeting stays `queued` with no failure
/// reason, posts `StoreBusy` (not `ModelsMissing`) and waits for the
/// store, not for models. An install's resume leaves it alone, and
/// "Process again" says what it waits for. Once the lock is let go, the
/// store's retry processes it to `ready`.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_the_store_keeps_locked_past_its_busy_timeout_waits_queued_and_then_runs() {
    let engine = Arc::new(HeldWarmUp::default());
    let world = world(engine.clone(), |dependencies| dependencies);
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    let mut events = pipeline.dependencies().events.subscribe();
    let meeting = world.enqueue_call(0);
    engine.gate.reached().await;
    let lock = world.lock();
    let held_at = std::time::Instant::now();
    engine.gate.open.notify_one();

    world.until_waiting_for_store(meeting).await;
    let waited = held_at.elapsed();
    assert!(
        waited >= Duration::from_secs(5),
        "the write waited out the busy timeout"
    );
    // The park writes nothing over a meeting still `queued`, so it does
    // not wait out the busy timeout a second time.
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

/// A lock that comes once the merge committed the transcript: the
/// cleanup's write is refused, and so is the park's write of `queued`, so
/// the meeting stays `processing` in the store. It waits all the same, its
/// transcript, its recording and its asset row as the run left them, and
/// the retry once the lock is let go processes it to `ready`.
#[tokio::test(flavor = "multi_thread")]
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

    world.until_waiting_for_store(meeting).await;
    pipeline.wait_until_idle().await;
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

/// A speaker match the store refuses (the speaker memory reads the store)
/// does not cost the meeting its speakers, as another failure there would:
/// the run waits for the store, and the retry matches them.
#[tokio::test(flavor = "multi_thread")]
async fn a_speaker_match_the_store_refuses_waits_instead_of_dropping_the_match() {
    let world = world(Arc::new(FakeSpeechEngine::default()), |mut dependencies| {
        dependencies.speaker_memory = Arc::new(LockedOnce::default());
        dependencies
    });
    let pipeline = &world.pipeline;
    let meeting = world.enqueue_call(0);
    world.until_waiting_for_store(meeting).await;
    pipeline.wait_until_idle().await;
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

/// A meeting waiting for its models is not taken by the store's retry: it
/// stays waiting for them, unstarted, and "Process again" still says to
/// download them.
#[tokio::test(flavor = "multi_thread")]
async fn the_store_retry_leaves_a_meeting_waiting_for_models_alone() {
    let engine = Arc::new(Uninstalled::default());
    let world = world(engine.clone(), |dependencies| dependencies);
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

/// An install's resume the store refuses (here its write that marks a
/// waiting meeting whose asset row is gone failed): the meetings it took
/// wait for the store from then on, their cards say so (`StoreBusy`), and
/// the store's retry is woken. Once the lock is let go, the retry starts
/// them: the one with an asset runs to `ready`, the other is failed as
/// the resume would have.
#[tokio::test(flavor = "multi_thread")]
async fn a_resume_the_store_refuses_waits_for_the_store_retry() {
    let engine = Arc::new(Uninstalled::default());
    let world = world(engine.clone(), |dependencies| dependencies);
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    // Oldest first: the resume meets the lock before it starts the other.
    let gone = world.enqueue_call(0);
    let kept = world.enqueue_call(1);
    pipeline.wait_until_idle().await;
    let mut both = vec![gone, kept];
    both.sort();
    assert_eq!(waits.waiting(), both);
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
    let mut events = pipeline.dependencies().events.subscribe();
    let lock = world.lock();

    let refused = pipeline.resume_waiting().unwrap_err();
    assert!(refused.is_busy(), "{refused}");
    assert_eq!(waits.waiting(), Vec::<Uuid>::new());
    assert_eq!(waits.waiting_for_store(), both);
    tokio::time::timeout(PATIENCE, waits.until_store_retry())
        .await
        .expect("the store's retry was woken");
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

/// The launch's resume the store refuses is owed to the store's retry,
/// and stays owed while the lock refuses the retry too; the retry runs it
/// again once the lock is let go: the queued meeting is processed, not
/// left for the next launch.
#[tokio::test(flavor = "multi_thread")]
async fn a_launch_resume_the_store_refuses_runs_again_on_the_store_retry() {
    let world = world(Arc::new(FakeSpeechEngine::default()), |dependencies| {
        dependencies
    });
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    // A meeting the last process left queued without its asset row, older
    // than one with it: the resume's write that fails the first meets the
    // lock.
    let mut gone = sample_data::meeting();
    gone.id = Uuid::new_v4();
    gone.state = MeetingState::Queued;
    world.store.save_meeting(&gone).unwrap();
    let mut queued = sample_data::meeting();
    queued.id = Uuid::new_v4();
    queued.state = MeetingState::Queued;
    queued.started_at += chrono::Duration::minutes(1);
    let asset = steno_pipeline::fixtures::two_lane_call(
        &world.audio,
        queued.id,
        AudioRetention::KeepForever,
    )
    .unwrap();
    world
        .store
        .save_meeting_with_asset(&queued, &asset)
        .unwrap();
    world.without_busy_timeout();
    let lock = world.lock();

    let refused = pipeline.resume_unfinished().unwrap_err();
    assert!(refused.is_busy(), "{refused}");
    tokio::time::timeout(PATIENCE, waits.until_store_retry())
        .await
        .expect("the store's retry was woken");
    assert_eq!(world.state(queued.id), MeetingState::Queued);
    // A retry while the lock is still held is refused too: the launch's
    // resume stays owed.
    assert!(pipeline.resume_waiting_for_store().unwrap_err().is_busy());
    assert_eq!(world.state(queued.id), MeetingState::Queued);

    drop(lock);
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [queued.id]);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(queued.id), MeetingState::Ready);
    assert!(matches!(world.state(gone.id), MeetingState::Failed { .. }));
}

/// A store error that is not the lock (a full disk, from the speaker
/// memory) is not a wait for the store: it costs the match only, as
/// before, and the meeting runs to `ready`.
#[tokio::test(flavor = "multi_thread")]
async fn a_store_error_that_is_not_the_lock_does_not_wait_for_the_store() {
    let world = world(Arc::new(FakeSpeechEngine::default()), |mut dependencies| {
        dependencies.speaker_memory = Arc::new(FullOnce::default());
        dependencies
    });
    let pipeline = &world.pipeline;
    let meeting = world.enqueue_call(0);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
    assert_eq!(
        pipeline.dependencies().model_waits.waiting_for_store(),
        Vec::<Uuid>::new()
    );
}

/// A speaker memory over the store whose first match meets a full disk:
/// boxed, as `StoreSpeakerMemory` returns it.
#[derive(Default)]
struct FullOnce {
    refused: AtomicBool,
    inner: InMemorySpeakerMemory,
}

#[async_trait]
impl SpeakerMemory for FullOnce {
    async fn candidates(
        &self,
        embedding: &Embedding,
        limit: usize,
    ) -> BoundaryResult<Vec<SpeakerMatch>> {
        if !self.refused.swap(true, Ordering::SeqCst) {
            let full = StoreError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                Some("database or disk is full".to_owned()),
            ));
            assert!(!full.is_busy());
            let boxed: BoxError = Box::new(full);
            return Err(boxed);
        }
        self.inner.candidates(embedding, limit).await
    }
}

/// A retry the lock refuses again (at its write that fails a meeting
/// whose asset row is gone) keeps both meetings waiting for the store,
/// not for models, and wakes the next retry. A retry that fails for
/// another reason (the store refuses writes) drops them: they stay
/// `queued`, unfailed, and the next launch processes them.
#[tokio::test(flavor = "multi_thread")]
async fn a_retry_the_lock_refuses_keeps_its_meetings_and_another_failure_leaves_them_queued() {
    let engine = Arc::new(Uninstalled::default());
    let world = world(engine.clone(), |dependencies| dependencies);
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    let gone = world.enqueue_call(0);
    let kept = world.enqueue_call(1);
    pipeline.wait_until_idle().await;
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
    assert!(pipeline.resume_waiting().unwrap_err().is_busy());
    let mut both = vec![gone, kept];
    both.sort();
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
        .read(|connection| Ok(connection.pragma_update(None, "query_only", true)?))
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
        .read(|connection| Ok(connection.pragma_update(None, "query_only", false)?))
        .unwrap();
    pipeline.quit();
    let launch = world.launch();
    assert_eq!(launch.resume_unfinished().unwrap(), [kept]);
    launch.wait_until_idle().await;
    assert_eq!(world.state(kept), MeetingState::Ready);
    assert!(matches!(world.state(gone), MeetingState::Failed { .. }));
}

/// The store's retry that reruns a launch's resume puts back the meeting
/// waiting for the store that another operation (a re-export) holds at
/// that moment, and only that one: a held meeting that did not wait for
/// the store is left `queued` for the next launch, as a launch leaves it.
#[tokio::test(flavor = "multi_thread")]
async fn an_owed_launch_resume_puts_back_the_held_meeting_that_waits_for_the_store() {
    let world = world(Arc::new(FakeSpeechEngine::default()), |mut dependencies| {
        dependencies.speaker_memory = Arc::new(LockedOnce::default());
        dependencies
    });
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    let parked = world.enqueue_call(2);
    world.until_waiting_for_store(parked).await;
    pipeline.wait_until_idle().await;
    // Older than `parked`, so the launch's resume meets the lock at its
    // write that fails `gone`, before it reaches the others.
    let mut gone = sample_data::meeting();
    gone.id = Uuid::new_v4();
    gone.state = MeetingState::Queued;
    world.store.save_meeting(&gone).unwrap();
    let mut other = sample_data::meeting();
    other.id = Uuid::new_v4();
    other.state = MeetingState::Queued;
    other.started_at += chrono::Duration::minutes(1);
    let asset = steno_pipeline::fixtures::two_lane_call(
        &world.audio,
        other.id,
        AudioRetention::KeepForever,
    )
    .unwrap();
    world.store.save_meeting_with_asset(&other, &asset).unwrap();
    world.without_busy_timeout();
    let lock = world.lock();
    assert!(pipeline.resume_unfinished().unwrap_err().is_busy());
    drop(lock);

    let held = [
        pipeline.claim_redeliver(parked).unwrap(),
        pipeline.claim_redeliver(other.id).unwrap(),
    ];
    assert_eq!(
        pipeline.resume_waiting_for_store().unwrap(),
        Vec::<Uuid>::new()
    );
    assert!(matches!(world.state(gone.id), MeetingState::Failed { .. }));
    assert_eq!(waits.waiting_for_store(), [parked]);
    drop(held);

    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [parked]);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(parked), MeetingState::Ready);
    assert_eq!(world.state(other.id), MeetingState::Queued);
}

/// Sets up a meeting left `processing` by a park whose `queued` write met
/// the lock, as in `a_run_the_store_stops_after_the_merge_keeps_its_transcript_and_its_audio`:
/// the world, the meeting, the lock still held and the transcript's
/// segment count.
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
    world.until_waiting_for_store(meeting).await;
    world.pipeline.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Processing);
    (world, meeting, lock, segments)
}

/// The app quits while such a meeting is `processing`, the lock is let
/// go, and the next launch resumes it to `ready`, its transcript not
/// doubled.
#[tokio::test(flavor = "multi_thread")]
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

/// The same, but the next launch comes while the lock is still held: its
/// run waits for the store again, unfailed, and its retry processes the
/// meeting once the lock is let go.
#[tokio::test(flavor = "multi_thread")]
async fn a_meeting_the_lock_left_processing_waits_again_at_a_launch_under_the_lock() {
    let (world, meeting, lock, segments) = left_processing_by_the_lock().await;
    world.pipeline.quit();
    let launch = world.launch();
    assert_eq!(launch.resume_unfinished().unwrap(), [meeting]);
    until_waiting_for_store(&launch, meeting).await;
    launch.wait_until_idle().await;
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

/// A quit while a meeting waits `queued` for the store: a retry after the
/// quit starts nothing, the meeting is not failed, and the next launch
/// processes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_quit_while_a_meeting_waits_for_the_store_leaves_it_for_the_next_launch() {
    let engine = Arc::new(HeldWarmUp::default());
    let world = world(engine.clone(), |dependencies| dependencies);
    world.without_busy_timeout();
    let meeting = world.enqueue_call(0);
    engine.gate.reached().await;
    let lock = world.lock();
    engine.gate.open.notify_one();
    world.until_waiting_for_store(meeting).await;
    world.pipeline.wait_until_idle().await;
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

/// A quit while the run still waits out the store's five-second busy
/// timeout on its first write: the meeting is not failed and not recorded
/// as waiting, and the next launch processes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_quit_while_a_run_waits_on_the_lock_leaves_the_meeting_for_the_next_launch() {
    let engine = Arc::new(HeldWarmUp::default());
    let world = world(engine.clone(), |dependencies| dependencies);
    let meeting = world.enqueue_call(0);
    engine.gate.reached().await;
    let lock = world.lock();
    let held_at = std::time::Instant::now();
    engine.gate.open.notify_one();
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

/// A models install while a meeting waits for the store and the other
/// process still holds the lock: the install's resume takes only the
/// meeting waiting for models, whose run then waits for the store too.
/// Neither is failed, and the store's retry processes both once the lock
/// is let go.
#[tokio::test(flavor = "multi_thread")]
async fn a_models_install_under_the_lock_leaves_both_meetings_waiting_for_the_store() {
    let engine = Arc::new(Uninstalled::default());
    engine.installed.store(true, Ordering::SeqCst);
    let world = world(engine.clone(), |mut dependencies| {
        dependencies.speaker_memory = Arc::new(LockedOnce::default());
        dependencies
    });
    let pipeline = &world.pipeline;
    let waits = pipeline.dependencies().model_waits.clone();
    let parked = world.enqueue_call(0);
    world.until_waiting_for_store(parked).await;
    pipeline.wait_until_idle().await;
    engine.installed.store(false, Ordering::SeqCst);
    let models = world.enqueue_call(1);
    pipeline.wait_until_idle().await;
    assert_eq!(waits.waiting(), [models]);
    assert_eq!(waits.waiting_for_store(), [parked]);

    world.without_busy_timeout();
    let lock = world.lock();
    engine.installed.store(true, Ordering::SeqCst);
    let resumed = pipeline.resume_waiting();
    world.until_waiting_for_store(models).await;
    pipeline.wait_until_idle().await;
    assert!(
        matches!(&resumed, Ok(started) if started == &[models])
            || matches!(&resumed, Err(failure) if failure.is_busy()),
        "{resumed:?}"
    );
    assert_eq!(waits.waiting(), Vec::<Uuid>::new());
    let mut both = vec![parked, models];
    both.sort();
    assert_eq!(waits.waiting_for_store(), both);
    for meeting in both.iter().copied() {
        assert_eq!(world.state(meeting).failure_reason(), None);
    }

    drop(lock);
    let mut started = pipeline.resume_waiting_for_store().unwrap();
    started.sort();
    assert_eq!(started, both);
    pipeline.wait_until_idle().await;
    for meeting in both {
        assert_eq!(world.state(meeting), MeetingState::Ready);
    }
}

/// A meeting waiting for the store does not keep an update's install
/// out: the in-flight set reads idle, so the install takes its hold, and a
/// retry during the hold runs nothing. The install quits the app; the
/// meeting stays `queued`, unfailed, and the next launch processes it.
#[tokio::test(flavor = "multi_thread")]
async fn an_update_hold_during_a_store_wait_runs_nothing_and_the_relaunch_processes_it() {
    let world = world(Arc::new(FakeSpeechEngine::default()), |mut dependencies| {
        dependencies.speaker_memory = Arc::new(LockedOnce::default());
        dependencies
    });
    let pipeline = &world.pipeline;
    let meeting = world.enqueue_call(0);
    world.until_waiting_for_store(meeting).await;
    pipeline.wait_until_idle().await;
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

/// The same install given up (its hold dropped, no quit): the run the
/// retry started under the hold goes on to `ready`.
#[tokio::test(flavor = "multi_thread")]
async fn a_retry_under_an_update_hold_runs_once_the_hold_is_given_up() {
    let world = world(Arc::new(FakeSpeechEngine::default()), |mut dependencies| {
        dependencies.speaker_memory = Arc::new(LockedOnce::default());
        dependencies
    });
    let pipeline = &world.pipeline;
    let meeting = world.enqueue_call(0);
    world.until_waiting_for_store(meeting).await;
    pipeline.wait_until_idle().await;
    let hold = pipeline.dependencies().in_flight.try_hold().expect("idle");
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [meeting]);
    drop(hold);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(meeting), MeetingState::Ready);
}
