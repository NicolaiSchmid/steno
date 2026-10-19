//! The host's `Pipeline` over the processing pipeline and the retention
//! sweep, with the reload the Settings commands ask for.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use chrono::Utc;
use steno_core::protocols::BoundaryResult;
use steno_core::{AudioRetention, MeetingStateKind};
use steno_host::services::{Pipeline, ProcessAgainRefusal};
use steno_pipeline::{
    InFlight, LiveRecording, ModelWaits, Operation, PipelineDependencies, PipelineFailure,
    ProcessingPipeline, QuitLatch, ReprocessError, RetentionSweep, SweepIncomplete,
};
use steno_speech::SpeechRuntime;
use uuid::Uuid;

use crate::app::BuildError;
use crate::block_on;

/// The speech engine a pipeline was built with: the engine id the
/// settings named at the build and where it runs. The recorder's warm-up
/// reads it, not the id stored now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltEngine {
    pub engine_id: String,
    pub runtime: SpeechRuntime,
}

/// One pipeline's dependencies and the speech engine they hold.
pub struct BuiltPipeline {
    pub dependencies: PipelineDependencies,
    pub engine: BuiltEngine,
}

/// Rebuilds the dependencies and their engine from the stored settings.
pub type MakeDependencies = Arc<dyn Fn() -> Result<BuiltPipeline, BuildError> + Send + Sync>;

/// Whether the models a pipeline on the runtime needs are installed now.
pub type ModelsInstalled = Arc<dyn Fn(SpeechRuntime) -> bool + Send + Sync>;

/// When [`CurrentPipeline`] tries again the meetings that wait for the
/// store's write lock: `first` after a meeting starts waiting, then twice
/// as long after each try, up to `longest`. A quiet spell longer than
/// `longest` starts again from `first`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreRetry {
    /// The wait before the first try.
    pub first: Duration,
    /// The longest wait between two tries.
    pub longest: Duration,
}

impl StoreRetry {
    /// The app's: 15 s, doubling to 4 minutes.
    pub const APP: StoreRetry = StoreRetry {
        first: Duration::from_secs(15),
        longest: Duration::from_secs(4 * 60),
    };

    /// The wait before the next try, given the last wait and how long ago
    /// that try ended.
    fn next_wait(self, last: Option<(Duration, Duration)>) -> Duration {
        match last {
            Some((wait, since)) if since <= self.longest => {
                wait.saturating_mul(2).min(self.longest)
            }
            _ => self.first,
        }
    }
}

/// The current pipeline and the engine it was built with, swapped
/// together.
struct Current {
    pipeline: ProcessingPipeline,
    engine: BuiltEngine,
}

impl Current {
    fn new(
        built: BuiltPipeline,
        latch: &QuitLatch,
        in_flight: &InFlight,
        waits: &ModelWaits,
        live: &LiveRecording,
    ) -> Self {
        Current {
            pipeline: ProcessingPipeline::new(
                built
                    .dependencies
                    .with_quit_latch(latch.clone())
                    .with_in_flight(in_flight.clone())
                    .with_model_waits(waits.clone())
                    .with_live_recording(live.clone()),
            ),
            engine: built.engine,
        }
    }
}

/// The current pipeline behind a swap: a reload replaces it first, so a
/// Save never waits for a run; the retired pipeline is kept until idle,
/// so meetings in flight finish on their dependencies. Enqueues resolve
/// `current()` per call. Every pipeline it builds shares one
/// [`QuitLatch`], one [`InFlight`] set, one [`ModelWaits`] and one
/// [`LiveRecording`], so quits, claims, waits and the recorder's state
/// reach the retired ones too. A task of its own retries the meetings
/// waiting for the store ([`StoreRetry`]) until this value is dropped.
pub struct CurrentPipeline {
    current: Arc<Mutex<Current>>,
    make: MakeDependencies,
    runtime: tokio::runtime::Handle,
    quit_latch: QuitLatch,
    in_flight: InFlight,
    model_waits: ModelWaits,
    /// The recorder's state, which every build waits on before it runs an
    /// engine in this process.
    live_recording: LiveRecording,
    models_installed: ModelsInstalled,
    store_retry: tokio::task::JoinHandle<()>,
}

impl Drop for CurrentPipeline {
    fn drop(&mut self) {
        self.store_retry.abort();
    }
}

impl CurrentPipeline {
    /// The pipeline over `built`, reloading through `make`.
    #[must_use]
    pub fn new(
        built: BuiltPipeline,
        make: MakeDependencies,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let quit_latch = QuitLatch::default();
        let in_flight = InFlight::default();
        let model_waits = ModelWaits::default();
        let live_recording = LiveRecording::default();
        let current = Arc::new(Mutex::new(Current::new(
            built,
            &quit_latch,
            &in_flight,
            &model_waits,
            &live_recording,
        )));
        let store_retry = runtime.spawn(retry_the_store(
            Arc::downgrade(&current),
            model_waits.clone(),
            StoreRetry::APP,
        ));
        CurrentPipeline {
            current,
            make,
            runtime,
            quit_latch,
            in_flight,
            model_waits,
            live_recording,
            models_installed: Arc::new(|_| true),
            store_retry,
        }
    }

    /// Tries the store again on `retry` instead of [`StoreRetry::APP`].
    #[must_use]
    pub fn retrying_the_store(mut self, retry: StoreRetry) -> Self {
        self.store_retry.abort();
        self.store_retry = self.runtime.spawn(retry_the_store(
            Arc::downgrade(&self.current),
            self.model_waits.clone(),
            retry,
        ));
        self
    }

    /// [`resume_waiting`](Self::resume_waiting) starts the waiting meetings
    /// only while `installed` holds for the current runtime, so a partial
    /// install or a save with models still missing leaves their cards asking
    /// for the download. Without it every resume starts them.
    #[must_use]
    pub fn resuming_when(mut self, installed: ModelsInstalled) -> Self {
        self.models_installed = installed;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Current> {
        lock(&self.current)
    }

    #[must_use]
    pub fn current(&self) -> ProcessingPipeline {
        self.lock().pipeline.clone()
    }

    /// The current pipeline and its speech engine, read together.
    #[must_use]
    pub fn current_with_engine(&self) -> (ProcessingPipeline, BuiltEngine) {
        let current = self.lock();
        (current.pipeline.clone(), current.engine.clone())
    }

    /// [`ProcessingPipeline::resume_waiting`] on the current pipeline, from
    /// any thread, once its models are installed: the meetings any of its
    /// pipelines left `queued` for missing models. Called after a model
    /// install and after every [`reload`](Self::reload); returns at once
    /// while a model is missing or nothing waits. A failure is logged and the
    /// meetings stay `queued` for the next try.
    pub fn resume_waiting(&self) {
        let _entered = self.runtime.enter();
        let (pipeline, engine) = self.current_with_engine();
        if !(self.models_installed)(engine.runtime) {
            return;
        }
        match pipeline.resume_waiting() {
            Ok(resumed) if !resumed.is_empty() => {
                tracing::info!(count = resumed.len(), "resumed meetings waiting for models");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "meetings waiting for models could not be resumed");
            }
        }
    }

    /// Quits every pipeline this one built or builds from now on (see
    /// [`ProcessingPipeline::quit`]). Its mark removals sync the disk, so an
    /// async caller runs this on the blocking pool.
    pub fn quit(&self) {
        self.current().quit();
    }

    /// Whether [`quit`](Self::quit) ran: the app is shutting down.
    #[must_use]
    pub fn quitting(&self) -> bool {
        self.quit_latch.is_set()
    }

    /// The in-flight set every pipeline this one builds shares, which an
    /// update's install asks and holds.
    #[must_use]
    pub fn in_flight(&self) -> &InFlight {
        &self.in_flight
    }

    /// Whether a recording is under way, as every pipeline it builds sees
    /// it: no job runs an in-process engine while it is live.
    #[must_use]
    pub fn live_recording(&self) -> &LiveRecording {
        &self.live_recording
    }

