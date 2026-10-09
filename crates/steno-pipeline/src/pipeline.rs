//! The post-meeting pipeline: one typed function per stage, `progress`
//! posted as each stage starts, and one place that turns any error into
//! `failed(reason)`. Swift: `Sources/StenoCore/Pipeline/ProcessingPipeline.swift`
//! and `Pipeline/Stages/*.swift`.
//!
//! Lanes are decoded one at a time inside the stage that needs them, so at
//! most one `AudioBuffer16k` is alive. One operation runs per meeting at a
//! time: a second `process`, `rerun_summary` or `redeliver` on a meeting
//! in flight fails instead of interleaving writes with the first, also
//! across a reload, whose pipelines share one in-flight set
//! ([`InFlight`]). Stage durations feed the `stageRate` table when the
//! meeting was alone in flight for the whole stage, so overlapping runs
//! never pollute the rates.
//! Once a job's lanes are transcribed and no other job is between its
//! warm-up and its last lane, the speech engine is released
//! ([`SpeechEngine::release`]): the speech sidecar's child exits and its
//! working set goes back before the diarizer runs. The claims belong to the
//! engine ([`SharedSpeechEngine`]), not to one pipeline, so pipelines that
//! share an engine (the services keep it across a reload) never release it
//! under each other's jobs.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Instant;

use chrono::{DateTime, Utc};
use steno_core::{
    AudioAsset, AudioBuffer16k, AudioDecoder, AudioLane, AudioRetention, CleanupInput, Delivery,
    DeliveryDispatcher, DeliveryStatus, Diarizer, LanguageTag, LlmUsage, Meeting, MeetingEvent,
    MeetingOperation, MeetingSource, MeetingState, MeetingStateKind, MeetingSummarizer,
    Participant, ParticipantRole, PipelineStage, RawSegment, RecordingLayout, Settings, Speaker,
    SpeakerAssignment, SpeakerMemory, SpeechEngine, Store, StoreError, SummaryInput,
    SummaryTemplate, TimeRange, TitleOrigin, TranscriptCleaner, TranscriptSegment, busy_file,
    derived_uuid,
    paths::{file_url, file_url_path},
    protocols::{BoxError, DEFAULT_MATCH_MARGIN},
};
use tokio::sync::{
    Mutex as AsyncMutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock as AsyncRwLock,
    oneshot,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::crash_loop::{CountedRun, MAX_CRASHED_RUNS, OpenRuns, RunCount, TOO_MANY_CRASHED_RUNS};
use crate::estimator::{ProcessingEstimator, StageRates, StageSample, absorbing};
use crate::events::MeetingEventBus;
use crate::export_retries::ExportRetries;
use crate::lane_merger::{ClusterSpeaker, LaneMerger};
use crate::run::ProcessingRun;

/// The one failure type: any error inside a stage becomes this, and
/// [`ProcessingPipeline::process`] marks the meeting failed in one place.
/// Swift: `PipelineFailure` in `Sources/StenoCore/Pipeline/PipelineStage.swift`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub struct PipelineFailure {
    pub stage: PipelineStage,
    pub reason: String,
    /// Wraps a store error that another connection's lock caused
    /// ([`StoreError::is_busy`]). Rust only: Swift's failure kept no such
    /// mark, and its intake did not retry.
    busy: bool,
}

impl PipelineFailure {
    #[must_use]
    pub fn new(stage: PipelineStage, reason: impl Into<String>) -> Self {
        PipelineFailure {
            stage,
            reason: reason.into(),
            busy: false,
        }
    }

    /// The failure wraps a busy store error: nothing is wrong with the
    /// call, and a caller that can wait may try it again.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// `error` itself when it already is a `PipelineFailure`, bare or
    /// boxed as a boundary error (the stage it carries wins), else a
    /// failure for `stage` describing `error`. Swift: `PipelineFailure.wrapping`.
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
                busy: any
                    .downcast_ref::<StoreError>()
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

/// Why [`ProcessingPipeline::reprocess`] did not start a run: a refusal,
/// or the store failing. The text is for logs and the CLI; a button shows
/// its own words for each variant. Rust only.
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
    /// The meeting has no recording on record. A button treats it like
    /// [`AudioGone`](Self::AudioGone).
    #[error("meeting {0} has no recording on record")]
    NoAsset(Uuid),
    /// The master is not on disk: the retention sweep removed it, or
    /// something else did. The sidecars alone are not enough, since the
    /// run ends by mixing the master down.
    #[error("meeting {0}'s audio is no longer on disk")]
    AudioGone(Uuid),
    /// Another operation holds the meeting (a run, a summary rerun or a
    /// redelivery), or a background run holds its recording, on any
    /// pipeline sharing the in-flight set.
    #[error("meeting {0} is busy")]
    Busy(Uuid),
    /// The app is exiting: nothing was saved, and the meeting can be
    /// processed again after the next launch.
    #[error("the app is quitting")]
    Quitting,
    /// The store failed reading the meeting or saving it queued.
    #[error(transparent)]
    Pipeline(#[from] PipelineFailure),
}

/// The log target of a background run's failure (`enqueue`,
/// `resume_unfinished`). The CLI, which waits for its run and prints the
/// failure itself as Swift's `steno process` did, turns it off.
pub const BACKGROUND_RUN_LOG: &str = "steno_pipeline::background";

/// A re-run or a re-export whose meeting is already claimed: awaiting it
/// does the work, dropping it unawaited releases the meeting. See
/// [`ProcessingPipeline::claim_rerun_summary`].
pub type Operation = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;

/// Wall-clock stamps for rows; tests pin it. Swift: `PipelineDependencies.now`
/// in `Sources/StenoCore/Pipeline/ProcessingPipeline.swift`.
pub type Now = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// A monotonic clock in seconds, for stage durations. Swift:
/// `PipelineDependencies.clock` (a `Clock<Duration>`) in
/// `Sources/StenoCore/Pipeline/ProcessingPipeline.swift`; tests pass a
/// manual one.
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

/// The app's exit as the pipelines see it ([`ProcessingPipeline::quit`]).
/// Clones share it, so one latch quits every pipeline built over
/// dependencies that carry it ([`PipelineDependencies::with_quit_latch`]).
/// Once it is set, no job starts, and a job that fails leaves its meeting
/// `queued` or `processing` for the next launch's
/// [`resume_unfinished`](ProcessingPipeline::resume_unfinished) instead of
/// marking it `failed`: the exit can end a job (a session's end can kill
/// the speech sidecar before the app), and that is no failure of the
/// meeting's. Setting it also takes back every background run still going
/// on those pipelines from the crash-loop guard's count
/// ([`crate::crash_loop`]), so an ordinary exit never counts as a crash.
/// Rust only: the Swift pipeline ran in the app's process and died with
/// it, and the next launch resumed the job.
#[derive(Debug, Clone, Default)]
pub struct QuitLatch(Arc<Latch>);

/// The flag, and the runs counted while it was clear, under one lock.
#[derive(Debug, Default)]
struct Latch {
    set: AtomicBool,
    open: Mutex<OpenRuns>,
}

impl QuitLatch {
    /// Sets the latch; it is never cleared. Every run counted on a
    /// pipeline sharing it and still going is taken back.
    pub fn set(&self) {
        let mut open = self.open();
        self.0.set.store(true, Ordering::SeqCst);
        open.take_back_all();
    }

    /// Whether the latch is set: the app is exiting.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0.set.load(Ordering::SeqCst)
    }

    /// Counts a run under `count`, unless the latch is set; the check and
    /// the count are one step, so the exit takes back every run counted
    /// before it.
    pub(crate) fn open_run(&self, count: RunCount) -> Option<u64> {
        let mut open = self.open();
        (!self.is_set()).then(|| open.open(count))
    }

    /// Ends the run under `key` ([`OpenRuns::close`]), under the lock that
    /// counts runs, so a clear never interleaves with another run's count.
    pub(crate) fn close_run(&self, key: u64, count: &RunCount, succeeded: bool) {
        self.open().close(key, count, succeeded);
    }

