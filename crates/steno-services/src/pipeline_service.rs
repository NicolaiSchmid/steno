//! The host's `Pipeline` over the processing pipeline and the retention
//! sweep, with the reload the Settings commands ask for. Swift:
//! `AppEnvironment.reloadPipeline`, `runRetentionSweep`.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use steno_core::AudioRetention;
use steno_host::services::Pipeline;
use steno_pipeline::{PipelineDependencies, ProcessingPipeline, RetentionSweep};
use uuid::Uuid;

/// Rebuilds the dependencies from the stored settings and the API key.
pub type MakeDependencies =
    Arc<dyn Fn() -> Result<PipelineDependencies, String> + Send + Sync>;

/// The current pipeline behind a swap: a reload replaces it first, so a
/// Save never waits for a run in progress; the retired pipeline is kept
/// until it is idle, so meetings in flight finish on the dependencies they
/// started with.
pub struct PipelineHandle {
    current: Mutex<ProcessingPipeline>,
    make: MakeDependencies,
    runtime: tokio::runtime::Handle,
}

impl PipelineHandle {
    #[must_use]
    pub fn new(pipeline: ProcessingPipeline, make: MakeDependencies, runtime: tokio::runtime::Handle) -> Self {
        PipelineHandle {
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

    fn block<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        tokio::task::block_in_place(|| self.runtime.block_on(future))
    }

    /// Replaces the pipeline with one built from the stored settings and
    /// the secret store's API key.
    pub fn reload(&self) -> Result<(), String> {
        let replacement = ProcessingPipeline::new((self.make)()?);
        let retired = std::mem::replace(
            &mut *self
                .current
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            replacement,
        );
        self.runtime.spawn(async move { retired.wait_until_idle().await });
        Ok(())
    }
}

pub struct RealPipeline {
    pub handle: Arc<PipelineHandle>,
    pub sweep: RetentionSweep,
}

impl Pipeline for RealPipeline {
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> Result<(), String> {
        let pipeline = self.handle.current();
        self.handle
            .block(pipeline.rerun_summary(meeting_id, template_id))
            .map_err(|error| error.to_string())
    }

    fn redeliver(&self, meeting_id: Uuid) -> Result<(), String> {
        let pipeline = self.handle.current();
        self.handle
            .block(pipeline.redeliver(meeting_id))
            .map_err(|error| error.to_string())
    }

    fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> Result<(), String> {
        let pipeline = self.handle.current();
        self.handle
            .block(pipeline.apply_retention(meeting_id, rule))
            .map_err(|error| error.to_string())
    }

    fn reload(&self) -> Result<(), String> {
        self.handle.reload()
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
