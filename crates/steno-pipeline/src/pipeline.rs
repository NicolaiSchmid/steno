//! The post-meeting pipeline: one typed function per stage, `progress`
//! posted as each stage starts, and one place that turns any error into
//! `failed(reason)`. Swift: `Sources/StenoCore/Pipeline/ProcessingPipeline.swift`
//! and `Pipeline/Stages/*.swift`.
//!
//! Lanes are decoded one at a time inside the stage that needs them, so at
//! most one `AudioBuffer16k` is alive. One operation runs per meeting at a
//! time: a second `process`, `rerun_summary` or `redeliver` on a meeting
//! in flight fails instead of interleaving writes with the first. Stage
//! durations feed the `stageRate` table when the meeting was alone in
//! flight for the whole stage, so overlapping runs never pollute the rates.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use chrono::{DateTime, Utc};
use steno_core::{
    AudioAsset, AudioBuffer16k, AudioDecoder, AudioLane, AudioRetention, CleanupInput,
    DeliveryDispatcher, DeliveryStatus, Diarizer, LanguageTag, LlmUsage, Meeting, MeetingEvent,
    MeetingSource, MeetingState, MeetingStateKind, MeetingSummarizer, Participant, ParticipantRole,
    PipelineStage, RawSegment, RecordingLayout, Settings, Speaker, SpeakerAssignment,
    SpeakerMemory, SpeechEngine, Store, StoreError, SummaryInput, SummaryTemplate, TimeRange,
    TitleOrigin, TranscriptCleaner, TranscriptSegment, derived_uuid, paths::file_url,
    protocols::DEFAULT_MATCH_MARGIN,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::estimator::{ProcessingEstimator, StageRates, StageSample, absorbing};
use crate::events::MeetingEventBus;
use crate::lane_merger::{ClusterSpeaker, LaneMerger};
use crate::run::ProcessingRun;

/// The one failure type: any error inside a stage becomes this, and
/// [`ProcessingPipeline::process`] marks the meeting failed in one place.
/// Swift: `PipelineFailure` in `Sources/StenoCore/Pipeline/PipelineStage.swift`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub struct PipelineFailure {
    pub stage: PipelineStage,
    pub reason: String,
}

impl PipelineFailure {
    #[must_use]
    pub fn new(stage: PipelineStage, reason: impl Into<String>) -> Self {
        PipelineFailure {
            stage,
            reason: reason.into(),
        }
    }

    /// A failure for `stage` describing `error`.
    #[must_use]
    pub fn wrapping(error: &dyn fmt::Display, stage: PipelineStage) -> Self {
        PipelineFailure::new(stage, error.to_string())
    }
}

impl fmt::Display for PipelineFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.stage.as_str(), self.reason)
    }
}

type Result<T> = std::result::Result<T, PipelineFailure>;

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
    pub speech_engine: Arc<dyn SpeechEngine>,
    pub diarizer: Arc<dyn Diarizer>,
    pub speaker_memory: Arc<dyn SpeakerMemory>,
    pub cleaner: Option<Arc<dyn TranscriptCleaner>>,
    pub summarizer: Option<Arc<dyn MeetingSummarizer>>,
    pub dispatcher: Arc<dyn DeliveryDispatcher>,
    pub store: Arc<Store>,
    pub events: MeetingEventBus,
    pub now: Now,
    pub clock: Arc<dyn MonotonicClock>,
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
            speech_engine,
            diarizer,
            speaker_memory,
            cleaner: None,
            summarizer: None,
            dispatcher,
            store,
            events,
            now: Arc::new(Utc::now),
            clock: Arc::new(SystemClock::default()),
        }
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
}

/// The bookkeeping behind the pipeline's one mutex.
#[derive(Default)]
struct State {
    /// Meetings with an operation in progress.
    in_flight: BTreeSet<Uuid>,
    /// Counts every admission to `in_flight`; a stage whose count moved
    /// was not alone for its whole span.
    admissions: u64,
    runs: HashMap<Uuid, ProcessingRun>,
    /// The background runs started by `enqueue` and `resume_unfinished`,
    /// by asset id.
    running: HashMap<Uuid, JoinHandle<()>>,
}

struct Inner {
    dependencies: PipelineDependencies,
    state: Mutex<State>,
    /// Serialises `warm_up`: one preparation at a time, so two runs that
    /// start together load each engine once.
    preparing: AsyncMutex<()>,
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
}

struct Merged {
    segments: Vec<TranscriptSegment>,
    speakers: Vec<Speaker>,
}