    fn open(&self) -> MutexGuard<'_, OpenRuns> {
        self.0
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The meetings with an operation in progress and the recordings being
/// processed, shared by every pipeline whose dependencies carry it, so a
/// reload's new pipeline refuses a meeting the retired one still holds.
/// Clones share one set. Its lock is never held while a pipeline's own
/// state lock is taken.
/// Swift: `inFlight`, `admissions` and `running` of `ProcessingPipeline`
/// in `Sources/StenoCore/Pipeline/ProcessingPipeline.swift`, which each
/// pipeline kept for itself, so after `reloadPipeline()` the new one could
/// run beside a retired one. Rust only: the sharing.
#[derive(Debug, Clone, Default)]
pub struct InFlight(Arc<Mutex<InFlightSet>>);

#[derive(Debug, Default)]
struct InFlightSet {
    /// Meetings with an operation in progress.
    meetings: BTreeSet<Uuid>,
    /// Counts every admission to `meetings`; a stage whose count moved
    /// was not alone for its whole span.
    admissions: u64,
    /// The assets claimed for a background run
    /// ([`ProcessingPipeline::claim_start`], for `enqueue`, `reprocess` and
    /// `resume_unfinished`), from the claim to the run's end.
    assets: BTreeSet<Uuid>,
}

impl InFlight {
    fn lock(&self) -> MutexGuard<'_, InFlightSet> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A speech engine with the jobs that claim it, shared by every pipeline
/// built over it: a job claims the engine from its warm-up to its last
/// lane, and the engine is released only once no job on any of those
/// pipelines holds a claim. The pipelines' warm-ups (the diarizer's
/// too) and that release take one lock, so a warm-up never overlaps a
/// release. Clones share the engine and its claims; [`new`](Self::new)
/// starts with none. The services hand a clone to each pipeline a reload
/// builds over the same engine, and keep the sidecar's for the run and a
/// [`WeakSpeechEngine`] to the in-process one. Rust only: Swift has no
/// release.
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
/// // A new value over the same engine starts claims of its own: share by
/// // cloning.
/// let separate = SharedSpeechEngine::new(engine.engine().clone());
/// assert!(!separate.ptr_eq(&engine));
/// // It derefs to the engine.
/// assert_eq!(shared.id(), "fake-engine");
/// ```
#[derive(Clone)]
pub struct SharedSpeechEngine(Arc<SharedEngineState>);

/// What the pipelines over one [`SharedSpeechEngine`] share: the engine
/// and its claims.
struct SharedEngineState {
    engine: Arc<dyn SpeechEngine>,
    /// Jobs between their warm-up and their last lane ([`SpeechClaim`]).
    count: Mutex<usize>,
    /// Serialises the warm-ups (`warm_up`, `warm_up_diarizer`) and the
    /// release after a job's lanes: one preparation at a time, so two runs
    /// that start together load each engine once, and a warm-up never
    /// overlaps a release.
    preparing: AsyncMutex<()>,
}

impl SharedSpeechEngine {
    /// `engine` with no claims on it yet, shared with no other value: a
    /// pipeline built over it releases the engine under the jobs of any
    /// other pipeline on the same engine. Pipelines that run on one
    /// engine share a clone instead; the app's come from
    /// `steno_services::speech::SpeechEngines`.
    #[must_use]
    pub fn new(engine: Arc<dyn SpeechEngine>) -> Self {
        SharedSpeechEngine(Arc::new(SharedEngineState {
            engine,
            count: Mutex::new(0),
            preparing: AsyncMutex::new(()),
        }))
    }

    /// The engine itself.
    #[must_use]
    pub fn engine(&self) -> &Arc<dyn SpeechEngine> {
        &self.0.engine
    }

    /// Whether `other` is a clone of this one: the same engine with the
    /// same claims.
    #[must_use]
    pub fn ptr_eq(&self, other: &SharedSpeechEngine) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// A handle that finds this engine and its claims again while a
    /// clone of it is alive, without keeping them alive itself.
    #[must_use]
    pub fn downgrade(&self) -> WeakSpeechEngine {
        WeakSpeechEngine(Arc::downgrade(&self.0))
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

    /// The lock the warm-ups and the release take, shared by every
    /// pipeline over this engine.
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

/// A [`SharedSpeechEngine`] that does not keep the engine alive, from
/// [`SharedSpeechEngine::downgrade`]: the services keep one for the
/// in-process engine, so a reload back to it while a retired pipeline
/// still runs on it gets the same engine and claims, and its model is
/// freed once no pipeline holds it. Rust only.
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

/// Everything the pipeline needs, and the only injection axis: the app
/// and the CLI pass real implementations, tests pass the fakes in
/// `steno_core::testing`. `cleaner` and `summarizer` are `None` when no
/// LLM endpoint is configured: the cleanup and summarize stages then post
/// their progress and write nothing, so the meeting lands `ready` with
/// `summary == None` instead of a fabricated summary. Swift:
/// `PipelineDependencies`.
#[derive(Clone)]
pub struct PipelineDependencies {
    pub decoder: Arc<dyn AudioDecoder>,
    /// The speech engine with its claims: [`new`](Self::new) gives it
    /// claims of its own; [`with_speech_engine`](Self::with_speech_engine)
    /// shares the caller's.
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
    /// A fresh latch from [`new`](Self::new); clones share theirs, and
    /// [`with_quit_latch`](Self::with_quit_latch) shares the caller's.
    pub quit_latch: QuitLatch,
    /// A fresh in-flight set from [`new`](Self::new); clones share theirs,
    /// and [`with_in_flight`](Self::with_in_flight) shares the caller's.
    pub in_flight: InFlight,
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
        }
    }

    /// Runs on `engine`, sharing its claims with every pipeline over a
    /// clone of it, instead of the engine [`new`](Self::new) was given.
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

    /// Carries `latch` instead of the fresh one from [`new`](Self::new), so
    /// every pipeline built over dependencies that carry it quits on one
    /// [`QuitLatch::set`] (the services' reloads share the app's).
    #[must_use]
    pub fn with_quit_latch(mut self, latch: QuitLatch) -> Self {
        self.quit_latch = latch;
        self
    }

    /// Carries `in_flight` instead of the fresh set from [`new`](Self::new),
    /// as [`with_quit_latch`](Self::with_quit_latch) carries a latch.
    #[must_use]
    pub fn with_in_flight(mut self, in_flight: InFlight) -> Self {
        self.in_flight = in_flight;
        self
    }
}

/// The pipeline's own runs and background tasks; the meetings in flight
/// are in [`InFlight`].
#[derive(Default)]
struct State {
    runs: HashMap<Uuid, ProcessingRun>,
    /// The background runs started by `enqueue` and `resume_unfinished`,
    /// by asset id, and by `redeliver_unfinished`, by a key of their own.
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
/// language. Swift: `Transcription` in `Pipeline/Stages/DecodeTranscribe.swift`.
struct Transcription {
    lanes: BTreeMap<AudioLane, Vec<RawSegment>>,
    language: Option<LanguageTag>,
}

/// A decoded lane with the lane it came from.
struct DecodedLane {
    lane: AudioLane,
    buffer: AudioBuffer16k,
}

/// What the diarize stage hands on. Swift: `Diarization` in
/// `Pipeline/Stages/Diarize.swift`.
struct Diarization {
    speakers: Vec<Speaker>,
    cluster_speakers: Vec<ClusterSpeaker>,
    /// The lane the clusters cover; `None` when nothing was diarized.
    lane: Option<AudioLane>,
}

impl Diarization {
    const NONE: Diarization = Diarization {
        speakers: Vec::new(),
        cluster_speakers: Vec::new(),
        lane: None,
    };

    /// What `diarize` falls back to for a meeting with no stored speakers:
    /// the transcript is kept, and `lane`, the lane that was to be diarized,
    /// becomes one unknown speaker the user can still name. A mic lane is
    /// diarized only when it is the room (a call whose tap carried no
    /// conversation), so it becomes the room speaker too and the other
    /// party's words never go to "me". The speaker has no embedding, so
    /// confirming it teaches no voice, and an id of its own, so a later run
    /// that diarizes never inherits its confirmation. Rust only: Swift
    /// fails the meeting.
    fn one_room_speaker(meeting_id: Uuid, lane: AudioLane) -> Diarization {
        Diarization {
            speakers: vec![Self::room_speaker(meeting_id)],
            cluster_speakers: vec![Self::whole_recording(Self::room_speaker_id(meeting_id))],
            lane: Some(lane),
        }
    }

    /// The id of [`one_room_speaker`](Self::one_room_speaker)'s speaker.
    fn room_speaker_id(meeting_id: Uuid) -> Uuid {
        derived_uuid(meeting_id, "speaker-room")
    }

    fn room_speaker(meeting_id: Uuid) -> Speaker {
        Speaker {
            id: Self::room_speaker_id(meeting_id),
            meeting_id,
            // The first label a diarizer hands out.
            cluster_label: "Speaker 1".to_owned(),
            assignment: SpeakerAssignment::Unknown,
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: None,
            cluster_confidence: 0.0,
        }
    }

