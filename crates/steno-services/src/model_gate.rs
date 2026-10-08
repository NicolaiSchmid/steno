//! No download inside a pipeline run: the speech engine and the diarizer
//! the app's pipelines run, each behind a gate that refuses a call while
//! its models are not installed. The refusal is
//! [`PipelineFailure::models_missing`], so the meeting stays `queued` with
//! no failure reason on its row and is processed once Settings or
//! onboarding installed the models ([`ResumingSpeechModels`] then resumes
//! it). The speech sidecar's own install is turned off as well
//! (`SidecarConfig::install_models`), so a file removed between the gate's
//! check and the child's load is refused too, never fetched; an engine's
//! error while its models are gone is the same refusal. The `steno`
//! command's engines (`crates/steno-cli/src/wiring.rs`) keep downloading
//! on first use, for a command a user runs. Rust only: the Swift pipeline
//! downloaded inside the run.
//!
//! | Item | What it does |
//! |------|--------------|
//! | [`InstalledCheck`] | Whether a boundary's models are on disk now |
//! | [`GatedSpeechEngine`] | A speech engine that refuses while its models are missing |
//! | [`GatedDiarizer`] | The same for the diarizer |
//! | [`ResumingSpeechModels`] | The model service that resumes the waiting meetings after an install |

use std::collections::BTreeSet;
use std::sync::Arc;

use steno_core::protocols::{BoundaryResult, BoxError};
use steno_core::{
    AudioBuffer16k, DiarizationResult, Diarizer, LanguageTag, PipelineStage, RawSegment,
    SpeechEngine, async_trait,
};
use steno_diarize::DiarizeError;
use steno_host::services::{ModelNotice, SpeechModels};
use steno_host::speech::ModelAsset;
use steno_pipeline::PipelineFailure;
use steno_speech::SpeechError;

use crate::pipeline::CurrentPipeline;

/// Whether the models a boundary loads are installed now.
pub type InstalledCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// `call`, run only while `installed` holds, else the refusal for
/// `stage`; its error as [`refusing`] maps it.
async fn gated<T>(
    installed: &InstalledCheck,
    stage: PipelineStage,
    call: impl Future<Output = BoundaryResult<T>>,
) -> BoundaryResult<T> {
    if !installed() {
        return Err(Box::new(PipelineFailure::models_missing(stage)));
    }
    call.await
        .map_err(|error| refusing(installed, stage, error))
}

/// An error of the boundary behind the gate as the refusal for `stage`
/// when it says the models are not installed: the speech sidecar's
/// `NotInstalled` (its install turned off) or the diarizer's (it never
/// downloads, and deletes a file that fails its checksum after a failed
/// load), as the boundary boxes it; or when `installed` no longer holds:
/// the models went between the gate's check and the load (Remove in
/// Settings), and the `CoreML` engine's failed load says so in its own
/// words. Any other error as it is.
fn refusing(installed: &InstalledCheck, stage: PipelineStage, error: BoxError) -> BoxError {
    let not_installed = matches!(
        error.downcast_ref::<SpeechError>(),
        Some(SpeechError::NotInstalled { .. })
    ) || matches!(
        error.downcast_ref::<DiarizeError>(),
        Some(DiarizeError::NotInstalled { .. })
    );
    if not_installed || !installed() {
        Box::new(PipelineFailure::models_missing(stage))
    } else {
        error
    }
}

/// A speech engine that refuses `prepare` (stage `decode`, as the warm-up
/// attributes it) and `transcribe` while its models are not installed.
pub struct GatedSpeechEngine {
    inner: Arc<dyn SpeechEngine>,
    installed: InstalledCheck,
}

impl GatedSpeechEngine {
    #[must_use]
    pub fn new(inner: Arc<dyn SpeechEngine>, installed: InstalledCheck) -> Self {
        GatedSpeechEngine { inner, installed }
    }
}

#[async_trait]
impl SpeechEngine for GatedSpeechEngine {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn supported_languages(&self) -> &BTreeSet<LanguageTag> {
        self.inner.supported_languages()
    }

    async fn prepare(&self) -> BoundaryResult<()> {
        gated(&self.installed, PipelineStage::Decode, self.inner.prepare()).await
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        gated(
            &self.installed,
            PipelineStage::Transcribe,
            self.inner.transcribe(audio, hint),
        )
        .await
    }

    async fn release(&self) -> BoundaryResult<()> {
        self.inner.release().await
    }
}

