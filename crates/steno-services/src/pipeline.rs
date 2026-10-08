//! The host's `Pipeline` over the processing pipeline and the retention
//! sweep, with the reload the Settings commands ask for. Swift:
//! `AppEnvironment.reloadPipeline`, `runRetentionSweep`, and the
//! `Task`s `MeetingDetailViewModel` ran the re-runs in.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use steno_core::AudioRetention;
use steno_core::protocols::BoundaryResult;
use steno_host::services::Pipeline;
use steno_pipeline::{
    ExportRetries, InFlight, Operation, PipelineDependencies, PipelineFailure, ProcessingPipeline,
    QuitLatch, RetentionSweep, SweepIncomplete,
};
use steno_speech::SpeechRuntime;
use uuid::Uuid;

use crate::app::BuildError;
use crate::block_on;

/// The speech engine a pipeline was built with: the engine id the
/// settings named at the build and where [`SpeechSetup::runtime`] runs it.
/// The recorder's warm-up reads it from [`CurrentPipeline`], not the id
/// stored now (Rust only: Swift asked about the stored id).
///
/// [`SpeechSetup::runtime`]: crate::speech::SpeechSetup::runtime
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltEngine {
    /// The stored engine id the build read.
    pub engine_id: String,
    /// Where the pipeline's speech engine runs.
    pub runtime: SpeechRuntime,
}

/// One pipeline's dependencies and the speech engine they hold.
pub struct BuiltPipeline {
    /// What `ProcessingPipeline::new` takes.
    pub dependencies: PipelineDependencies,
    /// Which speech engine `dependencies` hold and where it runs.
    pub engine: BuiltEngine,
}

/// Rebuilds the dependencies, and the engine they hold, from the stored
/// settings and the API key.
pub type MakeDependencies = Arc<dyn Fn() -> Result<BuiltPipeline, BuildError> + Send + Sync>;

/// The pipeline [`CurrentPipeline`] holds, with the engine it was built
/// with, swapped together.
struct Current {
    pipeline: ProcessingPipeline,
    engine: BuiltEngine,
}

impl Current {
    /// The pipeline over `built`, sharing `latch` and `in_flight`.
    fn new(built: BuiltPipeline, latch: &QuitLatch, in_flight: &InFlight) -> Self {
        Current {
            pipeline: ProcessingPipeline::new(
                built
                    .dependencies
                    .with_quit_latch(latch.clone())
                    .with_in_flight(in_flight.clone()),
            ),
            engine: built.engine,
        }
    }
}

