//! `steno_core::Diarizer` over a [`Pipeline`] whose backend loads on first
//! use.

use std::sync::Mutex;

use steno_core::{AudioBuffer16k, BoundaryResult, DiarizationResult, Diarizer, async_trait};

use crate::backend::TensorBackend;
use crate::error::DiarizeError;
use crate::pipeline::{DiarizerConfig, Pipeline};

/// Builds the backend when the diarizer is first prepared or used;
/// downloads and model loading happen here.
pub type BackendLoader =
    Box<dyn Fn() -> Result<Box<dyn TensorBackend>, DiarizeError> + Send + Sync>;

/// The diarizer the pipeline holds as `Arc<dyn Diarizer>`. Calls run one
/// after another behind the lock, as `FluidDiarizer` runs its calls, so
/// one backend instance serves every meeting the pipeline processes.
pub struct ModelDiarizer {
    config: DiarizerConfig,
    loader: BackendLoader,
    pipeline: Mutex<Option<Pipeline>>,
}

impl std::fmt::Debug for ModelDiarizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelDiarizer")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ModelDiarizer {
    #[must_use]
    pub fn new(config: DiarizerConfig, loader: BackendLoader) -> Self {
        ModelDiarizer {
            config,
            loader,
            pipeline: Mutex::new(None),
        }
    }

    /// A diarizer over a backend that already exists.
    #[must_use]
    pub fn with_backend(config: DiarizerConfig, backend: Box<dyn TensorBackend>) -> Self {
        ModelDiarizer {
            config: config.clone(),
            loader: Box::new(|| Err(DiarizeError::message("the backend was consumed"))),
            pipeline: Mutex::new(Some(Pipeline::new(backend, config))),
        }
    }

    /// The ONNX Runtime backend over the model store: the two model files
    /// are fetched on first use.
    #[cfg(feature = "onnx")]
    #[must_use]
    pub fn onnx(config: DiarizerConfig, store: crate::models::ModelStore, threads: usize) -> Self {
        ModelDiarizer::new(
            config,
            Box::new(move || {
                let backend = crate::onnx::OnnxBackend::from_store(&store, threads)?;
                Ok(Box::new(backend) as Box<dyn TensorBackend>)
            }),
        )
    }

    /// The `CoreML` backend over `FluidAudio`'s compiled models in
    /// `models_dir` (`Segmentation.mlmodelc`, `FBank.mlmodelc`,
    /// `Embedding.mlmodelc`).
    #[cfg(all(feature = "coreml", target_os = "macos"))]
    #[must_use]
    pub fn coreml(config: DiarizerConfig, models_dir: std::path::PathBuf) -> Self {
        ModelDiarizer::new(
            config,
            Box::new(move || {
                let backend = crate::coreml::CoreMlBackend::load(&models_dir)?;
                Ok(Box::new(backend) as Box<dyn TensorBackend>)
            }),
        )
    }

    /// Runs `body` on the loaded pipeline, loading it first when needed.
    fn with_pipeline<T>(
        &self,
        body: impl FnOnce(&mut Pipeline) -> Result<T, DiarizeError>,
    ) -> Result<T, DiarizeError> {
        let mut guard = self.pipeline.lock().map_err(|_| DiarizeError::Poisoned)?;
        if guard.is_none() {
            let backend = (self.loader)()?;
            *guard = Some(Pipeline::new(backend, self.config.clone()));
        }
        let pipeline = guard.as_mut().expect("loaded above");
        body(pipeline)
    }
}

#[async_trait]
impl Diarizer for ModelDiarizer {
    async fn prepare(&self) -> BoundaryResult<()> {
        self.with_pipeline(|_| Ok(()))?;
        Ok(())
    }

    async fn diarize(&self, audio: &AudioBuffer16k) -> BoundaryResult<DiarizationResult> {
        Ok(self.with_pipeline(|pipeline| pipeline.diarize(audio))?)
    }
}