    /// `speaker_id` over the whole recording.
    fn whole_recording(speaker_id: Uuid) -> ClusterSpeaker {
        ClusterSpeaker {
            speaker_id,
            ranges: vec![TimeRange {
                lower: 0.0,
                upper: f64::INFINITY,
            }],
        }
    }
}

struct Merged {
    segments: Vec<TranscriptSegment>,
    speakers: Vec<Speaker>,
}

/// The sample clip is at most ten seconds.
pub const SAMPLE_CLIP_SECONDS: f64 = 10.0;

/// Elects a meeting's language from tagged segments by summed duration;
/// `None` when no segment is tagged. Ties break on the tag so the result is
/// stable. Swift: `LanguageElection.elect`.
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
/// one room lane. Swift: `diarizedLane`.
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
/// then the microphone heard everyone (a phone on speaker next to the Mac,
/// a call app the tap missed) and the mic lane is the room lane to
/// diarize, instead of being "me" wholesale. Swift:
/// `diarizedLane(source:lanes:transcription:)`.
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

/// The tap carried no conversation when its speech stays under both
/// bounds: this share of the mic's speech, and
/// [`TAP_CONVERSATION_MAXIMUM_SECONDS`] outright. A notification chime or a
/// hallucinated word on a silent tap stays under both; a partner who
/// mostly listens still clears the seconds. Swift:
/// `tapConversationMinimumShare`.
pub const TAP_CONVERSATION_MINIMUM_SHARE: f64 = 0.05;
/// Swift: `tapConversationMaximumSeconds`.
pub const TAP_CONVERSATION_MAXIMUM_SECONDS: f64 = 10.0;

/// True when the mic lane holds speech and the system lane holds less than
/// [`TAP_CONVERSATION_MINIMUM_SHARE`] of it and less than
/// [`TAP_CONVERSATION_MAXIMUM_SECONDS`]. Swift: `tapCarriedNoConversation`.
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

fn attributing<T, E: fmt::Display + 'static>(
    stage: PipelineStage,
    result: std::result::Result<T, E>,
) -> Result<T> {
    result.map_err(|error| PipelineFailure::wrapping(&error, stage))
}

/// The reason a claimed operation that panicked fails with.
pub const OPERATION_PANICKED: &str = "the operation stopped unexpectedly";

/// `work`, with a panic inside it turned into a failure for the stage
/// `stage()` returns. `stage` runs only after the panic, so it can name the
/// stage the work had reached. An operation therefore always ends in a
/// result, and a failure is posted or persisted. The panic message itself
/// goes to stderr through the panic hook, as any panic's does.
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

/// Whether every row of `deliveries` went through (none for a deleted
/// meeting).
fn all_delivered(deliveries: &[Delivery]) -> bool {
    deliveries
        .iter()
        .all(|delivery| delivery.status == DeliveryStatus::Delivered)
}

/// The failure saved on a delivery an export left `pending` when no
/// failure names a reason: the process ended mid-export, or the
/// dispatcher returned without finishing the row.
const EXPORT_INTERRUPTED: &str = "the export stopped before it finished";

/// The row an operation needs, or a failure for `stage` that says what
/// is `missing`.
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
        ProcessingPipeline {
            inner: Arc::new(Inner {
                dependencies,
                state: Mutex::new(State::default()),
            }),
        }
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
    /// one's in-flight set (a reload's too), so a test can tell that a run
    /// has been admitted.
    #[must_use]
    pub fn in_flight(&self) -> Vec<Uuid> {
        self.in_flight_set().meetings.iter().copied().collect()
    }

    /// Sets the pipeline's [`QuitLatch`] for the app's exit, which quits
    /// every pipeline sharing it; `enqueue` still saves the meeting
    /// `queued` with its asset.
    pub fn quit(&self) {
        self.inner.dependencies.quit_latch.set();
    }

    fn quitting(&self) -> bool {
        self.inner.dependencies.quit_latch.is_set()
    }

    /// Writes `Meeting(queued)` plus the asset in one transaction and starts
    /// `process` in the background. The app (Mac recordings) calls this;
    /// the phone intake saves its rows itself and calls
    /// [`ProcessingPipeline::enqueue_saved`]. Fails when the asset or the
    /// meeting is already in flight. Needs a `tokio` runtime. Once the
    /// pipeline [quits](Self::quit), the meeting is saved and stays
    /// `queued` for the next launch.
    pub fn enqueue(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        let claim = self.claim_or_refuse(meeting, asset)?;
        self.enqueue_claimed(meeting, asset, claim, Store::save_meeting_with_asset)
    }

    /// [`ProcessingPipeline::enqueue`] of a Mac recording that stopped, the
    /// local intake's: the rows are written through
    /// [`Store::save_stopped_recording`], so only while the meeting is
    /// still `recording`, checked in the commit's own transaction. Rust
    /// only.
    pub fn enqueue_stopped_recording(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        let claim = self.claim_or_refuse(meeting, asset)?;
        self.enqueue_claimed(meeting, asset, claim, Store::save_stopped_recording)
    }

    /// [`ProcessingPipeline::enqueue`] of a meeting the caller saved
    /// `queued` with its asset: starts `process` in the background and
    /// writes nothing. The phone intake's, which commits the meeting with
    /// its `complete` receipt in one durable transaction
    /// ([`Store::save_admission_durably`]). Fails when the asset or the
    /// meeting is already in flight. Once the pipeline [quits](Self::quit),
    /// nothing starts and the meeting waits `queued` for the next launch.
    /// Swift: `ProcessingPipeline.enqueueSaved`.
    pub fn enqueue_saved(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        let claim = self.claim_or_refuse(meeting, asset)?;
        self.start(asset, Turn::Now(None), claim);
        Ok(())
    }