/// A diarizer that refuses `prepare` and `diarize` (stage `diarize`)
/// while its models are not installed, before its loader can fetch them.
pub struct GatedDiarizer {
    inner: Arc<dyn Diarizer>,
    installed: InstalledCheck,
}

impl GatedDiarizer {
    #[must_use]
    pub fn new(inner: Arc<dyn Diarizer>, installed: InstalledCheck) -> Self {
        GatedDiarizer { inner, installed }
    }
}

#[async_trait]
impl Diarizer for GatedDiarizer {
    async fn prepare(&self) -> BoundaryResult<()> {
        gated(
            &self.installed,
            PipelineStage::Diarize,
            self.inner.prepare(),
        )
        .await
    }

    async fn diarize(&self, audio: &AudioBuffer16k) -> BoundaryResult<DiarizationResult> {
        gated(
            &self.installed,
            PipelineStage::Diarize,
            self.inner.diarize(audio),
        )
        .await
    }
}

/// The host's model service with one addition: once a download from
/// Settings or onboarding installed its models, `resume` runs, so the
/// meetings a run left `queued` for them are processed. Every other call
/// goes to `inner` as it is.
pub struct ResumingSpeechModels<M> {
    inner: M,
    resume: Box<dyn Fn() + Send + Sync>,
}

impl<M: SpeechModels> ResumingSpeechModels<M> {
    #[must_use]
    pub fn new(inner: M, resume: Box<dyn Fn() + Send + Sync>) -> Self {
        ResumingSpeechModels { inner, resume }
    }

    /// The app's: an install resumes, on `pipeline`'s current pipeline,
    /// the meetings its pipelines left waiting
    /// ([`CurrentPipeline::resume_unfinished`]).
    #[must_use]
    pub fn resuming(inner: M, pipeline: Arc<CurrentPipeline>) -> Self {
        Self::new(inner, Box::new(move || pipeline.resume_unfinished()))
    }
}

impl<M: SpeechModels> SpeechModels for ResumingSpeechModels<M> {
    fn is_installed(&self, asset: ModelAsset) -> bool {
        self.inner.is_installed(asset)
    }

    fn engine_installed(&self, engine_id: &str) -> bool {
        self.inner.engine_installed(engine_id)
    }

    fn installed_size(&self, asset: ModelAsset) -> Option<i64> {
        self.inner.installed_size(asset)
    }

    fn download(
        &self,
        asset: ModelAsset,
        progress: &mut dyn FnMut(f64, &str),
    ) -> BoundaryResult<()> {
        self.inner.download(asset, progress)?;
        (self.resume)();
        Ok(())
    }

    fn remove(&self, asset: ModelAsset) -> BoundaryResult<()> {
        self.inner.remove(asset)
    }

