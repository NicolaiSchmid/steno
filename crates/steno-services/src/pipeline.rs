//! The host's `Pipeline` over the processing pipeline and the retention
//! sweep, with the reload the Settings commands ask for. Swift:
//! `AppEnvironment.reloadPipeline`, `runRetentionSweep`, and the
//! `Task`s `MeetingDetailViewModel` ran the re-runs in.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use steno_core::AudioRetention;
use steno_host::services::Pipeline;
use steno_pipeline::{Operation, PipelineDependencies, ProcessingPipeline, RetentionSweep};
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
    /// is the call's error, then runs the claimed operation on the
    /// runtime in the background; its failure reaches the window as the
    /// `OperationFailed` event it posts.
    fn start<E: std::fmt::Display>(
        &self,
        claim: impl FnOnce(&ProcessingPipeline) -> Result<Operation, E>,
    ) -> Result<(), String> {
        let operation = claim(&self.current()).map_err(|failure| failure.to_string())?;
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
/// here would park the runtime's workers on that lock (the event loop,
/// the flush timer and the poll all take it) until nobody is left to
/// drive the request. A failure after the claim is posted as
/// `MeetingEvent::OperationFailed`. `apply_retention` touches the store
/// only and completes in place, so the detail the host reads back right
/// after already shows the new rule.
pub struct HostPipeline {
    pub pipeline: Arc<CurrentPipeline>,
    pub sweep: RetentionSweep,
}

impl Pipeline for HostPipeline {
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> Result<(), String> {
        self.pipeline
            .start(|pipeline| pipeline.claim_rerun_summary(meeting_id, template_id))
    }

    fn redeliver(&self, meeting_id: Uuid) -> Result<(), String> {
        self.pipeline
            .start(|pipeline| pipeline.claim_redeliver(meeting_id))
    }

    fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> Result<(), String> {
        let pipeline = self.pipeline.current();
        block_on(
            &self.pipeline.runtime,
            pipeline.apply_retention(meeting_id, rule),
        )
        .map_err(|error| error.to_string())
    }

    fn reload(&self) -> Result<(), String> {
        self.pipeline.reload().map_err(|error| error.to_string())
    }

    fn keep_all_recordings(&self) -> Result<i64, String> {
        self.sweep
            .keep_all()
            .map(|count| i64::try_from(count).unwrap_or(i64::MAX))
            .map_err(|error| error.to_string())
    }
}

/// The sweep at launch and after every processed meeting; a partial sweep
/// is logged, not fatal.
pub fn run_sweep(sweep: &RetentionSweep) {
    if let Err(error) = sweep.run(Utc::now()) {
        tracing::warn!(%error, "retention sweep incomplete");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use steno_core::{
        MeetingEvent, MeetingOperation, MeetingState, MeetingSummarizer, PipelineStage, Store,
        SummaryInput, SummaryOutput, async_trait, protocols::BoundaryResult, testing::sample_data,
    };
    use steno_pipeline::EventReceiver;

    use super::*;
    use crate::testing::{fake_dependencies, temp_store};

    /// How long a test waits for background work before it fails.
    const PATIENCE: Duration = Duration::from_secs(5);

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

    fn pipeline(
        store: &Arc<Store>,
        summarizer: Option<Arc<dyn MeetingSummarizer>>,
        runtime: tokio::runtime::Handle,
    ) -> HostPipeline {
        let dependencies = fake_dependencies(store, "fake-engine").with_llm(None, summarizer);
        let make: MakeDependencies = {
            let dependencies = dependencies.clone();
            Arc::new(move || Ok(dependencies.clone()))
        };
        HostPipeline {
            pipeline: Arc::new(CurrentPipeline::new(
                ProcessingPipeline::new(dependencies),
                make,
                runtime,
            )),
            sweep: RetentionSweep::new(store.clone()),
        }
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

    /// Calls the host's synchronous method on a thread of its own, as the
    /// host does, failing the test if it does not return in time. Not a
    /// `spawn_blocking` task: the runtime waits for those when it shuts
    /// down, so a call stuck on the work would hang the test instead of
    /// failing it.
    async fn call<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = sender.send(f());
        });
        tokio::time::timeout(PATIENCE, receiver)
            .await
            .expect("the call returned without waiting for the work")
            .unwrap()
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_summary_re_run_returns_before_the_summarizer_answers() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let (summarizer, release, asked) = held();
        let service = pipeline(
            &store,
            Some(Arc::new(summarizer)),
            tokio::runtime::Handle::current(),
        );

        let returned = call(move || service.rerun_summary(meeting.id, &meeting.template_id)).await;
        assert_eq!(returned, Ok(()));
        // The run was started: the summarizer is being asked.
        tokio::time::timeout(PATIENCE, asked.notified())
            .await
            .expect("the re-run reached the summarizer");
        release.notify_one();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_re_run_or_re_export_is_the_call_s_error() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let without_llm = pipeline(&store, None, tokio::runtime::Handle::current());
        let id = meeting.id;
        assert_eq!(
            call(move || without_llm.rerun_summary(id, "default")).await,
            Err("summarize: no LLM endpoint is configured".to_owned())
        );

        let (summarizer, release, asked) = held();
        let service = Arc::new(pipeline(
            &store,
            Some(Arc::new(summarizer)),
            tokio::runtime::Handle::current(),
        ));
        let first = service.clone();
        assert_eq!(
            call(move || first.rerun_summary(id, "default")).await,
            Ok(())
        );
        tokio::time::timeout(PATIENCE, asked.notified())
            .await
            .expect("the re-run reached the summarizer");
        let busy = format!("meeting {id} is already being processed");
        let second = service.clone();
        assert_eq!(
            call(move || second.redeliver(id)).await,
            Err(format!("deliver: {busy}"))
        );
        let third = service.clone();
        assert_eq!(
            call(move || third.rerun_summary(id, "default")).await,
            Err(format!("summarize: {busy}"))
        );
        release.notify_one();

        let unknown = Uuid::new_v4();
        let fourth = service.clone();
        assert_eq!(
            call(move || fourth.redeliver(unknown)).await,
            Err(format!("deliver: meeting {unknown} not found"))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_run_that_fails_later_posts_the_failure_on_the_bus() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let (summarizer, release, asked) = held();
        let service = Arc::new(pipeline(
            &store,
            Some(Arc::new(summarizer)),
            tokio::runtime::Handle::current(),
        ));
        let mut receiver = service.pipeline.current().dependencies().events.subscribe();
        let id = meeting.id;
        let caller = service.clone();
        assert_eq!(
            call(move || caller.rerun_summary(id, "default")).await,
            Ok(())
        );
        tokio::time::timeout(PATIENCE, asked.notified())
            .await
            .expect("the re-run reached the summarizer");
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
                reason: "summarize: released without a summary".to_owned(),
            }
        );
        // The meeting is released for the next operation.
        let again = service.clone();
        tokio::time::timeout(PATIENCE, async {
            while !service.pipeline.current().in_flight().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the meeting was released");
        assert_eq!(call(move || again.redeliver(id)).await, Ok(()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_export_returns_at_once_and_runs_in_the_background() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let service = pipeline(&store, None, tokio::runtime::Handle::current());
        let mut receiver = service.pipeline.current().dependencies().events.subscribe();
        let id = meeting.id;
        assert_eq!(call(move || service.redeliver(id)).await, Ok(()));
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
