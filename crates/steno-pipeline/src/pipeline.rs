//! The post-meeting pipeline: one typed function per stage, `progress`
//! posted as each stage starts, and one place that turns any error into
//! `failed(reason)`. Lanes are decoded one at a time, one operation runs per
//! meeting at a time across every pipeline sharing an [`InFlight`] set, and
//! the speech engine is released once no job needs it.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Instant;

use chrono::{DateTime, Utc};
use steno_core::{
    AudioAsset, AudioBuffer16k, AudioDamage, AudioDecoder, AudioLane, AudioRetention, CleanupInput,
    DeliveryDispatcher, DeliveryStatus, Diarizer, LanguageTag, LlmUsage, Meeting, MeetingEvent,
    MeetingOperation, MeetingSource, MeetingState, MeetingStateKind, MeetingSummarizer,
    Participant, ParticipantRole, PipelineStage, RawSegment, RecordingLayout, Settings, Speaker,
    SpeakerAssignment, SpeakerMemory, SpeechEngine, Store, StoreError, SummaryInput,
    SummaryTemplate, TimeRange, TitleOrigin, TranscriptCleaner, TranscriptSegment, derived_uuid,
    paths::{file_url, file_url_path},
    protocols::{BoxError, DEFAULT_MATCH_MARGIN},
    room_speaker_id,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::estimator::{ProcessingEstimator, StageRates, StageSample, absorbing};
use crate::events::MeetingEventBus;
use crate::intake::RecordingResult;
use crate::lane_merger::{ClusterSpeaker, LaneMerger};
use crate::live_recording::LiveRecording;
use crate::run::ProcessingRun;
use crate::sample_clips::{self, ClipProbe, ClipStep};

/// The one failure type: [`ProcessingPipeline::process`] marks the meeting
/// failed with it in one place, unless it is
/// [`FailureKind::ModelsMissing`] or wraps a busy store error.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub struct PipelineFailure {
    pub stage: PipelineStage,
    pub reason: String,
    pub kind: FailureKind,
    /// Wraps a store error another connection's lock caused
    /// ([`StoreError::is_busy`]).
    busy: bool,
}

/// What a [`PipelineFailure`] means for its meeting.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// The meeting is marked failed with the reason.
    #[default]
    Failed,
    /// A model the run needs is not installed: the meeting stays `queued`,
    /// and [`ProcessingPipeline::resume_waiting`] processes it once the
    /// models are installed.
    ModelsMissing,
}

impl PipelineFailure {
    /// The reason of [`PipelineFailure::models_missing`].
    pub const MODELS_MISSING: &'static str = "Download the speech models in Settings";

    /// A failure of the kind [`FailureKind::Failed`].
    #[must_use]
    pub fn new(stage: PipelineStage, reason: impl Into<String>) -> Self {
        PipelineFailure {
            stage,
            reason: reason.into(),
            kind: FailureKind::Failed,
            busy: false,
        }
    }

    /// The failure wraps a busy store error: the call may be tried again. A
    /// run that meets one leaves its meeting `queued`, waiting for the store.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// The refusal of a speech engine or diarizer whose models are not
    /// installed, at `stage`; [`Self::wrapping`] keeps it when boxed.
    ///
    /// ```
    /// use steno_core::PipelineStage;
    /// use steno_pipeline::PipelineFailure;
    ///
    /// let refusal = PipelineFailure::models_missing(PipelineStage::Transcribe);
    /// assert!(refusal.is_models_missing());
    /// let boxed: steno_core::protocols::BoxError = Box::new(refusal.clone());
    /// assert_eq!(PipelineFailure::wrapping(&boxed, PipelineStage::Decode), refusal);
    /// ```
    #[must_use]
    pub fn models_missing(stage: PipelineStage) -> Self {
        PipelineFailure {
            stage,
            reason: Self::MODELS_MISSING.to_owned(),
            kind: FailureKind::ModelsMissing,
            busy: false,
        }
    }

    /// Whether this is [`FailureKind::ModelsMissing`].
    #[must_use]
    pub fn is_models_missing(&self) -> bool {
        self.kind == FailureKind::ModelsMissing
    }

    /// `error` itself when it already is a `PipelineFailure`, bare or boxed
    /// (its stage and kind win), else a failure for `stage` describing it.
    #[must_use]
    pub fn wrapping<E: fmt::Display + 'static>(error: &E, stage: PipelineStage) -> Self {
        let any: &dyn Any = error;
        let carried = any.downcast_ref::<PipelineFailure>().or_else(|| {
            any.downcast_ref::<BoxError>()
                .and_then(|boxed| boxed.downcast_ref::<PipelineFailure>())
        });
        match carried {
            Some(failure) => failure.clone(),
            None => PipelineFailure {
                // Bare, or boxed by a boundary over the store.
                busy: any
                    .downcast_ref::<StoreError>()
                    .or_else(|| {
                        any.downcast_ref::<BoxError>()
                            .and_then(|boxed| boxed.downcast_ref::<StoreError>())
                    })
                    .is_some_and(StoreError::is_busy),
                ..PipelineFailure::new(stage, error.to_string())
            },
        }
    }
}

impl fmt::Display for PipelineFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.stage.as_str(), self.reason)
    }
}

type Result<T> = std::result::Result<T, PipelineFailure>;

/// Why [`ProcessingPipeline::reprocess`] did not start a run. The text is
/// for logs and the CLI.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReprocessError {
    /// No meeting has the id.
    #[error("meeting {0} not found")]
    MeetingNotFound(Uuid),
    /// The meeting is recording, queued or processing.
    #[error("meeting {meeting_id} is {} and not finished yet", state.as_str())]
    Unfinished {
        /// The meeting asked for.
        meeting_id: Uuid,
        /// Its state.
        state: MeetingStateKind,
    },
    /// A run left the meeting `queued` for missing models, and it waits for
    /// them, not for a run.
    #[error("meeting {0} waits for the speech models to be installed")]
    WaitingForModels(Uuid),
    /// A run left the meeting `queued` because another process held the
    /// store's write lock; the store retry or the next launch starts it.
    #[error("meeting {0} waits for another process to let go of the database")]
    WaitingForStore(Uuid),
    /// The meeting is finished, but [`Meeting::offers_process_again`] does
    /// not offer it.
    #[error("meeting {0} is not offered to be processed again")]
    NotOffered(Uuid),
    /// The meeting has no recording on record.
    #[error("meeting {0} has no recording on record")]
    NoAsset(Uuid),
    /// The master is not on disk; the sidecars alone are not enough.
    #[error("meeting {0}'s audio is no longer on disk")]
    AudioGone(Uuid),
    /// Another operation or background run holds the meeting.
    #[error("meeting {0} is busy")]
    Busy(Uuid),
    /// The app is exiting: nothing was saved.
    #[error("the app is quitting")]
    Quitting,
    /// The store failed reading the meeting or saving it queued.
    #[error(transparent)]
    Pipeline(#[from] PipelineFailure),
}

/// The log target of a background run's failure; the CLI, which prints the
/// failure itself, turns it off.
pub const BACKGROUND_RUN_LOG: &str = "steno_pipeline::background";

/// A re-run or re-export whose meeting is already claimed: awaiting it does
/// the work, dropping it unawaited releases the meeting.
pub type Operation = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;

/// Wall-clock stamps for rows; tests pin it.
pub type Now = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Whether the models the speech engine and the diarizer load are installed.
pub type ModelsInstalled = Arc<dyn Fn() -> bool + Send + Sync>;

/// A monotonic clock in seconds, for stage durations.
pub trait MonotonicClock: Send + Sync {
    fn seconds(&self) -> f64;
}