    /// Replaces the pipeline with one built from the stored settings and the
    /// API key; a failed build keeps the current one. The speech engine and
    /// the diarizer are kept while the engine id runs where it did. Then it
    /// resumes the meetings waiting for models.
    pub fn reload(&self) -> Result<(), BuildError> {
        let replacement = Current::new(
            (self.make)()?,
            &self.quit_latch,
            &self.in_flight,
            &self.model_waits,
            &self.live_recording,
        );
        let retired = std::mem::replace(&mut *self.lock(), replacement).pipeline;
        self.runtime
            .spawn(async move { retired.wait_until_idle().await });
        self.resume_waiting();
        Ok(())
    }

    /// Claims the meeting on the current pipeline in place, so a refusal is
    /// the call's error, then spawns the operation; its failure reaches the
    /// detail as the `OperationFailed` event it posts.
    fn claim_and_spawn(
        &self,
        claim: impl FnOnce(&ProcessingPipeline) -> Result<Operation, PipelineFailure>,
    ) -> Result<(), PipelineFailure> {
        let operation = claim(&self.current())?;
        self.runtime.spawn(async move {
            let _ = operation.await;
        });
        Ok(())
    }
}

fn lock(current: &Mutex<Current>) -> std::sync::MutexGuard<'_, Current> {
    current
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The store's retry: waits until something waits for the store, then
/// `retry`'s wait, then tries on the current pipeline off the async
/// workers. Ends once the [`CurrentPipeline`] is gone or quits.
async fn retry_the_store(current: Weak<Mutex<Current>>, waits: ModelWaits, retry: StoreRetry) {
    let mut last = None::<(Duration, tokio::time::Instant)>;
    loop {
        waits.until_store_retry().await;
        let wait = retry.next_wait(last.map(|(wait, at)| (wait, at.elapsed())));
        tokio::time::sleep(wait).await;
        let Some(pipeline) = current
            .upgrade()
            .map(|current| lock(&current).pipeline.clone())
        else {
            return;
        };
        if pipeline.dependencies().quit_latch.is_set() {
            return;
        }
        match tokio::task::spawn_blocking(move || pipeline.resume_waiting_for_store()).await {
            Ok(Ok(resumed)) if !resumed.is_empty() => {
                tracing::info!(
                    count = resumed.len(),
                    "resumed meetings waiting for the database"
                );
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) if error.is_busy() => {
                tracing::info!(%error, "meetings waiting for the database could not be resumed yet");
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    %error,
                    "meetings waiting for the database could not be resumed; \
                     they stay queued for the next launch"
                );
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "a retry of the meetings waiting for the database ended early; \
                     they stay queued for the next launch"
                );
            }
        }
        last = Some((wait, tokio::time::Instant::now()));
    }
}

/// The host's `Pipeline`. A summary re-run and a re-export are claimed in
/// place, so a refusal is the call's error; the work then runs on the
/// runtime and the call returns, since the host calls with its state
/// locked and waiting here would park the runtime's workers. A later
/// failure is posted as `MeetingEvent::OperationFailed`.
/// `apply_retention` and "Process again" complete their writes in place,
/// so the detail read right after shows them.
pub struct HostPipeline {
    pub pipeline: Arc<CurrentPipeline>,
    pub sweep: RetentionSweep,
}

impl Pipeline for HostPipeline {
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> BoundaryResult<()> {
        Ok(self
            .pipeline
            .claim_and_spawn(|pipeline| pipeline.claim_rerun_summary(meeting_id, template_id))?)
    }

    fn redeliver(&self, meeting_id: Uuid) -> BoundaryResult<()> {
        Ok(self
            .pipeline
            .claim_and_spawn(|pipeline| pipeline.claim_redeliver(meeting_id))?)
    }

    fn process_again(&self, meeting_id: Uuid) -> Result<(), ProcessAgainRefusal> {
        // `reprocess` spawns on the current runtime; the host calls from its
        // own thread.
        let _runtime = self.pipeline.runtime.enter();
        self.pipeline
            .current()
            .process_again(meeting_id)
            .map_err(process_again_refusal)
    }

    fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> BoundaryResult<()> {
        let pipeline = self.pipeline.current();
        Ok(block_on(
            &self.pipeline.runtime,
            pipeline.apply_retention(meeting_id, rule),
        )?)
    }

    fn reload(&self) -> BoundaryResult<()> {
        Ok(self.pipeline.reload()?)
    }

    fn keep_all_recordings(&self) -> BoundaryResult<i64> {
        Ok(self
            .sweep
            .keep_all()
            .map(|count| i64::try_from(count).unwrap_or(i64::MAX))?)
    }
}

/// The host's reading of a refused [`ProcessingPipeline::process_again`]:
/// a queued or processing meeting is already being processed unless it
/// waits for its models or the store.
fn process_again_refusal(error: ReprocessError) -> ProcessAgainRefusal {
    match error {
        ReprocessError::MeetingNotFound(_) => ProcessAgainRefusal::MeetingGone,
        ReprocessError::Unfinished {
            state: MeetingStateKind::Queued | MeetingStateKind::Processing,
            ..
        }
        | ReprocessError::Busy(_) => ProcessAgainRefusal::Busy,
        ReprocessError::WaitingForModels(_) => ProcessAgainRefusal::ModelsMissing,
        ReprocessError::WaitingForStore(_) => ProcessAgainRefusal::StoreBusy,
        ReprocessError::Unfinished { .. } | ReprocessError::NotOffered(_) => {
            ProcessAgainRefusal::NotOffered
        }
        ReprocessError::NoAsset(_) | ReprocessError::AudioGone(_) => {
            ProcessAgainRefusal::RecordingGone
        }
        ReprocessError::Quitting => ProcessAgainRefusal::Quitting,
        ReprocessError::Pipeline(failure) => {
            ProcessAgainRefusal::CouldNotStart(failure.to_string())
        }
    }
}