    /// [`ProcessingPipeline::claim_start`] for `enqueue` and
    /// `enqueue_saved`, refused as a failed `decode`.
    fn claim_or_refuse(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<AssetClaim> {
        self.claim_start(meeting.id, asset.id).ok_or_else(|| {
            PipelineFailure::new(
                PipelineStage::Decode,
                format!("meeting {} is already being processed", meeting.id),
            )
        })
    }

    /// `enqueue` once the asset is claimed, its rows written through
    /// `save`: the claim is held from the check to the run's end, so a
    /// second call cannot start a second run.
    fn enqueue_claimed(
        &self,
        meeting: &Meeting,
        asset: &AudioAsset,
        claim: AssetClaim,
        save: fn(&Store, &Meeting, &AudioAsset) -> std::result::Result<(), StoreError>,
    ) -> Result<()> {
        let mut queued = meeting.clone();
        queued.state = MeetingState::Queued;
        queued.updated_at = self.now();
        let mut asset = asset.clone();
        asset.meeting_id = meeting.id;
        attributing(PipelineStage::Decode, save(self.store(), &queued, &asset))?;
        // Processed afresh: earlier runs that ended with the app no longer
        // count against it.
        RunCount::of(&asset).clear();
        self.start(&asset, Turn::Now(None), claim);
        Ok(())
    }

    /// Processes a `ready` or `failed` meeting again from `decode`, with its
    /// audio asset, as [`enqueue`](Self::enqueue) does: the crash-loop
    /// guard's count starts afresh, so a meeting launch recovery gave up on
    /// ([`TOO_MANY_CRASHED_RUNS`]) gets new tries. Refused, with the
    /// [`ReprocessError`] that says why, when the meeting or its asset is
    /// missing, when it is recording, queued or processing, when its master
    /// is gone (the retention sweep keeps the asset row when it removes the
    /// files), when another operation holds it, and once the pipeline
    /// [quits](Self::quit). The retention stamp an earlier run left goes,
    /// so a run that fails never leaves the audio to the sweep; the run's
    /// retention stage stamps it again. The refusals' text is for logs and
    /// the CLI; a button shows its own words for each variant. Needs a
    /// `tokio` runtime. Rust only: Swift had no such action.
    pub fn reprocess(&self, meeting_id: Uuid) -> std::result::Result<(), ReprocessError> {
        if self.quitting() {
            return Err(ReprocessError::Quitting);
        }
        let meeting = attributing(PipelineStage::Decode, self.store().meeting(meeting_id))?
            .ok_or(ReprocessError::MeetingNotFound(meeting_id))?;
        let state = meeting.state.kind();
        if !matches!(state, MeetingStateKind::Ready | MeetingStateKind::Failed) {
            return Err(ReprocessError::Unfinished { meeting_id, state });
        }
        let mut asset = attributing(PipelineStage::Decode, self.store().asset(meeting_id))?
            .ok_or(ReprocessError::NoAsset(meeting_id))?;
        // The persist stage mixes the master down, so sidecars alone would
        // fail the run at its end.
        if !file_url_path(&asset.url).is_some_and(|master| master.exists()) {
            return Err(ReprocessError::AudioGone(meeting_id));
        }
        let claim = self
            .claim_start(meeting_id, asset.id)
            .ok_or(ReprocessError::Busy(meeting_id))?;
        asset.expires_at = None;
        Ok(self.enqueue_claimed(&meeting, &asset, claim, Store::save_meeting_with_asset)?)
    }

    /// Claims `asset_id` for a background run of `meeting_id` in the
    /// in-flight set. The run holds the claim until it ends, and a run that
    /// never starts drops it. `None` while the asset runs or is claimed, or
    /// the meeting is in flight, on any pipeline sharing the set. The check and the
    /// claim are one step under the set's lock.
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

    /// Launch recovery for the queue: every meeting a previous process left
    /// `queued` or `processing` is processed again from `decode`, oldest
    /// first, in the background like `enqueue`. A meeting whose asset row
    /// is missing is marked failed, and so is one whose runs ended with the
    /// app [`MAX_CRASHED_RUNS`] times ([`crate::crash_loop`]), with
    /// [`TOO_MANY_CRASHED_RUNS`]; its audio is kept, and a retention
    /// stamp an earlier run left is cleared so the sweep keeps it too. A
    /// meeting with any such run waits, then runs alone, so its crashes
    /// are charged to it, not to the meetings waiting behind it. Returns
    /// the meetings whose processing was started, those that wait last:
    /// none once the pipeline [quits](Self::quit).
    pub fn resume_unfinished(&self) -> Result<Vec<Uuid>> {
        if self.quitting() {
            return Ok(Vec::new());
        }
        let meetings = attributing(
            PipelineStage::Decode,
            self.store()
                .meetings_in_states(&[MeetingStateKind::Queued, MeetingStateKind::Processing]),
        )?;
        let turns = Arc::new(AsyncRwLock::new(()));
        let mut resumed = Vec::new();
        let mut alone = Vec::new();
        for meeting in meetings {
            if self.in_flight_set().meetings.contains(&meeting.id) {
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
            let Some(claim) = self.claim_start(meeting.id, asset.id) else {
                continue;
            };
            let count = RunCount::of(&asset);
            let crashed = count.read();
            if crashed >= MAX_CRASHED_RUNS {
                tracing::warn!(
                    meeting_id = %meeting.id,
                    runs = crashed,
                    "processing ended with the app too often; marked failed"
                );
                // Processing it again needs the audio: a stamp left from
                // an earlier run must not let the sweep take it.
                if asset.expires_at.take().is_some() {
                    attributing(PipelineStage::Retention, self.store().save_asset(&asset))?;
                }
                attributing(
                    PipelineStage::Decode,
                    self.store().set_state(
                        meeting.id,
                        MeetingState::Failed {
                            reason: TOO_MANY_CRASHED_RUNS.to_owned(),
                        },
                        self.now(),
                    ),
                )?;
                count.clear();
                continue;
            }
            if crashed > 0 {
                alone.push((meeting.id, asset, claim));
                continue;
            }
            // Never refused: no run asks to go alone before the loop ends.
            let shared = turns.clone().try_read_owned().ok();
            self.start(&asset, Turn::Now(shared), claim);
            resumed.push(meeting.id);
        }
        let mut turns_alone = Vec::new();
        for (meeting_id, asset, claim) in alone {
            let (turn, waiting) = oneshot::channel();
            turns_alone.push(turn);
            self.start(&asset, Turn::Alone(waiting), claim);
            resumed.push(meeting_id);
        }
        if !turns_alone.is_empty() {
            // Oldest first: each write guard waits for the runs that
            // started at once, then for the run before it to drop its own.
            tokio::spawn(async move {
                for turn in turns_alone {
                    let _ = turn.send(turns.clone().write_owned().await);
                }
            });
        }
        Ok(resumed)
    }

    /// Launch recovery for the exports (P28 of
    /// `.plans/2026-10-07-stable-promotion.md`): re-exports every ready
    /// meeting with a delivery a previous process left `pending`, or one
    /// that `failed` more than a day ([`ExportRetries::INTERVAL`]) before
    /// the pipeline's clock, after it, or was never attempted.
    ///
    /// A launch retries a failed export at most once, and once a day at
    /// most. Each launch re-export is counted in `retries` before it runs,
    /// so an exit mid-export counts too, and the count is reset once every
    /// row is delivered. After [`ExportRetries::LIMIT`] launch re-exports in a row
    /// that did not deliver every row, the meeting is left for the user,
    /// whose re-export resets the count. A launch re-export that leaves a
    /// row `pending` saves it failed, and so does the launch for a meeting
    /// it stopped retrying, so the detail says the export keeps failing.
    ///
    /// The meetings run one at a time in one background task, each claimed
    /// when its turn comes, so the others stay free for the user meanwhile;
    /// a meeting another operation holds is skipped, since its own run
    /// exports it, and so is one that, once claimed, is no longer ready
    /// with a row `pending` or failed (the user exported it meanwhile).
    /// The claim takes the meeting and not its asset, so a background run
    /// that holds the asset and has not admitted the meeting yet would then
    /// fail as already being processed; no such run starts on a ready
    /// meeting until Process again does, and this claim must then refuse
    /// the asset too.
    ///
    /// A failure is logged, not posted as `OperationFailed`: the user did
    /// not ask for this export, so the detail must not say "Export again
    /// failed". Returns the meetings to re-export, none once the
    /// pipeline [quits](Self::quit). Needs a `tokio` runtime. Rust only.
    pub fn redeliver_unfinished(&self, retries: &Arc<ExportRetries>) -> Result<Vec<Uuid>> {
        if self.quitting() {
            return Ok(Vec::new());
        }
        let store = self.store();
        // A meeting whose deliveries all went through since (or that was
        // deleted) starts again from 0.
        retries.retain(|meeting_id| {
            store
                .deliveries(meeting_id)
                .map_or(true, |rows| !all_delivered(&rows))
        });
        let found = attributing(
            PipelineStage::Deliver,
            store.meetings_with_unfinished_deliveries(self.now(), ExportRetries::INTERVAL),
        )?;
        let (stopped, owed): (Vec<Uuid>, Vec<Uuid>) = found
            .into_iter()
            .partition(|meeting_id| retries.stopped(*meeting_id));
        for meeting_id in stopped {
            // Refused: the operation holding it saves its own rows.
            if let Ok(_admitted) = self.admit(meeting_id, PipelineStage::Deliver) {
                self.fail_pending_deliveries(meeting_id, EXPORT_INTERRUPTED);
            }
        }
        if !owed.is_empty() {
            let pipeline = self.clone();
            let (meetings, retries) = (owed.clone(), retries.clone());
            self.spawn_tracked(Uuid::new_v4(), None, async move {
                for meeting_id in meetings {
                    if pipeline.quitting() {
                        return;
                    }
                    pipeline.redeliver_at_launch(meeting_id, &retries).await;
                }
            });
        }
        Ok(owed)
    }

    /// One meeting of [`redeliver_unfinished`](Self::redeliver_unfinished):
    /// claimed now, then counted in `retries` before the re-export runs
    /// and reset once every row is delivered. A refused claim, or a
    /// meeting no longer owed once claimed, is not counted. The file is
    /// written on a blocking thread.
    async fn redeliver_at_launch(&self, meeting_id: Uuid, retries: &Arc<ExportRetries>) {
        // Refused: a meeting in flight exports at the end of its run.
        let Ok(admitted) = self.admit(meeting_id, PipelineStage::Deliver) else {
            return;
        };
        // Read under the claim: since the launch found it, the user may
        // have exported it, or a run may have left it not ready.
        let Some(meeting) = self.still_owed(meeting_id) else {
            return;
        };
        let operation = self.deliver_again_claimed(meeting, admitted);
        // Counted before the export writes its rows, so the detail those
        // writes reload already reads the count: the last failure before
        // the stop shows "keeps failing" at once.
        let counted = retries.clone();
        let _ = tokio::task::spawn_blocking(move || counted.attempted(meeting_id)).await;
        if let Err(failure) = operation.await {
            tracing::warn!(
                target: BACKGROUND_RUN_LOG,
                %meeting_id,
                stage = failure.stage.as_str(),
                "launch re-export failed"
            );
            tracing::debug!(target: BACKGROUND_RUN_LOG, %meeting_id, %failure, "launch re-export failure");
        }
        // Read once the claim is released. A re-export the user causes
        // meanwhile resets the count as well, so whichever reset comes
        // first, the count ends at 0; a refused one leaves it alone.
        if self
            .store()
            .deliveries(meeting_id)
            .is_ok_and(|rows| all_delivered(&rows))
        {
            let retries = retries.clone();
            let _ = tokio::task::spawn_blocking(move || retries.reset(meeting_id)).await;
        }
    }

    /// Saves the meeting's deliveries still `pending` as failed with
    /// `reason`, keeping their receipt and their last attempt, stamped now
    /// when there is none so the row waits a day like any failure. The
    /// caller holds the meeting's claim. A store error is logged and leaves
    /// the rows.
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

    /// Spawns `work` among the background runs
    /// [`wait_until_idle`](Self::wait_until_idle) waits for, under `key`,
    /// holding `claim` until it ends. The state lock is held from the spawn
    /// to the insert, so the task cannot finish and remove its entry before
    /// the entry exists; the task's [`Running`] mark removes the entry and
    /// then releases the claim however the task ends, a panic included.
    fn spawn_tracked(
        &self,
        key: Uuid,
        claim: Option<AssetClaim>,
        work: impl Future<Output = ()> + Send + 'static,
    ) {
        let pipeline = self.clone();
        let mut state = self.state();
        let handle = tokio::spawn(async move {
            let _running = Running {
                pipeline,
                key,
                _claim: claim,
            };
            work.await;
        });
        state.running.insert(key, handle);
    }

    /// Processes `asset` in the background
    /// ([`spawn_tracked`](Self::spawn_tracked)) under its [`AssetClaim`],
    /// which the run holds until it ends; a run that does not start
    /// releases it. Once the pipeline quits, nothing starts. The run is
    /// counted in the meeting's folder before it starts (on this thread,
    /// unless it waits for its [`Turn`]), and the count is cleared when it
    /// ends or the run taken back when the app exits first
    /// ([`CountedRun`]).
    fn start(&self, asset: &AudioAsset, turn: Turn, claim: AssetClaim) {
        let asset_id = asset.id;
        let latch = self.inner.dependencies.quit_latch.clone();
        let count = RunCount::of(asset);
        // A run that goes alone is counted when its turn comes.
        let counted = match &turn {
            Turn::Now(_) => match CountedRun::start(&latch, count.clone()) {
                Some(counted) => Some(counted),
                None => return not_started(asset_id),
            },
            Turn::Alone(_) => None,
        };
        let pipeline = self.clone();
        self.spawn_tracked(asset_id, Some(claim), async move {
            let (_shared, _alone) = match turn {
                Turn::Now(shared) => (shared, None),
                Turn::Alone(waiting) => match waiting.await {
                    Ok(alone) => (None, Some(alone)),
                    Err(_) => return not_started(asset_id),
                },
            };
            let Some(mut counted) = counted.or_else(|| CountedRun::start(&latch, count)) else {
                return not_started(asset_id);
            };
            let result = pipeline.process(asset_id).await;
            if result.is_ok() {
                counted.succeeded();
            }
            drop(counted);
            if let Err(failure) = result {
                if pipeline.quitting() {
                    // The exit ended the job, which `process` did not persist.
                    tracing::debug!(
                        target: BACKGROUND_RUN_LOG,
                        %asset_id,
                        %failure,
                        "processing stopped by the exit"
                    );
                    return;
                }
                // The reason can name the audio file (a decode error) or
                // quote the model, so warn carries the stage only; the
                // meeting row has the whole reason.
                tracing::warn!(
                    target: BACKGROUND_RUN_LOG,
                    %asset_id,
                    stage = failure.stage.as_str(),
                    "processing failed"
                );
                tracing::debug!(target: BACKGROUND_RUN_LOG, %asset_id, %failure, "processing failure");
            }
        });
    }

    /// Waits for every background task: the processing `enqueue` and
    /// `resume_unfinished` started and the re-exports of
    /// `redeliver_unfinished`; the CLI and the tests call it before
    /// reading results.
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

    /// Loads the speech engine and the diarizer now, so a run that starts
    /// later finds them resident. Concurrent calls are serialised, across
    /// the pipelines that share the engine too; a failed
    /// load is retried by the next call rather than cached. Errors carry
    /// `decode` for the engine and `diarize` for the diarizer, as the
    /// Swift `warmUp` in `ProcessingPipeline.swift` attributes them.
    pub async fn warm_up(&self) -> Result<()> {
        let _guard = self.speech_engine().preparing().await;
        let dependencies = &self.inner.dependencies;
        attributing(
            PipelineStage::Decode,
            dependencies.speech_engine.prepare().await,
        )?;
        attributing(
            PipelineStage::Diarize,
            dependencies.diarizer.prepare().await,
        )?;
        Ok(())
    }

    /// [`warm_up`](Self::warm_up) for `meeting_id`'s job: a diarizer that
    /// does not load goes through [`carry_on_after`](Self::carry_on_after)
    /// and does not fail the job: `diarize` then falls back to
    /// [`diarization_without_diarizer`](Self::diarization_without_diarizer).
    /// Rust only: Swift fails the meeting.
    async fn warm_up_for_job(&self, meeting_id: Uuid) -> Result<()> {
        match self.warm_up().await {
            Err(failure) if failure.stage == PipelineStage::Diarize => {
                self.carry_on_after(&failure, meeting_id)
            }
            result => result,
        }
    }

    /// [`warm_up`](Self::warm_up) for the diarizer alone, for a speech
    /// engine that frees its models after each job (the speech sidecar):
    /// loading that engine ahead of a job would keep the child's working
    /// set resident until the job is done, outside any claim. Rust only:
    /// Swift's `warmUp` loads both.
    pub async fn warm_up_diarizer(&self) -> Result<()> {
        let _guard = self.speech_engine().preparing().await;
        attributing(
            PipelineStage::Diarize,
            self.inner.dependencies.diarizer.prepare().await,
        )
    }

    /// Runs every stage: `queued → processing → ready`, or `failed(reason)`
    /// with whatever was persisted so far; a panic fails the meeting too. A
    /// failing `diarize` or `match_speakers` does not fail it: the
    /// transcript is kept with the speakers stored for the meeting, or,
    /// when none are stored, one unknown speaker for the diarized lane.
    /// Once `persist` has marked the meeting `ready` nothing downgrades it,
    /// not an error or a panic later in the run: the meeting is delivered
    /// and its retention applied, and the panic's or the `retention`
    /// error's failure is returned to the caller.
    /// Once the pipeline [quits](Self::quit), a failure is returned and
    /// not persisted, and a call made after it fails at once: the meeting
    /// stays `queued` or `processing`, which the next launch's
    /// `resume_unfinished` processes again.
    pub async fn process(&self, asset_id: Uuid) -> Result<()> {
        if self.quitting() {
            return Err(PipelineFailure::new(
                PipelineStage::Decode,
                "the app is quitting",
            ));
        }
        let asset = required(
            PipelineStage::Decode,
            self.store().asset_by_id(asset_id),
            || format!("audio asset {asset_id} not found"),
        )?;
        let meeting = required(
            PipelineStage::Decode,
            self.store().meeting(asset.meeting_id),
            || format!("meeting {} not found", asset.meeting_id),
        )?;
        let meeting_id = meeting.id;
        self.exclusively(meeting_id, PipelineStage::Decode, async {
            // A panic fails the meeting like any stage failure, so it
            // never stays `processing` (undeletable, and run again at every
            // launch). Boxed: the stages' future is too large to move
            // into the helper by value.
            let until_persist = unless_it_panics(
                || self.stage_in_progress(meeting_id),
                Box::pin(self.process_until_persist(&asset, meeting)),
            )
            .await;
            let persisted = match until_persist {
                Ok(asset) => asset,
                Err(failure) if self.quitting() => return Err(failure),
                Err(failure) => {
                    // A failure after `persist` marked the meeting ready
                    // leaves it ready, and a ready meeting is delivered:
                    // nothing resumes it to deliver it later.
                    let ready = self
                        .store()
                        .meeting(meeting_id)
                        .ok()
                        .flatten()
                        .is_some_and(|stored| stored.state == MeetingState::Ready);
                    if ready {
                        self.deliver(meeting_id).await;
                        let _ = self.stamp_deferred_retention(meeting_id).await;
                        return Err(failure);
                    }
                    let _ = self.store().set_state(
                        meeting_id,
                        MeetingState::Failed {
                            reason: failure.to_string(),
                        },
                        self.now(),
                    );
                    return Err(failure);
                }
            };
            self.deliver(meeting_id).await;
            self.retention(&persisted).await
        })
        .await
    }

    async fn process_until_persist(
        &self,
        asset: &AudioAsset,
        meeting: Meeting,
    ) -> Result<AudioAsset> {
        let claim = self.speech_engine().claim();
        let transcribed: Result<_> = async {
            self.warm_up_for_job(meeting.id).await?;
            attributing(
                PipelineStage::Decode,
                self.store()
                    .set_state(meeting.id, MeetingState::Processing, self.now()),
            )?;
            let settings = attributing(PipelineStage::Decode, self.store().settings())?;
            self.begin_run(
                &meeting,
                &asset.lanes,
                None,
                PipelineStage::ALL.to_vec(),
                &settings,
            )?;
            let mut current = meeting;
            current.state = MeetingState::Processing;
            let (transcription, last) = self.decode_and_transcribe(asset, current.id).await?;
            Ok((current, settings, transcription, last))
        }
        .await;
        self.finish_speech(claim).await;
        let (mut current, settings, transcription, last) = transcribed?;
        // The last decoded lane is handed to `diarize` and dropped there, so
        // no buffer is alive from `match_speakers` on. The lane to diarize
        // is decided from the transcription (a call whose tap carried
        // nothing falls back to its mic lane); a handed buffer of another
        // lane is dropped before `diarize` decodes the right one, so one
        // buffer is alive at a time.
        let lane =
            diarized_lane_after_transcription(current.source, &asset.lanes, &transcription.lanes);
        let handed = last.filter(|decoded| Some(decoded.lane) == lane);
        let mut diarized = match self.diarize(asset, &current, lane, handed).await {
            Ok(diarized) => diarized,
            Err(failure) => {
                self.carry_on_after(&failure, current.id)?;
                self.diarization_without_diarizer(current.id, lane)?
            }
        };
        current.language = transcription.language;
        match self
            .match_speakers(&diarized.speakers, current.id, &settings)
            .await
        {
            Ok(matched) => diarized.speakers = matched,
            // The speakers stay as diarize or the stored fallback made
            // them.
            Err(failure) => self.carry_on_after(&failure, current.id)?,
        }
        let merged = self
            .merge(&current, &transcription.lanes, &diarized)
            .await?;
        // This run's usage starts from the cleanup pass (None when it was
        // skipped) and the summarize stage adds its own.
        current.llm_usage = self
            .cleanup(&current, merged.segments, &merged.speakers)
            .await?;
        // The meeting was read when the run started. What the user changed
        // since wins: the template to summarize with, a title they typed
        // or a calendar event they attached (either keeps the model's title
        // out), and the speakers they named or merged during cleanup, with
        // the segments a merge repointed (their text is the cleaned text).
        let stored = required(
            PipelineStage::Summarize,
            self.store().meeting(current.id),
            || format!("meeting {} not found", current.id),
        )?;
        current.template_id = stored.template_id;
        current.title = stored.title;
        current.title_origin = stored.title_origin;
        current.calendar_event_id = stored.calendar_event_id;
        let speakers = attributing(PipelineStage::Summarize, self.store().speakers(current.id))?;
        let segments = attributing(PipelineStage::Summarize, self.store().segments(current.id))?;
        let current = self.summarize(current, &segments, &speakers).await?;
        self.persist(&current, asset).await
    }

    /// A stage whose failure costs only the speaker labels (`diarize`,
    /// `match_speakers`, the diarizer's warm-up) is logged with `meeting_id`
    /// and its stage only, and the run goes on with the transcript. Once
    /// the pipeline quits the failure is returned instead: the exit may
    /// have caused it, and the meeting stays `processing` for the next
    /// launch rather than being finished without its speakers. Rust only:
    /// Swift fails the meeting.
    fn carry_on_after(&self, failure: &PipelineFailure, meeting_id: Uuid) -> Result<()> {
        if self.quitting() {
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

    /// What the `diarize` stage falls back to when the diarizer fails or
    /// did not load. A meeting with stored speakers (a re-run) keeps them
    /// as stored, assignments, embeddings and clips included, so no
    /// confirmation or voice is lost. Each covers the spans of the stored
    /// segments it owns on `lane`, so the new transcript's segments on
    /// `lane` map onto them; a speaker that owns none on `lane` (the lane
    /// diarized last time was the other one) keeps its row and gets no
    /// segment. The earlier fallback speaker, when it is the only one on
    /// `lane`, covers the whole recording again. When no stored speaker
    /// owns a segment on `lane`, [`Diarization::one_room_speaker`] covers
    /// it next to the stored rows; a meeting without any falls back to it
    /// alone. Rust only: Swift fails the meeting.
    fn diarization_without_diarizer(
        &self,
        meeting_id: Uuid,
        lane: Option<AudioLane>,
    ) -> Result<Diarization> {
        let Some(lane) = lane else {
            return Ok(Diarization::NONE);
        };
        let me = LaneMerger::me_speaker_id(meeting_id);
        let mut speakers: Vec<Speaker> =
            attributing(PipelineStage::Diarize, self.store().speakers(meeting_id))?
                .into_iter()
                .filter(|speaker| speaker.id != me)
                .collect();
        if speakers.is_empty() {
            return Ok(Diarization::one_room_speaker(meeting_id, lane));
        }
        let segments = attributing(PipelineStage::Diarize, self.store().segments(meeting_id))?;
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
        let room = Diarization::room_speaker_id(meeting_id);
        let cluster_speakers = match owners.as_slice() {
            [only] if only.speaker_id == room => vec![Diarization::whole_recording(room)],
            // A stored room speaker that owns nothing on `lane` was the
            // room of the other lane: its confirmation does not carry over,
            // and its id is taken, so the lane's segments get no speaker.
            [] if !speakers.iter().any(|speaker| speaker.id == room) => {
                speakers.push(Diarization::room_speaker(meeting_id));
                vec![Diarization::whole_recording(room)]
            }
            _ => owners,
        };
        Ok(Diarization {
            speakers,
            cluster_speakers,
            lane: Some(lane),
        })
    }

    /// Ends `claim` and releases the speech engine when no other job, on
    /// this pipeline or another over the same engine, is between its
    /// warm-up and its last lane. Another claim seen before
    /// the lock ends the call at once, so a finisher never waits on a
    /// warm-up only to leave the engine loaded. Under `preparing`, the
    /// count is checked again: a job that claims the engine meanwhile
    /// either keeps it loaded or warms it up again after the release,
    /// never before it. A job that panics or is cancelled only drops its
    /// claim, so the engine stays loaded until the next job ends. A failed
    /// release is logged and never fails the job.
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

    /// Summarize again with another template, then deliver. A failure is
    /// returned to the caller and leaves the meeting's state, summary and
    /// deliveries as they were; only `process` marks failed. Without a
    /// summarizer the call fails instead of writing a placeholder.
    pub async fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> Result<()> {
        self.claim_rerun_summary(meeting_id, template_id)?.await
    }

    /// The synchronous half of [`rerun_summary`](Self::rerun_summary): the
    /// meeting is looked up, the summarizer checked and the meeting
    /// claimed now, so a refusal reaches the caller before anything runs;
    /// the returned [`Operation`] does the work and can be spawned. A
    /// failure of the work is returned and also posted as
    /// [`MeetingEvent::OperationFailed`], for a caller that does not wait.
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
        let export = attributing(PipelineStage::Summarize, self.store().export(meeting_id))?;
        let settings = attributing(PipelineStage::Summarize, self.store().settings())?;
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
        )?;
        let mut current = meeting;
        current.template_id = template_id.to_owned();
        current.state = MeetingState::Ready;
        self.summarize(current, &export.segments, &export.speakers)
            .await?;
        // The new summary is owed to the vault, so an exit before
        // `deliver` leaves `pending` rows for the next launch.
        self.inner.dependencies.dispatcher.mark_pending(meeting_id);
        self.deliver(meeting_id).await;
        self.stamp_deferred_retention(meeting_id).await
    }

    /// Deliver only: the one re-export entry point. Stamps an audio asset
    /// whose expiry was deferred by a failed delivery once this one
    /// succeeds.
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

    /// The re-export [`claim_redeliver`](Self::claim_redeliver) reports,
    /// without the `OperationFailed` event, which the launch's re-exports
    /// use directly. A row the re-export leaves `pending` is saved failed
    /// before the claim is released.
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
    /// failed: what the launch re-exports once it holds the claim. A store
    /// error reads as not owed, and the next launch looks again.
    fn still_owed(&self, meeting_id: Uuid) -> Option<Meeting> {
        let store = self.store();
        let meeting = store.meeting(meeting_id).ok().flatten()?;
        let owed = meeting.state == MeetingState::Ready
            && store.deliveries(meeting_id).is_ok_and(|rows| {
                rows.iter().any(|row| {
                    matches!(
                        row.status,
                        DeliveryStatus::Pending | DeliveryStatus::Failed(_)
                    )
                })
            });
        owed.then_some(meeting)
    }

    /// [`claim_deliver_again`](Self::claim_deliver_again) once `admitted`
    /// holds the meeting.
    fn deliver_again_claimed(&self, meeting: Meeting, admitted: Admitted) -> Operation {
        let meeting_id = meeting.id;
        let pipeline = self.clone();
        Box::pin(async move {
            let result =
                unless_it_panics(|| PipelineStage::Deliver, pipeline.deliver_again(&meeting)).await;
            // A row still `pending` here was left by a panic or a
            // dispatcher that stopped mid-export: saved failed while the
            // claim is held, so the count and the detail see a failure.
            let reason = result
                .as_ref()
                .err()
                .map_or(EXPORT_INTERRUPTED, |failure| failure.reason.as_str());
            pipeline.fail_pending_deliveries(meeting_id, reason);
            drop(admitted);
            result
        })
    }

    async fn deliver_again(&self, meeting: &Meeting) -> Result<()> {
        let meeting_id = meeting.id;
        let settings = attributing(PipelineStage::Deliver, self.store().settings())?;
        let lanes = attributing(PipelineStage::Deliver, self.store().asset(meeting_id))?
            .map(|a| a.lanes)
            .unwrap_or_default();
        self.begin_run(
            meeting,
            &lanes,
            None,
            vec![PipelineStage::Deliver],
            &settings,
        )?;
        self.deliver(meeting_id).await;
        self.stamp_deferred_retention(meeting_id).await
    }

    /// Posts `OperationFailed` for a failed `operation` and hands the
    /// result on.
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
    /// clears its stamp, then the deferred-case rules decide whether a new
    /// stamp is written now. Safe while the meeting is processing: the
    /// stages read the row again before they write it.
    pub async fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> Result<()> {
        let mut asset = required(
            PipelineStage::Retention,
            self.store().asset(meeting_id),
            || format!("meeting {meeting_id} has no audio asset"),
        )?;
        asset.retention = rule;
        asset.expires_at = None;
        attributing(PipelineStage::Retention, self.store().save_asset(&asset))?;
        self.stamp_deferred_retention(meeting_id).await
    }

    // Stage plumbing

    /// Starts the meeting's run over `stages`, estimated from the learned
    /// rates; the operation's [`Admitted`] mark removes it when the
    /// operation ends.
    fn begin_run(
        &self,
        meeting: &Meeting,
        lanes: &[AudioLane],
        tokens: Option<i64>,
        stages: Vec<PipelineStage>,
        settings: &Settings,
    ) -> Result<()> {
        let stage = *stages.first().unwrap_or(&PipelineStage::Decode);
        let rows = attributing(stage, self.store().stage_rates())?;
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

    /// Marks `meeting_id` in flight until the returned mark is dropped,
    /// or fails for `stage` when another operation holds it. The mark is
    /// cleared however the operation ends: completed, failed, panicked or
    /// dropped mid-way (a cancelled task).
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

    /// The stage `meeting_id`'s run last posted, the stage a panic is
    /// attributed to; `decode` before the first post.
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
    /// meeting's run. A stage called outside a run posts over a fresh run
    /// on the seeds that is not kept.
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
    /// the stage, and measures it on the dependencies' clock. The duration
    /// is recorded as a rate sample once the stage is complete when the
    /// meeting was alone in flight for the whole stage; a body that failed
    /// records nothing.
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
        let value = attributing(stage, body.await)?;
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
            // The rates are a convenience; a bookkeeping failure never
            // fails a run.
            if let Err(error) = self.record(&sample) {
                tracing::debug!(%error, "stage rate not recorded");
            }
        }
        Ok(value)
    }

    /// Folds one measurement into its row in one write: the first sample
    /// replaces the seed outright, later ones move the average.
    /// Swift: `MeetingStore.record`.
    fn record(&self, sample: &StageSample) -> std::result::Result<(), StoreError> {
        self.store()
            .update_stage_rate(sample.stage, &sample.key, sample.recorded_at, |current| {
                let current =
                    current.unwrap_or_else(|| StageRates::seeds().rate(sample.stage, &sample.key));
                absorbing(current, sample.seconds_per_unit)
            })
            .map(drop)
    }

    // Stages

    /// Per lane: decode, then transcribe with the previous lane's dominant
    /// language as the hint. Each buffer goes out of scope before the next
    /// lane is decoded, except the last, which is returned for `diarize`.
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
        for (index, lane) in ordered_lanes(&asset.lanes).into_iter().enumerate() {
            // Release the previous lane before decoding the next.
            drop(last.take());
            let buffer = if index == 0 {
                self.run(
                    PipelineStage::Decode,
                    0,
                    meeting_id,
                    decoder.decode(asset, lane),
                )
                .await?
            } else {
                attributing(PipelineStage::Decode, decoder.decode(asset, lane).await)?
            };
            let lane_hint = hint.clone();
            let segments = self
                .run(
                    PipelineStage::Transcribe,
                    index,
                    meeting_id,
                    engine.transcribe(&buffer, lane_hint.as_ref()),
                )
                .await?;
            hint = elect_language(&segments).or(hint);
            lanes.insert(lane, segments);
            last = Some(DecodedLane { lane, buffer });
        }
        let language = elect_language(lanes.values().flatten());
        Ok((Transcription { lanes, language }, last))
    }