/// `std::time::Instant` since the clock was made.
#[derive(Debug)]
pub struct SystemClock {
    start: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemClock {
    fn seconds(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

/// The app's exit as the pipelines see it; clones share it. Once set, no job
/// starts, and a job that fails leaves its meeting for the next launch's
/// resume instead of marking it `failed`: the exit can end a job, and that
/// is no failure of the meeting's.
#[derive(Debug, Clone, Default)]
pub struct QuitLatch(Arc<AtomicBool>);

impl QuitLatch {
    /// Sets the latch for good.
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether the latch is set: the app is exiting.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The meetings with an operation in progress and the recordings being
/// processed, shared by every pipeline whose dependencies carry it, so a
/// reload's new pipeline refuses a meeting the retired one still holds. Its
/// lock is never held while a pipeline's own state lock is taken.
///
/// An update's install relaunches only once [`is_idle`](Self::is_idle)
/// holds, and while its [`JobHold`] lives no job starts.
#[derive(Debug, Clone, Default)]
pub struct InFlight(Arc<SharedInFlight>);

/// The set, and the wake-up for the jobs a [`JobHold`] kept waiting.
#[derive(Debug, Default)]
struct SharedInFlight {
    set: Mutex<InFlightSet>,
    released: tokio::sync::Notify,
}

#[derive(Debug, Default)]
struct InFlightSet {
    /// Meetings with an operation in progress.
    meetings: BTreeSet<Uuid>,
    /// Counts every admission to `meetings`; a stage whose count moved was
    /// not alone.
    admissions: u64,
    /// The assets claimed for a background run, until the run ends.
    assets: BTreeSet<Uuid>,
    /// A [`JobHold`] lives.
    held: bool,
}

impl InFlightSet {
    fn is_idle(&self) -> bool {
        self.meetings.is_empty() && self.assets.is_empty()
    }
}

impl InFlight {
    fn lock(&self) -> MutexGuard<'_, InFlightSet> {
        self.0
            .set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether no operation holds a meeting and no background run holds
    /// its asset. A meeting waiting for models or the store is not in
    /// flight.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.lock().is_idle()
    }

    /// While idle and no other hold lives, a hold that keeps every job from
    /// starting until it is dropped; `None` otherwise. Claims still succeed
    /// meanwhile, and their jobs start once the hold goes.
    ///
    /// ```
    /// use steno_pipeline::InFlight;
    ///
    /// let in_flight = InFlight::default();
    /// let hold = in_flight.try_hold().expect("idle");
    /// assert!(in_flight.try_hold().is_none(), "one hold at a time");
    /// drop(hold);
    /// assert!(in_flight.try_hold().is_some());
    /// ```
    #[must_use]
    pub fn try_hold(&self) -> Option<JobHold> {
        let mut set = self.lock();
        if set.held || !set.is_idle() {
            return None;
        }
        set.held = true;
        Some(JobHold(self.clone()))
    }

    /// Returns once no [`JobHold`] lives.
    async fn until_released(&self) {
        loop {
            // Before the check, so a release in between still wakes it.
            let released = self.0.released.notified();
            let held = self.lock().held;
            if !held {
                return;
            }
            released.await;
        }
    }
}

/// [`InFlight::try_hold`]'s hold: until it is dropped no job starts.
#[derive(Debug)]
#[must_use = "the jobs start again once the hold is dropped"]
pub struct JobHold(InFlight);

impl Drop for JobHold {
    fn drop(&mut self) {
        self.0.lock().held = false;
        self.0.0.released.notify_waiters();
    }
}

/// The meetings runs left `queued` for missing models or a busy store, and
/// how many resumes ran, shared by every pipeline over dependencies that
/// carry it. Three rules keep a waiting meeting from being skipped or run
/// twice:
///
/// 1. A refused run records its meeting as waiting only once it holds it no
///    more, so a resume never finds it held by the run that left it.
/// 2. A resume counts itself and takes the waiting meetings, starting those
///    no operation holds and putting the others back.
/// 3. A refused run that finds another resume count than when it started
///    records nothing and starts the meeting again at once, on the newest
///    pipeline built over the value.
///
/// The store's waits follow rule 1; their retry repeats while a meeting
/// waits ([`until_store_retry`](Self::until_store_retry)), so rule 3 is not
/// needed there.
#[derive(Debug, Clone, Default)]
pub struct ModelWaits(Arc<SharedWaits>);

/// The waits under one lock, and the wake-up of the store's retry.
#[derive(Debug, Default)]
struct SharedWaits {
    waits: Mutex<Waits>,
    store_retry: tokio::sync::Notify,
}

/// The waiting meetings, the resume count and the newest pipeline.
#[derive(Debug, Default)]
struct Waits {
    waiting: BTreeSet<Uuid>,
    resumes: u64,
    /// The pipeline a restart goes to (rule 3), while it lives.
    newest: Weak<Inner>,
    /// The meetings that wait for the store.
    for_store: BTreeSet<Uuid>,
    /// A launch's resume the store refused, owed to the next retry.
    unfinished_owed: bool,
}

/// What the store's retry takes.
struct StoreWaits {
    meetings: BTreeSet<Uuid>,
    unfinished_owed: bool,
}

impl ModelWaits {
    /// The meetings waiting now.
    #[must_use]
    pub fn waiting(&self) -> Vec<Uuid> {
        self.lock().waiting.iter().copied().collect()
    }

    /// The meetings waiting for the store now.
    #[must_use]
    pub fn waiting_for_store(&self) -> Vec<Uuid> {
        self.lock().for_store.iter().copied().collect()
    }

    /// Returns once something waits for the store's retry. A wake-up while
    /// nobody awaits is kept for the next call.
    pub async fn until_store_retry(&self) {
        self.0.store_retry.notified().await;
    }

    /// Whether `meeting_id` waits now.
    fn holds(&self, meeting_id: Uuid) -> bool {
        self.lock().waiting.contains(&meeting_id)
    }

    /// Whether `meeting_id` waits for the store now.
    fn holds_for_store(&self, meeting_id: Uuid) -> bool {
        self.lock().for_store.contains(&meeting_id)
    }

    /// How many resumes ran, read when a run starts.
    fn resumes(&self) -> u64 {
        self.lock().resumes
    }

    /// Records `meeting_id` as waiting and returns true, unless a resume
    /// ran since the count was `seen` (rules 1 and 3); one step.
    fn wait_unless_resumed(&self, meeting_id: Uuid, seen: u64) -> bool {
        let mut waits = self.lock();
        if waits.resumes != seen {
            return false;
        }
        waits.waiting.insert(meeting_id);
        true
    }

    /// Counts a resume and takes every waiting meeting.
    fn resume(&self) -> BTreeSet<Uuid> {
        let mut waits = self.lock();
        waits.resumes += 1;
        std::mem::take(&mut waits.waiting)
    }

    /// Puts back meetings a resume took and could not start.
    fn put_back(&self, meetings: impl IntoIterator<Item = Uuid>) {
        self.lock().waiting.extend(meetings);
    }

    /// Records `meetings` as waiting for the store and wakes its retry,
    /// unless empty.
    fn wait_for_store(&self, meetings: impl IntoIterator<Item = Uuid>) {
        let mut meetings = meetings.into_iter().peekable();
        if meetings.peek().is_none() {
            return;
        }
        self.lock().for_store.extend(meetings);
        self.0.store_retry.notify_one();
    }

    /// Owes a launch's resume to the store's retry and wakes it.
    fn owe_unfinished(&self) {
        self.lock().unfinished_owed = true;
        self.0.store_retry.notify_one();
    }

    /// Takes what waits for the store.
    fn take_store_waits(&self) -> StoreWaits {
        let mut waits = self.lock();
        StoreWaits {
            meetings: std::mem::take(&mut waits.for_store),
            unfinished_owed: std::mem::take(&mut waits.unfinished_owed),
        }
    }

    /// Makes `pipeline` the one a restart goes to (rule 3).
    fn built(&self, pipeline: &ProcessingPipeline) {
        self.lock().newest = Arc::downgrade(&pipeline.inner);
    }

    /// The newest pipeline built over the value, while it lives.
    fn newest(&self) -> Option<ProcessingPipeline> {
        let inner = self.lock().newest.upgrade()?;
        Some(ProcessingPipeline { inner })
    }

    fn lock(&self) -> MutexGuard<'_, Waits> {
        self.0
            .waits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A speech engine with the jobs that claim it, shared by every pipeline
/// built over it: a job claims the engine from its warm-up to its last
/// lane, and the engine is released only once no claim is left. Warm-ups
/// and the release take one lock, so they never overlap. Clones share the
/// engine and its claims.
///
/// ```
/// use std::sync::Arc;
///
/// use steno_core::testing::FakeSpeechEngine;
/// use steno_pipeline::SharedSpeechEngine;
///
/// let engine = SharedSpeechEngine::new(Arc::new(FakeSpeechEngine::default()));
/// let shared = engine.clone();
/// assert!(shared.ptr_eq(&engine));
/// // A new value over the same engine starts claims of its own.
/// let separate = SharedSpeechEngine::new(engine.engine().clone());
/// assert!(!separate.ptr_eq(&engine));
/// assert_eq!(shared.id(), "fake-engine");
/// ```
#[derive(Clone)]
pub struct SharedSpeechEngine(Arc<SharedEngineState>);

/// The engine and claims the pipelines over one [`SharedSpeechEngine`] share.
struct SharedEngineState {
    engine: Arc<dyn SpeechEngine>,
    /// Jobs between their warm-up and their last lane ([`SpeechClaim`]).
    count: Mutex<usize>,
    /// Serialises the warm-ups and the release after a job's lanes, so each
    /// engine loads once and a warm-up never overlaps a release.
    preparing: AsyncMutex<()>,
    /// The jobs' turn at an in-process engine, held from the wait for a
    /// recording to finish through the call.
    turn: AsyncMutex<()>,
}

impl SharedSpeechEngine {
    /// `engine` with claims of its own; pipelines on one engine share a
    /// clone instead.
    #[must_use]
    pub fn new(engine: Arc<dyn SpeechEngine>) -> Self {
        SharedSpeechEngine(Arc::new(SharedEngineState {
            engine,
            count: Mutex::new(0),
            preparing: AsyncMutex::new(()),
            turn: AsyncMutex::new(()),
        }))
    }

    /// The engine itself.
    #[must_use]
    pub fn engine(&self) -> &Arc<dyn SpeechEngine> {
        &self.0.engine
    }

    /// Whether `other` is a clone of this one.
    #[must_use]
    pub fn ptr_eq(&self, other: &SharedSpeechEngine) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// A handle that finds this engine again without keeping it alive.
    #[must_use]
    pub fn downgrade(&self) -> WeakSpeechEngine {
        WeakSpeechEngine(Arc::downgrade(&self.0))
    }

    /// How many jobs claim the engine now.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn claims(&self) -> usize {
        *self.claim_count()
    }

    /// The number of claims, under their lock.
    fn claim_count(&self) -> MutexGuard<'_, usize> {
        self.0
            .count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Counts a job as needing the engine until the claim is dropped or
    /// handed to [`ProcessingPipeline::finish_speech`].
    fn claim(&self) -> SpeechClaim {
        *self.claim_count() += 1;
        SpeechClaim {
            engine: self.clone(),
        }
    }

    /// The lock the warm-ups and the release take.
    async fn preparing(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.0.preparing.lock().await
    }
}

impl Deref for SharedSpeechEngine {
    type Target = dyn SpeechEngine;

    fn deref(&self) -> &Self::Target {
        self.0.engine.as_ref()
    }
}

/// A [`SharedSpeechEngine`] that does not keep the engine alive, so a
/// reload back to the in-process engine gets the same engine and claims.
///
/// ```
/// use std::sync::Arc;
///
/// use steno_core::testing::FakeSpeechEngine;
/// use steno_pipeline::SharedSpeechEngine;
///
/// let engine = SharedSpeechEngine::new(Arc::new(FakeSpeechEngine::default()));
/// let weak = engine.downgrade();
/// assert!(weak.upgrade().is_some_and(|again| again.ptr_eq(&engine)));
/// drop(engine);
/// assert!(weak.upgrade().is_none());
/// ```
#[derive(Clone)]
pub struct WeakSpeechEngine(Weak<SharedEngineState>);

impl WeakSpeechEngine {
    /// The engine with its claims, while a clone of it is alive.
    #[must_use]
    pub fn upgrade(&self) -> Option<SharedSpeechEngine> {
        self.0.upgrade().map(SharedSpeechEngine)
    }
}

/// Everything the pipeline needs, and the only injection axis. `cleaner`
/// and `summarizer` are `None` when no LLM endpoint is configured: those
/// stages then write nothing, and the meeting lands `ready` without a
/// summary.
#[derive(Clone)]
pub struct PipelineDependencies {
    pub decoder: Arc<dyn AudioDecoder>,
    /// The speech engine with its claims.
    pub speech_engine: SharedSpeechEngine,
    pub diarizer: Arc<dyn Diarizer>,
    pub speaker_memory: Arc<dyn SpeakerMemory>,
    pub cleaner: Option<Arc<dyn TranscriptCleaner>>,
    pub summarizer: Option<Arc<dyn MeetingSummarizer>>,
    pub dispatcher: Arc<dyn DeliveryDispatcher>,
    pub store: Arc<Store>,
    pub events: MeetingEventBus,
    pub now: Now,
    pub clock: Arc<dyn MonotonicClock>,
    /// Clones share it.
    pub quit_latch: QuitLatch,
    /// Clones share it.
    pub in_flight: InFlight,
    /// Called at each step of a run's sample clips, for crash tests.
    clip_probe: Option<ClipProbe>,
    /// Clones share it.
    pub model_waits: ModelWaits,
    /// Whether the speech engine runs in this process, so it waits for a
    /// recording to end.
    pub speech_in_process: bool,
    /// Whether a recording is under way; no in-process engine runs
    /// meanwhile.
    pub live_recording: LiveRecording,
    /// While it is false a run refuses with
    /// [`PipelineFailure::models_missing`] before the engine loads, and an
    /// engine or diarizer call that fails once the models are gone is that
    /// refusal too, so the meeting waits `queued` instead of failing.
    pub models_installed: ModelsInstalled,
}

impl PipelineDependencies {
    /// The wall clock and the system monotonic clock.
    #[must_use]
    pub fn new(
        decoder: Arc<dyn AudioDecoder>,
        speech_engine: Arc<dyn SpeechEngine>,
        diarizer: Arc<dyn Diarizer>,
        speaker_memory: Arc<dyn SpeakerMemory>,
        dispatcher: Arc<dyn DeliveryDispatcher>,
        store: Arc<Store>,
        events: MeetingEventBus,
    ) -> Self {
        PipelineDependencies {
            decoder,
            speech_engine: SharedSpeechEngine::new(speech_engine),
            diarizer,
            speaker_memory,
            cleaner: None,
            summarizer: None,
            dispatcher,
            store,
            events,
            now: Arc::new(Utc::now),
            clock: Arc::new(SystemClock::default()),
            quit_latch: QuitLatch::default(),
            in_flight: InFlight::default(),
            clip_probe: None,
            model_waits: ModelWaits::default(),
            speech_in_process: false,
            live_recording: LiveRecording::default(),
            models_installed: Arc::new(|| true),
        }
    }

    /// Runs on `engine`, sharing its claims.
    #[must_use]
    pub fn with_speech_engine(mut self, engine: SharedSpeechEngine) -> Self {
        self.speech_engine = engine;
        self
    }

    #[must_use]
    pub fn with_llm(
        mut self,
        cleaner: Option<Arc<dyn TranscriptCleaner>>,
        summarizer: Option<Arc<dyn MeetingSummarizer>>,
    ) -> Self {
        self.cleaner = cleaner;
        self.summarizer = summarizer;
        self
    }

    #[must_use]
    pub fn with_now(mut self, now: Now) -> Self {
        self.now = now;
        self
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn MonotonicClock>) -> Self {
        self.clock = clock;
        self
    }

    /// Carries `latch`, so every pipeline over it quits on one
    /// [`QuitLatch::set`].
    #[must_use]
    pub fn with_quit_latch(mut self, latch: QuitLatch) -> Self {
        self.quit_latch = latch;
        self
    }

    /// Carries `in_flight`, shared like the latch.
    #[must_use]
    pub fn with_in_flight(mut self, in_flight: InFlight) -> Self {
        self.in_flight = in_flight;
        self
    }

    /// Calls `probe` at each [`ClipStep`] of a run's sample clips.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_clip_probe(mut self, probe: ClipProbe) -> Self {
        self.clip_probe = Some(probe);
        self
    }

    /// Carries `waits`, so a resume on any pipeline over it sees every
    /// meeting left waiting.
    #[must_use]
    pub fn with_model_waits(mut self, waits: ModelWaits) -> Self {
        self.model_waits = waits;
        self
    }

    /// Refuses runs while `installed` is false
    /// ([`Self::models_installed`]).
    #[must_use]
    pub fn with_models_installed(mut self, installed: ModelsInstalled) -> Self {
        self.models_installed = installed;
        self
    }

    /// Runs the speech engine in this process: it waits for a recording.
    #[must_use]
    pub fn with_speech_in_process(mut self, in_process: bool) -> Self {
        self.speech_in_process = in_process;
        self
    }

    /// Waits for `live` to be idle before each in-process engine call.
    #[must_use]
    pub fn with_live_recording(mut self, live: LiveRecording) -> Self {
        self.live_recording = live;
        self
    }
}

/// The pipeline's own runs and background tasks.
#[derive(Default)]
struct State {
    runs: HashMap<Uuid, ProcessingRun>,
    /// The background runs, by asset id or, for `redeliver_unfinished`, a
    /// key of their own. A refused run's entry stays until its meeting
    /// waits or starts again.
    running: HashMap<Uuid, JoinHandle<()>>,
}

struct Inner {
    dependencies: PipelineDependencies,
    state: Mutex<State>,
}

/// The pipeline. Cheap to clone: every clone shares the state.
#[derive(Clone)]
pub struct ProcessingPipeline {
    inner: Arc<Inner>,
}

impl fmt::Debug for ProcessingPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessingPipeline")
            .field("in_flight", &self.in_flight())
            .finish_non_exhaustive()
    }
}

/// The two stages' joint hand-off: per-lane raw segments and the elected
/// language.
struct Transcription {
    lanes: BTreeMap<AudioLane, Vec<RawSegment>>,
    language: Option<LanguageTag>,
    /// The longest decoded lane, in seconds.
    seconds: f64,
}

/// A decoded lane with the lane it came from.
struct DecodedLane {
    lane: AudioLane,
    buffer: AudioBuffer16k,
}

/// What the diarize stage hands on.
struct Diarization {
    speakers: Vec<Speaker>,
    cluster_speakers: Vec<ClusterSpeaker>,
    /// The lane the clusters cover; `None` when nothing was diarized.
    lane: Option<AudioLane>,
    /// The room speaker a segment on `lane` that no cluster covers goes to,
    /// set only when the diarizer failed. Its stored row marks the speakers
    /// as incomplete ([`steno_core::results_need_the_audio`]).
    fallback: Option<Uuid>,
}

impl Diarization {
    const NONE: Diarization = Diarization {
        speakers: Vec::new(),
        cluster_speakers: Vec::new(),
        lane: None,
        fallback: None,
    };

    /// The one unknown room speaker the diarizer fallback gives uncovered
    /// segments, labelled after the highest stored "Speaker N" in `taken`
    /// (compared lower-cased), so it never shares a stored speaker's name.
    /// It has no embedding, so confirming it teaches no voice, and an id of
    /// its own, so a later diarized run never inherits its confirmation.
    fn room_speaker(meeting_id: Uuid, taken: &[Speaker]) -> Speaker {
        let next = taken
            .iter()
            .filter_map(|speaker| {
                let label = speaker.cluster_label.to_lowercase();
                label.strip_prefix("speaker ")?.parse::<u32>().ok()
            })
            .max()
            .map_or(1, |highest| highest.saturating_add(1));
        Speaker {
            id: room_speaker_id(meeting_id),
            meeting_id,
            cluster_label: format!("Speaker {next}"),
            assignment: SpeakerAssignment::Unknown,
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: None,
            cluster_confidence: 0.0,
        }
    }
}

struct Merged {
    segments: Vec<TranscriptSegment>,
    speakers: Vec<Speaker>,
    /// The rows the merge replaced, as its transaction read them.
    replaced: Vec<Speaker>,
}

/// The sample clip is at most ten seconds.
pub const SAMPLE_CLIP_SECONDS: f64 = 10.0;

/// Elects a meeting's language from tagged segments by summed duration;
/// `None` when no segment is tagged. Ties break on the tag.
#[must_use]
pub fn elect_language<'a>(
    segments: impl IntoIterator<Item = &'a RawSegment>,
) -> Option<LanguageTag> {
    let mut totals: BTreeMap<LanguageTag, f64> = BTreeMap::new();
    for segment in segments {
        if let Some(language) = &segment.language {
            *totals.entry(language.clone()).or_insert(0.0) += segment.end - segment.start;
        }
    }
    totals
        .into_iter()
        .max_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.0.cmp(&a.0))
        })
        .map(|(tag, _)| tag)
}

/// `mic` before `system` before `mixed`, so "me" sets the hint.
#[must_use]
pub fn ordered_lanes(lanes: &[AudioLane]) -> Vec<AudioLane> {
    AudioLane::ALL
        .iter()
        .copied()
        .filter(|lane| lanes.contains(lane))
        .collect()
}

/// Which lane carries the voices to diarize: the tap in a call, else the
/// one room lane.
#[must_use]
pub fn diarized_lane(source: MeetingSource, lanes: &[AudioLane]) -> Option<AudioLane> {
    let preferred = if source == MeetingSource::MacCall {
        AudioLane::System
    } else {
        AudioLane::Mixed
    };
    if lanes.contains(&preferred) {
        return Some(preferred);
    }
    ordered_lanes(lanes).last().copied()
}

