//! The host's `Pipeline` over the processing pipeline and the retention
//! sweep, with the reload the Settings commands ask for. Swift:
//! `AppEnvironment.reloadPipeline`, `runRetentionSweep`, and the
//! `Task`s `MeetingDetailViewModel` ran the re-runs in.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use steno_core::AudioRetention;
use steno_host::services::Pipeline;
use steno_pipeline::{PipelineDependencies, ProcessingPipeline, RetentionSweep};
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
}

/// The host's `Pipeline`. A summary re-run and a re-export are started on
/// the runtime and return at once: the host calls them with its state
/// locked, and both talk to the network, so waiting here would park the
/// runtime's workers on that lock (the event loop, the flush timer and the
/// poll all take it) until nobody is left to drive the request. Their
/// outcome reaches the window through the pipeline's events and the
/// meeting row; a failure is logged. `apply_retention` touches the store
/// only and completes in place, so the detail the host reads back right
/// after already shows the new rule.
pub struct HostPipeline {
    pub pipeline: Arc<CurrentPipeline>,
    pub sweep: RetentionSweep,
}

impl Pipeline for HostPipeline {
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> Result<(), String> {
        let pipeline = self.pipeline.current();
        let template_id = template_id.to_owned();
        self.pipeline.runtime.spawn(async move {
            if let Err(error) = pipeline.rerun_summary(meeting_id, &template_id).await {
                tracing::warn!(%meeting_id, %error, "summary re-run failed");
            }
        });
        Ok(())
    }

    fn redeliver(&self, meeting_id: Uuid) -> Result<(), String> {
        let pipeline = self.pipeline.current();
        self.pipeline.runtime.spawn(async move {
            if let Err(error) = pipeline.redeliver(meeting_id).await {
                tracing::warn!(%meeting_id, %error, "re-export failed");
            }
        });
        Ok(())
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
    use std::time::{Duration, Instant};

    use steno_core::{
        MeetingState, MeetingSummarizer, Store, SummaryInput, SummaryOutput, async_trait,
        protocols::BoundaryResult, testing::sample_data,
    };

    use super::*;
    use crate::testing::{fake_dependencies, temp_store};

    /// A summarizer that answers only once released.
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
        summarizer: Arc<dyn MeetingSummarizer>,
        runtime: tokio::runtime::Handle,
    ) -> HostPipeline {
        let dependencies = fake_dependencies(store, "fake-engine").with_llm(None, Some(summarizer));
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_summary_re_run_returns_before_the_summarizer_answers() {
        let (_dir, store) = temp_store();
        let mut meeting = sample_data::meeting();
        meeting.state = MeetingState::Ready;
        store.save_meeting(&meeting).unwrap();
        let release = Arc::new(tokio::sync::Notify::new());
        let asked = Arc::new(tokio::sync::Notify::new());
        let service = pipeline(
            &store,
            Arc::new(HeldSummarizer {
                release: release.clone(),
                asked: asked.clone(),
            }),
            tokio::runtime::Handle::current(),
        );

        let started = Instant::now();
        let returned = tokio::task::spawn_blocking(move || {
            service.rerun_summary(meeting.id, &meeting.template_id)
        })
        .await
        .unwrap();
        assert_eq!(returned, Ok(()));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the call waited for the summarizer"
        );
        // The run was started: the summarizer is being asked.
        tokio::time::timeout(Duration::from_secs(5), asked.notified())
            .await
            .expect("the re-run reached the summarizer");
        release.notify_one();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_export_returns_at_once_and_runs_in_the_background() {
        let (_dir, store) = temp_store();
        let mut meeting = sample_data::meeting();
        meeting.state = MeetingState::Ready;
        store.save_meeting(&meeting).unwrap();
        let service = pipeline(
            &store,
            Arc::new(steno_core::testing::FakeSummarizer::default()),
            tokio::runtime::Handle::current(),
        );
        let pipeline = service.pipeline.current();
        let returned = tokio::task::spawn_blocking(move || service.redeliver(meeting.id))
            .await
            .unwrap();
        assert_eq!(returned, Ok(()));
        // Without destinations the re-export is a no-op that completes.
        tokio::time::timeout(Duration::from_secs(5), async {
            while !pipeline.in_flight().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the re-export finished");
    }
}
