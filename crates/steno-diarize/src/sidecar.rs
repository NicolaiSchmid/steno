//! [`SidecarDiarizer`], the `steno_core::Diarizer` that runs the ONNX models
//! in the speech sidecar, so an ONNX Runtime crash ends the child, not the
//! app. The child runs the whole [`Pipeline::diarize`](crate::Pipeline::diarize),
//! one request a lane, and sends the clusters back bit for bit. Models are
//! installed here; the child only loads the files it is handed.

use std::sync::Arc;

use steno_core::{AudioBuffer16k, BoundaryResult, DiarizationResult, Diarizer, async_trait};
use steno_speech::sidecar::DiarizerModels;
use steno_speech::{SidecarError, SidecarSpeechEngine, SpeechError};

use crate::error::DiarizeError;
use crate::models::{self, Install};
use crate::pipeline::DiarizerConfig;

/// The diarizer the app and `steno process` run: the ONNX models in the
/// child of `engine`, sharing its deadline, memory ceiling and crash handling
/// ([`SidecarSpeechEngine::diarize`]).
///
/// `prepare` installs the models as `install` allows and starts no child. A
/// child that dies, hangs or overruns while it diarizes fails the call, and
/// the next call starts a new child. When the load fails, the files are
/// hashed: a corrupt one is deleted and the call is
/// [`DiarizeError::NotInstalled`]. Audio under
/// [`DiarizerConfig::MINIMUM_AUDIO_SECONDS`] starts no child.
///
/// ```no_run
/// use std::sync::Arc;
///
/// use steno_diarize::{Install, SidecarDiarizer};
/// use steno_speech::{ModelStore, SidecarConfig, SidecarSpeechEngine};
///
/// # fn run() -> std::io::Result<()> {
/// let engine = Arc::new(SidecarSpeechEngine::new(
///     ModelStore::from_environment(),
///     SidecarConfig::beside_current_exe()?,
/// ));
/// let diarizer = SidecarDiarizer::new(engine, Install::Never, 4);
/// # Ok(())
/// # }
/// ```
pub struct SidecarDiarizer {
    engine: Arc<SidecarSpeechEngine>,
    install: Install,
    threads: usize,
}

impl std::fmt::Debug for SidecarDiarizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarDiarizer")
            .field("install", &self.install)
            .field("threads", &self.threads)
            .finish_non_exhaustive()
    }
}

impl SidecarDiarizer {
    /// A diarizer in `engine`'s child; `install` says whether a missing model is
    /// downloaded first. `threads` is the intra-op thread count per session; zero
    /// lets ONNX Runtime decide.
    #[must_use]
    pub fn new(engine: Arc<SidecarSpeechEngine>, install: Install, threads: usize) -> Self {
        SidecarDiarizer {
            engine,
            install,
            threads,
        }
    }

    /// The installed models, downloading where `install` allows; on a blocking
    /// thread.
    async fn models(&self) -> Result<DiarizerModels, DiarizeError> {
        let store = self.engine.store().clone();
        let install = self.install;
        let paths = tokio::task::spawn_blocking(move || models::paths(&store, install))
            .await
            .map_err(DiarizeError::backend)??;
        Ok(DiarizerModels {
            segmentation: paths.segmentation,
            embedding: paths.embedding,
            intra_threads: self.threads,
        })
    }
}

#[async_trait]
impl Diarizer for SidecarDiarizer {
    async fn prepare(&self) -> BoundaryResult<()> {
        self.models().await?;
        Ok(())
    }

    async fn diarize(&self, audio: &AudioBuffer16k) -> BoundaryResult<DiarizationResult> {
        if audio.duration() < DiarizerConfig::MINIMUM_AUDIO_SECONDS {
            return Ok(DiarizationResult::default());
        }
        let models = self.models().await?;
        match self.engine.diarize(models, audio).await {
            Ok(clusters) => Ok(DiarizationResult { clusters }),
            Err(error @ SpeechError::Sidecar(SidecarError::DiarizerLoad(_))) => {
                // Whatever ended the load: a corrupt file is deleted and reported as not
                // installed.
                let store = self.engine.store().clone();
                let error = DiarizeError::Sidecar(error);
                let checked =
                    tokio::task::spawn_blocking(move || models::after_failed_load(&store, error))
                        .await?;
                Err(checked.into())
            }
            Err(error) => Err(DiarizeError::Sidecar(error).into()),
        }
    }
}