/// [`diarized_lane`], except for a call whose tap carried no conversation:
/// then the microphone heard everyone and the mic lane is diarized as the
/// room.
#[must_use]
pub fn diarized_lane_after_transcription(
    source: MeetingSource,
    lanes: &[AudioLane],
    transcription: &BTreeMap<AudioLane, Vec<RawSegment>>,
) -> Option<AudioLane> {
    if source == MeetingSource::MacCall
        && lanes.contains(&AudioLane::Mic)
        && tap_carried_no_conversation(transcription)
    {
        return Some(AudioLane::Mic);
    }
    diarized_lane(source, lanes)
}

/// The tap carried no conversation when its speech stays under both this
/// share of the mic's speech and [`TAP_CONVERSATION_MAXIMUM_SECONDS`]: a
/// chime on a silent tap stays under both, a partner who mostly listens
/// still clears the seconds.
pub const TAP_CONVERSATION_MINIMUM_SHARE: f64 = 0.05;
/// See [`TAP_CONVERSATION_MINIMUM_SHARE`].
pub const TAP_CONVERSATION_MAXIMUM_SECONDS: f64 = 10.0;

/// True when the mic lane holds speech and the system lane holds less than
/// both conversation bounds.
#[must_use]
pub fn tap_carried_no_conversation(lanes: &BTreeMap<AudioLane, Vec<RawSegment>>) -> bool {
    let (Some(mic), Some(system)) = (lanes.get(&AudioLane::Mic), lanes.get(&AudioLane::System))
    else {
        return false;
    };
    let speech = |segments: &[RawSegment]| {
        segments
            .iter()
            .fold(0.0, |total, segment| total + segment.duration())
    };
    let mic_speech = speech(mic);
    let tap_speech = speech(system);
    mic_speech > 0.0
        && tap_speech < mic_speech * TAP_CONVERSATION_MINIMUM_SHARE
        && tap_speech < TAP_CONVERSATION_MAXIMUM_SECONDS
}

/// `meeting`, `queued` at `now`: what `enqueue` and `enqueue_new` save.
fn queued(meeting: &Meeting, now: DateTime<Utc>) -> Meeting {
    Meeting {
        state: MeetingState::Queued,
        updated_at: now,
        ..meeting.clone()
    }
}

fn attributing<T, E: fmt::Display + 'static>(
    stage: PipelineStage,
    result: std::result::Result<T, E>,
) -> Result<T> {
    result.map_err(|error| PipelineFailure::wrapping(&error, stage))
}

/// The reason a claimed operation that panicked fails with.
pub const OPERATION_PANICKED: &str = "the operation stopped unexpectedly";

/// `work`, with a panic inside it turned into a failure for the stage
/// `stage()` returns (called only after the panic, so it names the stage
/// reached).
async fn unless_it_panics<T>(
    stage: impl FnOnce() -> PipelineStage,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    use futures_util::FutureExt as _;
    std::panic::AssertUnwindSafe(work)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(PipelineFailure::new(stage(), OPERATION_PANICKED)))
}

/// The failure reason launch recovery gives a meeting the last process
/// ended mid-run; one sentence, as the list shows only the first.
pub const CLOSED_WHILE_PROCESSING: &str = "Steno closed while processing. Process again.";

/// How long after its last attempt a failed export is retried at launch.
pub const REDELIVER_AFTER: chrono::TimeDelta = chrono::TimeDelta::hours(24);

/// The failure saved on a delivery an export left `pending` with no
/// reason.
const EXPORT_INTERRUPTED: &str = "the export stopped before it finished";

/// The row an operation needs, or a failure for `stage` naming `missing`.
fn required<T, E: fmt::Display + 'static>(
    stage: PipelineStage,
    result: std::result::Result<Option<T>, E>,
    missing: impl FnOnce() -> String,
) -> Result<T> {
    attributing(stage, result)?.ok_or_else(|| PipelineFailure::new(stage, missing()))
}

impl ProcessingPipeline {
    #[must_use]
    pub fn new(dependencies: PipelineDependencies) -> Self {
        let pipeline = ProcessingPipeline {
            inner: Arc::new(Inner {
                dependencies,
                state: Mutex::new(State::default()),
            }),
        };
        pipeline.inner.dependencies.model_waits.built(&pipeline);
        pipeline
    }

    #[must_use]
    pub fn dependencies(&self) -> &PipelineDependencies {
        &self.inner.dependencies
    }

    fn store(&self) -> &Arc<Store> {
        &self.inner.dependencies.store
    }

    /// The speech engine and its claims.
    fn speech_engine(&self) -> &SharedSpeechEngine {
        &self.inner.dependencies.speech_engine
    }

    fn now(&self) -> DateTime<Utc> {
        (self.inner.dependencies.now)()
    }

    /// The in-flight set every pipeline sharing it sees.
    fn in_flight_set(&self) -> MutexGuard<'_, InFlightSet> {
        self.inner.dependencies.in_flight.lock()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Meetings with an operation in progress on any pipeline sharing this
    /// one's in-flight set.
    #[must_use]
    pub fn in_flight(&self) -> Vec<Uuid> {
        self.in_flight_set().meetings.iter().copied().collect()
    }

    /// Sets the [`QuitLatch`] for the app's exit and saves the meetings in
    /// flight or waiting for the store that read `processing` back as
    /// `queued`, so the next launch
    /// resumes them instead of failing them as a crash; best effort, and it
    /// writes the store, so async callers use `spawn_blocking`. `enqueue`
    /// still saves its meeting `queued`.
    pub fn quit(&self) {
        self.inner.dependencies.quit_latch.set();
        let waiting = self.inner.dependencies.model_waits.waiting_for_store();
        for meeting_id in self.in_flight().into_iter().chain(waiting) {
            let _ = self
                .store()
                .update_meeting(meeting_id, self.now(), |meeting| {
                    if meeting.state == MeetingState::Processing {
                        meeting.state = MeetingState::Queued;
                    }
                    Ok(())
                });
        }
    }

    fn quitting(&self) -> bool {
        self.inner.dependencies.quit_latch.is_set()
    }

