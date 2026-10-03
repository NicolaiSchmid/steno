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
    Operation, PipelineDependencies, PipelineFailure, ProcessingPipeline, RetentionSweep,
    SweepIncomplete,
};
use uuid::Uuid;

use crate::app::BuildError;
use crate::block_on;

/// Rebuilds the dependencies from the stored settings and the API key.
pub type MakeDependencies = Arc<dyn Fn() -> Result<PipelineDependencies, BuildError> + Send + Sync>;

/// The current pipeline behind a swap: a reload replaces it first, so a
/// Save never waits for a run in progress; the retired pipeline is kept
/// until it is idle, so meetings in flight finish on the dependencies they
/// started with. Everything that enqueues resolves `current()` per call,
/// so a recording that ends after a reload goes through the new one.
/// Held by `App`, the recorder, the phone intake and [`HostPipeline`].
pub struct CurrentPipeline {
    current: Mutex<ProcessingPipeline>,
    make: MakeDependencies,
    runtime: tokio::runtime::Handle,
}

impl CurrentPipeline {
    #[must_use]
    pub fn new(
        pipeline: ProcessingPipeline,
        make: MakeDependencies,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        CurrentPipeline {
            current: Mutex::new(pipeline),
            make,
            runtime,
        }
    }

    #[must_use]
    pub fn current(&self) -> ProcessingPipeline {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replaces the pipeline with one built from the stored settings and
    /// the secret store's API key.
    pub fn reload(&self) -> Result<(), BuildError> {
        let replacement = ProcessingPipeline::new((self.make)()?);
        let retired = std::mem::replace(
            &mut *self
                .current
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            replacement,
        );
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

    use steno_core::{
        Delivery, DeliveryDispatcher, MeetingEvent, MeetingOperation, MeetingState,
        MeetingSummarizer, PipelineStage, Store, SummaryInput, SummaryOutput, async_trait,
        testing::sample_data,
    };
    use steno_pipeline::EventReceiver;

    use super::*;
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
        let log = crate::testing::CapturedLog::warnings();
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