    /// Runs the diarizer over `lane` and turns every cluster into a
    /// `Speaker` with a deterministic id. Each cluster's clip is written as
    /// 16 kHz WAV beside the master. A mic lane in which the diarizer hears
    /// fewer than two voices is the user alone (headphones, the tap
    /// permission missing): the stage returns no clusters and no lane,
    /// writes no clip, and the merge keeps the mic as "me". `handed` is
    /// reused when it carries `lane`, else the lane is decoded here.
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
            let layout = RecordingLayout::from_asset(asset);
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
                        let path = layout.sample_clip(id);
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
                && let Some(layout) = &layout
            {
                attributing(PipelineStage::Diarize, layout.create_directories(true))?;
                attributing(PipelineStage::Diarize, write_sample_clips(&clips))?;
            }
            Ok(Diarization {
                speakers,
                cluster_speakers,
                lane: Some(lane),
            })
        })
        .await
    }

    /// `SpeakerMemory::match_voice` at the settings' threshold for every
    /// speaker with an embedding: a hit becomes `suggested`, a miss stays
    /// `unknown`. Nothing is confirmed here.
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
    /// the speakers and the meeting's elected language in one transaction.
    /// When the asset has a `mic` lane that is "me" (not the lane diarized
    /// as the room), the "me" participant and speaker exist before any
    /// segment points at them. When the mic lane is the room, a "me"
    /// participant an earlier run of this pipeline created is removed
    /// again; one the app wrote stays.
    async fn merge(
        &self,
        meeting: &Meeting,
        lanes: &BTreeMap<AudioLane, Vec<RawSegment>>,
        diarization: &Diarization,
    ) -> Result<Merged> {
        let store = self.store();
        let now = self.now();
        self.run(PipelineStage::Merge, 0, meeting.id, async {
            let mut all_speakers = diarization.speakers.clone();
            let mut me_speaker_id = None;
            let mic_is_room = diarization.lane == Some(AudioLane::Mic);
            if mic_is_room {
                store.delete_participant(LaneMerger::me_participant_id(meeting.id))?;
            } else if lanes.contains_key(&AudioLane::Mic) {
                let me = ensure_me_participant(store, meeting.id)?;
                let me_speaker = LaneMerger::me_speaker(meeting.id, me.person_id);
                me_speaker_id = Some(me_speaker.id);
                all_speakers.push(me_speaker);
            }
            let segments = LaneMerger::merge(
                meeting.id,
                lanes,
                &diarization.cluster_speakers,
                me_speaker_id,
                diarization.lane,
            );
            let mut updated = meeting.clone();
            updated.updated_at = now;
            store.replace_transcript(&updated, &segments, &all_speakers)?;
            Ok::<_, StoreError>(Merged {
                segments,
                speakers: all_speakers,
            })
        })
        .await
    }

    /// Runs the `TranscriptCleaner` and persists the cleaned `text` by
    /// segment id; `raw_text`, ids, order and count are the merge stage's
    /// and must come back intact. The speakers are not written again, so a
    /// speaker the user named while the pass ran stays named. Returns the
    /// pass's usage. Without a cleaner the stage posts its progress and
    /// leaves the merged segments untouched with no usage.
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
            let participants = attributing(PipelineStage::Cleanup, store.participants(meeting.id))?;
            let people = attributing(PipelineStage::Cleanup, store.persons())?;
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
                store.update_segment_texts(&updated, &corrected),
            )?;
            Ok(Some(output.usage))
        })
        .await
    }

    /// Runs the `MeetingSummarizer` for `meeting.template_id` and persists
    /// summary, tasks, decisions and speaker name suggestions with the
    /// meeting's title, language and summed usage in one transaction. A
    /// calendar title and a title the user typed stay; a default title is
    /// replaced by the model's. Without a summarizer only the meeting's
    /// processing columns are persisted: the summary, tasks, decisions and
    /// suggestions an earlier run wrote stay. Rust only: Swift clears them.
    /// The template is not stored (see [`Meeting::apply_processing_results`]).
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
                attributing(
                    PipelineStage::Summarize,
                    store.save_processing_results(&updated),
                )?;
                return Ok::<_, PipelineFailure>(updated);
            };
            let template =
                SummaryTemplate::bundled_with_id(&meeting.template_id).ok_or_else(|| {
                    PipelineFailure::new(
                        PipelineStage::Summarize,
                        format!("unknown summary template {}", meeting.template_id),
                    )
                })?;
            let participants =
                attributing(PipelineStage::Summarize, store.participants(meeting_id))?;
            let people = attributing(PipelineStage::Summarize, store.persons())?;
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
            attributing(
                PipelineStage::Summarize,
                store.replace_summary(
                    &updated,
                    &output.tasks,
                    &output.decisions,
                    &output.speaker_names,
                ),
            )?;
            Ok(updated)
        })
        .await
    }

    /// Writes the mixdown for every asset that is not already AAC, marks the
    /// meeting `ready`, and posts `SpeakersNeedReview` when any speaker is
    /// not confirmed. The asset row is read again before the write, so a
    /// retention changed while the meeting was processing survives it.
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
            let mut updated = attributing(PipelineStage::Persist, store.asset_by_id(asset.id))?
                .unwrap_or_else(|| asset.clone());
            if let Some(mixdown) = mixdown {
                updated.mixdown_url = Some(mixdown);
            }
            attributing(PipelineStage::Persist, store.save_asset(&updated))?;
            // Read before the ready write, so a failed read never fails a
            // meeting already marked ready.
            let unconfirmed: Vec<Uuid> =
                attributing(PipelineStage::Persist, store.speakers(meeting.id))?
                    .into_iter()
                    .filter(|speaker| !speaker.assignment.is_confirmed())
                    .map(|speaker| speaker.id)
                    .collect();
            // The export is owed before the meeting is ready, so an exit
            // in between leaves a `pending` row for the next launch. Marked
            // after the ready write instead, an exit between the two would
            // leave a ready meeting nothing exports; marked before, a
            // failed ready write leaves `pending` rows on a failed meeting,
            // which keep its audio and which Process again exports.
            self.inner.dependencies.dispatcher.mark_pending(meeting.id);
            attributing(
                PipelineStage::Persist,
                store.set_state(meeting.id, MeetingState::Ready, now),
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

    /// `DeliveryDispatcher::deliver_all`: never fails, every destination's
    /// outcome is a `Delivery` row.
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
    /// `RetentionApplied`. Deletion waits for delivery: when any delivery of
    /// the meeting is not delivered the asset is left unstamped and nothing
    /// is posted, not even the stage's progress.
    async fn retention(&self, asset: &AudioAsset) -> Result<()> {
        let store = self.store();
        let events = &self.inner.dependencies.events;
        let meeting_id = asset.meeting_id;
        let delivered = attributing(PipelineStage::Retention, store.deliveries(meeting_id))?
            .iter()
            .all(|delivery| delivery.status == DeliveryStatus::Delivered);
        if !delivered {
            return Ok(());
        }
        let now = self.now();
        self.run(PipelineStage::Retention, 0, meeting_id, async {
            let mut updated = store
                .asset_by_id(asset.id)?
                .unwrap_or_else(|| asset.clone());
            updated.expires_at = updated.retention.expiry(now);
            store.save_asset(&updated)?;
            events.post(MeetingEvent::RetentionApplied { meeting_id });
            Ok::<_, StoreError>(())
        })
        .await
    }

    /// The deferred case after `deliver` ran again: an asset with a finite
    /// retention and no stamp gets one now if every delivery succeeded. Only
    /// a `ready` meeting whose master is still on disk is stamped.
    async fn stamp_deferred_retention(&self, meeting_id: Uuid) -> Result<()> {
        let store = self.store();
        let Some(asset) = attributing(PipelineStage::Retention, store.asset(meeting_id))? else {
            return Ok(());
        };
        if asset.retention == AudioRetention::KeepForever || asset.expires_at.is_some() {
            return Ok(());
        }
        let ready = attributing(PipelineStage::Retention, store.meeting(meeting_id))?
            .is_some_and(|meeting| meeting.state == MeetingState::Ready);
        let master_exists =
            steno_core::paths::file_url_path(&asset.url).is_some_and(|path| path.exists());
        if !ready || !master_exists {
            return Ok(());
        }
        self.retention(&asset).await
    }
}