    /// For an in-process engine, the job's turn at it, taken once no
    /// recording is under way; meanwhile the meeting says it waits
    /// ([`MeetingEvent::WaitingForRecording`]). `None` for an engine whose
    /// abort ends only its child.
    async fn in_process_turn(
        &self,
        meeting_id: Uuid,
        stage: PipelineStage,
    ) -> Result<Option<tokio::sync::MutexGuard<'_, ()>>> {
        if !self.inner.dependencies.speech_in_process {
            return Ok(None);
        }
        let live = &self.inner.dependencies.live_recording;
        let mut waits = live.is_live();
        if waits {
            self.waiting_for_recording(meeting_id);
        }
        let turn = self.speech_engine().0.turn.lock().await;
        if live.is_live() {
            if !waits {
                self.waiting_for_recording(meeting_id);
            }
            waits = true;
            live.until_idle().await;
        }
        if waits {
            tracing::info!(target: BACKGROUND_RUN_LOG, %meeting_id, "the recording is over; the speech engine runs");
        }
        if self.quitting() {
            return Err(PipelineFailure::new(stage, "the app is quitting"));
        }
        Ok(Some(turn))
    }

    /// Says that `meeting_id`'s job waits for the recording under way.
    fn waiting_for_recording(&self, meeting_id: Uuid) {
        tracing::info!(target: BACKGROUND_RUN_LOG, %meeting_id, "waiting for the recording to finish before the speech engine runs");
        self.inner
            .dependencies
            .events
            .post(MeetingEvent::WaitingForRecording { meeting_id });
    }

    /// Writes `Meeting(queued)` plus the asset in one transaction and starts
    /// `process` in the background. Fails when the asset or meeting is in
    /// flight. Needs a `tokio` runtime. After a [quit](Self::quit) the
    /// meeting stays `queued` for the next launch.
    pub fn enqueue(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        let claim = self.claim_or_refuse(meeting.id, asset)?;
        self.enqueue_claimed(meeting.id, asset, claim, |store, now, asset| {
            store.save_meeting_with_asset(&queued(meeting, now), asset)
        })
    }

    /// [`ProcessingPipeline::enqueue`] of a meeting with no row yet; a row
    /// with the id written meanwhile fails it
    /// ([`Store::insert_meeting_with_asset`]).
    pub fn enqueue_new(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        let claim = self.claim_or_refuse(meeting.id, asset)?;
        self.enqueue_claimed(meeting.id, asset, claim, |store, now, asset| {
            store.insert_meeting_with_asset(&queued(meeting, now), asset)
        })
    }

    /// [`ProcessingPipeline::enqueue`] of the stopped Mac recording
    /// `meeting_id`, written through [`Store::save_stopped_recording`] only
    /// while it is still `recording`. The asset gets `retention`, else the
    /// settings' default. Returns the meeting as saved.
    pub fn enqueue_stopped_recording(
        &self,
        meeting_id: Uuid,
        result: &RecordingResult,
        retention: Option<AudioRetention>,
    ) -> Result<Meeting> {
        let claim = self.claim_or_refuse(meeting_id, &result.asset)?;
        self.enqueue_claimed(meeting_id, &result.asset, claim, |store, now, asset| {
            store.save_stopped_recording(
                meeting_id,
                result.duration,
                result.end_reason.clone(),
                asset,
                retention,
                now,
            )
        })
    }

    /// [`ProcessingPipeline::enqueue`] of a meeting the caller saved
    /// `queued` with its asset: starts `process` and writes nothing. Fails
    /// when the asset or meeting is in flight.
    pub fn enqueue_saved(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        let claim = self.claim_or_refuse(meeting.id, asset)?;
        self.start(asset, claim);
        Ok(())
    }

    /// [`ProcessingPipeline::claim_start`], refused as a failed `decode`.
    fn claim_or_refuse(&self, meeting_id: Uuid, asset: &AudioAsset) -> Result<AssetClaim> {
        self.claim_start(meeting_id, asset.id).ok_or_else(|| {
            PipelineFailure::new(
                PipelineStage::Decode,
                format!("meeting {meeting_id} is already being processed"),
            )
        })
    }

    /// `enqueue` once the asset is claimed: writes the rows through `save`
    /// and holds the claim to the run's end. Returns what `save` returned.
    fn enqueue_claimed<T>(
        &self,
        meeting_id: Uuid,
        asset: &AudioAsset,
        claim: AssetClaim,
        save: impl FnOnce(&Store, DateTime<Utc>, &AudioAsset) -> std::result::Result<T, StoreError>,
    ) -> Result<T> {
        let mut asset = asset.clone();
        asset.meeting_id = meeting_id;
        let saved = attributing(
            PipelineStage::Decode,
            save(self.store(), self.now(), &asset),
        )?;
        self.start(&asset, claim);
        Ok(saved)
    }

    /// Processes a `ready` or `failed` meeting again from `decode`, with a
    /// fresh run. Refused with a [`ReprocessError`] when the
    /// meeting, its asset or its master is missing, when it is not
    /// finished, when another operation holds it, and after a
    /// [quit](Self::quit). Clears the earlier retention stamp so a failed
    /// run never leaves the audio to the sweep. Needs a `tokio` runtime.
    pub fn reprocess(&self, meeting_id: Uuid) -> std::result::Result<(), ReprocessError> {
        self.reprocess_if(meeting_id, |_, _| Ok(true))
    }

    /// "Process again": [`reprocess`](Self::reprocess), refused with
    /// [`ReprocessError::NotOffered`] unless the meeting as read now
    /// [offers it](Meeting::offers_process_again), so a stale view cannot
    /// run a complete meeting again.
    pub fn process_again(&self, meeting_id: Uuid) -> std::result::Result<(), ReprocessError> {
        self.reprocess_if(meeting_id, |meeting, asset| {
            let store = self.store();
            let speakers = attributing(PipelineStage::Decode, store.speakers(meeting.id))?;
            let segments = attributing(PipelineStage::Decode, store.segments(meeting.id))?;
            Ok(meeting.offers_process_again(&speakers, &segments, asset))
        })
    }

    /// [`reprocess`](Self::reprocess) for a finished meeting `offered`
    /// accepts. The asset is claimed first and the meeting read under the
    /// claim, so the run starts from the rows a finished run saved.
    fn reprocess_if(
        &self,
        meeting_id: Uuid,
        offered: impl FnOnce(&Meeting, Option<&AudioAsset>) -> Result<bool>,
    ) -> std::result::Result<(), ReprocessError> {
        if self.quitting() {
            return Err(ReprocessError::Quitting);
        }
        let asset = attributing(PipelineStage::Decode, self.store().asset(meeting_id))?;
        let claim = match &asset {
            Some(asset) => Some(
                self.claim_start(meeting_id, asset.id)
                    .ok_or(ReprocessError::Busy(meeting_id))?,
            ),
            None => None,
        };
        let meeting = attributing(PipelineStage::Decode, self.store().meeting(meeting_id))?
            .ok_or(ReprocessError::MeetingNotFound(meeting_id))?;
        let state = meeting.state.kind();
        if !matches!(state, MeetingStateKind::Ready | MeetingStateKind::Failed) {
            let waits = &self.inner.dependencies.model_waits;
            if waits.holds(meeting_id) {
                return Err(ReprocessError::WaitingForModels(meeting_id));
            }
            if waits.holds_for_store(meeting_id) {
                return Err(ReprocessError::WaitingForStore(meeting_id));
            }
            return Err(ReprocessError::Unfinished { meeting_id, state });
        }
        if !offered(&meeting, asset.as_ref())? {
            return Err(ReprocessError::NotOffered(meeting_id));
        }
        let (mut asset, claim) = asset
            .zip(claim)
            .ok_or(ReprocessError::NoAsset(meeting_id))?;
        // The persist stage mixes the master down; sidecars alone fail.
        if !file_url_path(&asset.url).is_some_and(|master| master.exists()) {
            return Err(ReprocessError::AudioGone(meeting_id));
        }
        asset.expires_at = None;
        self.enqueue_claimed(meeting_id, &asset, claim, |store, now, asset| {
            store.save_meeting_with_asset(&queued(&meeting, now), asset)
        })
        .map_err(Into::into)
    }

    /// Claims `asset_id` for a background run of `meeting_id`, held until
    /// the run ends. `None` while the asset or meeting is in flight on any
    /// pipeline sharing the set; check and claim are one step.
    fn claim_start(&self, meeting_id: Uuid, asset_id: Uuid) -> Option<AssetClaim> {
        let mut in_flight = self.in_flight_set();
        if in_flight.meetings.contains(&meeting_id) || !in_flight.assets.insert(asset_id) {
            return None;
        }
        Some(AssetClaim {
            pipeline: self.clone(),
            asset_id,
        })
    }

    /// Launch recovery: every meeting left `queued` or `processing` is
    /// processed again from `decode`, oldest first, in the background. A
    /// meeting whose asset row is missing is marked failed, and so is one
    /// still `processing`, which the last process ended mid-run
    /// ([`CLOSED_WHILE_PROCESSING`]), so a recording that crashes the app
    /// is not read again at every launch. Counts as a resume of the
    /// meetings waiting for models; one the busy store refuses is owed to
    /// [`resume_waiting_for_store`](Self::resume_waiting_for_store).
    /// Returns the meetings started.
    pub fn resume_unfinished(&self) -> Result<Vec<Uuid>> {
        if self.quitting() {
            return Ok(Vec::new());
        }
        let waits = &self.inner.dependencies.model_waits;
        waits.resume();
        self.resume_among(None)
            .map(|(resumed, _)| resumed)
            .inspect_err(|failure| {
                if failure.is_busy() {
                    waits.owe_unfinished();
                }
            })
    }

    /// [`resume_unfinished`](Self::resume_unfinished) for the meetings
    /// waiting for missing models alone, called once a model install
    /// finished. A waiting meeting another operation holds stays waiting
    /// until the next resume, and so do all when the store fails; a busy
    /// store makes them wait for the store's retry instead.
    pub fn resume_waiting(&self) -> Result<Vec<Uuid>> {
        if self.quitting() {
            return Ok(Vec::new());
        }
        let waiting = self.inner.dependencies.model_waits.resume();
        if waiting.is_empty() {
            return Ok(Vec::new());
        }
        self.start_waiting(waiting)
    }

    /// Starts `meetings`, taken from the waiting ones, as
    /// [`resume_waiting`](Self::resume_waiting) does.
    fn start_waiting(&self, meetings: BTreeSet<Uuid>) -> Result<Vec<Uuid>> {
        let waits = &self.inner.dependencies.model_waits;
        match self.resume_among(Some(&meetings)) {
            Ok((resumed, busy)) => {
                waits.put_back(busy);
                Ok(resumed)
            }
            Err(failure) if failure.is_busy() && !self.quitting() => {
                for &meeting_id in &meetings {
                    self.post_store_busy(meeting_id);
                }
                waits.wait_for_store(meetings);
                Err(failure)
            }
            Err(failure) => {
                waits.put_back(meetings);
                Err(failure)
            }
        }
    }

    /// Starts again the meetings runs left `queued` because another process
    /// held the store's write lock, or runs an owed launch resume instead.
    /// A meeting a run holds goes back to waiting for the store, and so do
    /// all when the store is busy again; either wakes the next retry
    /// ([`ModelWaits::until_store_retry`]). Another failure leaves them
    /// `queued` for the next launch. The meetings waiting for models are
    /// left waiting, except by an owed launch resume. Called on a timer
    /// while anything waits for the store. Needs a `tokio` runtime.
    pub fn resume_waiting_for_store(&self) -> Result<Vec<Uuid>> {
        if self.quitting() {
            return Ok(Vec::new());
        }
        let waits = &self.inner.dependencies.model_waits;
        let StoreWaits {
            meetings,
            unfinished_owed,
        } = waits.take_store_waits();
        // An owed launch resume takes every `queued` meeting.
        let mut for_models = BTreeSet::new();
        let among = if unfinished_owed {
            for_models = waits.resume();
            None
        } else if meetings.is_empty() {
            return Ok(Vec::new());
        } else {
            Some(&meetings)
        };
        match self.resume_among(among) {
            Ok((resumed, held)) => {
                waits.put_back(held.iter().copied().filter(|id| for_models.contains(id)));
                waits.wait_for_store(held.into_iter().filter(|id| meetings.contains(id)));
                Ok(resumed)
            }
            Err(failure) => {
                waits.put_back(for_models);
                if failure.is_busy() {
                    if unfinished_owed {
                        waits.owe_unfinished();
                    } else {
                        waits.wait_for_store(meetings);
                    }
                }
                // Not the lock: they stay `queued` for the next launch.
                Err(failure)
            }
        }
    }

    /// Before a meeting that waited for models starts again, posts its
    /// `decode` progress, so its card stops asking for the download.
    fn leaving_the_wait(&self, among: Option<&BTreeSet<Uuid>>, meeting_id: Uuid) {
        if among.is_some() {
            self.post(PipelineStage::Decode, 0, meeting_id);
        }
    }

    /// Both resumes' body over every `queued` or `processing` meeting, or
    /// those in `among`: the meetings started, and those a run holds.
    fn resume_among(&self, among: Option<&BTreeSet<Uuid>>) -> Result<(Vec<Uuid>, Vec<Uuid>)> {
        let meetings = attributing(
            PipelineStage::Decode,
            self.store()
                .meetings_in_states(&[MeetingStateKind::Queued, MeetingStateKind::Processing]),
        )?;
        let mut resumed = Vec::new();
        let mut busy = Vec::new();
        for meeting in meetings {
            if among.is_some_and(|among| !among.contains(&meeting.id)) {
                continue;
            }
            // Keeps a held meeting whose asset row is gone from being
            // marked failed under its run.
            if self.in_flight_set().meetings.contains(&meeting.id) {
                busy.push(meeting.id);
                continue;
            }
            let Some(mut asset) =
                attributing(PipelineStage::Decode, self.store().asset(meeting.id))?
            else {
                attributing(
                    PipelineStage::Decode,
                    self.store().set_state(
                        meeting.id,
                        MeetingState::Failed {
                            reason:
                                "Processing was interrupted and the recording's asset is missing"
                                    .to_owned(),
                        },
                        self.now(),
                    ),
                )?;
                continue;
            };
            // Only the launch's own resume: a run of this process that met
            // the busy store may still read `processing`.
            if among.is_none() && meeting.state == MeetingState::Processing {
                tracing::warn!(meeting_id = %meeting.id, "processing ended with the app; marked failed");
                // An earlier run's stamp must not let the sweep take it.
                if asset.expires_at.take().is_some() {
                    attributing(PipelineStage::Retention, self.store().save_asset(&asset))?;
                }
                let failed = MeetingState::Failed {
                    reason: CLOSED_WHILE_PROCESSING.to_owned(),
                };
                attributing(
                    PipelineStage::Decode,
                    self.store().set_state(meeting.id, failed, self.now()),
                )?;
                continue;
            }
            let Some(claim) = self.claim_start(meeting.id, asset.id) else {
                busy.push(meeting.id);
                continue;
            };
            self.leaving_the_wait(among, meeting.id);
            self.start(&asset, claim);
            resumed.push(meeting.id);
        }
        Ok((resumed, busy))
    }

    /// Launch recovery for the exports: re-exports every ready meeting with
    /// a delivery left `pending`, never attempted, or `failed` more than
    /// [`REDELIVER_AFTER`] ago.
    ///
    /// The meetings run one at a time in one background task, each claimed
    /// when its turn comes; one another operation holds, or no longer owed
    /// once claimed, is skipped. A failure is logged, not posted: the user
    /// did not ask for this export. Needs a `tokio` runtime.
    pub fn redeliver_unfinished(&self) -> Result<Vec<Uuid>> {
        if self.quitting() {
            return Ok(Vec::new());
        }
        let owed = attributing(
            PipelineStage::Deliver,
            self.store()
                .meetings_with_unfinished_deliveries(self.now(), REDELIVER_AFTER),
        )?;
        if !owed.is_empty() {
            let pipeline = self.clone();
            let meetings = owed.clone();
            self.spawn_tracked(Uuid::new_v4(), None, async move {
                for meeting_id in meetings {
                    if pipeline.quitting() {
                        break;
                    }
                    pipeline.redeliver_at_launch(meeting_id).await;
                }
                None
            });
        }
        Ok(owed)
    }

    /// One meeting of [`redeliver_unfinished`](Self::redeliver_unfinished):
    /// claimed and re-exported. A refused claim or a meeting no longer owed
    /// is skipped.
    async fn redeliver_at_launch(&self, meeting_id: Uuid) {
        // Refused: a meeting in flight exports at the end of its run.
        let Ok(admitted) = self.admit(meeting_id, PipelineStage::Deliver) else {
            return;
        };
        // The user may have exported it since, or a run left it not ready.
        let Some(meeting) = self.still_owed(meeting_id).await else {
            return;
        };
        if let Err(failure) = self.deliver_again_claimed(meeting, admitted).await {
            tracing::warn!(
                target: BACKGROUND_RUN_LOG,
                %meeting_id,
                stage = failure.stage.as_str(),
                "launch re-export failed"
            );
            tracing::debug!(target: BACKGROUND_RUN_LOG, %meeting_id, %failure, "launch re-export failure");
        }
    }

    /// Saves the meeting's `pending` deliveries as failed with `reason`,
    /// stamping an attempt when there is none so the row waits a day. The
    /// caller holds the claim; a store error is logged.
    fn fail_pending_deliveries(&self, meeting_id: Uuid, reason: &str) {
        let store = self.store();
        let rows = match store.deliveries(meeting_id) {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(target: BACKGROUND_RUN_LOG, %meeting_id, %error, "deliveries could not be read");
                return;
            }
        };
        for mut row in rows {
            if row.status != DeliveryStatus::Pending {
                continue;
            }
            row.status = DeliveryStatus::Failed(reason.to_owned());
            row.last_attempt_at.get_or_insert(self.now());
            if let Err(error) = store.save_delivery(&row) {
                tracing::warn!(target: BACKGROUND_RUN_LOG, %meeting_id, %error, "a delivery could not be saved failed");
            }
        }
    }

    /// Spawns `work` among the background runs under `key`, holding `claim`
    /// until it ends. The state lock spans the spawn and the insert, so the
    /// task cannot remove its entry before it exists. A refusal `work`
    /// hands back goes to [`set_aside`](Self::set_aside) after the claim is
    /// released and before the entry goes (rule 1 on [`ModelWaits`]).
    fn spawn_tracked(
        &self,
        key: Uuid,
        claim: Option<AssetClaim>,
        work: impl Future<Output = Option<Refused>> + Send + 'static,
    ) {
        let pipeline = self.clone();
        let mut state = self.state();
        let handle = tokio::spawn(async move {
            let mut running = Running {
                pipeline: pipeline.clone(),
                key,
                claim,
            };
            let refused = work.await;
            drop(running.claim.take());
            if let Some(refused) = refused {
                pipeline.set_aside(refused);
            }
            drop(running);
        });
        state.running.insert(key, handle);
    }

    /// Processes `asset` in the background under its [`AssetClaim`]; nothing
    /// starts once the pipeline quits.
    fn start(&self, asset: &AudioAsset, claim: AssetClaim) {
        let asset_id = asset.id;
        if self.quitting() {
            not_started(asset_id);
            return;
        }
        let pipeline = self.clone();
        self.spawn_tracked(asset_id, Some(claim), async move {
            if pipeline.quitting() {
                return not_started(asset_id);
            }
            let (result, refused) = pipeline.process_held(asset_id).await;
            if let Err(failure) = result {
                pipeline.log_run_failure(asset_id, &failure, refused.as_ref());
            }
            refused
        });
    }

    /// Logs a background run's `failure`: debug after the exit, info for a
    /// refusal, warn otherwise.
    fn log_run_failure(
        &self,
        asset_id: Uuid,
        failure: &PipelineFailure,
        refused: Option<&Refused>,
    ) {
        if self.quitting() {
            // The exit ended the job, which `process` did not persist.
            tracing::debug!(
                target: BACKGROUND_RUN_LOG,
                %asset_id,
                %failure,
                "processing stopped by the exit"
            );
            return;
        }
        if failure.is_models_missing() {
            tracing::info!(
                target: BACKGROUND_RUN_LOG,
                %asset_id,
                stage = failure.stage.as_str(),
                "processing waits for the models to be installed"
            );
            return;
        }
        if refused.is_some_and(|refused| matches!(refused.awaits, Awaits::Store)) {
            tracing::info!(
                target: BACKGROUND_RUN_LOG,
                %asset_id,
                stage = failure.stage.as_str(),
                "processing waits for another process to let go of the database"
            );
            return;
        }
        // The reason can name the audio file, so warn carries the stage only.
        tracing::warn!(
            target: BACKGROUND_RUN_LOG,
            %asset_id,
            stage = failure.stage.as_str(),
            "processing failed"
        );
        tracing::debug!(target: BACKGROUND_RUN_LOG, %asset_id, %failure, "processing failure");
    }

    /// Waits for every background task. A refused run is waited for until
    /// its meeting waits or starts again; a restart on another pipeline is
    /// that pipeline's to wait for.
    pub async fn wait_until_idle(&self) {
        loop {
            let handle = {
                let mut state = self.state();
                let Some(key) = state.running.keys().next().copied() else {
                    return;
                };
                state.running.remove(&key)
            };
            if let Some(handle) = handle {
                let _ = handle.await;
            }
        }
    }

    /// Loads the speech engine and the diarizer now. Concurrent calls are
    /// serialised across pipelines sharing the engine; a failed load is
    /// retried by the next call. Errors carry `decode` for the engine and
    /// `diarize` for the diarizer.
    pub async fn warm_up(&self) -> Result<()> {
        if !(self.inner.dependencies.models_installed)() {
            return Err(PipelineFailure::models_missing(PipelineStage::Decode));
        }
        let _guard = self.speech_engine().preparing().await;
        let dependencies = &self.inner.dependencies;
        self.models_gone(attributing(
            PipelineStage::Decode,
            dependencies.speech_engine.prepare().await,
        ))?;
        self.models_gone(attributing(
            PipelineStage::Diarize,
            dependencies.diarizer.prepare().await,
        ))?;
        Ok(())
    }

    /// `result`'s failure as the models-missing refusal when the models are
    /// no longer installed.
    fn models_gone<T>(&self, result: Result<T>) -> Result<T> {
        result.map_err(|failure| {
            if failure.is_models_missing() || (self.inner.dependencies.models_installed)() {
                failure
            } else {
                PipelineFailure::models_missing(failure.stage)
            }
        })
    }

    /// [`warm_up`](Self::warm_up) for `meeting_id`'s job: a diarizer that
    /// does not load does not fail the job, which then falls back to
    /// [`diarization_without_diarizer`](Self::diarization_without_diarizer).
    async fn warm_up_for_job(&self, meeting_id: Uuid) -> Result<()> {
        match self.warm_up().await {
            // The speech engine loaded before the diarizer failed.
            Err(failure) if failure.stage == PipelineStage::Diarize => {
                self.carry_on_after(&failure, meeting_id)
            }
            warmed => warmed,
        }
    }

    /// [`warm_up`](Self::warm_up) for the diarizer alone, for a speech
    /// engine that frees its models after each job: loading it early would
    /// keep it resident outside any claim.
    pub async fn warm_up_diarizer(&self) -> Result<()> {
        let _guard = self.speech_engine().preparing().await;
        self.models_gone(attributing(
            PipelineStage::Diarize,
            self.inner.dependencies.diarizer.prepare().await,
        ))
    }

    /// Runs every stage: `queued → processing → ready`, or `failed(reason)`
    /// with whatever was persisted so far; a panic fails the meeting too.
    /// Missing models or a busy store leave the meeting `queued` instead,
    /// waiting for a resume. A failing `diarize` or `match_speakers` keeps
    /// the transcript with the stored speakers or one unknown speaker. Once
    /// `persist` marked the meeting `ready` nothing downgrades it; a later
    /// failure is returned only.
    ///
    /// After a [quit](Self::quit), failures are not persisted and the
    /// meeting stays for the next launch. While a [`JobHold`] lives the run
    /// waits before it reads anything.
    pub async fn process(&self, asset_id: Uuid) -> Result<()> {
        let (result, refused) = self.process_held(asset_id).await;
        if let Some(refused) = refused {
            self.set_aside(refused);
        }
        result
    }

    /// [`process`](Self::process) up to the release of the meeting, plus
    /// what [`set_aside`](Self::set_aside) needs for a refused run.
    async fn process_held(&self, asset_id: Uuid) -> (Result<()>, Option<Refused>) {
        self.until_jobs_may_start().await;
        if self.quitting() {
            let quitting = PipelineFailure::new(PipelineStage::Decode, "the app is quitting");
            return (Err(quitting), None);
        }
        let found = required(
            PipelineStage::Decode,
            self.store()
                .reading(move |store| {
                    let Some(asset) = store.asset_by_id(asset_id)? else {
                        return Ok(None);
                    };
                    let meeting = store.meeting(asset.meeting_id)?;
                    Ok(Some((asset, meeting)))
                })
                .await,
            || format!("audio asset {asset_id} not found"),
        )
        .and_then(|(asset, meeting)| {
            let meeting = meeting.ok_or_else(|| {
                PipelineFailure::new(
                    PipelineStage::Decode,
                    format!("meeting {} not found", asset.meeting_id),
                )
            })?;
            Ok((asset, meeting))
        });
        let (asset, meeting) = match found {
            Ok(found) => found,
            Err(failure) => return (Err(failure), None),
        };
        let meeting_id = meeting.id;
        let mut refused = None;
        let result = self
            .exclusively(meeting_id, PipelineStage::Decode, async {
                let resumes_seen = self.inner.dependencies.model_waits.resumes();
                // So a panic never leaves the meeting `processing`. Boxed:
                // the stages' future is too large to move by value.
                let until_persist = unless_it_panics(
                    || self.stage_in_progress(meeting_id),
                    Box::pin(self.process_until_persist(&asset, meeting)),
                )
                .await;
                let persisted = match until_persist {
                    Ok(asset) => asset,
                    Err(failure) if self.quitting() => return Err(failure),
                    Err(failure) if failure.is_models_missing() => {
                        self.park(meeting_id);
                        refused = Some(Refused {
                            meeting_id,
                            awaits: Awaits::Models { resumes_seen },
                        });
                        return Err(failure);
                    }
                    Err(failure) => {
                        // A failure after `persist` leaves it ready, and a
                        // ready meeting is delivered now or never.
                        let ready = self
                            .store()
                            .reading(move |store| store.meeting(meeting_id))
                            .await
                            .ok()
                            .flatten()
                            .is_some_and(|stored| stored.state == MeetingState::Ready);
                        if ready {
                            self.deliver(meeting_id).await;
                            let _ = self
                                .stamp_deferred_retention(meeting_id, Stamp::Automatic)
                                .await;
                            return Err(failure);
                        }
                        if failure.is_busy() {
                            self.park_for_store(meeting_id);
                            refused = Some(Refused {
                                meeting_id,
                                awaits: Awaits::Store,
                            });
                            return Err(failure);
                        }
                        let (reason, now) = (failure.to_string(), self.now());
                        let _ = self
                            .store()
                            .writing(move |store| {
                                store.set_state(meeting_id, MeetingState::Failed { reason }, now)
                            })
                            .await;
                        return Err(failure);
                    }
                };
                self.deliver(meeting_id).await;
                self.retention(&persisted, Stamp::Automatic).await
            })
            .await;
        (result, refused)
    }

    /// Once nothing holds a refused run's meeting, records it as waiting
    /// for models or the store. After a quit a store wait is not recorded.
    fn set_aside(&self, refused: Refused) {
        match refused.awaits {
            Awaits::Models { resumes_seen } => {
                self.wait_for_models(refused.meeting_id, resumes_seen);
            }
            Awaits::Store if self.quitting() => {}
            Awaits::Store => self
                .inner
                .dependencies
                .model_waits
                .wait_for_store([refused.meeting_id]),
        }
    }

    /// Records a refused run's meeting as waiting for its models, or starts
    /// it again on the newest pipeline when a resume ran since
    /// (rules 1 and 3 on [`ModelWaits`]).
    fn wait_for_models(&self, meeting_id: Uuid, resumes_seen: u64) {
        let waits = &self.inner.dependencies.model_waits;
        if waits.wait_unless_resumed(meeting_id, resumes_seen) || self.quitting() {
            return;
        }
        tracing::info!(
            target: BACKGROUND_RUN_LOG,
            %meeting_id,
            "a resume ran during the refused run; processing again"
        );
        let newest = waits.newest().unwrap_or_else(|| self.clone());
        if let Err(failure) = newest.start_waiting(BTreeSet::from([meeting_id])) {
            tracing::warn!(
                target: BACKGROUND_RUN_LOG,
                %meeting_id,
                stage = failure.stage.as_str(),
                "the refused meeting could not be started again"
            );
        }
    }

    /// Leaves a run refused for missing models `queued` and posts
    /// [`MeetingEvent::ModelsMissing`]. Best effort.
    fn park(&self, meeting_id: Uuid) {
        let _ = self
            .store()
            .set_state(meeting_id, MeetingState::Queued, self.now());
        self.inner
            .dependencies
            .events
            .post(MeetingEvent::ModelsMissing { meeting_id });
    }

    /// Leaves a run that met the busy store `queued` and posts
    /// [`MeetingEvent::StoreBusy`]. Only a meeting the run marked
    /// `processing` is written back, so the park does not wait on the lock
    /// again. Best effort.
    fn park_for_store(&self, meeting_id: Uuid) {
        let processing = self
            .store()
            .meeting(meeting_id)
            .ok()
            .flatten()
            .is_some_and(|stored| stored.state == MeetingState::Processing);
        if processing {
            let _ = self
                .store()
                .set_state(meeting_id, MeetingState::Queued, self.now());
        }
        self.post_store_busy(meeting_id);
    }

    /// Posts [`MeetingEvent::StoreBusy`] for `meeting_id`.
    fn post_store_busy(&self, meeting_id: Uuid) {
        self.inner
            .dependencies
            .events
            .post(MeetingEvent::StoreBusy { meeting_id });
    }

    async fn process_until_persist(
        &self,
        asset: &AudioAsset,
        meeting: Meeting,
    ) -> Result<AudioAsset> {
        let claim = self.speech_engine().claim();
        let transcribed: Result<_> = async {
            let turn = self
                .in_process_turn(meeting.id, PipelineStage::Decode)
                .await?;
            self.warm_up_for_job(meeting.id).await?;
            drop(turn);
            let (meeting_id, now) = (meeting.id, self.now());
            let settings = attributing(
                PipelineStage::Decode,
                self.store()
                    .writing(move |store| {
                        store.set_state(meeting_id, MeetingState::Processing, now)?;
                        store.settings()
                    })
                    .await,
            )?;
            self.begin_run(
                &meeting,
                &asset.lanes,
                None,
                PipelineStage::ALL.to_vec(),
                &settings,
            )
            .await?;
            let mut current = meeting;
            current.state = MeetingState::Processing;
            let (transcription, last) = self.decode_and_transcribe(asset, current.id).await?;
            Ok((current, settings, transcription, last))
        }
        .await;
        self.finish_speech(claim).await;
        let (mut current, settings, transcription, last) = transcribed?;
        // A recording that announced no length gets the decoded one.
        if current.duration <= 0.0 {
            current.duration = transcription.seconds;
        }
        // The last decoded lane goes to `diarize` and is dropped there; a
        // handed buffer of the wrong lane is dropped before `diarize` decodes
        // the right one, so one buffer is alive at a time.
        let lane =
            diarized_lane_after_transcription(current.source, &asset.lanes, &transcription.lanes);
        let handed = last.filter(|decoded| Some(decoded.lane) == lane);
        let mut diarized = match self.diarize(asset, &current, lane, handed).await {
            Ok(diarized) => diarized,
            Err(failure) => {
                self.carry_on_after(&failure, current.id)?;
                self.diarization_without_diarizer(current.id, lane).await?
            }
        };
        current.language = transcription.language;
        match self
            .match_speakers(&diarized.speakers, current.id, &settings)
            .await
        {
            Ok(matched) => diarized.speakers = matched,
            // The speakers stay as diarize or its fallback made them.
            Err(failure) => self.carry_on_after(&failure, current.id)?,
        }
        attributing(
            PipelineStage::Merge,
            sample_clips::reach(self.dependencies().clip_probe.as_ref(), ClipStep::Merging),
        )?;
        let merged = self.merge(&current, transcription.lanes, diarized).await?;
        self.sweep_sample_clips(asset, &merged).await;
        // Starts from the cleanup pass's usage; summarize adds its own.
        current.llm_usage = self
            .cleanup(&current, merged.segments, &merged.speakers)
            .await?;
        // What the user changed since the run started wins: the template,
        // a typed title or attached event, and the speakers they named or
        // merged.
        let meeting_id = current.id;
        let (stored, speakers, segments) = required(
            PipelineStage::Summarize,
            self.store()
                .reading(move |store| {
                    let Some(stored) = store.meeting(meeting_id)? else {
                        return Ok(None);
                    };
                    Ok(Some((
                        stored,
                        store.speakers(meeting_id)?,
                        store.segments(meeting_id)?,
                    )))
                })
                .await,
            || format!("meeting {meeting_id} not found"),
        )?;
        current.template_id = stored.template_id;
        current.title = stored.title;
        current.title_origin = stored.title_origin;
        current.calendar_event_id = stored.calendar_event_id;
        let current = self.summarize(current, &segments, &speakers).await?;
        self.persist(&current, asset).await
    }

    /// A failure that costs only the speaker labels (`diarize`,
    /// `match_speakers`, the diarizer's warm-up) is logged and the run goes
    /// on. Returned instead: missing models and a busy store, so the meeting
    /// waits rather than finishing without speakers, and any failure once
    /// the pipeline quits.
    fn carry_on_after(&self, failure: &PipelineFailure, meeting_id: Uuid) -> Result<()> {
        if failure.is_models_missing() || failure.is_busy() || self.quitting() {
            return Err(failure.clone());
        }
        tracing::warn!(
            target: BACKGROUND_RUN_LOG,
            %meeting_id,
            stage = failure.stage.as_str(),
            "stage failed; the run goes on and keeps the transcript"
        );
        tracing::debug!(target: BACKGROUND_RUN_LOG, %meeting_id, %failure, "stage failure");
        Ok(())
    }

    /// Removes the clip files of the meeting's speakers that no speaker row
    /// names from the meeting's own `speakers/` folder, once the merge
    /// committed ([`sample_clips::sweep_after_merge`]). Uncommitted clips are
    /// safe: only this meeting's speakers' files go, the run holds the
    /// meeting until the sweep returns, and only
    /// [`RecordingLayout::own_folder`] is swept. Runs off the async workers;
    /// a failure is logged.
    async fn sweep_sample_clips(&self, asset: &AudioAsset, merged: &Merged) {
        let Some(layout) = RecordingLayout::own_folder(asset) else {
            return;
        };
        let probe = self.dependencies().clip_probe.clone();
        if sample_clips::reach(probe.as_ref(), ClipStep::Sweeping).is_err() {
            return;
        }
        let (replaced, committed) = (merged.replaced.clone(), merged.speakers.clone());
        let swept = self
            .store()
            .reading(move |store| {
                sample_clips::sweep_after_merge(
                    store,
                    &layout.speakers_directory(),
                    &replaced,
                    &committed,
                    probe.as_ref(),
                )
            })
            .await;
        if let Err(error) = swept {
            tracing::warn!(
                target: BACKGROUND_RUN_LOG,
                meeting_id = %asset.meeting_id,
                %error,
                "the sample clips no speaker row names were not swept"
            );
        }
    }

    /// The `diarize` fallback when the diarizer fails or did not load. A
    /// meeting's stored speakers are kept as stored and cover the spans of
    /// the segments they own on `lane` ([`LaneMerger::cluster_covering`]).
    /// A segment no stored span reaches goes to the room speaker
    /// ([`Diarization::fallback`]).
    async fn diarization_without_diarizer(
        &self,
        meeting_id: Uuid,
        lane: Option<AudioLane>,
    ) -> Result<Diarization> {
        let Some(lane) = lane else {
            return Ok(Diarization::NONE);
        };
        let me = LaneMerger::me_speaker_id(meeting_id);
        let (speakers, segments) = attributing(
            PipelineStage::Diarize,
            self.store()
                .reading(move |store| {
                    Ok((store.speakers(meeting_id)?, store.segments(meeting_id)?))
                })
                .await,
        )?;
        let speakers: Vec<Speaker> = speakers
            .into_iter()
            .filter(|speaker| speaker.id != me)
            .collect();
        let owners: Vec<ClusterSpeaker> = speakers
            .iter()
            .map(|speaker| ClusterSpeaker {
                speaker_id: speaker.id,
                ranges: segments
                    .iter()
                    .filter(|segment| {
                        segment.lane == lane && segment.speaker_id == Some(speaker.id)
                    })
                    .map(|segment| TimeRange {
                        lower: segment.start,
                        upper: segment.end,
                    })
                    .collect(),
            })
            .filter(|owner| !owner.ranges.is_empty())
            .collect();
        let room = room_speaker_id(meeting_id);
        let room_of_the_other_lane = speakers.iter().any(|speaker| speaker.id == room)
            && !owners.iter().any(|owner| owner.speaker_id == room);
        Ok(Diarization {
            speakers,
            cluster_speakers: owners,
            lane: Some(lane),
            fallback: (!room_of_the_other_lane).then_some(room),
        })
    }

    /// Ends `claim` and releases the speech engine when no other job on any
    /// pipeline over it holds a claim. The count is checked again under
    /// `preparing`, so a job claiming meanwhile keeps it loaded or warms it
    /// up after the release. A failed release is logged.
    async fn finish_speech(&self, claim: SpeechClaim) {
        drop(claim);
        if *self.speech_engine().claim_count() > 0 {
            return;
        }
        let _guard = self.speech_engine().preparing().await;
        if *self.speech_engine().claim_count() > 0 {
            return;
        }
        if let Err(error) = self.speech_engine().release().await {
            tracing::warn!(target: BACKGROUND_RUN_LOG, "the speech engine was not released");
            tracing::debug!(target: BACKGROUND_RUN_LOG, %error, "speech engine release failure");
        }
    }

    /// Summarize again with another template, then deliver. A failure
    /// leaves the meeting as it was. Without a summarizer the call fails.
    pub async fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> Result<()> {
        self.claim_rerun_summary(meeting_id, template_id)?.await
    }

    /// The synchronous half of [`rerun_summary`](Self::rerun_summary): a
    /// refusal reaches the caller before anything runs; the returned
    /// [`Operation`] does the work, posting a failure as
    /// [`MeetingEvent::OperationFailed`].
    pub fn claim_rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> Result<Operation> {
        let meeting = required(
            PipelineStage::Summarize,
            self.store().meeting(meeting_id),
            || format!("meeting {meeting_id} not found"),
        )?;
        if self.inner.dependencies.summarizer.is_none() {
            return Err(PipelineFailure::new(
                PipelineStage::Summarize,
                "no LLM endpoint is configured",
            ));
        }
        let admitted = self.admit(meeting_id, PipelineStage::Summarize)?;
        let pipeline = self.clone();
        let template_id = template_id.to_owned();
        Ok(Box::pin(async move {
            pipeline.until_jobs_may_start().await;
            let result = unless_it_panics(
                || PipelineStage::Summarize,
                pipeline.resummarize(meeting, &template_id),
            )
            .await;
            drop(admitted);
            pipeline.reporting(meeting_id, MeetingOperation::SummaryRerun, result)
        }))
    }

    async fn resummarize(&self, meeting: Meeting, template_id: &str) -> Result<()> {
        let meeting_id = meeting.id;
        let (export, settings) = attributing(
            PipelineStage::Summarize,
            self.store()
                .reading(move |store| Ok((store.export(meeting_id)?, store.settings()?)))
                .await,
        )?;
        let lanes = export
            .audio
            .as_ref()
            .map(|a| a.lanes.clone())
            .unwrap_or_default();
        self.begin_run(
            &meeting,
            &lanes,
            Some(ProcessingEstimator::token_count(&export.segments)),
            vec![PipelineStage::Summarize, PipelineStage::Deliver],
            &settings,
        )
        .await?;
        let mut current = meeting;
        current.template_id = template_id.to_owned();
        current.state = MeetingState::Ready;
        self.summarize(current, &export.segments, &export.speakers)
            .await?;
        // An exit before `deliver` leaves `pending` rows for the next launch.
        let dispatcher = self.inner.dependencies.dispatcher.clone();
        let _ = self
            .store()
            .writing(move |_| {
                dispatcher.mark_pending(meeting_id);
                Ok(())
            })
            .await;
        self.deliver(meeting_id).await;
        self.stamp_deferred_retention(meeting_id, Stamp::Automatic)
            .await
    }

    /// Deliver only: the one re-export entry point. Stamps an asset whose
    /// expiry a failed delivery deferred once this one succeeds.
    pub async fn redeliver(&self, meeting_id: Uuid) -> Result<()> {
        self.claim_redeliver(meeting_id)?.await
    }

    /// The synchronous half of [`redeliver`](Self::redeliver), as
    /// [`claim_rerun_summary`](Self::claim_rerun_summary) is of the re-run.
    pub fn claim_redeliver(&self, meeting_id: Uuid) -> Result<Operation> {
        let operation = self.claim_deliver_again(meeting_id)?;
        let pipeline = self.clone();
        Ok(Box::pin(async move {
            let result = operation.await;
            pipeline.reporting(meeting_id, MeetingOperation::Reexport, result)
        }))
    }

    /// [`claim_redeliver`](Self::claim_redeliver) without the
    /// `OperationFailed` event. A row left `pending` is saved failed before
    /// the claim is released.
    fn claim_deliver_again(&self, meeting_id: Uuid) -> Result<Operation> {
        let meeting = required(
            PipelineStage::Deliver,
            self.store().meeting(meeting_id),
            || format!("meeting {meeting_id} not found"),
        )?;
        let admitted = self.admit(meeting_id, PipelineStage::Deliver)?;
        Ok(self.deliver_again_claimed(meeting, admitted))
    }

    /// The meeting, when it is still `ready` with a delivery `pending` or
    /// failed. A store error reads as not owed.
    async fn still_owed(&self, meeting_id: Uuid) -> Option<Meeting> {
        self.store()
            .reading(move |store| {
                let Some(meeting) = store.meeting(meeting_id)? else {
                    return Ok(None);
                };
                let owed = meeting.state == MeetingState::Ready
                    && store.deliveries(meeting_id).is_ok_and(|rows| {
                        rows.iter().any(|row| {
                            matches!(
                                row.status,
                                DeliveryStatus::Pending | DeliveryStatus::Failed(_)
                            )
                        })
                    });
                Ok(owed.then_some(meeting))
            })
            .await
            .ok()
            .flatten()
    }

    /// [`claim_deliver_again`](Self::claim_deliver_again) once `admitted`
    /// holds the meeting.
    fn deliver_again_claimed(&self, meeting: Meeting, admitted: Admitted) -> Operation {
        let meeting_id = meeting.id;
        let pipeline = self.clone();
        Box::pin(async move {
            pipeline.until_jobs_may_start().await;
            let result =
                unless_it_panics(|| PipelineStage::Deliver, pipeline.deliver_again(&meeting)).await;
            // A row still `pending` here was left by a panic or a stopped
            // dispatcher: saved failed while the claim is held.
            let reason = result
                .as_ref()
                .err()
                .map_or(EXPORT_INTERRUPTED, |failure| failure.reason.as_str())
                .to_owned();
            // On the blocking pool, through the pipeline's own handle.
            let store = pipeline.store().clone();
            let _ = store
                .writing(move |_| {
                    pipeline.fail_pending_deliveries(meeting_id, &reason);
                    Ok(())
                })
                .await;
            drop(admitted);
            result
        })
    }

    async fn deliver_again(&self, meeting: &Meeting) -> Result<()> {
        let meeting_id = meeting.id;
        let (settings, asset) = attributing(
            PipelineStage::Deliver,
            self.store()
                .reading(move |store| Ok((store.settings()?, store.asset(meeting_id)?)))
                .await,
        )?;
        let lanes = asset.map(|a| a.lanes).unwrap_or_default();
        self.begin_run(
            meeting,
            &lanes,
            None,
            vec![PipelineStage::Deliver],
            &settings,
        )
        .await?;
        self.deliver(meeting_id).await;
        self.stamp_deferred_retention(meeting_id, Stamp::Automatic)
            .await
    }

    /// Posts `OperationFailed` for a failed `operation`.
    fn reporting(
        &self,
        meeting_id: Uuid,
        operation: MeetingOperation,
        result: Result<()>,
    ) -> Result<()> {
        if let Err(failure) = &result {
            self.inner
                .dependencies
                .events
                .post(MeetingEvent::OperationFailed {
                    meeting_id,
                    operation,
                    stage: failure.stage,
                    failure: failure.to_string(),
                });
        }
        result
    }

    /// The per-meeting keep: `rule` replaces the asset's retention and
    /// clears its stamp durably, then the deferred-case rules may stamp it
    /// again. Safe while processing: the stages reread the row.
    pub async fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> Result<()> {
        required(
            PipelineStage::Retention,
            self.store()
                .writing(move |store| {
                    let Some(mut asset) = store.asset(meeting_id)? else {
                        return Ok(None);
                    };
                    asset.retention = rule;
                    asset.expires_at = None;
                    store.save_asset_durably(&asset).map(Some)
                })
                .await,
            || format!("meeting {meeting_id} has no audio asset"),
        )?;
        self.stamp_deferred_retention(meeting_id, Stamp::Chosen)
            .await
    }

    // Stage plumbing

    /// Starts the meeting's run over `stages`, estimated from the learned
    /// rates.
    async fn begin_run(
        &self,
        meeting: &Meeting,
        lanes: &[AudioLane],
        tokens: Option<i64>,
        stages: Vec<PipelineStage>,
        settings: &Settings,
    ) -> Result<()> {
        let stage = *stages.first().unwrap_or(&PipelineStage::Decode);
        let rows = attributing(stage, self.store().reading(Store::stage_rates).await)?;
        let estimator = ProcessingEstimator::new(
            meeting.duration,
            lanes.to_vec(),
            tokens,
            self.inner.dependencies.speech_engine.id(),
            &ProcessingEstimator::llm_model_key(settings),
            StageRates::from_rows(&rows),
        );
        let run = ProcessingRun::new(estimator, stages, self.inner.dependencies.clock.seconds());
        self.state().runs.insert(meeting.id, run);
        Ok(())
    }

    /// Returns once no [`JobHold`] lives.
    async fn until_jobs_may_start(&self) {
        self.inner.dependencies.in_flight.until_released().await;
    }

    /// Marks `meeting_id` in flight for the duration of `body`; a second
    /// operation on the same meeting fails for `stage`.
    async fn exclusively<T>(
        &self,
        meeting_id: Uuid,
        stage: PipelineStage,
        body: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let _admitted = self.admit(meeting_id, stage)?;
        body.await
    }

    /// Marks `meeting_id` in flight until the mark drops, however the
    /// operation ends, or fails for `stage` when another holds it.
    fn admit(&self, meeting_id: Uuid, stage: PipelineStage) -> Result<Admitted> {
        let mut in_flight = self.in_flight_set();
        if !in_flight.meetings.insert(meeting_id) {
            return Err(PipelineFailure::new(
                stage,
                format!("meeting {meeting_id} is already being processed"),
            ));
        }
        in_flight.admissions += 1;
        Ok(Admitted {
            pipeline: self.clone(),
            meeting_id,
        })
    }

    /// The stage `meeting_id`'s run last posted; `decode` before the first.
    fn stage_in_progress(&self, meeting_id: Uuid) -> PipelineStage {
        self.state()
            .runs
            .get(&meeting_id)
            .and_then(ProcessingRun::last_stage)
            .unwrap_or(PipelineStage::Decode)
    }

    /// Replaces the run's guessed token count with the transcript's.
    fn revise(&self, tokens: i64, meeting_id: Uuid) {
        if let Some(run) = self.state().runs.get_mut(&meeting_id) {
            run.estimator.tokens = tokens;
        }
    }

    /// Posts `progress` for `stage` (lane `lane` inside transcribe) on the
    /// meeting's run.
    fn post(&self, stage: PipelineStage, lane: usize, meeting_id: Uuid) {
        let elapsed_now = self.inner.dependencies.clock.seconds();
        let progress = {
            let mut guard = self.state();
            if let Some(run) = guard.runs.get_mut(&meeting_id) {
                run.progress(stage, lane, elapsed_now - run.started_at)
            } else {
                let mut run = ProcessingRun::new(
                    ProcessingEstimator::new(
                        0.0,
                        Vec::new(),
                        None,
                        self.inner.dependencies.speech_engine.id(),
                        StageRates::NO_MODEL,
                        StageRates::seeds(),
                    ),
                    PipelineStage::ALL.to_vec(),
                    elapsed_now,
                );
                run.progress(stage, lane, 0.0)
            }
        };
        self.inner.dependencies.events.post(MeetingEvent::Progress {
            meeting_id,
            progress,
        });
    }

    /// Posts `progress` for `stage`, runs `body` attributing its errors to
    /// the stage, and records its duration as a rate sample when it
    /// succeeded with the meeting alone in flight throughout.
    async fn run<T, E: fmt::Display + 'static>(
        &self,
        stage: PipelineStage,
        lane: usize,
        meeting_id: Uuid,
        body: impl Future<Output = std::result::Result<T, E>>,
    ) -> Result<T> {
        self.post(stage, lane, meeting_id);
        let (alone, admissions_before) = {
            let in_flight = self.in_flight_set();
            (in_flight.meetings.len() == 1, in_flight.admissions)
        };
        let clock = &self.inner.dependencies.clock;
        let started = clock.seconds();
        let value = attributing(stage, body.await);
        let value = if matches!(stage, PipelineStage::Transcribe | PipelineStage::Diarize) {
            self.models_gone(value)?
        } else {
            value?
        };
        let seconds = clock.seconds() - started;
        let sample = {
            let alone = alone && self.in_flight_set().admissions == admissions_before;
            let mut guard = self.state();
            let now = self.now();
            guard
                .runs
                .get_mut(&meeting_id)
                .and_then(|run| run.measure(stage, lane, seconds, alone, now))
        };
        if let Some(sample) = sample {
            // The rates are a convenience; this never fails a run.
            if let Err(error) = self.record(sample).await {
                tracing::debug!(%error, "stage rate not recorded");
            }
        }
        Ok(value)
    }

    /// Folds one measurement into its row: the first sample replaces the
    /// seed, later ones move the average.
    async fn record(&self, sample: StageSample) -> std::result::Result<(), StoreError> {
        self.store()
            .writing(move |store| {
                store
                    .update_stage_rate(sample.stage, &sample.key, sample.recorded_at, |current| {
                        let current = current
                            .unwrap_or_else(|| StageRates::seeds().rate(sample.stage, &sample.key));
                        absorbing(current, sample.seconds_per_unit)
                    })
                    .map(drop)
            })
            .await
    }

    // Stages

    /// Per lane: decode, then transcribe with the previous lane's dominant
    /// language as the hint; only the last buffer survives, for `diarize`.
    /// What any lane's decoder replaced by silence is logged, and a damaged
    /// recording under a finite rule is kept forever, so the sweep never
    /// takes it; the user can still delete it.
    async fn decode_and_transcribe(
        &self,
        asset: &AudioAsset,
        meeting_id: Uuid,
    ) -> Result<(Transcription, Option<DecodedLane>)> {
        let decoder = &self.inner.dependencies.decoder;
        let engine = &self.inner.dependencies.speech_engine;
        let mut lanes: BTreeMap<AudioLane, Vec<RawSegment>> = BTreeMap::new();
        let mut hint: Option<LanguageTag> = None;
        let mut last: Option<DecodedLane> = None;
        let mut damage = AudioDamage::default();
        let mut seconds: f64 = 0.0;
        for (index, lane) in ordered_lanes(&asset.lanes).into_iter().enumerate() {
            // Release the previous lane before decoding the next.
            drop(last.take());
            let decode = decoder.decode(asset, lane);
            let buffer = if index == 0 {
                self.run(PipelineStage::Decode, 0, meeting_id, decode)
                    .await?
            } else {
                attributing(PipelineStage::Decode, decode.await)?
            };
            damage = damage.max(buffer.damage);
            seconds = seconds.max(buffer.duration());
            let lane_hint = hint.clone();
            let turn = self
                .in_process_turn(meeting_id, PipelineStage::Transcribe)
                .await?;
            let transcription = engine.transcribe(&buffer, lane_hint.as_ref());
            let segments = self
                .run(PipelineStage::Transcribe, index, meeting_id, transcription)
                .await?;
            drop(turn);
            hint = elect_language(&segments).or(hint);
            lanes.insert(lane, segments);
            last = Some(DecodedLane { lane, buffer });
        }
        if !damage.is_none() {
            tracing::warn!(
                meeting = %meeting_id,
                parts = damage.parts,
                seconds = damage.seconds,
                "parts of the recording did not decode and were replaced by silence"
            );
            if asset.retention != AudioRetention::KeepForever {
                let asset_id = asset.id;
                attributing(
                    PipelineStage::Decode,
                    self.store()
                        .writing(move |store| {
                            let Some(mut kept) = store.asset_by_id(asset_id)? else {
                                return Ok(());
                            };
                            kept.retention = AudioRetention::KeepForever;
                            kept.expires_at = None;
                            store.save_asset_durably(&kept)
                        })
                        .await,
                )?;
            }
        }
        let language = elect_language(lanes.values().flatten());
        Ok((
            Transcription {
                lanes,
                language,
                seconds,
            },
            last,
        ))
    }

    /// Runs the diarizer over `lane` and turns every cluster into a
    /// `Speaker` with a deterministic id, its clip written as 16 kHz WAV
    /// under the meeting's own `speakers/` folder ([`sample_clips`]); a
    /// master outside its own folder gets no clips. A mic lane with fewer
    /// than two voices is the user alone: no clusters, no lane. `handed` is
    /// reused when it carries `lane`.
    async fn diarize(
        &self,
        asset: &AudioAsset,
        meeting: &Meeting,
        lane: Option<AudioLane>,
        handed: Option<DecodedLane>,
    ) -> Result<Diarization> {
        let decoder = &self.inner.dependencies.decoder;
        let diarizer = &self.inner.dependencies.diarizer;
        let meeting_id = meeting.id;
        self.run(PipelineStage::Diarize, 0, meeting_id, async {
            let Some(lane) = lane else {
                return Ok::<_, PipelineFailure>(Diarization::NONE);
            };
            let buffer = match handed {
                Some(handed_lane) if handed_lane.lane == lane => handed_lane.buffer,
                _ => attributing(PipelineStage::Diarize, decoder.decode(asset, lane).await)?,
            };
            let result = attributing(PipelineStage::Diarize, diarizer.diarize(&buffer).await)?;
            if lane == AudioLane::Mic && result.clusters.len() < 2 {
                return Ok(Diarization::NONE);
            }
            let mut labels = BTreeSet::new();
            for cluster in &result.clusters {
                if !labels.insert(cluster.label.clone()) {
                    return Err(PipelineFailure::new(
                        PipelineStage::Diarize,
                        format!("diarizer returned two clusters labelled {}", cluster.label),
                    ));
                }
            }
            let layout = RecordingLayout::own_folder(asset);
            let run_id = Uuid::new_v4();
            let mut speakers = Vec::new();
            let mut cluster_speakers = Vec::new();
            let mut clips = Vec::new();
            for cluster in result.clusters {
                let id = derived_uuid(meeting_id, &format!("speaker-{}", cluster.label));
                let mut clip_url = None;
                if let (Some(range), Some(layout)) = (cluster.sample_clip_range, layout.as_ref()) {
                    let capped = TimeRange {
                        lower: range.lower,
                        upper: range.upper.min(range.lower + SAMPLE_CLIP_SECONDS),
                    };
                    let clip = buffer.slice(capped);
                    if !clip.is_empty() {
                        let path = layout.run_sample_clip(id, run_id);
                        clip_url = Some(file_url(&path, false));
                        clips.push((path, clip));
                    }
                }
                speakers.push(Speaker {
                    id,
                    meeting_id,
                    cluster_label: cluster.label,
                    assignment: SpeakerAssignment::Unknown,
                    embedding: cluster.embedding,
                    sample_clip_range: cluster.sample_clip_range,
                    sample_clip_url: clip_url,
                    cluster_confidence: cluster.cluster_confidence,
                });
                cluster_speakers.push(ClusterSpeaker {
                    speaker_id: id,
                    ranges: cluster.ranges,
                });
            }
            if !clips.is_empty()
                && let Some(layout) = layout
            {
                // Off the async workers: each clip is synced. Clips a dropped
                // run leaves go with the meeting's next sweep.
                let probe = self.dependencies().clip_probe.clone();
                let written =
                    off_the_workers(move || sample_clips::write(&layout, &clips, probe.as_ref()))
                        .await
                        .flatten();
                attributing(PipelineStage::Diarize, written)?;
            }
            Ok(Diarization {
                speakers,
                cluster_speakers,
                lane: Some(lane),
                fallback: None,
            })
        })
        .await
    }

    /// `SpeakerMemory::match_voice` for every speaker with an embedding: a
    /// hit becomes `suggested`, a miss stays `unknown`.
    async fn match_speakers(
        &self,
        speakers: &[Speaker],
        meeting_id: Uuid,
        settings: &Settings,
    ) -> Result<Vec<Speaker>> {
        let memory = &self.inner.dependencies.speaker_memory;
        let threshold = settings.speaker_match_threshold;
        self.run(PipelineStage::MatchSpeakers, 0, meeting_id, async {
            let mut matched = Vec::with_capacity(speakers.len());
            for speaker in speakers {
                let mut speaker = speaker.clone();
                speaker.assignment = SpeakerAssignment::Unknown;
                if let Some(embedding) = &speaker.embedding
                    && let Some(found) = memory
                        .match_voice(embedding, threshold, DEFAULT_MATCH_MARGIN)
                        .await?
                {
                    speaker.assignment = SpeakerAssignment::Suggested {
                        person_id: found.person.id,
                        similarity: found.similarity,
                    };
                }
                matched.push(speaker);
            }
            Ok::<_, steno_core::protocols::BoxError>(matched)
        })
        .await
    }

    /// Merges the lanes into one ordered transcript and persists it with
    /// the speakers and the elected language in one transaction. A `mic`
    /// lane that is "me" gets its participant and speaker first; when the
    /// mic lane is the room, a "me" participant an earlier run created is
    /// removed.
    async fn merge(
        &self,
        meeting: &Meeting,
        lanes: BTreeMap<AudioLane, Vec<RawSegment>>,
        diarization: Diarization,
    ) -> Result<Merged> {
        let now = self.now();
        let meeting_id = meeting.id;
        let mut updated = meeting.clone();
        let merged = self.store().writing(move |store| {
            let mut all_speakers = diarization.speakers;
            let mut me_speaker_id = None;
            let mic_is_room = diarization.lane == Some(AudioLane::Mic);
            if mic_is_room {
                store.delete_participant(LaneMerger::me_participant_id(meeting_id))?;
            } else if lanes.contains_key(&AudioLane::Mic) {
                let me = ensure_me_participant(store, meeting_id)?;
                let me_speaker = LaneMerger::me_speaker(meeting_id, me.person_id);
                me_speaker_id = Some(me_speaker.id);
                all_speakers.push(me_speaker);
            }
            let segments = LaneMerger::merge(
                meeting_id,
                &lanes,
                &diarization.cluster_speakers,
                me_speaker_id,
                diarization.lane,
                diarization.fallback,
            );
            // The fallback's room row is written once, when it holds a segment.
            if let Some(room) = diarization.fallback
                && segments
                    .iter()
                    .any(|segment| segment.speaker_id == Some(room))
                && !all_speakers.iter().any(|speaker| speaker.id == room)
            {
                let row = Diarization::room_speaker(meeting_id, &all_speakers);
                all_speakers.push(row);
            }
            updated.updated_at = now;
            let replaced = store.replace_transcript(&updated, &segments, &all_speakers)?;
            Ok(Merged {
                segments,
                speakers: all_speakers,
                replaced,
            })
        });
        self.run(PipelineStage::Merge, 0, meeting_id, merged).await
    }

    /// Runs the `TranscriptCleaner` and persists the cleaned `text` by
    /// segment id; `raw_text`, ids, order and count must come back intact.
    /// The speakers are not rewritten. Returns the pass's usage; without a
    /// cleaner it writes nothing.
    async fn cleanup(
        &self,
        meeting: &Meeting,
        segments: Vec<TranscriptSegment>,
        speakers: &[Speaker],
    ) -> Result<Option<LlmUsage>> {
        self.revise(ProcessingEstimator::token_count(&segments), meeting.id);
        let store = self.store();
        let cleaner = self.inner.dependencies.cleaner.clone();
        let now = self.now();
        self.run(PipelineStage::Cleanup, 0, meeting.id, async {
            let Some(cleaner) = cleaner else {
                return Ok::<_, PipelineFailure>(None);
            };
            let meeting_id = meeting.id;
            let (participants, people) = attributing(
                PipelineStage::Cleanup,
                store
                    .reading(move |store| Ok((store.participants(meeting_id)?, store.persons()?)))
                    .await,
            )?;
            let output = attributing(
                PipelineStage::Cleanup,
                cleaner
                    .clean(&CleanupInput {
                        segments: segments.clone(),
                        language: meeting.language.clone(),
                        participants,
                        speakers: speakers.to_vec(),
                        known_people: people,
                    })
                    .await,
            )?;
            if output.segments.len() != segments.len() {
                return Err(PipelineFailure::new(
                    PipelineStage::Cleanup,
                    format!(
                        "cleaner returned {} segments for {}",
                        output.segments.len(),
                        segments.len()
                    ),
                ));
            }
            let mut corrected = Vec::with_capacity(segments.len());
            for (original, candidate) in segments.into_iter().zip(output.segments) {
                if candidate.id != original.id {
                    return Err(PipelineFailure::new(
                        PipelineStage::Cleanup,
                        format!("cleaner reordered segment {}", original.id),
                    ));
                }
                let mut segment = original;
                segment.text = candidate.text;
                corrected.push(segment);
            }
            let mut updated = meeting.clone();
            updated.updated_at = now;
            attributing(
                PipelineStage::Cleanup,
                store
                    .writing(move |store| store.update_segment_texts(&updated, &corrected))
                    .await,
            )?;
            Ok(Some(output.usage))
        })
        .await
    }

    /// Runs the `MeetingSummarizer` for `meeting.template_id` and persists
    /// summary, tasks, decisions and name suggestions with the title,
    /// language and usage in one transaction. A calendar or typed title
    /// stays. Without a summarizer only the processing columns are written.
    async fn summarize(
        &self,
        meeting: Meeting,
        segments: &[TranscriptSegment],
        speakers: &[Speaker],
    ) -> Result<Meeting> {
        let store = self.store();
        let summarizer = self.inner.dependencies.summarizer.clone();
        let now = self.now();
        let meeting_id = meeting.id;
        self.run(PipelineStage::Summarize, 0, meeting_id, async {
            let mut updated = meeting.clone();
            let Some(summarizer) = summarizer else {
                updated.updated_at = now;
                return attributing(
                    PipelineStage::Summarize,
                    store
                        .writing(move |store| {
                            store.save_processing_results(&updated)?;
                            Ok(updated)
                        })
                        .await,
                );
            };
            let template =
                SummaryTemplate::bundled_with_id(&meeting.template_id).ok_or_else(|| {
                    PipelineFailure::new(
                        PipelineStage::Summarize,
                        format!("unknown summary template {}", meeting.template_id),
                    )
                })?;
            let (participants, people) = attributing(
                PipelineStage::Summarize,
                store
                    .reading(move |store| Ok((store.participants(meeting_id)?, store.persons()?)))
                    .await,
            )?;
            let output = attributing(
                PipelineStage::Summarize,
                summarizer
                    .summarize(&SummaryInput {
                        meeting: meeting.clone(),
                        segments: segments.to_vec(),
                        speakers: speakers.to_vec(),
                        participants,
                        known_people: people,
                        template: template.clone(),
                    })
                    .await,
            )?;
            let mut summary = output.summary;
            summary.template_id.clone_from(&template.id);
            updated.summary = Some(summary);
            if meeting.calendar_event_id.is_none()
                && meeting.title_origin != TitleOrigin::User
                && !output.title.is_empty()
            {
                updated.title = output.title;
                updated.title_origin = TitleOrigin::Summary;
            }
            if let Some(language) = output.language {
                updated.language = Some(language);
            }
            updated.llm_usage = Some(
                meeting.llm_usage.unwrap_or(LlmUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    requests: 0,
                }) + output.usage,
            );
            updated.updated_at = now;
            let (tasks, decisions, names) = (output.tasks, output.decisions, output.speaker_names);
            attributing(
                PipelineStage::Summarize,
                store
                    .writing(move |store| {
                        store.replace_summary(&updated, &tasks, &decisions, &names)?;
                        Ok(updated)
                    })
                    .await,
            )
        })
        .await
    }

    /// Writes the mixdown unless the asset is AAC, marks the meeting
    /// `ready`, and posts `SpeakersNeedReview` for unconfirmed speakers. The
    /// asset row is reread, so a retention changed meanwhile survives.
    async fn persist(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<AudioAsset> {
        let decoder = &self.inner.dependencies.decoder;
        let store = self.store();
        let events = &self.inner.dependencies.events;
        let now = self.now();
        self.run(PipelineStage::Persist, 0, meeting.id, async {
            let mut mixdown = None;
            if asset.format != steno_core::AudioFormat::M4aAac {
                let layout = RecordingLayout::from_asset(asset).ok_or_else(|| {
                    PipelineFailure::new(
                        PipelineStage::Persist,
                        format!("not a file URL: {}", asset.url),
                    )
                })?;
                attributing(PipelineStage::Persist, layout.create_directories(false))?;
                let path = layout.mixdown(decoder.mixdown_format());
                attributing(PipelineStage::Persist, decoder.mixdown(asset, &path).await)?;
                mixdown = Some(file_url(&path, false));
            }
            let (meeting_id, fallback) = (meeting.id, asset.clone());
            let dispatcher = self.inner.dependencies.dispatcher.clone();
            let (updated, unconfirmed) = attributing(
                PipelineStage::Persist,
                store
                    .writing(move |store| {
                        let mut updated = store.asset_by_id(fallback.id)?.unwrap_or(fallback);
                        if let Some(mixdown) = mixdown {
                            updated.mixdown_url = Some(mixdown);
                        }
                        store.save_asset(&updated)?;
                        // Before the ready write, so a failed read never
                        // fails a ready meeting.
                        let unconfirmed: Vec<Uuid> = store
                            .speakers(meeting_id)?
                            .into_iter()
                            .filter(|speaker| !speaker.assignment.is_confirmed())
                            .map(|speaker| speaker.id)
                            .collect();
                        // The export is owed before the meeting is ready, so
                        // an exit in between leaves a `pending` row for the
                        // next launch rather than a ready meeting nothing
                        // exports.
                        dispatcher.mark_pending(meeting_id);
                        store.set_state(meeting_id, MeetingState::Ready, now)?;
                        Ok((updated, unconfirmed))
                    })
                    .await,
            )?;
            if !unconfirmed.is_empty() {
                events.post(MeetingEvent::SpeakersNeedReview {
                    meeting_id: meeting.id,
                    speaker_ids: unconfirmed,
                });
            }
            Ok::<_, PipelineFailure>(updated)
        })
        .await
    }

    /// `DeliveryDispatcher::deliver_all`: never fails; every outcome is a
    /// `Delivery` row.
    async fn deliver(&self, meeting_id: Uuid) {
        let dispatcher = &self.inner.dependencies.dispatcher;
        let _ = self
            .run(PipelineStage::Deliver, 0, meeting_id, async {
                dispatcher.deliver_all(meeting_id).await;
                Ok::<_, PipelineFailure>(())
            })
            .await;
    }

    /// Stamps `expires_at` from the asset's retention, then posts
    /// `RetentionApplied`. An undelivered delivery leaves the asset
    /// unstamped with nothing posted; with [`Stamp::Automatic`] so do stored
    /// results that may still need the audio
    /// ([`Self::stored_results_need_the_audio`]).
    async fn retention(&self, asset: &AudioAsset, stamp: Stamp) -> Result<()> {
        let store = self.store();
        let events = &self.inner.dependencies.events;
        let meeting_id = asset.meeting_id;
        let delivered = attributing(
            PipelineStage::Retention,
            store
                .reading(move |store| store.deliveries(meeting_id))
                .await,
        )?
        .iter()
        .all(|delivery| delivery.status == DeliveryStatus::Delivered);
        if !delivered {
            return Ok(());
        }
        if stamp == Stamp::Automatic && self.stored_results_need_the_audio(asset).await? {
            tracing::info!(
                target: BACKGROUND_RUN_LOG,
                %meeting_id,
                "the recording is kept: the meeting's speakers or transcript are incomplete"
            );
            return Ok(());
        }
        let now = self.now();
        let fallback = asset.clone();
        self.run(PipelineStage::Retention, 0, meeting_id, async {
            store
                .writing(move |store| {
                    let mut updated = store.asset_by_id(fallback.id)?.unwrap_or(fallback);
                    updated.expires_at = updated.retention.expiry(now);
                    // Durable, so the stamp never outlives the transcript it
                    // was kept for.
                    store.save_asset_durably(&updated)
                })
                .await?;
            events.post(MeetingEvent::RetentionApplied { meeting_id });
            Ok::<_, StoreError>(())
        })
        .await
    }

    /// [`steno_core::results_need_the_audio`] over the stored rows.
    async fn stored_results_need_the_audio(&self, asset: &AudioAsset) -> Result<bool> {
        let meeting_id = asset.meeting_id;
        let stored = attributing(
            PipelineStage::Retention,
            self.store()
                .reading(move |store| {
                    let Some(meeting) = store.meeting(meeting_id)? else {
                        return Ok(None);
                    };
                    Ok(Some((
                        meeting,
                        store.speakers(meeting_id)?,
                        store.segments(meeting_id)?,
                    )))
                })
                .await,
        )?;
        let Some((meeting, speakers, segments)) = stored else {
            return Ok(false);
        };
        Ok(steno_core::results_need_the_audio(
            &meeting, &speakers, &segments, asset,
        ))
    }

    /// After `deliver` ran again, stamps an unstamped finite retention when
    /// every delivery succeeded, for a `ready` meeting whose master is on
    /// disk.
    async fn stamp_deferred_retention(&self, meeting_id: Uuid, stamp: Stamp) -> Result<()> {
        let (asset, meeting) = attributing(
            PipelineStage::Retention,
            self.store()
                .reading(move |store| Ok((store.asset(meeting_id)?, store.meeting(meeting_id)?)))
                .await,
        )?;
        let Some(asset) = asset else {
            return Ok(());
        };
        if asset.retention == AudioRetention::KeepForever || asset.expires_at.is_some() {
            return Ok(());
        }
        let ready = meeting.is_some_and(|meeting| meeting.state == MeetingState::Ready);
        let master_exists =
            steno_core::paths::file_url_path(&asset.url).is_some_and(|path| path.exists());
        if !ready || !master_exists {
            return Ok(());
        }
        self.retention(&asset, stamp).await
    }
}

