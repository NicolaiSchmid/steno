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
        let waits = self.pipeline.dependencies().model_waits.clone();
        tokio::time::timeout(PATIENCE, async {
            while !waits.waiting_for_store().contains(&meeting_id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the meeting waits for the store");
    }
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
    assert!(
        held_at.elapsed() >= Duration::from_secs(5),
        "the write waited out the busy timeout"
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

    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [meeting]);
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
/// which runs it again once the lock is let go: the queued meeting is
/// processed, not left for the next launch.
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

    drop(lock);
    assert_eq!(pipeline.resume_waiting_for_store().unwrap(), [queued.id]);
    pipeline.wait_until_idle().await;
    assert_eq!(world.state(queued.id), MeetingState::Ready);
    assert!(matches!(world.state(gone.id), MeetingState::Failed { .. }));
}