    fn display_name(&self, asset: ModelAsset) -> &'static str {
        self.inner.display_name(asset)
    }

    fn source_repo(&self, asset: ModelAsset) -> &'static str {
        self.inner.source_repo(asset)
    }

    fn notices(&self, asset: ModelAsset) -> Vec<ModelNotice> {
        self.inner.notices(asset)
    }

    fn expected_bytes(&self, asset: ModelAsset) -> i64 {
        self.inner.expected_bytes(asset)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use steno_core::testing::{FakeDiarizer, FakeSpeechEngine};

    use super::*;

    fn flag(value: bool) -> (Arc<AtomicBool>, InstalledCheck) {
        let flag = Arc::new(AtomicBool::new(value));
        let read = flag.clone();
        (flag, Arc::new(move || read.load(Ordering::SeqCst)))
    }

    fn refusal(error: &BoxError) -> Option<PipelineStage> {
        error
            .downcast_ref::<PipelineFailure>()
            .filter(|failure| failure.is_models_missing())
            .map(|failure| failure.stage)
    }

    /// Missing models: each call is refused at its stage and never reaches
    /// the engine or diarizer behind; installed, each passes through.
    #[tokio::test]
    async fn a_gate_refuses_every_call_until_the_models_are_installed() {
        let (installed, check) = flag(false);
        let engine = Arc::new(FakeSpeechEngine::default());
        let diarizer = Arc::new(FakeDiarizer::default());
        let gated = GatedSpeechEngine::new(engine.clone(), check.clone());
        let gated_diarizer = GatedDiarizer::new(diarizer.clone(), check);
        let audio = AudioBuffer16k::silence(1.0);
        assert_eq!(
            refusal(&gated.prepare().await.unwrap_err()),
            Some(PipelineStage::Decode)
        );
        assert_eq!(
            refusal(&gated.transcribe(&audio, None).await.unwrap_err()),
            Some(PipelineStage::Transcribe)
        );
        assert_eq!(
            refusal(&gated_diarizer.prepare().await.unwrap_err()),
            Some(PipelineStage::Diarize)
        );
        assert_eq!(
            refusal(&gated_diarizer.diarize(&audio).await.unwrap_err()),
            Some(PipelineStage::Diarize)
        );
        assert_eq!(engine.preparations.count(), 0);
        assert_eq!(engine.transcriptions.count(), 0);
        assert_eq!(diarizer.preparations.count(), 0);
        assert_eq!(diarizer.diarizations.count(), 0);

        installed.store(true, Ordering::SeqCst);
        gated.prepare().await.unwrap();
        gated.transcribe(&audio, None).await.unwrap();
        gated_diarizer.prepare().await.unwrap();
        gated_diarizer.diarize(&audio).await.unwrap();
        gated.release().await.unwrap();
        assert_eq!(engine.releases.count(), 1);
    }

    /// The sidecar's own `NotInstalled` (its install turned off) is the
    /// same refusal; any other error passes as it is while the models are
    /// installed.
    #[test]
    fn a_not_installed_from_the_engine_is_the_refusal() {
        let (_, installed) = flag(true);
        let not_installed: BoxError = Box::new(SpeechError::NotInstalled {
            asset: "x".to_owned(),
            directory: "/m/x".into(),
            missing: vec!["a".to_owned()],
        });
        assert_eq!(
            refusal(&refusing(
                &installed,
                PipelineStage::Transcribe,
                not_installed
            )),
            Some(PipelineStage::Transcribe)
        );
        let other: BoxError = "the child died".into();
        let passed = refusing(&installed, PipelineStage::Transcribe, other);
        assert_eq!(refusal(&passed), None);
        assert_eq!(passed.to_string(), "the child died");
    }

    /// The diarizer's `NotInstalled` (it never downloads, or its load
    /// deleted a file that failed its checksum), boxed as `ModelDiarizer`
    /// returns it, is the refusal for the diarize stage while the gate's
    /// check still holds, so the pipeline's diarizer fallback passes it
    /// on and the meeting waits.
    #[test]
    fn a_not_installed_from_the_diarizer_is_the_refusal() {
        let (_, installed) = flag(true);
        let not_installed: BoxError = Box::new(DiarizeError::NotInstalled {
            asset: steno_diarize::models::ASSET_ID.to_owned(),
            directory: "/m/onnx/diarization".into(),
            missing: vec![steno_diarize::models::EMBEDDING_FILE.to_owned()],
        });
        assert_eq!(
            refusal(&refusing(&installed, PipelineStage::Diarize, not_installed)),
            Some(PipelineStage::Diarize)
        );
        let other: BoxError = Box::new(DiarizeError::metadata("a shape"));
        assert_eq!(
            refusal(&refusing(&installed, PipelineStage::Diarize, other)),
            None
        );
    }

    /// A speech engine or diarizer whose load fails because its models
    /// went after the gate let the call through (Remove in Settings), in
    /// its own words as the `CoreML` engine and the diarizer's loader put
    /// it: the call is the refusal, so the meeting waits instead of
    /// failing.
    #[tokio::test]
    async fn a_load_that_fails_once_the_models_are_gone_is_the_refusal() {
        struct Removing {
            installed: Arc<AtomicBool>,
        }

        impl Removing {
            fn fail(&self) -> BoundaryResult<()> {
                self.installed.store(false, Ordering::SeqCst);
                Err("loading /m/Encoder.mlmodelc: no such file".into())
            }
        }

        #[async_trait]
        impl SpeechEngine for Removing {
            fn id(&self) -> &'static str {
                "removing"
            }

            fn supported_languages(&self) -> &BTreeSet<LanguageTag> {
                static NONE: BTreeSet<LanguageTag> = BTreeSet::new();
                &NONE
            }

            async fn prepare(&self) -> BoundaryResult<()> {
                self.fail()
            }

            async fn transcribe(
                &self,
                _: &AudioBuffer16k,
                _: Option<&LanguageTag>,
            ) -> BoundaryResult<Vec<RawSegment>> {
                self.fail().map(|()| Vec::new())
            }
        }

        #[async_trait]
        impl Diarizer for Removing {
            async fn prepare(&self) -> BoundaryResult<()> {
                self.fail()
            }

            async fn diarize(&self, _: &AudioBuffer16k) -> BoundaryResult<DiarizationResult> {
                Err("unused".into())
            }
        }

        let (installed, check) = flag(true);
        let removing = Arc::new(Removing {
            installed: installed.clone(),
        });
        let engine = GatedSpeechEngine::new(removing.clone(), check.clone());
        assert_eq!(
            refusal(&engine.prepare().await.unwrap_err()),
            Some(PipelineStage::Decode)
        );
        installed.store(true, Ordering::SeqCst);
        assert_eq!(
            refusal(
                &engine
                    .transcribe(&AudioBuffer16k::silence(1.0), None)
                    .await
                    .unwrap_err()
            ),
            Some(PipelineStage::Transcribe)
        );
        installed.store(true, Ordering::SeqCst);
        let diarizer = GatedDiarizer::new(removing, check);
        assert_eq!(
            refusal(&diarizer.prepare().await.unwrap_err()),
            Some(PipelineStage::Diarize)
        );
    }

    /// A download that installed its models resumes the waiting meetings;
    /// a failed one does not.
    #[test]
    fn an_install_resumes_the_waiting_meetings_and_a_failed_one_does_not() {
        let resumed = Arc::new(AtomicUsize::new(0));
        let models = ResumingSpeechModels::new(steno_host::fakes::FakeSpeechModels::default(), {
            let resumed = resumed.clone();
            Box::new(move || {
                resumed.fetch_add(1, Ordering::SeqCst);
            })
        });
        models
            .download(ModelAsset::ParakeetV3, &mut |_, _| {})
            .unwrap();
        assert_eq!(resumed.load(Ordering::SeqCst), 1);
        models.inner.fail_downloads(Some("offline"));
        assert!(
            models
                .download(ModelAsset::OfflineDiarizer, &mut |_, _| {})
                .is_err()
        );
        assert_eq!(resumed.load(Ordering::SeqCst), 1);
    }

    /// The speech sidecar behind a gate that lets every call through,
    /// over an empty models directory and a mirror on a closed port: the
    /// app's engines turn the sidecar's install off, so the child's load
    /// is refused for the missing models before any download starts, and
    /// nothing is written.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_apps_sidecar_refuses_missing_models_past_its_gate() {
        use crate::speech::{SpeechEngines, testing};
        use steno_speech::{SpeechRuntime, SpeechSettings};

        let dir = tempfile::tempdir().unwrap();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mirror = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let engines = SpeechEngines::gated(
            testing::setup(
                dir.path(),
                SpeechSettings {
                    models_mirror: Some(mirror),
                    ..SpeechSettings::default()
                },
            ),
            Arc::new(|_| true),
            Arc::new(|| true),
        );
        let error = engines
            .engine(SpeechRuntime::OnnxSidecar)
            .prepare()
            .await
            .unwrap_err();
        assert_eq!(refusal(&error), Some(PipelineStage::Decode), "{error}");
        assert!(testing::files_under(dir.path()).is_empty());
    }

    /// The app's engines over an empty models directory, with a mirror
    /// that counts its requests: the speech engine on either runtime and
    /// the diarizer refuse for missing models, and nothing asks the mirror
    /// for a file. The speech sidecar's install is off in them.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_apps_engines_refuse_missing_models_and_download_nothing() {
        use crate::speech::{SpeechEngines, testing};
        use steno_speech::{SpeechRuntime, SpeechSettings};

        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mirror = format!("http://{}", listener.local_addr().unwrap());
        let engines = SpeechEngines::new(testing::setup(
            dir.path(),
            SpeechSettings {
                models_mirror: Some(mirror),
                ..SpeechSettings::default()
            },
        ));
        for runtime in [SpeechRuntime::CoreMlInProcess, SpeechRuntime::OnnxSidecar] {
            let engine = engines.engine(runtime);
            assert_eq!(
                refusal(&engine.prepare().await.unwrap_err()),
                Some(PipelineStage::Decode),
                "{runtime:?}"
            );
            assert_eq!(
                refusal(
                    &engine
                        .transcribe(&AudioBuffer16k::silence(1.0), None)
                        .await
                        .unwrap_err()
                ),
                Some(PipelineStage::Transcribe),
                "{runtime:?}"
            );
        }
        let diarizer = engines.diarizer();
        assert_eq!(
            refusal(&diarizer.prepare().await.unwrap_err()),
            Some(PipelineStage::Diarize)
        );
        assert!(
            matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "no download was asked for"
        );
        assert!(
            testing::files_under(dir.path()).is_empty(),
            "nothing was written to the models directory"
        );
    }
}