/// Who asks for a retention stamp: the pipeline, which waits while the
/// results could need the audio, or the user's own rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stamp {
    /// After a run, a re-export or a summary re-run.
    Automatic,
    /// After the user applied a rule.
    Chosen,
}

/// One operation's in-flight mark; dropping it clears the mark and the run.
struct Admitted {
    pipeline: ProcessingPipeline,
    meeting_id: Uuid,
}

impl Drop for Admitted {
    fn drop(&mut self) {
        self.pipeline
            .in_flight_set()
            .meetings
            .remove(&self.meeting_id);
        self.pipeline.state().runs.remove(&self.meeting_id);
    }
}

/// A job's claim on the speech engine; dropping it ends the claim.
struct SpeechClaim {
    engine: SharedSpeechEngine,
}

impl Drop for SpeechClaim {
    fn drop(&mut self) {
        *self.engine.claim_count() -= 1;
    }
}

/// The debug line of a run the exit kept from starting.
fn not_started(asset_id: Uuid) -> Option<Refused> {
    tracing::debug!(
        target: BACKGROUND_RUN_LOG,
        %asset_id,
        "not started: the app is quitting"
    );
    None
}

/// An asset claimed for a background run; dropping it releases the claim.
struct AssetClaim {
    pipeline: ProcessingPipeline,
    asset_id: Uuid,
}