/// The in-flight mark of one operation; dropping it clears the mark and
/// the run, whichever way the operation ended.
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

/// A job's claim on the speech engine, counted in its
/// [`SharedSpeechEngine`]; dropping it ends the claim.
struct SpeechClaim {
    engine: SharedSpeechEngine,
}

impl Drop for SpeechClaim {
    fn drop(&mut self) {
        *self.engine.claim_count() -= 1;
    }
}

/// When a background run starts ([`ProcessingPipeline::start`]).
enum Turn {
    /// At once. A run launch recovery starts holds a read guard of the
    /// launch's turns, so the runs that go alone wait for it.
    Now(Option<OwnedRwLockReadGuard<()>>),
    /// After every run the launch started at once, and one at a time,
    /// oldest first: a meeting with runs that ended with the app waits for
    /// the launch's write guard and is counted only when it has it, so a
    /// crash is charged to the run that is running and not to the meetings
    /// waiting behind it.
    Alone(oneshot::Receiver<OwnedRwLockWriteGuard<()>>),
}

/// The debug line of a run the exit kept from starting.
fn not_started(asset_id: Uuid) {
    tracing::debug!(
        target: BACKGROUND_RUN_LOG,
        %asset_id,
        "not started: the app is quitting"
    );
}

/// An asset claimed in the in-flight set for a background run
/// ([`ProcessingPipeline::claim_start`]); dropping it releases the claim,
/// before the run starts or, held by the run's [`Running`] mark, when the
/// run ends.
struct AssetClaim {
    pipeline: ProcessingPipeline,
    asset_id: Uuid,
}

