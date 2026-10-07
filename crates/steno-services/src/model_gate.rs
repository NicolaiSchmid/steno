//! No download inside a pipeline run: the speech engine and the diarizer
//! the app's pipelines run, each behind a gate that refuses a call while
//! its models are not installed. The refusal is
//! [`PipelineFailure::models_missing`], so the meeting stays `queued`
//! without a reason and is processed once Settings installed the models
//! ([`ModelStoreSpeechModels`](crate::speech::ModelStoreSpeechModels)
//! then calls `resume_unfinished`). Downloads start from Settings and
//! onboarding only ([`ResumeAfterInstall`]). The speech sidecar's own install is turned off as
//! well (`SidecarConfig::install_models`), so a file removed between the
//! gate's check and the child's load is refused too, never fetched; its
//! `NotInstalled` is the same refusal. The CLI's engines keep downloading
//! on first use. Rust only: the Swift pipeline downloaded inside the run.

use std::collections::BTreeSet;
use std::sync::Arc;

use steno_core::protocols::{BoundaryResult, BoxError};
use steno_core::{
    AudioBuffer16k, DiarizationResult, Diarizer, LanguageTag, PipelineStage, RawSegment,
    SpeechEngine, async_trait,
};
use steno_host::services::{ModelNotice, SpeechModels};
use steno_host::speech::ModelAsset;
use steno_pipeline::PipelineFailure;
use steno_speech::SpeechError;

/// Whether the models a boundary loads are installed now.
pub type Installed = Arc<dyn Fn() -> bool + Send + Sync>;

/// `Ok` while `installed` holds, else the refusal for `stage`.
fn check(installed: &Installed, stage: PipelineStage) -> BoundaryResult<()> {
    if installed() {
        Ok(())
    } else {
        Err(Box::new(PipelineFailure::models_missing(stage)))
    }
}

/// An engine's `NotInstalled` (the speech sidecar's refusal with its
/// install turned off) as the refusal for `stage`; any other error as it
/// is.
fn refusing(stage: PipelineStage, error: BoxError) -> BoxError {
    if matches!(
        error.downcast_ref::<SpeechError>(),
        Some(SpeechError::NotInstalled { .. })
    ) {
        Box::new(PipelineFailure::models_missing(stage))
    } else {
        error
    }
}

/// A speech engine that refuses `prepare` (stage `decode`, as the warm-up
/// attributes it) and `transcribe` while its models are not installed.
pub struct GatedSpeechEngine {
    inner: Arc<dyn SpeechEngine>,
    installed: Installed,
}

impl GatedSpeechEngine {
    #[must_use]
    pub fn new(inner: Arc<dyn SpeechEngine>, installed: Installed) -> Self {
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
        check(&self.installed, PipelineStage::Decode)?;
        self.inner
            .prepare()
            .await
            .map_err(|error| refusing(PipelineStage::Decode, error))
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        check(&self.installed, PipelineStage::Transcribe)?;
        self.inner
            .transcribe(audio, hint)
            .await
            .map_err(|error| refusing(PipelineStage::Transcribe, error))
    }

    async fn release(&self) -> BoundaryResult<()> {
        self.inner.release().await
    }
}

/// A diarizer that refuses `prepare` and `diarize` (stage `diarize`)
/// while its models are not installed, before its loader can fetch them.
pub struct GatedDiarizer {
    inner: Arc<dyn Diarizer>,
    installed: Installed,
}

impl GatedDiarizer {
    #[must_use]
    pub fn new(inner: Arc<dyn Diarizer>, installed: Installed) -> Self {
        GatedDiarizer { inner, installed }
    }
}

#[async_trait]
impl Diarizer for GatedDiarizer {
    async fn prepare(&self) -> BoundaryResult<()> {
        check(&self.installed, PipelineStage::Diarize)?;
        self.inner.prepare().await
    }

    async fn diarize(&self, audio: &AudioBuffer16k) -> BoundaryResult<DiarizationResult> {
        check(&self.installed, PipelineStage::Diarize)?;
        self.inner.diarize(audio).await
    }
}

/// The host's model service with one addition: once a download from
/// Settings or onboarding installed its models, `resume` runs, so the
/// meetings a run left `queued` for them are processed
/// (`ProcessingPipeline::resume_unfinished`). Every other call goes to
/// `inner` as it is.
pub struct ResumeAfterInstall<M> {
    inner: M,
    resume: Box<dyn Fn() + Send + Sync>,
}

impl<M: SpeechModels> ResumeAfterInstall<M> {
    #[must_use]
    pub fn new(inner: M, resume: Box<dyn Fn() + Send + Sync>) -> Self {
        ResumeAfterInstall { inner, resume }
    }
}

impl<M: SpeechModels> SpeechModels for ResumeAfterInstall<M> {
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use steno_core::testing::{FakeDiarizer, FakeSpeechEngine};

    use super::*;

    fn flag(value: bool) -> (Arc<AtomicBool>, Installed) {
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
    /// same refusal; any other error passes as it is.
    #[test]
    fn a_not_installed_from_the_engine_is_the_refusal() {
        let not_installed: BoxError = Box::new(SpeechError::NotInstalled {
            asset: "x".to_owned(),
            directory: "/m/x".into(),
            missing: vec!["a".to_owned()],
        });
        assert_eq!(
            refusal(&refusing(PipelineStage::Transcribe, not_installed)),
            Some(PipelineStage::Transcribe)
        );
        let other: BoxError = "the child died".into();
        let passed = refusing(PipelineStage::Transcribe, other);
        assert_eq!(refusal(&passed), None);
        assert_eq!(passed.to_string(), "the child died");
    }

    /// A download that installed its models resumes the waiting meetings;
    /// a failed one does not.
    #[test]
    fn an_install_resumes_the_waiting_meetings_and_a_failed_one_does_not() {
        let resumed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let models = ResumeAfterInstall::new(steno_host::fakes::FakeSpeechModels::default(), {
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