impl Drop for AssetClaim {
    fn drop(&mut self) {
        self.pipeline.in_flight_set().assets.remove(&self.asset_id);
    }
}

/// A run refused for missing models or a busy store: its meeting and what
/// it awaits.
#[derive(Clone, Copy)]
struct Refused {
    meeting_id: Uuid,
    awaits: Awaits,
}

/// What a refused run's meeting waits for.
#[derive(Clone, Copy)]
enum Awaits {
    /// The models, with the resume count when the run started.
    Models { resumes_seen: u64 },
    /// The store's write lock.
    Store,
}

/// A background run's entry in `running` and its claim, released first.
/// Dropping it removes the entry only while it is the task's own: a
/// restart under the same asset id may have replaced it.
struct Running {
    pipeline: ProcessingPipeline,
    key: Uuid,
    claim: Option<AssetClaim>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut state = self.pipeline.state();
        let own = state
            .running
            .get(&self.key)
            .is_some_and(|handle| tokio::task::try_id() == Some(handle.id()));
        if own {
            state.running.remove(&self.key);
        }
    }
}

/// The participant with `role == me`, created as `Me` when none exists.
/// While it names nobody it takes the person the meeting's "me" speaker
/// was confirmed as, else [`Settings::self_person_id`], so the merge
/// confirms the mic lane as them.
pub fn ensure_me_participant(
    store: &Store,
    meeting_id: Uuid,
) -> std::result::Result<Participant, StoreError> {
    let existing = store
        .participants(meeting_id)?
        .into_iter()
        .find(|participant| participant.role == ParticipantRole::Me);
    if let Some(me) = &existing
        && me.person_id.is_some()
    {
        return Ok(me.clone());
    }
    let person = store
        .speakers(meeting_id)?
        .into_iter()
        .find(|speaker| speaker.id == LaneMerger::me_speaker_id(meeting_id))
        .and_then(|speaker| speaker.person_id())
        // Unreadable settings leave the mic lane unnamed rather than fail the run.
        .or_else(|| store.settings().ok()?.self_person_id)
        .map(|id| store.person(id))
        .transpose()?
        .flatten();
    let mut me = existing.clone().unwrap_or_else(|| Participant {
        id: LaneMerger::me_participant_id(meeting_id),
        meeting_id,
        person_id: None,
        display_name: LaneMerger::ME_SPEAKER_LABEL.to_owned(),
        role: ParticipantRole::Me,
        email: None,
    });
    if let Some(person) = person {
        me.person_id = Some(person.id);
        me.display_name = person.display_name;
    }
    if existing.as_ref() != Some(&me) {
        store.save_participant(&me)?;
    }
    Ok(me)
}

