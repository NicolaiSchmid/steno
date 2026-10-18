//! `steno_core::Diarizer` over a [`Pipeline`] whose backend loads on first
//! use, in this process: for `steno dev diarize-sweep`, the calibration and the
//! tests. The app diarizes in the sidecar ([`crate::SidecarDiarizer`]).

use std::sync::{Arc, Mutex, PoisonError};

use steno_core::{AudioBuffer16k, BoundaryResult, DiarizationResult, Diarizer, async_trait};

use crate::backend::DiarizationBackend;
use crate::error::DiarizeError;
use crate::pipeline::{DiarizerConfig, Pipeline};

/// Builds the backend on first use; model loading and allowed downloads
/// happen here.
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

/// A diarizer in this process, for `steno dev` and the tests. Calls run one
/// after another behind the lock, so one backend serves every call.
///
/// Each call runs on a tokio blocking thread (`spawn_blocking`), so `prepare`
/// and `diarize` must be awaited inside a tokio runtime; the audio is cloned
/// onto that thread. A panic inside a call comes back as the call's error and
/// the poisoned lock is reused, as the backends keep no state between calls.
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
    /// A diarizer whose backend `loader` builds on first use.
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

    /// The ONNX Runtime backend over `store`, `steno-speech`'s model store,
    /// loaded on first use. `install` says whether a missing file is downloaded
    /// then or fails the call with [`DiarizeError::NotInstalled`]; a load that
    /// fails is tried again by the next call. `threads` is the intra-op thread
    /// count per session; zero lets ONNX Runtime decide.
    ///
    /// ```no_run
    /// use steno_diarize::{DiarizerConfig, Install, ModelDiarizer};
    /// use steno_speech::ModelStore;
    ///
    /// let store = ModelStore::in_models_directory("/tmp/steno-support/Models".as_ref());
    /// let diarizer = ModelDiarizer::onnx(DiarizerConfig::default(), store, Install::Never, 4);
    /// ```
    #[cfg(feature = "onnx")]
    #[must_use]
    pub fn onnx(
        config: DiarizerConfig,
        store: steno_speech::ModelStore,
        install: crate::Install,
        threads: usize,
    ) -> Self {
        ModelDiarizer::new(
            config,
            Box::new(move || {
                let backend = crate::onnx::OnnxBackend::from_store(&store, install, threads)?;
                Ok(Box::new(backend) as Box<dyn DiarizationBackend>)
            }),
        )
    }

    /// The `CoreML` backend over `FluidAudio`'s compiled models in `models_dir`
    /// (see [`crate::coreml::model_directory`]).
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

    /// Runs `body` on the pipeline on a blocking thread, loading it first when
    /// needed.
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