/// The sweep at launch and after every processed meeting; a partial sweep
/// is logged at warn with a count, the paths at debug.
pub fn run_sweep(sweep: &RetentionSweep) {
    match sweep.run(Utc::now()) {
        Ok(_) => {}
        Err(SweepIncomplete::Files(failures)) => {
            tracing::warn!(
                count = failures.len(),
                "retention sweep could not remove every file"
            );
            for (path, error) in &failures {
                tracing::debug!(path = %path.display(), %error, "retention sweep kept a file");
            }
        }
        Err(SweepIncomplete::Store(error)) => {
            tracing::warn!(%error, "retention sweep could not read the store");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use steno_core::{
        AudioBuffer16k, Delivery, DeliveryDispatcher, LanguageTag, MeetingEvent, MeetingOperation,
        MeetingState, MeetingSummarizer, PipelineStage, RawSegment, SpeechEngine, Store,
        SummaryInput, SummaryOutput, async_trait,
        testing::{FakeSpeechEngine, sample_data},
    };
    use steno_pipeline::EventReceiver;

    use super::*;
    use crate::speech::SpeechEngines;
    use crate::testing::{
        PATIENCE, current_pipeline, eventually, fake_dependencies, on_own_thread, temp_store,
    };

    /// A summarizer that answers only once released, then fails.
    struct HeldSummarizer {
        release: Arc<tokio::sync::Notify>,
        asked: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl MeetingSummarizer for HeldSummarizer {
        async fn summarize(&self, _input: &SummaryInput) -> BoundaryResult<SummaryOutput> {
            self.asked.notify_one();
            self.release.notified().await;
            Err("released without a summary".into())
        }
    }

    /// A summarizer that panics.
    struct PanickingSummarizer;

    #[async_trait]
    impl MeetingSummarizer for PanickingSummarizer {
        async fn summarize(&self, _input: &SummaryInput) -> BoundaryResult<SummaryOutput> {
            panic!("the summarizer fell over");
        }
    }

    /// A dispatcher that delivers nothing until released, or panics.
    struct HeldDispatcher {
        release: Arc<tokio::sync::Notify>,
        asked: Arc<tokio::sync::Notify>,
        panics: bool,
    }

    #[async_trait]
    impl DeliveryDispatcher for HeldDispatcher {
        async fn deliver_all(&self, _meeting_id: Uuid) -> Vec<Delivery> {
            assert!(!self.panics, "the dispatcher fell over");
            self.asked.notify_one();
            self.release.notified().await;
            Vec::new()
        }
    }

    fn pipeline(
        store: &Arc<Store>,
        summarizer: Option<Arc<dyn MeetingSummarizer>>,
    ) -> HostPipeline {
        pipeline_over(
            fake_dependencies(store, "fake-engine").with_llm(None, summarizer),
            store,
        )
    }

    fn pipeline_over(dependencies: PipelineDependencies, store: &Arc<Store>) -> HostPipeline {
        HostPipeline {
            pipeline: current_pipeline(dependencies),
            sweep: RetentionSweep::new(store.clone()),
        }
    }

    fn held_re_exports(
        store: &Arc<Store>,
        panics: bool,
    ) -> (
        HostPipeline,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        let release = Arc::new(tokio::sync::Notify::new());
        let asked = Arc::new(tokio::sync::Notify::new());
        let mut dependencies = fake_dependencies(store, "fake-engine");
        dependencies.dispatcher = Arc::new(HeldDispatcher {
            release: release.clone(),
            asked: asked.clone(),
            panics,
        });
        (pipeline_over(dependencies, store), release, asked)
    }

    fn ready_meeting(store: &Store) -> steno_core::Meeting {
        let mut meeting = sample_data::meeting();
        meeting.state = MeetingState::Ready;
        store.save_meeting(&meeting).unwrap();
        meeting
    }

    fn held() -> (
        HeldSummarizer,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        let release = Arc::new(tokio::sync::Notify::new());
        let asked = Arc::new(tokio::sync::Notify::new());
        (
            HeldSummarizer {
                release: release.clone(),
                asked: asked.clone(),
            },
            release,
            asked,
        )
    }

    /// Calls one of the host's synchronous methods as the host does.
    fn call<T: Send + 'static>(
        f: impl FnOnce() -> BoundaryResult<T> + Send + 'static,
    ) -> Result<T, String> {
        on_own_thread(
            PATIENCE,
            "the call returned without waiting for the work",
            move || f().map_err(|error| error.to_string()),
        )
    }

    /// Waits until the held summarizer is asked.
    async fn reached(asked: &tokio::sync::Notify) {
        tokio::time::timeout(PATIENCE, asked.notified())
            .await
            .expect("the re-run reached the summarizer");
    }

    async fn next_event(
        receiver: &mut EventReceiver,
        wanted: impl Fn(&MeetingEvent) -> bool,
    ) -> MeetingEvent {
        tokio::time::timeout(PATIENCE, async {
            loop {
                let event = receiver.recv().await.unwrap();
                if wanted(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("the event was posted")
    }

    /// A file the sweep could not remove is counted at warn; its path is not.
    #[test]
    fn a_partial_sweep_warns_with_a_count_not_the_paths() {
        let (dir, store) = temp_store();
        let mut meeting = sample_data::meeting();
        meeting.state = MeetingState::Ready;
        // A directory where the master should be: removing it as a file fails.
        let master = dir.path().join("private-folder").join("audio.wav");
        std::fs::create_dir_all(master.join("inside")).unwrap();
        let asset = steno_core::AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: steno_core::paths::file_url(&master, false),
            format: steno_core::AudioFormat::Wav16kInt16,
            lanes: vec![steno_core::AudioLane::Mixed],
            sidecars_16k: std::collections::BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::KeepDays(1),
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        };
        store.save_meeting_with_asset(&meeting, &asset).unwrap();
        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        run_sweep(&RetentionSweep::new(store.clone()));
        let text = log.text();
        assert!(
            text.lines().any(
                |line| line.contains("retention sweep could not remove every file")
                    && line.contains("count=1")
            ),
            "{text}"
        );
        assert!(!text.contains("private-folder"), "{text}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_re_run_or_re_export_is_the_error_of_the_call() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let without_llm = pipeline(&store, None);
        let id = meeting.id;
        assert_eq!(
            call(move || without_llm.rerun_summary(id, "default")),
            Err("summarize: no LLM endpoint is configured".to_owned())
        );

        let (summarizer, release, asked) = held();
        let service = Arc::new(pipeline(&store, Some(Arc::new(summarizer))));
        let first = service.clone();
        assert_eq!(call(move || first.rerun_summary(id, "default")), Ok(()));
        reached(&asked).await;
        let busy = format!("meeting {id} is already being processed");
        let second = service.clone();
        assert_eq!(
            call(move || second.redeliver(id)),
            Err(format!("deliver: {busy}"))
        );
        let third = service.clone();
        assert_eq!(
            call(move || third.rerun_summary(id, "default")),
            Err(format!("summarize: {busy}"))
        );
        release.notify_one();

        let unknown = Uuid::new_v4();
        let fourth = service.clone();
        assert_eq!(
            call(move || fourth.redeliver(unknown)),
            Err(format!("deliver: meeting {unknown} not found"))
        );
    }

    fn phone_meeting() -> steno_core::Meeting {
        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        meeting.source = steno_core::MeetingSource::Phone;
        meeting.state = MeetingState::Recording;
        meeting
    }

    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../Tests/Fixtures/audio")
            .join(name)
    }

    /// The phone meeting of `upload`, kept by `retention`, processed through
    /// the real decoder.
    async fn process_phone_upload(
        service: &HostPipeline,
        store: &Store,
        meeting: &steno_core::Meeting,
        upload: &std::path::Path,
        retention: AudioRetention,
    ) -> MeetingState {
        let asset = steno_core::AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: steno_core::paths::file_url(upload, false),
            format: steno_core::AudioFormat::M4aAac,
            lanes: vec![steno_core::AudioLane::Mixed],
            sidecars_16k: std::collections::BTreeMap::new(),
            mixdown_url: None,
            retention,
            expires_at: None,
        };
        let pipeline = service.pipeline.current();
        pipeline.enqueue(meeting, &asset).unwrap();
        pipeline.wait_until_idle().await;
        store.meeting(meeting.id).unwrap().unwrap().state
    }

    /// A phone recording with no `moov` is salvaged and kept; the same file
    /// with another app's major brand fails, untouched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_killed_phone_recording_is_salvaged_into_a_ready_meeting() {
        let (dir, store) = temp_store();
        let service = pipeline_over(fake_dependencies(&store, "fake-engine"), &store);
        let killed = std::fs::read(fixture_path("speech-44k1-avaudiofile-killed.m4a")).unwrap();
        let meeting = phone_meeting();
        let upload = dir.path().join("killed.m4a");
        std::fs::write(&upload, &killed).unwrap();
        let state = process_phone_upload(
            &service,
            &store,
            &meeting,
            &upload,
            AudioRetention::KeepForever,
        )
        .await;
        assert_eq!(state, MeetingState::Ready);
        assert_eq!(
            std::fs::read(&upload).unwrap(),
            killed,
            "the recording is kept as it was"
        );
        let asset = store.asset(meeting.id).unwrap().unwrap();
        assert_eq!(asset.url, steno_core::paths::file_url(&upload, false));

        let mut other = killed.clone();
        other[8..12].copy_from_slice(b"isom");
        let second = phone_meeting();
        let upload = dir.path().join("other.m4a");
        std::fs::write(&upload, &other).unwrap();
        let state = process_phone_upload(
            &service,
            &store,
            &second,
            &upload,
            AudioRetention::KeepForever,
        )
        .await;
        assert!(matches!(state, MeetingState::Failed { .. }), "{state:?}");
        assert_eq!(
            std::fs::read(&upload).unwrap(),
            other,
            "the recording is kept as it was"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_run_that_fails_later_posts_the_failure_on_the_bus() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let (summarizer, release, asked) = held();
        let service = Arc::new(pipeline(&store, Some(Arc::new(summarizer))));
        let mut receiver = service.pipeline.current().dependencies().events.subscribe();
        let id = meeting.id;
        let caller = service.clone();
        assert_eq!(call(move || caller.rerun_summary(id, "default")), Ok(()));
        reached(&asked).await;
        release.notify_one();
        let event = next_event(&mut receiver, |event| {
            matches!(event, MeetingEvent::OperationFailed { .. })
        })
        .await;
        assert_eq!(
            event,
            MeetingEvent::OperationFailed {
                meeting_id: id,
                operation: MeetingOperation::SummaryRerun,
                stage: PipelineStage::Summarize,
                failure: "summarize: released without a summary".to_owned(),
            }
        );
        let again = service.clone();
        eventually("the meeting was released", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
        assert_eq!(call(move || again.redeliver(id)), Ok(()));
    }

    /// A re-export holds its meeting until it ends.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_running_re_export_holds_its_meeting_until_it_ends() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let id = meeting.id;
        let (service, release, asked) = held_re_exports(&store, false);
        let service = Arc::new(service);
        let first = service.clone();
        assert_eq!(call(move || first.redeliver(id)), Ok(()));
        reached(&asked).await;
        assert_eq!(service.pipeline.current().in_flight(), vec![id]);
        let busy = format!("meeting {id} is already being processed");
        let second = service.clone();
        assert_eq!(
            call(move || second.redeliver(id)),
            Err(format!("deliver: {busy}"))
        );
        release.notify_one();
        eventually("the re-export released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
    }

    /// A re-export on a retired pipeline still holds its meeting on the new
    /// one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn after_a_reload_a_meeting_the_retired_pipeline_re_exports_is_refused() {
        let (_dir, store) = temp_store();
        let id = ready_meeting(&store).id;
        let release = Arc::new(tokio::sync::Notify::new());
        let asked = Arc::new(tokio::sync::Notify::new());
        let make: MakeDependencies = {
            let (store, release, asked) = (store.clone(), release.clone(), asked.clone());
            Arc::new(move || {
                let mut dependencies = fake_dependencies(&store, "fake-engine");
                dependencies.dispatcher = Arc::new(HeldDispatcher {
                    release: release.clone(),
                    asked: asked.clone(),
                    panics: false,
                });
                Ok(crate::testing::built(dependencies))
            })
        };
        let service = Arc::new(HostPipeline {
            pipeline: Arc::new(CurrentPipeline::new(
                make().unwrap(),
                make,
                tokio::runtime::Handle::current(),
            )),
            sweep: RetentionSweep::new(store.clone()),
        });
        let first = service.clone();
        assert_eq!(call(move || first.redeliver(id)), Ok(()));
        reached(&asked).await;
        service.pipeline.reload().unwrap();
        assert_eq!(service.pipeline.current().in_flight(), vec![id]);
        let second = service.clone();
        assert_eq!(
            call(move || second.redeliver(id)),
            Err(format!("deliver: meeting {id} is already being processed"))
        );
        release.notify_one();
        eventually("the retired re-export released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
    }

    /// A panicking operation still posts `OperationFailed` and releases the
    /// meeting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_run_or_re_export_that_panics_posts_the_failure_and_releases_the_meeting() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let id = meeting.id;
        let rerun = Arc::new(pipeline(&store, Some(Arc::new(PanickingSummarizer))));
        let mut receiver = rerun.pipeline.current().dependencies().events.subscribe();
        let caller = rerun.clone();
        assert_eq!(call(move || caller.rerun_summary(id, "default")), Ok(()));
        let failed = |event: &MeetingEvent| matches!(event, MeetingEvent::OperationFailed { .. });
        assert_eq!(
            next_event(&mut receiver, failed).await,
            MeetingEvent::OperationFailed {
                meeting_id: id,
                operation: MeetingOperation::SummaryRerun,
                stage: PipelineStage::Summarize,
                failure: format!("summarize: {}", steno_pipeline::OPERATION_PANICKED),
            }
        );
        eventually("the re-run released its meeting", || {
            rerun.pipeline.current().in_flight().is_empty()
        })
        .await;

        let (reexport, _release, _asked) = held_re_exports(&store, true);
        let mut receiver = reexport
            .pipeline
            .current()
            .dependencies()
            .events
            .subscribe();
        let reexport = Arc::new(reexport);
        let caller = reexport.clone();
        assert_eq!(call(move || caller.redeliver(id)), Ok(()));
        assert_eq!(
            next_event(&mut receiver, failed).await,
            MeetingEvent::OperationFailed {
                meeting_id: id,
                operation: MeetingOperation::Reexport,
                stage: PipelineStage::Deliver,
                failure: format!("deliver: {}", steno_pipeline::OPERATION_PANICKED),
            }
        );
        eventually("the re-export released its meeting", || {
            reexport.pipeline.current().in_flight().is_empty()
        })
        .await;
    }

    /// Calls `process_again` from a thread outside the runtime.
    fn process_again(
        service: &Arc<HostPipeline>,
        meeting_id: Uuid,
    ) -> Result<(), ProcessAgainRefusal> {
        let service = service.clone();
        on_own_thread(PATIENCE, "process_again returned", move || {
            service.process_again(meeting_id)
        })
    }

    /// `meeting` saved in `state` with a six-second call under `audio`, and
    /// that duration.
    fn recorded_meeting(
        store: &Store,
        audio: &std::path::Path,
        state: MeetingState,
    ) -> (steno_core::Meeting, steno_core::AudioAsset) {
        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        meeting.state = state;
        meeting.duration = 6.0;
        let asset = steno_pipeline::fixtures::two_lane_call(
            audio,
            meeting.id,
            AudioRetention::KeepDays(30),
        )
        .unwrap();
        store.save_meeting_with_asset(&meeting, &asset).unwrap();
        (meeting, asset)
    }

    /// Each refusal of `reprocess` reaches the host as the detail's refusal.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_process_again_says_why() {
        let (dir, store) = temp_store();
        let (summarizer, release, asked) = held();
        let service = Arc::new(pipeline(&store, Some(Arc::new(summarizer))));
        let failed = || MeetingState::Failed {
            reason: "decode: unreadable".to_owned(),
        };

        assert_eq!(
            process_again(&service, Uuid::new_v4()),
            Err(ProcessAgainRefusal::MeetingGone)
        );

        let mut without_asset = sample_data::meeting();
        without_asset.id = Uuid::new_v4();
        without_asset.state = failed();
        store.save_meeting(&without_asset).unwrap();
        assert_eq!(
            process_again(&service, without_asset.id),
            Err(ProcessAgainRefusal::RecordingGone)
        );

        let (held_meeting, _) = recorded_meeting(&store, dir.path(), failed());
        let id = held_meeting.id;
        let caller = service.clone();
        assert_eq!(call(move || caller.rerun_summary(id, "default")), Ok(()));
        reached(&asked).await;
        assert_eq!(process_again(&service, id), Err(ProcessAgainRefusal::Busy));
        release.notify_one();
        eventually("the re-run released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;

        service.pipeline.quit();
        assert_eq!(
            process_again(&service, id),
            Err(ProcessAgainRefusal::Quitting)
        );

        assert_eq!(
            process_again_refusal(ReprocessError::Pipeline(PipelineFailure::new(
                PipelineStage::Decode,
                "the disk is full"
            ))),
            ProcessAgainRefusal::CouldNotStart("decode: the disk is full".to_owned())
        );
    }

    /// "Process again" by the meeting's state: the rule is checked before
    /// the files, and a refusal leaves the meeting as it was.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_refuses_a_meeting_it_is_not_offered_for() {
        let (dir, store) = temp_store();
        let service = Arc::new(pipeline(&store, None));
        let failed = MeetingState::Failed {
            reason: "decode: unreadable".to_owned(),
        };
        let mut refused = Vec::new();
        for (label, state, swept, refusal) in [
            (
                "ready",
                MeetingState::Ready,
                false,
                ProcessAgainRefusal::NotOffered,
            ),
            (
                "ready, swept",
                MeetingState::Ready,
                true,
                ProcessAgainRefusal::NotOffered,
            ),
            (
                "recording",
                MeetingState::Recording,
                false,
                ProcessAgainRefusal::NotOffered,
            ),
            (
                "queued",
                MeetingState::Queued,
                false,
                ProcessAgainRefusal::Busy,
            ),
            (
                "processing",
                MeetingState::Processing,
                false,
                ProcessAgainRefusal::Busy,
            ),
            (
                "failed, swept",
                failed,
                true,
                ProcessAgainRefusal::RecordingGone,
            ),
        ] {
            let (meeting, asset) = recorded_meeting(&store, dir.path(), state.clone());
            if swept {
                std::fs::remove_file(steno_core::paths::file_url_path(&asset.url).unwrap())
                    .unwrap();
            }
            assert_eq!(process_again(&service, meeting.id), Err(refusal), "{label}");
            refused.push((label, meeting.id, state));
        }
        service.pipeline.current().wait_until_idle().await;
        for (label, id, state) in refused {
            assert_eq!(meeting_state(&store, id), state, "{label}: unchanged");
        }
    }

    /// What every [`FakeSidecar`] shares: the children's counts and the gate
    /// the first transcription waits at.
    #[derive(Default)]
    struct Children {
        live: AtomicUsize,
        most: AtomicUsize,
        spawns: AtomicUsize,
        stops: AtomicUsize,
        gate_used: AtomicBool,
        entered: tokio::sync::Notify,
        open: tokio::sync::Notify,
    }

    impl Children {
        fn counts(&self) -> [usize; 4] {
            [&self.live, &self.most, &self.spawns, &self.stops].map(|n| n.load(Ordering::SeqCst))
        }
    }

    /// A speech engine that runs a pretend child: `prepare` or `transcribe`
    /// starts it, `release` stops it.
    struct FakeSidecar {
        inner: FakeSpeechEngine,
        children: Arc<Children>,
        child: AtomicBool,
    }

    impl FakeSidecar {
        fn new(children: &Arc<Children>) -> Self {
            FakeSidecar {
                inner: FakeSpeechEngine::default(),
                children: children.clone(),
                child: AtomicBool::new(false),
            }
        }

        fn start_child(&self) {
            if !self.child.swap(true, Ordering::SeqCst) {
                let live = self.children.live.fetch_add(1, Ordering::SeqCst) + 1;
                self.children.most.fetch_max(live, Ordering::SeqCst);
                self.children.spawns.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for FakeSidecar {
        fn id(&self) -> &str {
            self.inner.id()
        }

        fn supported_languages(&self) -> &std::collections::BTreeSet<LanguageTag> {
            self.inner.supported_languages()
        }

        async fn prepare(&self) -> BoundaryResult<()> {
            self.start_child();
            self.inner.prepare().await
        }

        async fn transcribe(
            &self,
            audio: &AudioBuffer16k,
            hint: Option<&LanguageTag>,
        ) -> BoundaryResult<Vec<RawSegment>> {
            self.start_child();
            let gate = &self.children;
            if !gate.gate_used.swap(true, Ordering::SeqCst) {
                gate.entered.notify_one();
                gate.open.notified().await;
            }
            self.inner.transcribe(audio, hint).await
        }

        async fn release(&self) -> BoundaryResult<()> {
            if self.child.swap(false, Ordering::SeqCst) {
                self.children.live.fetch_sub(1, Ordering::SeqCst);
                self.children.stops.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    /// A retired job and a new one both run on the one sidecar child through
    /// reloads, and only the retired job's end stops it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_while_a_job_transcribes_keeps_one_sidecar_child() {
        let (dir, store) = temp_store();
        let children = Arc::new(Children::default());
        let builds = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engines = Arc::new(SpeechEngines::with_builder(
            crate::speech::testing::setup(dir.path(), steno_speech::SpeechSettings::default()),
            Box::new({
                let (children, builds) = (children.clone(), builds.clone());
                move |runtime| -> Arc<dyn SpeechEngine> {
                    builds.lock().unwrap().push(runtime);
                    match runtime {
                        SpeechRuntime::OnnxSidecar => Arc::new(FakeSidecar::new(&children)),
                        SpeechRuntime::CoreMlInProcess => Arc::new(FakeSpeechEngine::default()),
                    }
                }
            }),
        ));
        // The runtime the stored engine id runs on, as the next build sees it.
        let runtime = Arc::new(std::sync::Mutex::new(SpeechRuntime::OnnxSidecar));
        let make: MakeDependencies = {
            let (store, engines, runtime) = (store.clone(), engines.clone(), runtime.clone());
            Arc::new(move || {
                let runtime = *runtime.lock().unwrap();
                Ok(BuiltPipeline {
                    dependencies: fake_dependencies(&store, "fake-engine")
                        .with_speech_engine(engines.engine(runtime)),
                    engine: BuiltEngine {
                        engine_id: "parakeet-v3".to_owned(),
                        runtime,
                    },
                })
            })
        };
        let current =
            CurrentPipeline::new(make().unwrap(), make, tokio::runtime::Handle::current());
        let switch_to = |next| {
            *runtime.lock().unwrap() = next;
            current.reload().unwrap();
            current.current()
        };

        let retired = current.current();
        let held = enqueue_call(dir.path(), &retired);
        tokio::time::timeout(PATIENCE, children.entered.notified())
            .await
            .expect("the first job is transcribing");

        let in_process = switch_to(SpeechRuntime::CoreMlInProcess);
        assert!(
            !same_engine(&in_process, &retired),
            "another runtime, another engine"
        );
        let back = switch_to(SpeechRuntime::OnnxSidecar);
        assert!(
            same_engine(&back, &retired),
            "back in the sidecar: the engine the retired job runs on"
        );
        let unchanged = switch_to(SpeechRuntime::OnnxSidecar);
        assert!(
            same_engine(&unchanged, &back),
            "a reload that keeps the runtime keeps the engine"
        );
        assert_eq!(
            *builds.lock().unwrap(),
            [SpeechRuntime::OnnxSidecar, SpeechRuntime::CoreMlInProcess],
            "one per runtime, each on its own"
        );

        let next = enqueue_call(dir.path(), &unchanged);
        unchanged.wait_until_idle().await;
        assert_eq!(meeting_state(&store, next), MeetingState::Ready);
        // [live, most at once, started, stopped]
        assert_eq!(
            children.counts(),
            [1, 1, 1, 0],
            "the new job ran on the retired job's child and left it running"
        );

        // The reload's wait took the retired job's handle: wait for the meeting
        // and the child instead.
        children.open.notify_one();
        eventually("the retired job finished and stopped the child", || {
            meeting_state(&store, held) == MeetingState::Ready
                && children.stops.load(Ordering::SeqCst) == 1
        })
        .await;
        assert_eq!(children.counts(), [0, 1, 1, 1]);
    }

    fn on_the_sidecar(dependencies: PipelineDependencies) -> BuiltPipeline {
        BuiltPipeline {
            dependencies,
            engine: BuiltEngine {
                engine_id: "parakeet-v3".to_owned(),
                runtime: SpeechRuntime::OnnxSidecar,
            },
        }
    }

    /// A reload shares the recorder's state with the retired build and the
    /// new one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_build_sees_the_recording_a_reload_too() {
        let (_dir, store) = temp_store();
        let make: MakeDependencies = {
            let store = store.clone();
            Arc::new(move || Ok(on_the_sidecar(fake_dependencies(&store, "fake-engine"))))
        };
        let current =
            CurrentPipeline::new(make().unwrap(), make, tokio::runtime::Handle::current());
        let retired = current.current();
        current.reload().unwrap();
        current.live_recording().set(true);
        assert!(
            retired.dependencies().live_recording.is_live(),
            "the retired build"
        );
        assert!(
            current.current().dependencies().live_recording.is_live(),
            "the reloaded build"
        );
    }

    /// A `CurrentPipeline` whose pipelines share `engine` behind one gate
    /// that refuses until the returned flag is set.
    fn gated_current(
        store: &Arc<Store>,
        engine: Arc<dyn SpeechEngine>,
    ) -> (Arc<AtomicBool>, CurrentPipeline) {
        let (installed, check) = crate::model_gate::testing::flag(false);
        let shared = steno_pipeline::SharedSpeechEngine::new(Arc::new(
            crate::model_gate::GatedSpeechEngine::new(engine, check.clone()),
        ));
        let events = steno_pipeline::MeetingEventBus::new();
        let make: MakeDependencies = {
            let store = store.clone();
            Arc::new(move || {
                let mut dependencies =
                    fake_dependencies(&store, "fake-engine").with_speech_engine(shared.clone());
                dependencies.events = events.clone();
                Ok(on_the_sidecar(dependencies))
            })
        };
        let current =
            CurrentPipeline::new(make().unwrap(), make, tokio::runtime::Handle::current())
                .resuming_when(Arc::new(move |_| check()));
        (installed, current)
    }

    /// Enqueues a two-lane call in `audio` on `pipeline`; its id.
    fn enqueue_call(audio: &std::path::Path, pipeline: &ProcessingPipeline) -> Uuid {
        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        let asset =
            steno_pipeline::fixtures::two_lane_call(audio, meeting.id, AudioRetention::KeepForever)
                .unwrap();
        pipeline.enqueue(&meeting, &asset).unwrap();
        meeting.id
    }

    fn meeting_state(store: &Store, id: Uuid) -> MeetingState {
        store.meeting(id).unwrap().unwrap().state
    }

    fn same_engine(a: &ProcessingPipeline, b: &ProcessingPipeline) -> bool {
        a.dependencies()
            .speech_engine
            .ptr_eq(&b.dependencies().speech_engine)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_export_returns_at_once_and_runs_in_the_background() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let service = pipeline(&store, None);
        let mut receiver = service.pipeline.current().dependencies().events.subscribe();
        let id = meeting.id;
        assert_eq!(call(move || service.redeliver(id)), Ok(()));
        let event = next_event(&mut receiver, |event| {
            matches!(event, MeetingEvent::Progress { .. })
        })
        .await;
        assert!(
            matches!(
                event,
                MeetingEvent::Progress { meeting_id, ref progress }
                    if meeting_id == id && progress.stage == PipelineStage::Deliver
            ),
            "{event:?}"
        );
    }

    /// A resume after a reload does not start again a meeting a retired
    /// pipeline still runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resume_after_a_reload_does_not_run_an_in_flight_meeting_twice() {
        let (dir, store) = temp_store();
        let children = Arc::new(Children::default());
        let sidecar = Arc::new(std::sync::Mutex::new(None::<Arc<FakeSidecar>>));
        let engines = Arc::new(SpeechEngines::with_builder(
            crate::speech::testing::setup(dir.path(), steno_speech::SpeechSettings::default()),
            Box::new({
                let (children, sidecar) = (children.clone(), sidecar.clone());
                move |_runtime| -> Arc<dyn SpeechEngine> {
                    let engine = Arc::new(FakeSidecar::new(&children));
                    *sidecar.lock().unwrap() = Some(engine.clone());
                    engine
                }
            }),
        ));
        let make: MakeDependencies = {
            let (store, engines) = (store.clone(), engines.clone());
            Arc::new(move || {
                Ok(on_the_sidecar(
                    fake_dependencies(&store, "fake-engine")
                        .with_speech_engine(engines.engine(SpeechRuntime::OnnxSidecar)),
                ))
            })
        };
        let current =
            CurrentPipeline::new(make().unwrap(), make, tokio::runtime::Handle::current());
        let retired = current.current();
        let held = enqueue_call(dir.path(), &retired);
        tokio::time::timeout(PATIENCE, children.entered.notified())
            .await
            .expect("the first job is transcribing");
        current.reload().unwrap();
        current.resume_waiting();
        children.open.notify_one();
        eventually("the meeting is ready and every run is done", || {
            meeting_state(&store, held) == MeetingState::Ready
                && current.current().in_flight().is_empty()
                && retired.in_flight().is_empty()
        })
        .await;
        current.current().wait_until_idle().await;
        let engine = sidecar.lock().unwrap().clone().unwrap();
        let after_held = engine.inner.transcriptions.count();
        let control = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, control), MeetingState::Ready);
        let one_run = engine.inner.transcriptions.count() - after_held;
        assert_eq!(
            after_held, one_run,
            "the in-flight meeting started again on the new pipeline"
        );
    }

    /// Models the `steno` command installed are picked up by the next reload.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_resumes_a_meeting_waiting_for_models_installed_meanwhile() {
        let (dir, store) = temp_store();
        let (installed, current) = gated_current(&store, Arc::new(FakeSpeechEngine::default()));
        let waiting = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);

        installed.store(true, Ordering::SeqCst);
        current.reload().unwrap();
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Ready);
        assert_eq!(current.model_waits.waiting(), Vec::<Uuid>::new());
    }

    /// A save while models are still missing starts no waiting meeting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_with_the_models_still_missing_leaves_the_waiting_meetings_waiting() {
        a_resume_with_the_models_still_missing_leaves_the_waiting_meetings_waiting(|current| {
            current.reload().unwrap();
        })
        .await;
    }

    /// Neither does an install that leaves another model missing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_install_with_a_model_still_missing_leaves_the_waiting_meetings_waiting() {
        a_resume_with_the_models_still_missing_leaves_the_waiting_meetings_waiting(
            CurrentPipeline::resume_waiting,
        )
        .await;
    }

    async fn a_resume_with_the_models_still_missing_leaves_the_waiting_meetings_waiting(
        resume: impl FnOnce(&CurrentPipeline),
    ) {
        let (dir, store) = temp_store();
        let (_installed, current) = gated_current(&store, Arc::new(FakeSpeechEngine::default()));
        let waiting = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        let mut events = current.current().dependencies().events.subscribe();

        resume(&current);
        current.current().wait_until_idle().await;
        let mut progress = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let steno_core::MeetingEvent::Progress { progress: p, .. } = event {
                progress.push(p.stage);
            }
        }
        assert_eq!(progress, Vec::<PipelineStage>::new(), "no run started");
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);
        assert_eq!(current.model_waits.waiting(), [waiting]);
    }

    /// "Process again" on a meeting waiting for models says to download them,
    /// and the meeting keeps waiting until an install resumes it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_on_a_meeting_waiting_for_models_says_to_download_them() {
        let (dir, store) = temp_store();
        let (installed, current) = gated_current(&store, Arc::new(FakeSpeechEngine::default()));
        let service = Arc::new(HostPipeline {
            pipeline: Arc::new(current),
            sweep: RetentionSweep::new(store.clone()),
        });
        let waiting = enqueue_call(dir.path(), &service.pipeline.current());
        service.pipeline.current().wait_until_idle().await;

        assert_eq!(
            process_again(&service, waiting),
            Err(ProcessAgainRefusal::ModelsMissing)
        );
        let parked = store.meeting(waiting).unwrap().unwrap();
        let room = steno_core::Speaker {
            id: steno_core::room_speaker_id(waiting),
            meeting_id: waiting,
            cluster_label: "Speaker 1".to_owned(),
            assignment: steno_core::SpeakerAssignment::Unknown,
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: None,
            cluster_confidence: 0.0,
        };
        store.replace_transcript(&parked, &[], &[room]).unwrap();
        assert_eq!(
            process_again(&service, waiting),
            Err(ProcessAgainRefusal::ModelsMissing),
            "kept incomplete, still waiting for the models"
        );
        service.pipeline.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);
        assert_eq!(service.pipeline.model_waits.waiting(), [waiting]);

        installed.store(true, Ordering::SeqCst);
        service.pipeline.resume_waiting();
        service.pipeline.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Ready);
    }

    /// A meeting left waiting before a reload is started by an install on
    /// the reload's pipeline.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_install_after_a_reload_resumes_the_meeting_the_retired_pipeline_left_waiting() {
        use steno_host::services::SpeechModels as _;
        let (dir, store) = temp_store();
        let (installed, check) = crate::model_gate::testing::flag(false);
        let make: MakeDependencies = {
            let store = store.clone();
            Arc::new(move || {
                let gated = crate::model_gate::GatedSpeechEngine::new(
                    Arc::new(FakeSpeechEngine::default()),
                    check.clone(),
                );
                Ok(on_the_sidecar(
                    fake_dependencies(&store, "fake-engine").with_speech_engine(
                        steno_pipeline::SharedSpeechEngine::new(Arc::new(gated)),
                    ),
                ))
            })
        };
        let current = Arc::new(CurrentPipeline::new(
            make().unwrap(),
            make,
            tokio::runtime::Handle::current(),
        ));
        let models = crate::model_gate::ResumingSpeechModels::resuming(
            steno_host::fakes::FakeSpeechModels::default(),
            current.clone(),
        );
        let waiting = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);
        current.reload().unwrap();
        installed.store(true, Ordering::SeqCst);
        models
            .download(steno_host::speech::ModelAsset::ParakeetV3, &mut |_, _| {})
            .unwrap();
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Ready);
    }

    /// Onboarding's Download of the missing speaker models resumes the
    /// waiting meeting once; a second Download processes nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_onboarding_rows_download_resumes_the_meeting_waiting_for_the_models() {
        use steno_bridge::{AssetIdParams, BridgeHost as _, BridgeTopic, BridgeWindow};
        use steno_host::speech::ModelAsset;
        let (dir, store) = temp_store();
        let (installed, check) = crate::model_gate::testing::flag(false);
        let engine = Arc::new(FakeSpeechEngine::default());
        let make: MakeDependencies = {
            let store = store.clone();
            let engine = engine.clone();
            Arc::new(move || {
                let gated =
                    crate::model_gate::GatedSpeechEngine::new(engine.clone(), check.clone());
                Ok(on_the_sidecar(
                    fake_dependencies(&store, "fake-engine").with_speech_engine(
                        steno_pipeline::SharedSpeechEngine::new(Arc::new(gated)),
                    ),
                ))
            })
        };
        let current = Arc::new(CurrentPipeline::new(
            make().unwrap(),
            make,
            tokio::runtime::Handle::current(),
        ));
        let waiting = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);

        let models = steno_host::fakes::FakeSpeechModels::default();
        models.set_installed(ModelAsset::ParakeetV3, None);
        models.set_on_download(move |_| installed.store(true, Ordering::SeqCst));
        let fakes = steno_host::fakes::FakeServices::new(Utc::now());
        let mut services = fakes.services();
        services.speech_models = Arc::new(crate::model_gate::ResumingSpeechModels::resuming(
            models,
            current.clone(),
        ));
        let host = steno_host::Host::new(
            store.clone(),
            services,
            steno_host::HostConfig {
                version: "0".to_owned(),
                zone: steno_host::labels::utc(),
                platform: steno_bridge::Platform::Macos,
                updates_managed: false,
                installed_bundle: false,
            },
        )
        .unwrap()
        .for_window(BridgeWindow::Onboarding);
        let row = host.snapshot(BridgeTopic::Onboarding).unwrap()["speechModels"].clone();
        assert_eq!(row["assetIDs"], serde_json::json!(["offlineDiarizer"]));
        let download = |asset_id: &str| {
            tokio::task::block_in_place(|| {
                host.settings_transcription_download(AssetIdParams {
                    asset_id: asset_id.to_owned(),
                })
            })
            .unwrap();
        };
        for asset_id in row["assetIDs"].as_array().unwrap() {
            download(asset_id.as_str().unwrap());
        }
        eventually(
            "the waiting meeting processed after the row's download",
            || meeting_state(&store, waiting) == MeetingState::Ready,
        )
        .await;
        assert_eq!(
            host.snapshot(BridgeTopic::Onboarding).unwrap()["speechModels"]["state"],
            "installed"
        );
        current.current().wait_until_idle().await;
        let processed = engine.transcriptions.count();
        assert!(processed > 0);
        download("offlineDiarizer");
        current.resume_waiting();
        current.current().wait_until_idle().await;
        assert_eq!(engine.transcriptions.count(), processed, "processed again");
        assert_eq!(meeting_state(&store, waiting), MeetingState::Ready);
    }

    /// A speech engine whose first transcription waits at a gate before it
    /// refuses for a missing model; later calls refuse at once.
    #[derive(Default)]
    struct RefusingAfterAGate {
        used: AtomicBool,
        entered: tokio::sync::Notify,
        open: tokio::sync::Notify,
        inner: FakeSpeechEngine,
    }

    #[async_trait]
    impl SpeechEngine for RefusingAfterAGate {
        fn id(&self) -> &str {
            self.inner.id()
        }

        fn supported_languages(&self) -> &std::collections::BTreeSet<LanguageTag> {
            self.inner.supported_languages()
        }

        async fn prepare(&self) -> BoundaryResult<()> {
            self.inner.prepare().await
        }

        async fn transcribe(
            &self,
            _audio: &AudioBuffer16k,
            _hint: Option<&LanguageTag>,
        ) -> BoundaryResult<Vec<RawSegment>> {
            if !self.used.swap(true, Ordering::SeqCst) {
                self.entered.notify_one();
                self.open.notified().await;
            }
            Err(Box::new(PipelineFailure::models_missing(
                PipelineStage::Transcribe,
            )))
        }
    }

    /// A refused run on a retired pipeline is started again on the current
    /// one, built from the latest settings.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_run_on_a_retired_pipeline_starts_its_meeting_again_on_the_current_one() {
        let (dir, store) = temp_store();
        let refusing = Arc::new(RefusingAfterAGate::default());
        let builds = Arc::new(AtomicUsize::new(0));
        let make: MakeDependencies = {
            let (store, refusing, builds) = (store.clone(), refusing.clone(), builds.clone());
            Arc::new(move || {
                let dependencies = fake_dependencies(&store, "fake-engine");
                Ok(on_the_sidecar(
                    if builds.fetch_add(1, Ordering::SeqCst) == 0 {
                        dependencies.with_speech_engine(steno_pipeline::SharedSpeechEngine::new(
                            refusing.clone(),
                        ))
                    } else {
                        dependencies
                    },
                ))
            })
        };
        let current =
            CurrentPipeline::new(make().unwrap(), make, tokio::runtime::Handle::current());
        let meeting = enqueue_call(dir.path(), &current.current());
        tokio::time::timeout(PATIENCE, refusing.entered.notified())
            .await
            .expect("the run is transcribing");
        current.reload().unwrap();
        current.resume_waiting();
        refusing.open.notify_one();
        eventually("the meeting is ready", || {
            meeting_state(&store, meeting) == MeetingState::Ready
        })
        .await;
        assert_eq!(current.model_waits.waiting(), Vec::<Uuid>::new());
    }

    /// An install's resume starts only meetings left waiting for models.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_install_resumes_no_queued_meeting_a_run_did_not_leave_waiting() {
        let (dir, store) = temp_store();
        let current = current_pipeline(fake_dependencies(&store, "fake-engine"));
        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        meeting.state = MeetingState::Queued;
        let asset = steno_pipeline::fixtures::two_lane_call(
            dir.path(),
            meeting.id,
            AudioRetention::KeepForever,
        )
        .unwrap();
        store.save_meeting_with_asset(&meeting, &asset).unwrap();

        current.resume_waiting();
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, meeting.id), MeetingState::Queued);

        assert_eq!(current.current().resume_unfinished().unwrap(), [meeting.id]);
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, meeting.id), MeetingState::Ready);
    }

    /// The fake engine, its first warm-up held until `open`.
    #[derive(Default)]
    struct HeldWarmUp {
        used: AtomicBool,
        entered: tokio::sync::Notify,
        open: tokio::sync::Notify,
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

        async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
            if !self.used.swap(true, Ordering::SeqCst) {
                self.entered.notify_one();
                self.open.notified().await;
            }
            self.inner.prepare().await
        }

        async fn transcribe(
            &self,
            audio: &AudioBuffer16k,
            hint: Option<&LanguageTag>,
        ) -> steno_core::protocols::BoundaryResult<Vec<RawSegment>> {
            self.inner.transcribe(audio, hint).await
        }
    }

    /// Another process's write lock on the store in `dir`.
    fn write_lock(dir: &std::path::Path) -> steno_core::testing::WriteLockHold {
        steno_core::testing::WriteLockHold::new(&dir.join("steno.sqlite"))
    }

    /// The store's retry within a test: tries every few milliseconds.
    const QUICK_RETRY: StoreRetry = StoreRetry {
        first: Duration::from_millis(20),
        longest: Duration::from_millis(80),
    };

    fn retrying(
        store: &Arc<Store>,
        engine: Arc<dyn SpeechEngine>,
        retry: StoreRetry,
    ) -> Arc<CurrentPipeline> {
        let dependencies = fake_dependencies(store, "fake-engine")
            .with_speech_engine(steno_pipeline::SharedSpeechEngine::new(engine));
        let make: MakeDependencies = {
            let dependencies = dependencies.clone();
            Arc::new(move || Ok(crate::testing::built(dependencies.clone())))
        };
        Arc::new(
            CurrentPipeline::new(
                crate::testing::built(dependencies),
                make,
                tokio::runtime::Handle::current(),
            )
            .retrying_the_store(retry),
        )
    }

    /// A run that meets a lock past the busy timeout leaves the meeting
    /// `queued`, and the retry processes it once the lock goes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_meeting_the_store_kept_locked_is_processed_by_the_retry_once_let_go() {
        let (dir, store) = temp_store();
        let engine = Arc::new(HeldWarmUp::default());
        let current = retrying(&store, engine.clone(), QUICK_RETRY);
        let mut events = current.current().dependencies().events.subscribe();
        let meeting = enqueue_call(dir.path(), &current.current());
        tokio::time::timeout(PATIENCE, engine.entered.notified())
            .await
            .expect("the run is warming up");
        let lock = write_lock(dir.path());
        engine.open.notify_one();
        tokio::time::timeout(Duration::from_secs(30), async {
            while events.recv().await.unwrap()
                != (MeetingEvent::StoreBusy {
                    meeting_id: meeting,
                })
            {}
        })
        .await
        .expect("the run left the meeting waiting for the store");
        // The retries meet the lock too.
        assert_eq!(meeting_state(&store, meeting), MeetingState::Queued);

        drop(lock);
        crate::testing::eventually_within(
            Duration::from_secs(30),
            "the retry processed the meeting",
            || meeting_state(&store, meeting) == MeetingState::Ready,
        )
        .await;
        current.current().wait_until_idle().await;
        assert_eq!(current.model_waits.waiting_for_store(), Vec::<Uuid>::new());
    }

    /// A resume the store refuses is retried, and the meeting processed once
    /// the lock goes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_install_resume_the_store_refuses_is_tried_again() {
        let (dir, store) = temp_store();
        let (installed, current) = gated_current(&store, Arc::new(FakeSpeechEngine::default()));
        let current = current.retrying_the_store(QUICK_RETRY);
        let gone = enqueue_call(dir.path(), &current.current());
        let mut later = sample_data::meeting();
        later.id = Uuid::new_v4();
        later.started_at += chrono::Duration::minutes(1);
        let asset = steno_pipeline::fixtures::two_lane_call(
            dir.path(),
            later.id,
            AudioRetention::KeepForever,
        )
        .unwrap();
        current.current().enqueue(&later, &asset).unwrap();
        current.current().wait_until_idle().await;
        let mut both = vec![gone, later.id];
        both.sort();
        assert_eq!(current.model_waits.waiting(), both);
        store
            .write(|transaction| {
                transaction.execute(
                    "DELETE FROM audioAsset WHERE meetingID = ?1",
                    [steno_core::json::uuid_string(gone)],
                )?;
                Ok(())
            })
            .unwrap();
        installed.store(true, Ordering::SeqCst);
        store
            .on_writer(|connection| connection.busy_timeout(Duration::ZERO))
            .unwrap();
        let lock = write_lock(dir.path());

        current.resume_waiting();
        assert_eq!(current.model_waits.waiting(), Vec::<Uuid>::new());
        // The resume met the lock before the meeting ran.
        assert_eq!(meeting_state(&store, later.id), MeetingState::Queued);
        drop(lock);
        eventually("the retry processed the meeting", || {
            meeting_state(&store, later.id) == MeetingState::Ready
        })
        .await;
        current.current().wait_until_idle().await;
        assert!(matches!(
            meeting_state(&store, gone),
            MeetingState::Failed { .. }
        ));
    }

    /// "Process again" on a meeting waiting for the store says what its card
    /// says and leaves it waiting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_on_a_meeting_waiting_for_the_store_says_so() {
        let (dir, store) = temp_store();
        let engine = Arc::new(HeldWarmUp::default());
        let service = Arc::new(HostPipeline {
            // No retry within the test.
            pipeline: retrying(
                &store,
                engine.clone(),
                StoreRetry {
                    first: Duration::from_secs(3600),
                    longest: Duration::from_secs(3600),
                },
            ),
            sweep: RetentionSweep::new(store.clone()),
        });
        store
            .on_writer(|connection| connection.busy_timeout(Duration::ZERO))
            .unwrap();
        let meeting = enqueue_call(dir.path(), &service.pipeline.current());
        tokio::time::timeout(PATIENCE, engine.entered.notified())
            .await
            .expect("the run is warming up");
        let _lock = write_lock(dir.path());
        engine.open.notify_one();
        eventually("the run left the meeting waiting for the store", || {
            service.pipeline.model_waits.waiting_for_store() == [meeting]
        })
        .await;

        assert_eq!(
            process_again(&service, meeting),
            Err(ProcessAgainRefusal::StoreBusy)
        );
        assert_eq!(service.pipeline.model_waits.waiting_for_store(), [meeting]);
        assert_eq!(meeting_state(&store, meeting), MeetingState::Queued);
    }

    /// The retry's waits double up to `longest` and start over after a quiet
    /// spell.
    #[test]
    fn the_store_retry_doubles_its_wait_and_starts_over_after_a_quiet_spell() {
        let retry = StoreRetry::APP;
        let secs = Duration::from_secs;
        assert_eq!(retry.next_wait(None), secs(15));
        assert_eq!(retry.next_wait(Some((secs(15), secs(1)))), secs(30));
        assert_eq!(retry.next_wait(Some((secs(120), secs(1)))), secs(240));
        assert_eq!(retry.next_wait(Some((secs(240), secs(1)))), secs(240));
        assert_eq!(
            retry.next_wait(Some((secs(60), secs(240)))),
            secs(120),
            "a spell as long as the longest wait is not quiet"
        );
        assert_eq!(retry.next_wait(Some((secs(240), secs(241)))), secs(15));
    }

    /// The retry backs off while the lock holds: at most 7 tries fit in 4 s
    /// with `first` at 20 ms, where no backoff makes dozens.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_store_retry_backs_off_while_the_lock_refuses_it() {
        let (dir, store) = temp_store();
        let engine = Arc::new(HeldWarmUp::default());
        let current = retrying(
            &store,
            engine.clone(),
            StoreRetry {
                first: Duration::from_millis(20),
                longest: Duration::from_secs(60),
            },
        );
        let mut events = current.current().dependencies().events.subscribe();
        store
            .on_writer(|connection| connection.busy_timeout(Duration::ZERO))
            .unwrap();
        let meeting = enqueue_call(dir.path(), &current.current());
        tokio::time::timeout(PATIENCE, engine.entered.notified())
            .await
            .expect("the run is warming up");
        let lock = write_lock(dir.path());
        engine.open.notify_one();
        let busy = MeetingEvent::StoreBusy {
            meeting_id: meeting,
        };
        tokio::time::timeout(PATIENCE, async {
            while events.recv().await.unwrap() != busy {}
        })
        .await
        .expect("the first park");
        let mut parks = 0;
        let window = tokio::time::sleep(Duration::from_secs(4));
        tokio::pin!(window);
        loop {
            tokio::select! {
                () = &mut window => break,
                event = events.recv() => match event {
                    Ok(event) if event == busy => parks += 1,
                    Ok(_) => {}
                    // Lagged behind: far more events than a backed-off retry posts.
                    Err(_) => parks += 100,
                },
            }
        }
        assert!(parks >= 2, "the retry kept trying: {parks}");
        assert!(parks <= 7, "the retry backed off: {parks} tries in 4 s");
        drop(lock);
        crate::testing::eventually_within(
            Duration::from_secs(60),
            "the retry processed the meeting",
            || meeting_state(&store, meeting) == MeetingState::Ready,
        )
        .await;
        current.current().wait_until_idle().await;
    }
}