/// The current pipeline behind a swap: a reload replaces it first, so a
/// Save never waits for a run in progress; the retired pipeline is kept
/// until it is idle, so meetings in flight finish on the dependencies they
/// started with. Everything that enqueues resolves `current()` per call,
/// so a recording that ends after a reload goes through the new one.
/// Every pipeline it builds shares one [`QuitLatch`], so
/// [`quit`](Self::quit) reaches the retired ones too, and one in-flight
/// set ([`InFlight`]), so the new one refuses a meeting a retired one
/// still holds. Held by `App`, the recorder, the phone intake and
/// [`HostPipeline`].
pub struct CurrentPipeline {
    current: Mutex<Current>,
    make: MakeDependencies,
    runtime: tokio::runtime::Handle,
    quit_latch: QuitLatch,
    in_flight: InFlight,
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
        CurrentPipeline {
            current: Mutex::new(Current::new(built, &quit_latch, &in_flight)),
            make,
            runtime,
            quit_latch,
            in_flight,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Current> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The pipeline new work goes to.
    #[must_use]
    pub fn current(&self) -> ProcessingPipeline {
        self.lock().pipeline.clone()
    }

    /// The current pipeline and the speech engine it was built with, read
    /// together, so a reload in between cannot pair one pipeline with
    /// another's engine.
    #[must_use]
    pub fn current_with_engine(&self) -> (ProcessingPipeline, BuiltEngine) {
        let current = self.lock();
        (current.pipeline.clone(), current.engine.clone())
    }

    /// Quits every pipeline this one built or builds from now on, the
    /// current one, the retired ones still finishing and a reload's
    /// replacement: see [`ProcessingPipeline::quit`].
    pub fn quit(&self) {
        self.quit_latch.set();
    }

    /// Replaces the pipeline with one built from the stored settings and
    /// the secret store's API key. A failed build keeps the current
    /// pipeline and its engine. The app's builds keep the speech engine,
    /// with its claims, while the stored engine id runs where it did, and
    /// the diarizer ([`SpeechEngines`](crate::speech::SpeechEngines)), so
    /// the retired pipeline's jobs and the new one's share them and the
    /// one sidecar child.
    pub fn reload(&self) -> Result<(), BuildError> {
        let replacement = Current::new((self.make)()?, &self.quit_latch, &self.in_flight);
        let retired = std::mem::replace(&mut *self.lock(), replacement).pipeline;
        self.runtime
            .spawn(async move { retired.wait_until_idle().await });
        Ok(())
    }

    /// Claims the meeting on the current pipeline in place, so a refusal
    /// is the call's error, then spawns the claimed operation on the
    /// runtime; its failure reaches the detail's error line as the
    /// `OperationFailed` event it posts.
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

/// The host's `Pipeline`. A summary re-run and a re-export are checked
/// and claimed in place, so a refusal (the meeting is busy, no LLM is set
/// up) comes back as the call's error, as the Swift detail saw it; the
/// work itself then runs on the runtime and the call returns. The host
/// calls with its state locked, and both talk to the network, so waiting
/// here would park the runtime's workers on that lock (the event loop and
/// the poll take it) until nobody is left to drive the request. A failure
/// after the claim is posted as `MeetingEvent::OperationFailed`, which the
/// host shows on the detail's error line. `apply_retention` touches the store
/// only and completes in place, so the detail the host reads back right
/// after already shows the new rule.
pub struct HostPipeline {
    pub pipeline: Arc<CurrentPipeline>,
    pub sweep: RetentionSweep,
    /// Reset by any re-export the user causes, and read for the detail's
    /// "keeps failing" line.
    pub export_retries: Arc<ExportRetries>,
}

impl Pipeline for HostPipeline {
    /// A summary re-run exports the new summary, so, once the meeting is
    /// claimed, it resets the count of launch re-exports as `redeliver`
    /// does.
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> BoundaryResult<()> {
        self.pipeline
            .claim_and_spawn(|pipeline| pipeline.claim_rerun_summary(meeting_id, template_id))?;
        self.export_retries.reset(meeting_id);
        Ok(())
    }

    /// Any re-export the user causes (Export again, or the re-export after
    /// a speaker change), which starts the meeting's count of launch
    /// re-exports from 0 once the meeting is claimed: a refused one leaves
    /// the count.
    fn redeliver(&self, meeting_id: Uuid) -> BoundaryResult<()> {
        self.pipeline
            .claim_and_spawn(|pipeline| pipeline.claim_redeliver(meeting_id))?;
        self.export_retries.reset(meeting_id);
        Ok(())
    }

    fn export_keeps_failing(&self, meeting_id: Uuid) -> bool {
        self.export_retries.stopped(meeting_id)
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

/// The sweep at launch and after every processed meeting; a partial sweep
/// is logged, not fatal. Warn says how many files stayed; their paths,
/// which name the meetings' folders, go to debug.
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
            export_retries: Arc::new(ExportRetries::in_memory()),
        }
    }

    /// A pipeline whose re-exports go through a [`HeldDispatcher`].
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

    /// Calls one of the host's synchronous methods as the host does; the
    /// error as the detail shows it.
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

    /// The next event matching `wanted`, failing the test after a while.
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

    /// A file the sweep could not remove is counted at warn; its path,
    /// which names the meeting's folder, is not.
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
    async fn a_summary_re_run_returns_before_the_summarizer_answers() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let (summarizer, release, asked) = held();
        let service = pipeline(&store, Some(Arc::new(summarizer)));

        let returned = call(move || service.rerun_summary(meeting.id, &meeting.template_id));
        assert_eq!(returned, Ok(()));
        // The run was started: the summarizer is being asked.
        reached(&asked).await;
        release.notify_one();
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

    /// A summary re-run exports the new summary, so it starts the count of
    /// failed launch re-exports from 0 as Export again does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_run_resets_the_failed_launch_re_exports() {
        let (dir, store) = temp_store();
        let id = ready_meeting(&store).id;
        let path = dir.path().join(ExportRetries::FILE_NAME);
        std::fs::write(&path, format!("{{\"{id}\": {}}}", ExportRetries::LIMIT)).unwrap();
        let (summarizer, release, asked) = held();
        let mut service = pipeline(&store, Some(Arc::new(summarizer)));
        service.export_retries = Arc::new(ExportRetries::new(&path));
        let service = Arc::new(service);
        let first = service.clone();
        assert_eq!(call(move || first.rerun_summary(id, "default")), Ok(()));
        assert!(!service.export_keeps_failing(id));
        assert_eq!(ExportRetries::new(&path).count(id), 0);
        reached(&asked).await;
        release.notify_one();
        eventually("the re-run released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
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
        // The meeting is released for the next operation.
        let again = service.clone();
        eventually("the meeting was released", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
        assert_eq!(call(move || again.redeliver(id)), Ok(()));
    }

    /// A re-export holds its meeting until it ends: while it runs, a
    /// second re-export and a re-run are refused; once it ends, the
    /// meeting is free.
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

    /// A reload builds its pipeline from fresh dependencies, yet a re-export
    /// the retired pipeline is still running holds its meeting on the new
    /// one too: a second re-export is refused instead of running alongside.
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
            export_retries: Arc::new(ExportRetries::in_memory()),
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

    /// The user's Export again starts the meeting's count of failed launch
    /// re-exports from 0, on disk too, so the detail no longer says the
    /// export keeps failing and the next launch retries it. One refused
    /// because the meeting is busy leaves the count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn export_again_resets_the_failed_launch_re_exports() {
        let (dir, store) = temp_store();
        let id = ready_meeting(&store).id;
        let path = dir.path().join(ExportRetries::FILE_NAME);
        std::fs::write(&path, format!("{{\"{id}\": {}}}", ExportRetries::LIMIT)).unwrap();
        let (mut service, release, asked) = held_re_exports(&store, false);
        service.export_retries = Arc::new(ExportRetries::new(&path));
        let service = Arc::new(service);
        assert!(service.export_keeps_failing(id));

        let busy = service.pipeline.current().claim_redeliver(id).unwrap();
        let refused = service.clone();
        assert!(call(move || refused.redeliver(id)).is_err());
        assert!(
            service.export_keeps_failing(id),
            "a refused one resets nothing"
        );
        assert_eq!(ExportRetries::new(&path).count(id), ExportRetries::LIMIT);
        drop(busy);

        let first = service.clone();
        assert_eq!(call(move || first.redeliver(id)), Ok(()));
        assert!(!service.export_keeps_failing(id));
        assert_eq!(ExportRetries::new(&path).count(id), 0);
        reached(&asked).await;
        release.notify_one();
        eventually("the re-export released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
    }

    /// A claimed operation that panics still reports: `OperationFailed`
    /// with its stage, and the meeting is released.
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

    /// What every [`FakeSidecar`] of a test shares: the children (alive
    /// now, the most alive at once, started and stopped) and the gate the
    /// first transcription waits at for `open` after notifying `entered`.
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

    /// A speech engine that runs a pretend child, as the sidecar engine
    /// does: `prepare` or `transcribe` starts it when none runs, `release`
    /// stops it.
    struct FakeSidecar {
        inner: FakeSpeechEngine,
        children: Arc<Children>,
        child: AtomicBool,
    }

    impl FakeSidecar {
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

    /// A job on a pipeline a reload retired still transcribes in the
    /// sidecar while a reload goes to the in-process engine and back and a
    /// job on the new pipeline transcribes too: both run on the one
    /// sidecar engine and its one child, the new job's end does not stop
    /// the child under the retired one's, and the retired job's end stops
    /// it once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_while_a_job_transcribes_keeps_one_sidecar_child() {
        let (dir, store) = temp_store();
        let children = Arc::new(Children::default());
        // The runtime of each build, in order.
        let builds = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engines = Arc::new(SpeechEngines::with_builder(
            crate::speech::testing::setup(dir.path(), steno_speech::SpeechSettings::default()),
            Box::new({
                let (children, builds) = (children.clone(), builds.clone());
                move |runtime| -> Arc<dyn SpeechEngine> {
                    builds.lock().unwrap().push(runtime);
                    match runtime {
                        SpeechRuntime::OnnxSidecar => Arc::new(FakeSidecar {
                            inner: FakeSpeechEngine::default(),
                            children: children.clone(),
                            child: AtomicBool::new(false),
                        }),
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

        // The reload's own wait took the retired job's handle, so
        // `retired.wait_until_idle` would return at once: wait for the
        // meeting and the child instead.
        children.open.notify_one();
        eventually("the retired job finished and stopped the child", || {
            meeting_state(&store, held) == MeetingState::Ready
                && children.stops.load(Ordering::SeqCst) == 1
        })
        .await;
        assert_eq!(children.counts(), [0, 1, 1, 1]);
    }

    /// Enqueues a two-lane call in `audio` on `pipeline` as a meeting of
    /// its own; its id.
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

    /// Whether `a` and `b` run on the same speech engine and its claims.
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
        // The re-export ran: its one stage posted progress.
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
}