/// Runs `work` (file work, no store call) on tokio's blocking pool. A panic
/// in `work` resumes here; a task cancelled by runtime shutdown is an
/// error.
pub(crate) async fn off_the_workers<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|join| match join.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            Err(cancelled) => std::io::Error::other(cancelled),
        })
}

/// Clamps to `-1...1`, scales to Int16 and writes a 16 kHz mono WAV.
pub fn write_wav_16k(path: &Path, buffer: &AudioBuffer16k) -> std::io::Result<()> {
    crate::fixtures::write_wav(path, &crate::fixtures::int16(&buffer.samples))
}

#[cfg(test)]
mod tests {
    use super::*;
    use steno_core::Delivery;

    /// The room speaker takes the label after the highest stored
    /// "Speaker N", so the gap at "Speaker 2" is not reused.
    #[test]
    fn the_room_speaker_takes_the_label_after_the_highest_stored_one() {
        let meeting_id = Uuid::new_v4();
        let stored = |label: &str| Speaker {
            cluster_label: label.to_owned(),
            ..Diarization::room_speaker(meeting_id, &[])
        };
        assert_eq!(
            Diarization::room_speaker(meeting_id, &[]).cluster_label,
            "Speaker 1"
        );
        let taken = [stored("Me"), stored("Speaker 1"), stored("speaker 3")];
        let room = Diarization::room_speaker(meeting_id, &taken);
        assert_eq!(room.cluster_label, "Speaker 4");
        assert_eq!(room.id, room_speaker_id(meeting_id));
        assert_eq!(room.embedding, None);
    }