struct Cleaned {
    segments: Vec<TranscriptSegment>,
    usage: Option<LlmUsage>,
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

fn attributing<T, E: fmt::Display>(
    stage: PipelineStage,
    result: std::result::Result<T, E>,
) -> Result<T> {
    result.map_err(|error| PipelineFailure::wrapping(&error, stage))
}

/// The row an operation needs, or a failure for `stage` that says what
/// is `missing`.
fn required<T, E: fmt::Display>(
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
                preparing: AsyncMutex::new(()),
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

    fn now(&self) -> DateTime<Utc> {
        (self.inner.dependencies.now)()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Meetings with an operation in progress, so a test can tell that a
    /// run has been admitted.
    #[must_use]
    pub fn in_flight(&self) -> Vec<Uuid> {
        self.state().in_flight.iter().copied().collect()
    }

    /// Writes `Meeting(queued)` plus the asset in one transaction and starts
    /// `process` in the background. The app (Mac recordings) and the phone
    /// intake both call this. Fails when the asset or the meeting is
    /// already in flight. Needs a `tokio` runtime.
    pub fn enqueue(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        {
            let state = self.state();
            if state.running.contains_key(&asset.id) || state.in_flight.contains(&meeting.id) {
                return Err(PipelineFailure::new(
                    PipelineStage::Decode,
                    format!("meeting {} is already being processed", meeting.id),
                ));
            }
        }
        let mut queued = meeting.clone();
        queued.state = MeetingState::Queued;
        queued.updated_at = self.now();
        let mut asset = asset.clone();
        asset.meeting_id = meeting.id;
        attributing(
            PipelineStage::Decode,
            self.store().save_meeting_with_asset(&queued, &asset),
        )?;
        self.start(asset.id);
        Ok(())
    }

    /// Launch recovery for the queue: every meeting a previous process left
    /// `queued` or `processing` is processed again from `decode`, oldest
    /// first, in the background like `enqueue`. A meeting whose asset row
    /// is missing is marked failed. Returns the meetings whose processing
    /// was started.
    pub fn resume_unfinished(&self) -> Result<Vec<Uuid>> {
        let meetings = attributing(
            PipelineStage::Decode,
            self.store()
                .meetings_in_states(&[MeetingStateKind::Queued, MeetingStateKind::Processing]),
        )?;
        let mut resumed = Vec::new();
        for meeting in meetings {
            if self.state().in_flight.contains(&meeting.id) {
                continue;
            }
            let Some(asset) = attributing(PipelineStage::Decode, self.store().asset(meeting.id))?
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
            if self.state().running.contains_key(&asset.id) {
                continue;
            }
            self.start(asset.id);
            resumed.push(meeting.id);
        }
        Ok(resumed)
    }

    /// The state lock is held from the spawn to the insert, so the task
    /// cannot finish and remove its entry before the entry exists.
    fn start(&self, asset_id: Uuid) {
        let pipeline = self.clone();
        let mut state = self.state();
        let handle = tokio::spawn(async move {
            if let Err(failure) = pipeline.process(asset_id).await {
                tracing::warn!(%asset_id, %failure, "processing failed");
            }
            pipeline.state().running.remove(&asset_id);
        });
        state.running.insert(asset_id, handle);
    }

    /// Waits for every processing task started by `enqueue` or
    /// `resume_unfinished`; the CLI and the tests call it before reading
    /// results.
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
    /// later finds them resident. Concurrent calls are serialised; a failed
    /// load is retried by the next call rather than cached. Errors carry
    /// `decode` for the engine and `diarize` for the diarizer, as the
    /// Swift `warmUp` in `ProcessingPipeline.swift` attributes them.
    pub async fn warm_up(&self) -> Result<()> {
        let _guard = self.inner.preparing.lock().await;
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

    /// Runs every stage: `queued → processing → ready`, or `failed(reason)`
    /// with whatever was persisted so far. Once `persist` has marked the
    /// meeting `ready` nothing downgrades it: a `retention` error is
    /// returned to the caller and the meeting stays ready and delivered.
    pub async fn process(&self, asset_id: Uuid) -> Result<()> {
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
            let persisted = match self.process_until_persist(&asset, meeting).await {
                Ok(asset) => asset,
                Err(failure) => {
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
        self.warm_up().await?;
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
        // The last decoded lane is handed to `diarize` and dropped there, so
        // no buffer is alive from `match_speakers` on.
        let (transcription, last) = self.decode_and_transcribe(asset, current.id).await?;
        let mut diarized = self.diarize(asset, &current, last).await?;
        current.language = transcription.language;
        diarized.speakers = self
            .match_speakers(diarized.speakers, current.id, &settings)
            .await?;
        let merged = self
            .merge(&current, &transcription.lanes, &diarized)
            .await?;
        let cleaned = self
            .cleanup(&current, merged.segments, &merged.speakers)
            .await?;
        // This run's usage starts from the cleanup pass (None when it was
        // skipped) and the summarize stage adds its own.
        current.llm_usage = cleaned.usage;
        let current = self
            .summarize(current, &cleaned.segments, &merged.speakers)
            .await?;
        self.persist(&current, asset).await
    }

    /// Summarize again with another template, then deliver. A failure is
    /// returned to the caller and leaves the meeting's state, summary and
    /// deliveries as they were; only `process` marks failed. Without a
    /// summarizer the call fails instead of writing a placeholder.
    pub async fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> Result<()> {
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
        self.exclusively(meeting_id, PipelineStage::Summarize, async {
            let export = required(
                PipelineStage::Summarize,
                self.store().export(meeting_id),
                || format!("meeting {meeting_id} not found"),
            )?;
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
            self.deliver(meeting_id).await;
            self.stamp_deferred_retention(meeting_id).await
        })
        .await
    }

    /// Deliver only: the one re-export entry point. Stamps an audio asset
    /// whose expiry was deferred by a failed delivery once this one
    /// succeeds.
    pub async fn redeliver(&self, meeting_id: Uuid) -> Result<()> {
        let meeting = required(
            PipelineStage::Deliver,
            self.store().meeting(meeting_id),
            || format!("meeting {meeting_id} not found"),
        )?;
        self.exclusively(meeting_id, PipelineStage::Deliver, async {
            let settings = attributing(PipelineStage::Deliver, self.store().settings())?;
            let lanes = attributing(PipelineStage::Deliver, self.store().asset(meeting_id))?
                .map(|a| a.lanes)
                .unwrap_or_default();
            self.begin_run(
                &meeting,
                &lanes,
                None,
                vec![PipelineStage::Deliver],
                &settings,
            )?;
            self.deliver(meeting_id).await;
            self.stamp_deferred_retention(meeting_id).await
        })
        .await
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
    /// rates; [`Admitted`] drops it when the operation ends.
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
    /// operation on the same meeting fails for `stage`. The mark is
    /// cleared when `body` completes or when the future is dropped
    /// mid-way (a cancelled task), never left behind.
    async fn exclusively<T>(
        &self,
        meeting_id: Uuid,
        stage: PipelineStage,
        body: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        {
            let mut guard = self.state();
            if !guard.in_flight.insert(meeting_id) {
                return Err(PipelineFailure::new(
                    stage,
                    format!("meeting {meeting_id} is already being processed"),
                ));
            }
            guard.admissions += 1;
        }
        let _admitted = Admitted {
            pipeline: self,
            meeting_id,
        };
        body.await
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
    async fn run<T, E: fmt::Display>(
        &self,
        stage: PipelineStage,
        lane: usize,
        meeting_id: Uuid,
        body: impl Future<Output = std::result::Result<T, E>>,
    ) -> Result<T> {
        self.post(stage, lane, meeting_id);
        let (alone, admissions_before) = {
            let guard = self.state();
            (guard.in_flight.len() == 1, guard.admissions)
        };
        let clock = &self.inner.dependencies.clock;
        let started = clock.seconds();
        let value = attributing(stage, body.await)?;
        let seconds = clock.seconds() - started;
        let sample = {
            let mut guard = self.state();
            let alone = alone && guard.admissions == admissions_before;
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

    /// Runs the diarizer over the diarized lane and turns every cluster
    /// into a `Speaker` with a deterministic id. Each cluster's clip is
    /// written as 16 kHz WAV beside the master.
    async fn diarize(
        &self,
        asset: &AudioAsset,
        meeting: &Meeting,
        handed: Option<DecodedLane>,
    ) -> Result<Diarization> {
        let decoder = &self.inner.dependencies.decoder;
        let diarizer = &self.inner.dependencies.diarizer;
        let meeting_id = meeting.id;
        self.run(PipelineStage::Diarize, 0, meeting_id, async {
            let Some(lane) = diarized_lane(meeting.source, &asset.lanes) else {
                return Ok::<_, PipelineFailure>(Diarization {
                    speakers: Vec::new(),
                    cluster_speakers: Vec::new(),
                });
            };
            let buffer = match handed {
                Some(handed_lane) if handed_lane.lane == lane => handed_lane.buffer,
                _ => attributing(PipelineStage::Diarize, decoder.decode(asset, lane).await)?,
            };
            let result = attributing(PipelineStage::Diarize, diarizer.diarize(&buffer).await)?;
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
                        attributing(PipelineStage::Diarize, layout.create_directories(true))?;
                        let path = layout.sample_clip(id);
                        attributing(PipelineStage::Diarize, write_wav_16k(&path, &clip))?;
                        clip_url = Some(file_url(&path, false));
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
            Ok(Diarization {
                speakers,
                cluster_speakers,
            })
        })
        .await
    }

    /// `SpeakerMemory::match_voice` at the settings' threshold for every
    /// speaker with an embedding: a hit becomes `suggested`, a miss stays
    /// `unknown`. Nothing is confirmed here.
    async fn match_speakers(
        &self,
        speakers: Vec<Speaker>,
        meeting_id: Uuid,
        settings: &Settings,
    ) -> Result<Vec<Speaker>> {
        let memory = &self.inner.dependencies.speaker_memory;
        let threshold = settings.speaker_match_threshold;
        self.run(PipelineStage::MatchSpeakers, 0, meeting_id, async {
            let mut matched = Vec::with_capacity(speakers.len());
            for mut speaker in speakers {
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
    /// When the asset has a `mic` lane, the "me" participant and speaker
    /// exist before any segment points at them.
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
            if lanes.contains_key(&AudioLane::Mic) {
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

    /// Runs the `TranscriptCleaner` and persists the cleaned `text`;
    /// `raw_text`, ids, order and count are the merge stage's and must come
    /// back intact. Without a cleaner the stage posts its progress and
    /// hands the merged segments on untouched with no usage.
    async fn cleanup(
        &self,
        meeting: &Meeting,
        segments: Vec<TranscriptSegment>,
        speakers: &[Speaker],
    ) -> Result<Cleaned> {
        self.revise(ProcessingEstimator::token_count(&segments), meeting.id);
        let store = self.store();
        let cleaner = self.inner.dependencies.cleaner.clone();
        let now = self.now();
        self.run(PipelineStage::Cleanup, 0, meeting.id, async {
            let Some(cleaner) = cleaner else {
                return Ok::<_, PipelineFailure>(Cleaned {
                    segments,
                    usage: None,
                });
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
                store.replace_transcript(&updated, &corrected, speakers),
            )?;
            Ok(Cleaned {
                segments: corrected,
                usage: Some(output.usage),
            })
        })
        .await
    }

    /// Runs the `MeetingSummarizer` for `meeting.template_id` and persists
    /// summary, tasks, decisions and speaker name suggestions with the
    /// meeting's title, language and summed usage in one transaction. A
    /// calendar title and a title the user typed stay; a default title is
    /// replaced by the model's. Without a summarizer the meeting is
    /// persisted with no summary and no tasks, decisions or suggestions.
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
                updated.summary = None;
                updated.updated_at = now;
                attributing(
                    PipelineStage::Summarize,
                    store.replace_summary(&updated, &[], &[], &[]),
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
            attributing(
                PipelineStage::Persist,
                store.set_state(meeting.id, MeetingState::Ready, now),
            )?;
            let unconfirmed: Vec<Uuid> =
                attributing(PipelineStage::Persist, store.speakers(meeting.id))?
                    .into_iter()
                    .filter(|speaker| !speaker.assignment.is_confirmed())
                    .map(|speaker| speaker.id)
                    .collect();
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
            steno_core::paths::path_from_file_url(&asset.url).is_some_and(|path| path.exists());
        if !ready || !master_exists {
            return Ok(());
        }
        self.retention(&asset).await
    }
}

/// The in-flight mark of one operation; dropping it clears the mark and
/// the run, whichever way the operation ended.
struct Admitted<'a> {
    pipeline: &'a ProcessingPipeline,
    meeting_id: Uuid,
}

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        let mut guard = self.pipeline.state();
        guard.in_flight.remove(&self.meeting_id);
        guard.runs.remove(&self.meeting_id);
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

/// Clamps to `-1...1`, scales to Int16 and writes a 16 kHz mono WAV, the
/// layout `WAVWriter` produced on the Swift side.
pub fn write_wav_16k(path: &Path, buffer: &AudioBuffer16k) -> std::io::Result<()> {
    crate::fixtures::write_wav(path, &crate::fixtures::int16(&buffer.samples))
}