impl Drop for AssetClaim {
    fn drop(&mut self) {
        self.pipeline.in_flight_set().assets.remove(&self.asset_id);
    }
}

/// A background run's entry in `running` (by asset id, or a key of its
/// own for a re-export); dropping it removes the entry and then releases
/// the processing run's claim, so a new claim of the asset never finds the
/// old run's entry.
struct Running {
    pipeline: ProcessingPipeline,
    key: Uuid,
    _claim: Option<AssetClaim>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.pipeline.state().running.remove(&self.key);
    }
}

/// The participant with `role == me`, created with the display name `Me`
/// when the app wrote none. Swift: `ensureMeParticipant`.
pub fn ensure_me_participant(
    store: &Store,
    meeting_id: Uuid,
) -> std::result::Result<Participant, StoreError> {
    if let Some(existing) = store
        .participants(meeting_id)?
        .into_iter()
        .find(|participant| participant.role == ParticipantRole::Me)
    {
        return Ok(existing);
    }
    let me = Participant {
        id: LaneMerger::me_participant_id(meeting_id),
        meeting_id,
        person_id: None,
        display_name: LaneMerger::ME_SPEAKER_LABEL.to_owned(),
        role: ParticipantRole::Me,
        email: None,
    };
    store.save_participant(&me)?;
    Ok(me)
}

