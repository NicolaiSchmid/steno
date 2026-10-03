//! `steno_core::Diarizer` over a [`Pipeline`] whose backend loads on first
//! use. Swift: `Sources/StenoSpeech/Diarization/FluidDiarizer.swift`.

use std::sync::{Arc, Mutex, PoisonError};

use steno_core::{AudioBuffer16k, BoundaryResult, DiarizationResult, Diarizer, async_trait};

use crate::backend::DiarizationBackend;
use crate::error::DiarizeError;
use crate::pipeline::{DiarizerConfig, Pipeline};

/// Builds the backend when the diarizer is first prepared or used;
/// downloads and model loading happen here.
pub type BackendLoader =
    Box<dyn Fn() -> Result<Box<dyn DiarizationBackend>, DiarizeError> + Send + Sync>;

/// The pipeline before and after its backend has loaded.
enum Slot {
    Pending(BackendLoader),
    Loaded(Pipeline),
}

impl Slot {
    /// The pipeline, loading the backend on the first call.
    fn pipeline(&mut self, config: &DiarizerConfig) -> Result<&mut Pipeline, DiarizeError> {
        if let Slot::Pending(loader) = self {
            *self = Slot::Loaded(Pipeline::new(loader()?, config.clone()));
        }
        match self {
            Slot::Loaded(pipeline) => Ok(pipeline),
            Slot::Pending(_) => unreachable!("loaded above"),
        }
    }
}

/// The diarizer the pipeline holds as `Arc<dyn Diarizer>`. Calls run one
/// after another behind the lock, as `FluidDiarizer` runs its calls, so
/// one backend instance serves every meeting the pipeline processes.
///
/// The work is minutes of model inference, so each call runs on one of
/// tokio's blocking threads (`spawn_blocking`) and the lock is taken and
/// released there, never across an `.await`; the executor keeps serving
/// the shell while a lane is analysed. `prepare` and `diarize` must
/// therefore be awaited inside a tokio runtime (any flavour, the
/// current-thread one included); outside one, `spawn_blocking` panics.
/// A panic inside a call comes back as an error, not a crash. The audio
/// is cloned onto that
/// thread (4 bytes a sample, 230 MB for an hour), the price of a
/// `'static` task over a borrowed buffer; the analysis itself holds more.
/// A panic mid-call leaves the pipeline as it was between calls, because
/// the backends keep no state from one call to the next, so a poisoned
/// lock is reused as core's store reuses its connection.
pub struct ModelDiarizer {
    config: DiarizerConfig,
    slot: Arc<Mutex<Slot>>,
}

impl std::fmt::Debug for ModelDiarizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelDiarizer")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ModelDiarizer {
    /// A diarizer whose backend `loader` builds on the first `prepare` or
    /// `diarize`.
    #[must_use]
    pub fn new(config: DiarizerConfig, loader: BackendLoader) -> Self {
        ModelDiarizer {
            config,
            slot: Arc::new(Mutex::new(Slot::Pending(loader))),
        }
    }

    /// A diarizer over a backend that already exists.
    #[must_use]
    pub fn with_backend(config: DiarizerConfig, backend: Box<dyn DiarizationBackend>) -> Self {
        ModelDiarizer {
            config: config.clone(),
            slot: Arc::new(Mutex::new(Slot::Loaded(Pipeline::new(backend, config)))),
        }
    }

    /// The ONNX Runtime backend over the model store: the two model files
    /// are fetched on first use. `threads` is the intra-op thread count of
    /// each session; zero lets ONNX Runtime decide.
    ///
    /// ```no_run
    /// use steno_core::StenoPaths;
    /// use steno_diarize::{DiarizerConfig, ModelDiarizer, ModelStore};
    ///
    /// let store = ModelStore::for_paths(&StenoPaths::new("/tmp/steno-support"));
    /// let diarizer = ModelDiarizer::onnx(DiarizerConfig::default(), store, 4);
    /// ```
    #[cfg(feature = "onnx")]
    #[must_use]
    pub fn onnx(config: DiarizerConfig, store: crate::models::ModelStore, threads: usize) -> Self {
        ModelDiarizer::new(
            config,
            Box::new(move || {
                let backend = crate::onnx::OnnxBackend::from_store(&store, threads)?;
                Ok(Box::new(backend) as Box<dyn DiarizationBackend>)
            }),
        )
    }

    /// The `CoreML` backend over `FluidAudio`'s compiled models in
    /// `models_dir` (`Segmentation.mlmodelc`, `FBank.mlmodelc`,
    /// `Embedding.mlmodelc`), which is [`crate::coreml::model_directory`]
    /// for the models the Swift app installed.
    #[cfg(all(feature = "coreml", target_os = "macos"))]
    #[must_use]
    pub fn coreml(config: DiarizerConfig, models_dir: std::path::PathBuf) -> Self {
        ModelDiarizer::new(
            config,
            Box::new(move || {
                let backend = crate::coreml::CoreMlBackend::load(&models_dir)?;
                Ok(Box::new(backend) as Box<dyn DiarizationBackend>)
            }),
        )
    }

    /// Runs `body` on the loaded pipeline on a blocking thread, loading
    /// the pipeline there first when needed.
    async fn run<T>(
        &self,
        body: impl FnOnce(&mut Pipeline) -> Result<T, DiarizeError> + Send + 'static,
    ) -> BoundaryResult<T>
    where
        T: Send + 'static,
    {
        let slot = Arc::clone(&self.slot);
        let config = self.config.clone();
        let value = tokio::task::spawn_blocking(move || {
            let mut guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
            body(guard.pipeline(&config)?)
        })
        .await??;
        Ok(value)
    }
}

#[async_trait]
impl Diarizer for ModelDiarizer {
    async fn prepare(&self) -> BoundaryResult<()> {
        self.run(|_| Ok(())).await
    }

    async fn diarize(&self, audio: &AudioBuffer16k) -> BoundaryResult<DiarizationResult> {
        let audio = audio.clone();
        self.run(move |pipeline| pipeline.diarize(&audio)).await
    }
}