    /// A stage's own failure keeps its stage bare or boxed; anything else
    /// becomes a failure of the stage that saw it.
    #[test]
    fn a_failure_keeps_its_stage_bare_or_boxed() {
        let carried = PipelineFailure::new(PipelineStage::Transcribe, "the model is missing");
        assert_eq!(
            PipelineFailure::wrapping(&carried, PipelineStage::Decode),
            carried
        );
        let boxed: BoxError = Box::new(carried.clone());
        assert_eq!(
            PipelineFailure::wrapping(&boxed, PipelineStage::Decode),
            carried
        );
        let other: BoxError = "no such file".into();
        assert_eq!(
            PipelineFailure::wrapping(&other, PipelineStage::Decode),
            PipelineFailure::new(PipelineStage::Decode, "no such file")
        );
    }

    /// `models_missing` keeps its kind, stage and reason through
    /// `wrapping`, bare and boxed.
    #[test]
    fn a_models_missing_refusal_keeps_its_kind_bare_or_boxed() {
        assert_eq!(
            PipelineFailure::new(PipelineStage::Decode, "x").kind,
            FailureKind::Failed
        );
        assert!(!PipelineFailure::new(PipelineStage::Decode, "x").is_models_missing());
        let refusal = PipelineFailure::models_missing(PipelineStage::Diarize);
        assert_eq!(refusal.kind, FailureKind::ModelsMissing);
        assert_eq!(refusal.reason, "Download the speech models in Settings");
        assert_eq!(
            refusal.to_string(),
            "diarize: Download the speech models in Settings"
        );
        let boxed: BoxError = Box::new(refusal.clone());
        for wrapped in [
            PipelineFailure::wrapping(&refusal, PipelineStage::Decode),
            PipelineFailure::wrapping(&boxed, PipelineStage::Decode),
        ] {
            assert_eq!(wrapped, refusal);
            assert!(wrapped.is_models_missing());
        }
    }

    /// A decoder nothing should reach: a wrongly started run fails at
    /// `decode`.
    struct NoDecoder;

    #[steno_core::async_trait]
    impl AudioDecoder for NoDecoder {
        async fn decode(
            &self,
            _asset: &AudioAsset,
            _lane: AudioLane,
        ) -> steno_core::protocols::BoundaryResult<AudioBuffer16k> {
            Err("no decoder in this test".into())
        }

        fn mixdown_format(&self) -> steno_core::AudioFormat {
            steno_core::AudioFormat::Wav16kInt16
        }

        async fn mixdown(
            &self,
            _asset: &AudioAsset,
            _to: &Path,
        ) -> steno_core::protocols::BoundaryResult<()> {
            Err("no decoder in this test".into())
        }
    }

    struct NoDispatcher;

    #[steno_core::async_trait]
    impl DeliveryDispatcher for NoDispatcher {
        async fn deliver_all(&self, _meeting_id: Uuid) -> Vec<Delivery> {
            Vec::new()
        }
    }

    fn pipeline_over(store: &Arc<Store>) -> ProcessingPipeline {
        ProcessingPipeline::new(PipelineDependencies::new(
            Arc::new(NoDecoder),
            Arc::new(steno_core::testing::FakeSpeechEngine::default()),
            Arc::new(steno_core::testing::FakeDiarizer::default()),
            Arc::new(steno_core::testing::InMemorySpeakerMemory::new(Vec::new())),
            Arc::new(NoDispatcher),
            store.clone(),
            MeetingEventBus::new(),
        ))
    }

    /// A waiting meeting another operation claimed when a resume runs is
    /// put back, and the next resume starts it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_waiting_meeting_whose_asset_is_claimed_waits_for_the_next_resume() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
        let mut meeting = steno_core::testing::sample_data::meeting();
        meeting.id = Uuid::new_v4();
        meeting.state = MeetingState::Queued;
        let asset = crate::fixtures::two_lane_call(
            &dir.path().join("audio"),
            meeting.id,
            AudioRetention::KeepForever,
        )
        .unwrap();
        store.save_meeting_with_asset(&meeting, &asset).unwrap();
        let pipeline = pipeline_over(&store);
        let waits = pipeline.dependencies().model_waits.clone();
        waits.put_back([meeting.id]);

        let claim = pipeline.claim_start(meeting.id, asset.id).unwrap();
        assert_eq!(pipeline.resume_waiting().unwrap(), Vec::<Uuid>::new());
        assert_eq!(waits.waiting(), [meeting.id]);
        drop(claim);

        assert_eq!(pipeline.resume_waiting().unwrap(), [meeting.id]);
        assert_eq!(waits.waiting(), Vec::<Uuid>::new());
        pipeline.wait_until_idle().await;
    }
}