/// Writes every speaker's sample clip. Each is written beside its path
/// first and moved into place once all are, in one rename that replaces
/// the earlier clip ([`busy_file::rename`], which on Windows tries a held
/// file again), so a failure while writing moves none and a path always
/// holds a whole clip. A move that fails after earlier ones landed leaves
/// those speakers with the new clips, so a kept speaker can then play
/// another run's voice; per-run clip names would close that (plan, "Open
/// after the port"). Rust only: Swift writes each clip in place.
fn write_sample_clips(clips: &[(PathBuf, AudioBuffer16k)]) -> std::io::Result<()> {
    let staged = |path: &Path| path.with_extension("wav.partial");
    if let Err(error) = clips
        .iter()
        .try_for_each(|(path, clip)| write_wav_16k(&staged(path), clip))
    {
        for (path, _) in clips {
            let _ = std::fs::remove_file(staged(path));
        }
        return Err(error);
    }
    clips
        .iter()
        .try_for_each(|(path, _)| busy_file::rename(&staged(path), path))
}

/// Clamps to `-1...1`, scales to Int16 and writes a 16 kHz mono WAV, the
/// layout `WAVWriter` produced on the Swift side.
pub fn write_wav_16k(path: &Path, buffer: &AudioBuffer16k) -> std::io::Result<()> {
    crate::fixtures::write_wav(path, &crate::fixtures::int16(&buffer.samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stage's own failure keeps its stage however it travels: bare, or
    /// boxed as a boundary error by a crate behind a seam; anything else
    /// becomes a failure of the stage that saw it. Swift:
    /// `PipelineFailure.wrapping`.
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
}
