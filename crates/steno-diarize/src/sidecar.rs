//! [`SidecarDiarizer`], the `steno_core::Diarizer` that runs the ONNX
//! models in the speech sidecar (invariant 4 of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`), so a crash in ONNX
//! Runtime during a diarization ends the child, not the app. Rust only:
//! Swift ran `FluidAudio` in its own process.
//!
//! The child runs the whole [`Pipeline::diarize`](crate::Pipeline::diarize)
//! with [`DiarizerConfig::default`](crate::DiarizerConfig::default) and
//! sends the clusters back bit for bit
//! (`steno_speech::sidecar::protocol::DiarizedCluster`). One request a
//! lane, rather than one per segmentation window and embedding (some 5 000
//! for an hour and the audio sent four times over): the child runs the
//! same code as [`ModelDiarizer`](crate::ModelDiarizer) in this process,
//! so the clusters are the in-process ones bit for bit, and the protocol
//! grows by two requests. `ModelDiarizer` and the in-process
//! [`Pipeline`](crate::Pipeline) stay for the calibration harness, which
//! sweeps the clustering cut over one analysis, and `steno dev
//! diarize-sweep`.
//!
//! The models are installed in this process ([`crate::models::paths`] under
//! the diarizer's [`Install`]); the child only loads the files it is
//! handed, and opens no connection. A load that fails, whether the child
//! refuses the files or dies or hangs while it loads them, is checked
//! against the manifest here ([`crate::models`]'s failed-load check), as
//! for [`ModelDiarizer`](crate::ModelDiarizer).

use std::sync::Arc;

use steno_core::{AudioBuffer16k, BoundaryResult, DiarizationResult, Diarizer, async_trait};
use steno_speech::sidecar::DiarizerModels;
use steno_speech::{SidecarError, SidecarSpeechEngine, SpeechError};

use crate::error::DiarizeError;
use crate::models::{self, Install};
use crate::pipeline::DiarizerConfig;

/// The diarizer the app and `steno process` run: the ONNX models in the
/// child of `engine`, the speech sidecar engine whose child, deadline,
/// memory ceiling and crash handling it shares
/// ([`SidecarSpeechEngine::diarize`]). The models come from that engine's
/// store, `<store root>/diarization/`.
///
/// `prepare` installs the models as `install` allows and starts no child;
/// `diarize` hands the lane to the running child, or to a new one that is
/// stopped after the call. A child that dies, hangs or overruns the
/// ceiling while it diarizes fails the call with the sidecar's error, and
/// the next call starts a new child. When the load of the models fails
/// instead, refused or crashed or hung alike, the files are hashed: one
/// that fails its checksum is deleted and the call is
/// [`DiarizeError::NotInstalled`], so a gate parks the meeting until a
/// download replaces it; intact files leave the sidecar's error. Audio under
/// [`DiarizerConfig::MINIMUM_AUDIO_SECONDS`] has no speakers and starts no
/// child, as in [`Pipeline::diarize`](crate::Pipeline::diarize).
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
    /// A diarizer in `engine`'s child; `install` says whether a missing
    /// model is downloaded first ([`Install::Allowed`]) or fails the call
    /// with [`DiarizeError::NotInstalled`] and no request
    /// ([`Install::Never`]). `threads` is the intra-op thread count of each
    /// session in the child; zero lets ONNX Runtime decide.
    #[must_use]
    pub fn new(engine: Arc<SidecarSpeechEngine>, install: Install, threads: usize) -> Self {
        SidecarDiarizer {
            engine,
            install,
            threads,
        }
    }

    /// The installed models, downloading them first where `install`
    /// allows; on a blocking thread, as a download takes minutes.
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
                // Whatever ended the load, even an abort a corrupt file
                // set off: a file that fails its checksum is deleted and
                // reported as not installed, so a download can replace it.
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
