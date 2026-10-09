//! The host's `Pipeline` over the processing pipeline and the retention
//! sweep, with the reload the Settings commands ask for. Swift:
//! `AppEnvironment.reloadPipeline`, `runRetentionSweep`, and the
//! `Task`s `MeetingDetailViewModel` ran the re-runs in.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use steno_core::protocols::BoundaryResult;
use steno_core::{AudioRetention, MeetingStateKind};
use steno_host::services::{Pipeline, ProcessAgainRefusal};
use steno_pipeline::{
    ExportRetries, InFlight, ModelWaits, Operation, PipelineDependencies, PipelineFailure,
    ProcessingPipeline, QuitLatch, ReprocessError, RetentionSweep, SweepIncomplete,
};
use steno_speech::SpeechRuntime;
use uuid::Uuid;

use crate::app::BuildError;
use crate::block_on;

/// The speech engine a pipeline was built with: the engine id the
/// settings named at the build and where [`SpeechSetup::runtime`] runs it.
/// The recorder's warm-up reads it from [`CurrentPipeline`], not the id
/// stored now (Rust only: Swift asked about the stored id).
///
/// [`SpeechSetup::runtime`]: crate::speech::SpeechSetup::runtime
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltEngine {
    /// The stored engine id the build read.
    pub engine_id: String,
    /// Where the pipeline's speech engine runs.
    pub runtime: SpeechRuntime,
}

/// One pipeline's dependencies and the speech engine they hold.
pub struct BuiltPipeline {
    /// What `ProcessingPipeline::new` takes.
    pub dependencies: PipelineDependencies,
    /// Which speech engine `dependencies` hold and where it runs.
    pub engine: BuiltEngine,
}

/// Rebuilds the dependencies, and the engine they hold, from the stored
/// settings and the API key.
pub type MakeDependencies = Arc<dyn Fn() -> Result<BuiltPipeline, BuildError> + Send + Sync>;

/// Whether the models a pipeline whose speech engine runs on the runtime
/// needs are installed now: the app's gates' checks
/// ([`SpeechEngines::models_installed`](crate::speech::SpeechEngines::models_installed)).
pub type ModelsInstalled = Arc<dyn Fn(SpeechRuntime) -> bool + Send + Sync>;

/// The pipeline [`CurrentPipeline`] holds, with the engine it was built
/// with, swapped together.
struct Current {
    pipeline: ProcessingPipeline,
    engine: BuiltEngine,
}

impl Current {
    /// The pipeline over `built`, sharing `latch`, `in_flight` and `waits`.
    fn new(
        built: BuiltPipeline,
        latch: &QuitLatch,
        in_flight: &InFlight,
        waits: &ModelWaits,
    ) -> Self {
        Current {
            pipeline: ProcessingPipeline::new(
                built
                    .dependencies
                    .with_quit_latch(latch.clone())
                    .with_in_flight(in_flight.clone())
                    .with_model_waits(waits.clone()),
            ),
            engine: built.engine,
        }
    }
}

/// The current pipeline behind a swap: a reload replaces it first, so a
/// Save never waits for a run in progress; the retired pipeline is kept
/// until it is idle, so meetings in flight finish on the dependencies they
/// started with. Everything that enqueues resolves `current()` per call,
/// so a recording that ends after a reload goes through the new one.
/// Every pipeline it builds shares one [`QuitLatch`], so
/// [`quit`](Self::quit) reaches the retired ones too, one in-flight set
/// ([`InFlight`]), so the new one refuses a meeting a retired one still
/// holds, and one [`ModelWaits`], so
/// [`resume_waiting`](Self::resume_waiting) starts the meetings any of
/// them left waiting for models, on the current one (the rules are on
/// [`ModelWaits`]). Held by `App`, the recorder, the phone intake and
/// [`HostPipeline`].
pub struct CurrentPipeline {
    current: Mutex<Current>,
    make: MakeDependencies,
    runtime: tokio::runtime::Handle,
    quit_latch: QuitLatch,
    in_flight: InFlight,
    model_waits: ModelWaits,
    /// Whether a resume starts the waiting meetings
    /// ([`resuming_when`](Self::resuming_when)).
    models_installed: ModelsInstalled,
}

impl CurrentPipeline {
    /// The pipeline over `built`, reloading through `make`.
    #[must_use]
    pub fn new(
        built: BuiltPipeline,
        make: MakeDependencies,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let quit_latch = QuitLatch::default();
        let in_flight = InFlight::default();
        let model_waits = ModelWaits::default();
        CurrentPipeline {
            current: Mutex::new(Current::new(built, &quit_latch, &in_flight, &model_waits)),
            make,
            runtime,
            quit_latch,
            in_flight,
            model_waits,
            models_installed: Arc::new(|_| true),
        }
    }

    /// [`resume_waiting`](Self::resume_waiting), an install's or a
    /// reload's, starts the waiting meetings only while `installed` holds
    /// for the current pipeline's runtime, so an install that leaves
    /// another model missing (the diarizer's finished before Parakeet v3)
    /// or a settings save with the models still missing leaves their cards
    /// asking for the download instead of starting each meeting only to
    /// see it refused again. Without it every resume starts them.
    #[must_use]
    pub fn resuming_when(mut self, installed: ModelsInstalled) -> Self {
        self.models_installed = installed;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Current> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The pipeline new work goes to.
    #[must_use]
    pub fn current(&self) -> ProcessingPipeline {
        self.lock().pipeline.clone()
    }

    /// The current pipeline and the speech engine it was built with, read
    /// together, so a reload in between cannot pair one pipeline with
    /// another's engine.
    #[must_use]
    pub fn current_with_engine(&self) -> (ProcessingPipeline, BuiltEngine) {
        let current = self.lock();
        (current.pipeline.clone(), current.engine.clone())
    }

    /// [`ProcessingPipeline::resume_waiting`] on the current pipeline,
    /// from any thread, once its models are installed
    /// ([`resuming_when`](Self::resuming_when)): the meetings a run on any
    /// of its pipelines left `queued` for missing models since the last
    /// resume start on the runtime. Called once a model install from
    /// Settings or onboarding finished, and after every
    /// [`reload`](Self::reload), so models the `steno` command installed
    /// into the app's models directory are picked up at the next settings
    /// change (or the next launch); it returns at once while a model is
    /// missing or nothing waits. A failure, a busy store included, is
    /// logged and not retried: the meetings stay `queued` for the next of
    /// these or the launch's recovery. Not the launch's recovery itself,
    /// which is [`ProcessingPipeline::resume_unfinished`] over every
    /// queued meeting.
    pub fn resume_waiting(&self) {
        let _entered = self.runtime.enter();
        let (pipeline, engine) = self.current_with_engine();
        if !(self.models_installed)(engine.runtime) {
            return;
        }
        match pipeline.resume_waiting() {
            Ok(resumed) if !resumed.is_empty() => {
                tracing::info!(count = resumed.len(), "resumed meetings waiting for models");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "meetings waiting for models could not be resumed");
            }
        }
    }

    /// Quits every pipeline this one built or builds from now on, the
    /// current one, the retired ones still finishing and a reload's
    /// replacement: see [`ProcessingPipeline::quit`].
    pub fn quit(&self) {
        self.quit_latch.set();
    }

    /// Replaces the pipeline with one built from the stored settings and
    /// the secret store's API key. A failed build keeps the current
    /// pipeline and its engine. The app's builds keep the speech engine,
    /// with its claims, while the stored engine id runs where it did, and
    /// the diarizer ([`SpeechEngines`](crate::speech::SpeechEngines)), so
    /// the retired pipeline's jobs and the new one's share them and the
    /// one sidecar child. Then it resumes the meetings waiting for models
    /// on the new pipeline once they are installed
    /// ([`resume_waiting`](Self::resume_waiting)).
    pub fn reload(&self) -> Result<(), BuildError> {
        let replacement = Current::new(
            (self.make)()?,
            &self.quit_latch,
            &self.in_flight,
            &self.model_waits,
        );
        let retired = std::mem::replace(&mut *self.lock(), replacement).pipeline;
        self.runtime
            .spawn(async move { retired.wait_until_idle().await });
        self.resume_waiting();
        Ok(())
    }

    /// Claims the meeting on the current pipeline in place, so a refusal
    /// is the call's error, then spawns the claimed operation on the
    /// runtime; its failure reaches the detail's error line as the
    /// `OperationFailed` event it posts.
    fn claim_and_spawn(
        &self,
        claim: impl FnOnce(&ProcessingPipeline) -> Result<Operation, PipelineFailure>,
    ) -> Result<(), PipelineFailure> {
        let operation = claim(&self.current())?;
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
/// here would park the runtime's workers on that lock (the event loop and
/// the poll take it) until nobody is left to drive the request. A failure
/// after the claim is posted as `MeetingEvent::OperationFailed`, which the
/// host shows on the detail's error line. `apply_retention` touches the store
/// only and completes in place, so the detail the host reads back right
/// after already shows the new rule. "Process again" saves the meeting
/// queued in place and spawns its run, so the detail the host reads back
/// shows it queued.
pub struct HostPipeline {
    pub pipeline: Arc<CurrentPipeline>,
    pub sweep: RetentionSweep,
    /// Reset by any re-export the user causes, and read for the detail's
    /// "keeps failing" line.
    pub export_retries: Arc<ExportRetries>,
}

impl Pipeline for HostPipeline {
    /// A summary re-run exports the new summary, so, once the meeting is
    /// claimed, it resets the count of launch re-exports as `redeliver`
    /// does.
    fn rerun_summary(&self, meeting_id: Uuid, template_id: &str) -> BoundaryResult<()> {
        self.pipeline
            .claim_and_spawn(|pipeline| pipeline.claim_rerun_summary(meeting_id, template_id))?;
        self.export_retries.reset(meeting_id);
        Ok(())
    }

    /// Export again, or the re-export after a speaker change. Once the
    /// meeting is claimed, its count of launch re-exports starts again from
    /// 0; a refused one leaves the count.
    fn redeliver(&self, meeting_id: Uuid) -> BoundaryResult<()> {
        self.pipeline
            .claim_and_spawn(|pipeline| pipeline.claim_redeliver(meeting_id))?;
        self.export_retries.reset(meeting_id);
        Ok(())
    }

    fn export_keeps_failing(&self, meeting_id: Uuid) -> bool {
        self.export_retries.stopped(meeting_id)
    }

    fn process_again(&self, meeting_id: Uuid) -> Result<(), ProcessAgainRefusal> {
        // `reprocess` spawns the run on the runtime it is called in; the
        // host calls from its own thread.
        let _runtime = self.pipeline.runtime.enter();
        self.pipeline
            .current()
            .process_again(meeting_id)
            .map_err(process_again_refusal)
    }

    /// From the current pipeline's [`DamagedAudio`](steno_pipeline::DamagedAudio),
    /// which every reload shares.
    fn damaged_audio(&self, meeting_id: Uuid) -> steno_core::AudioDamage {
        self.pipeline
            .current()
            .dependencies()
            .damaged_audio
            .damage(meeting_id)
    }

    /// [`DamagedAudio::may_be_damaged`](steno_pipeline::DamagedAudio::may_be_damaged)
    /// of the current pipeline's store.
    fn audio_may_be_damaged(&self, meeting_id: Uuid) -> bool {
        self.pipeline
            .current()
            .dependencies()
            .damaged_audio
            .may_be_damaged(meeting_id)
    }

    fn apply_retention(&self, meeting_id: Uuid, rule: AudioRetention) -> BoundaryResult<()> {
        let pipeline = self.pipeline.current();
        Ok(block_on(
            &self.pipeline.runtime,
            pipeline.apply_retention(meeting_id, rule),
        )?)
    }

    fn reload(&self) -> BoundaryResult<()> {
        Ok(self.pipeline.reload()?)
    }

    fn keep_all_recordings(&self) -> BoundaryResult<i64> {
        Ok(self
            .sweep
            .keep_all()
            .map(|count| i64::try_from(count).unwrap_or(i64::MAX))?)
    }
}

/// The host's reading of a refused [`ProcessingPipeline::process_again`]:
/// a queued or processing meeting is already being processed unless it
/// waits for its models, and a meeting without a recording on record
/// reads as one whose recording is gone, as the pipeline's docs ask.
fn process_again_refusal(error: ReprocessError) -> ProcessAgainRefusal {
    match error {
        ReprocessError::MeetingNotFound(_) => ProcessAgainRefusal::MeetingGone,
        ReprocessError::Unfinished {
            state: MeetingStateKind::Queued | MeetingStateKind::Processing,
            ..
        }
        | ReprocessError::Busy(_) => ProcessAgainRefusal::Busy,
        ReprocessError::WaitingForModels(_) => ProcessAgainRefusal::ModelsMissing,
        ReprocessError::Unfinished { .. } | ReprocessError::NotOffered(_) => {
            ProcessAgainRefusal::NotOffered
        }
        ReprocessError::NoAsset(_) | ReprocessError::AudioGone(_) => {
            ProcessAgainRefusal::RecordingGone
        }
        ReprocessError::Quitting => ProcessAgainRefusal::Quitting,
        ReprocessError::Pipeline(failure) => {
            ProcessAgainRefusal::CouldNotStart(failure.to_string())
        }
    }
}

/// The sweep at launch and after every processed meeting; a partial sweep
/// is logged, not fatal. Warn says how many files stayed; their paths,
/// which name the meetings' folders, go to debug.
pub fn run_sweep(sweep: &RetentionSweep) {
    match sweep.run(Utc::now()) {
        Ok(_) => {}
        Err(SweepIncomplete::Files(failures)) => {
            tracing::warn!(
                count = failures.len(),
                "retention sweep could not remove every file"
            );
            for (path, error) in &failures {
                tracing::debug!(path = %path.display(), %error, "retention sweep kept a file");
            }
        }
        Err(SweepIncomplete::Store(error)) => {
            tracing::warn!(%error, "retention sweep could not read the store");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use steno_core::{
        AudioBuffer16k, Delivery, DeliveryDispatcher, LanguageTag, MeetingEvent, MeetingOperation,
        MeetingState, MeetingSummarizer, PipelineStage, RawSegment, SpeechEngine, Store,
        SummaryInput, SummaryOutput, async_trait,
        testing::{FakeSpeechEngine, sample_data},
    };
    use steno_pipeline::EventReceiver;

    use super::*;
    use crate::speech::SpeechEngines;
    use crate::testing::{
        PATIENCE, current_pipeline, eventually, fake_dependencies, on_own_thread, temp_store,
    };

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

    /// A summarizer that panics.
    struct PanickingSummarizer;

    #[async_trait]
    impl MeetingSummarizer for PanickingSummarizer {
        async fn summarize(&self, _input: &SummaryInput) -> BoundaryResult<SummaryOutput> {
            panic!("the summarizer fell over");
        }
    }

    /// A dispatcher that delivers nothing until released, or panics.
    struct HeldDispatcher {
        release: Arc<tokio::sync::Notify>,
        asked: Arc<tokio::sync::Notify>,
        panics: bool,
    }

    #[async_trait]
    impl DeliveryDispatcher for HeldDispatcher {
        async fn deliver_all(&self, _meeting_id: Uuid) -> Vec<Delivery> {
            assert!(!self.panics, "the dispatcher fell over");
            self.asked.notify_one();
            self.release.notified().await;
            Vec::new()
        }
    }

    fn pipeline(
        store: &Arc<Store>,
        summarizer: Option<Arc<dyn MeetingSummarizer>>,
    ) -> HostPipeline {
        pipeline_over(
            fake_dependencies(store, "fake-engine").with_llm(None, summarizer),
            store,
        )
    }

    fn pipeline_over(dependencies: PipelineDependencies, store: &Arc<Store>) -> HostPipeline {
        HostPipeline {
            pipeline: current_pipeline(dependencies),
            sweep: RetentionSweep::new(store.clone()),
            export_retries: Arc::new(ExportRetries::in_memory()),
        }
    }

    /// A pipeline whose re-exports go through a [`HeldDispatcher`].
    fn held_re_exports(
        store: &Arc<Store>,
        panics: bool,
    ) -> (
        HostPipeline,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        let release = Arc::new(tokio::sync::Notify::new());
        let asked = Arc::new(tokio::sync::Notify::new());
        let mut dependencies = fake_dependencies(store, "fake-engine");
        dependencies.dispatcher = Arc::new(HeldDispatcher {
            release: release.clone(),
            asked: asked.clone(),
            panics,
        });
        (pipeline_over(dependencies, store), release, asked)
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

    /// Calls one of the host's synchronous methods as the host does; the
    /// error as the detail shows it.
    fn call<T: Send + 'static>(
        f: impl FnOnce() -> BoundaryResult<T> + Send + 'static,
    ) -> Result<T, String> {
        on_own_thread(
            PATIENCE,
            "the call returned without waiting for the work",
            move || f().map_err(|error| error.to_string()),
        )
    }

    /// Waits until the held summarizer is asked.
    async fn reached(asked: &tokio::sync::Notify) {
        tokio::time::timeout(PATIENCE, asked.notified())
            .await
            .expect("the re-run reached the summarizer");
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

    /// A file the sweep could not remove is counted at warn; its path,
    /// which names the meeting's folder, is not.
    #[test]
    fn a_partial_sweep_warns_with_a_count_not_the_paths() {
        let (dir, store) = temp_store();
        let mut meeting = sample_data::meeting();
        meeting.state = MeetingState::Ready;
        // A directory where the master should be: removing it as a file fails.
        let master = dir.path().join("private-folder").join("audio.wav");
        std::fs::create_dir_all(master.join("inside")).unwrap();
        let asset = steno_core::AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: steno_core::paths::file_url(&master, false),
            format: steno_core::AudioFormat::Wav16kInt16,
            lanes: vec![steno_core::AudioLane::Mixed],
            sidecars_16k: std::collections::BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::KeepDays(1),
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        };
        store.save_meeting_with_asset(&meeting, &asset).unwrap();
        let log = steno_pipeline::fixtures::CapturedLog::warnings();
        run_sweep(&RetentionSweep::new(store.clone()));
        let text = log.text();
        assert!(
            text.lines().any(
                |line| line.contains("retention sweep could not remove every file")
                    && line.contains("count=1")
            ),
            "{text}"
        );
        assert!(!text.contains("private-folder"), "{text}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_summary_re_run_returns_before_the_summarizer_answers() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let (summarizer, release, asked) = held();
        let service = pipeline(&store, Some(Arc::new(summarizer)));

        let returned = call(move || service.rerun_summary(meeting.id, &meeting.template_id));
        assert_eq!(returned, Ok(()));
        // The run was started: the summarizer is being asked.
        reached(&asked).await;
        release.notify_one();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_re_run_or_re_export_is_the_error_of_the_call() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let without_llm = pipeline(&store, None);
        let id = meeting.id;
        assert_eq!(
            call(move || without_llm.rerun_summary(id, "default")),
            Err("summarize: no LLM endpoint is configured".to_owned())
        );

        let (summarizer, release, asked) = held();
        let service = Arc::new(pipeline(&store, Some(Arc::new(summarizer))));
        let first = service.clone();
        assert_eq!(call(move || first.rerun_summary(id, "default")), Ok(()));
        reached(&asked).await;
        let busy = format!("meeting {id} is already being processed");
        let second = service.clone();
        assert_eq!(
            call(move || second.redeliver(id)),
            Err(format!("deliver: {busy}"))
        );
        let third = service.clone();
        assert_eq!(
            call(move || third.rerun_summary(id, "default")),
            Err(format!("summarize: {busy}"))
        );
        release.notify_one();

        let unknown = Uuid::new_v4();
        let fourth = service.clone();
        assert_eq!(
            call(move || fourth.redeliver(unknown)),
            Err(format!("deliver: meeting {unknown} not found"))
        );
    }

    /// A new phone meeting, still recording.
    fn phone_meeting() -> steno_core::Meeting {
        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        meeting.source = steno_core::MeetingSource::Phone;
        meeting.state = MeetingState::Recording;
        meeting
    }

    /// The phone meeting of `fixture` (a file in `Tests/Fixtures/audio/`,
    /// copied into `dir`) processed through `service`'s real decoder, to
    /// the state it ends in.
    async fn process_phone_fixture(
        service: &HostPipeline,
        store: &Store,
        dir: &std::path::Path,
        meeting: &steno_core::Meeting,
        fixture: &str,
    ) -> MeetingState {
        let upload = dir.join(format!("{fixture}-{}.m4a", Uuid::new_v4()));
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../Tests/Fixtures/audio")
                .join(fixture),
            &upload,
        )
        .unwrap();
        let asset = steno_core::AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: steno_core::paths::file_url(&upload, false),
            format: steno_core::AudioFormat::M4aAac,
            lanes: vec![steno_core::AudioLane::Mixed],
            sidecars_16k: std::collections::BTreeMap::new(),
            mixdown_url: None,
            retention: AudioRetention::KeepForever,
            expires_at: None,
        };
        let pipeline = service.pipeline.current();
        pipeline.enqueue(meeting, &asset).unwrap();
        pipeline.wait_until_idle().await;
        store.meeting(meeting.id).unwrap().unwrap().state
    }

    /// The damage the real decoder counts reaches the detail: a phone
    /// recording with three undecodable packets processes to Ready, and
    /// the host pipeline answers 3 parts and their 3 * 1 024 frames of
    /// silence for it, from the support directory's file, after a reload
    /// too; the same meeting processed again from a clean file answers
    /// none.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_damage_of_a_recording_reaches_the_detail() {
        let (dir, store) = temp_store();
        let damaged = Arc::new(steno_pipeline::DamagedAudio::in_directory(dir.path()));
        let service = pipeline_over(
            fake_dependencies(&store, "fake-engine").with_damaged_audio(damaged),
            &store,
        );
        let meeting = phone_meeting();
        let state = process_phone_fixture(
            &service,
            &store,
            dir.path(),
            &meeting,
            "tone-440-44k1-500ms-damaged.m4a",
        )
        .await;
        assert_eq!(state, MeetingState::Ready);
        let damage = service.damaged_audio(meeting.id);
        assert_eq!(damage.parts, 3);
        assert!(service.audio_may_be_damaged(meeting.id));
        assert!((damage.seconds - 3.0 * 1_024.0 / 44_100.0).abs() < 1e-9);
        service.pipeline.reload().unwrap();
        assert_eq!(service.damaged_audio(meeting.id), damage, "after a reload");
        assert_eq!(
            steno_pipeline::DamagedAudio::in_directory(dir.path()).parts(meeting.id),
            3,
            "on disk"
        );
        assert!(service.damaged_audio(Uuid::new_v4()).is_none());
        assert!(!service.audio_may_be_damaged(Uuid::new_v4()));

        let state = process_phone_fixture(
            &service,
            &store,
            dir.path(),
            &meeting,
            "tone-440-44k1-500ms.m4a",
        )
        .await;
        assert_eq!(state, MeetingState::Ready);
        assert!(service.damaged_audio(meeting.id).is_none());
        assert!(!service.audio_may_be_damaged(meeting.id));
    }

    /// A record of the damage that does not parse says nothing about any
    /// meeting, so the host pipeline answers that every recording may be
    /// damaged, while the detail's warning shows no damage it cannot
    /// read.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_damage_record_makes_every_recording_possibly_damaged() {
        let (dir, store) = temp_store();
        std::fs::write(
            dir.path().join(steno_pipeline::DamagedAudio::FILE_NAME),
            b"{",
        )
        .unwrap();
        let damaged = Arc::new(steno_pipeline::DamagedAudio::in_directory(dir.path()));
        let service = pipeline_over(
            fake_dependencies(&store, "fake-engine").with_damaged_audio(damaged),
            &store,
        );
        let meeting = Uuid::new_v4();
        assert!(service.audio_may_be_damaged(meeting));
        assert!(service.damaged_audio(meeting).is_none());
    }

    /// A damaged recording whose damage cannot be written down (a support
    /// directory that may not be written) fails the meeting at the decode
    /// stage, so the retention rule never stamps it, and the recording
    /// stays; a clean one, which changes nothing in the file, processes.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn damage_that_cannot_be_written_fails_the_meeting() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, store) = temp_store();
        let support = dir.path().join("support");
        std::fs::create_dir(&support).unwrap();
        let damaged = Arc::new(steno_pipeline::DamagedAudio::in_directory(&support));
        let service = pipeline_over(
            fake_dependencies(&store, "fake-engine").with_damaged_audio(damaged),
            &store,
        );
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o500)).unwrap();
        let meeting = phone_meeting();
        let clean = process_phone_fixture(
            &service,
            &store,
            dir.path(),
            &meeting,
            "tone-440-44k1-500ms.m4a",
        )
        .await;
        let second = phone_meeting();
        let damaged = process_phone_fixture(
            &service,
            &store,
            dir.path(),
            &second,
            "tone-440-44k1-500ms-damaged.m4a",
        )
        .await;
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(clean, MeetingState::Ready);
        // Root writes anywhere: then the damage was written and the
        // meeting is ready.
        if support
            .join(steno_pipeline::DamagedAudio::FILE_NAME)
            .exists()
        {
            return;
        }
        assert!(
            damaged.failure_reason().is_some_and(|reason| reason
                .contains("what could not be read in the recording could not be saved")),
            "{damaged:?}"
        );
        let asset = store.asset(second.id).unwrap().expect("the asset");
        let master = steno_core::paths::file_url_path(&asset.url).unwrap();
        assert!(master.exists(), "the recording stays");
    }

    /// A summary re-run exports the new summary, so it starts the count of
    /// launch re-exports from 0 as Export again does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_run_resets_the_launch_re_export_count() {
        let (dir, store) = temp_store();
        let id = ready_meeting(&store).id;
        let path = dir.path().join(ExportRetries::FILE_NAME);
        std::fs::write(&path, format!("{{\"{id}\": {}}}", ExportRetries::LIMIT)).unwrap();
        let (summarizer, release, asked) = held();
        let mut service = pipeline(&store, Some(Arc::new(summarizer)));
        service.export_retries = Arc::new(ExportRetries::new(&path));
        let service = Arc::new(service);
        let first = service.clone();
        assert_eq!(call(move || first.rerun_summary(id, "default")), Ok(()));
        assert!(!service.export_keeps_failing(id));
        assert_eq!(ExportRetries::new(&path).count(id), 0);
        reached(&asked).await;
        release.notify_one();
        eventually("the re-run released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_run_that_fails_later_posts_the_failure_on_the_bus() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let (summarizer, release, asked) = held();
        let service = Arc::new(pipeline(&store, Some(Arc::new(summarizer))));
        let mut receiver = service.pipeline.current().dependencies().events.subscribe();
        let id = meeting.id;
        let caller = service.clone();
        assert_eq!(call(move || caller.rerun_summary(id, "default")), Ok(()));
        reached(&asked).await;
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
                stage: PipelineStage::Summarize,
                failure: "summarize: released without a summary".to_owned(),
            }
        );
        // The meeting is released for the next operation.
        let again = service.clone();
        eventually("the meeting was released", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
        assert_eq!(call(move || again.redeliver(id)), Ok(()));
    }

    /// A re-export holds its meeting until it ends: while it runs, a
    /// second re-export and a re-run are refused; once it ends, the
    /// meeting is free.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_running_re_export_holds_its_meeting_until_it_ends() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let id = meeting.id;
        let (service, release, asked) = held_re_exports(&store, false);
        let service = Arc::new(service);
        let first = service.clone();
        assert_eq!(call(move || first.redeliver(id)), Ok(()));
        reached(&asked).await;
        assert_eq!(service.pipeline.current().in_flight(), vec![id]);
        let busy = format!("meeting {id} is already being processed");
        let second = service.clone();
        assert_eq!(
            call(move || second.redeliver(id)),
            Err(format!("deliver: {busy}"))
        );
        release.notify_one();
        eventually("the re-export released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
    }

    /// A reload builds its pipeline from fresh dependencies, yet a re-export
    /// the retired pipeline is still running holds its meeting on the new
    /// one too: a second re-export is refused instead of running alongside.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn after_a_reload_a_meeting_the_retired_pipeline_re_exports_is_refused() {
        let (_dir, store) = temp_store();
        let id = ready_meeting(&store).id;
        let release = Arc::new(tokio::sync::Notify::new());
        let asked = Arc::new(tokio::sync::Notify::new());
        let make: MakeDependencies = {
            let (store, release, asked) = (store.clone(), release.clone(), asked.clone());
            Arc::new(move || {
                let mut dependencies = fake_dependencies(&store, "fake-engine");
                dependencies.dispatcher = Arc::new(HeldDispatcher {
                    release: release.clone(),
                    asked: asked.clone(),
                    panics: false,
                });
                Ok(crate::testing::built(dependencies))
            })
        };
        let service = Arc::new(HostPipeline {
            pipeline: Arc::new(CurrentPipeline::new(
                make().unwrap(),
                make,
                tokio::runtime::Handle::current(),
            )),
            sweep: RetentionSweep::new(store.clone()),
            export_retries: Arc::new(ExportRetries::in_memory()),
        });
        let first = service.clone();
        assert_eq!(call(move || first.redeliver(id)), Ok(()));
        reached(&asked).await;
        service.pipeline.reload().unwrap();
        assert_eq!(service.pipeline.current().in_flight(), vec![id]);
        let second = service.clone();
        assert_eq!(
            call(move || second.redeliver(id)),
            Err(format!("deliver: meeting {id} is already being processed"))
        );
        release.notify_one();
        eventually("the retired re-export released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
    }

    /// The user's Export again starts the meeting's count of launch
    /// re-exports from 0, on disk too, so the detail no longer says the
    /// export keeps failing and the next launch retries it. One refused
    /// because the meeting is busy leaves the count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn export_again_resets_the_launch_re_export_count() {
        let (dir, store) = temp_store();
        let id = ready_meeting(&store).id;
        let path = dir.path().join(ExportRetries::FILE_NAME);
        std::fs::write(&path, format!("{{\"{id}\": {}}}", ExportRetries::LIMIT)).unwrap();
        let (mut service, release, asked) = held_re_exports(&store, false);
        service.export_retries = Arc::new(ExportRetries::new(&path));
        let service = Arc::new(service);
        assert!(service.export_keeps_failing(id));

        let busy = service.pipeline.current().claim_redeliver(id).unwrap();
        let refused = service.clone();
        assert!(call(move || refused.redeliver(id)).is_err());
        assert!(
            service.export_keeps_failing(id),
            "a refused one resets nothing"
        );
        assert_eq!(ExportRetries::new(&path).count(id), ExportRetries::LIMIT);
        drop(busy);

        let first = service.clone();
        assert_eq!(call(move || first.redeliver(id)), Ok(()));
        assert!(!service.export_keeps_failing(id));
        assert_eq!(ExportRetries::new(&path).count(id), 0);
        reached(&asked).await;
        release.notify_one();
        eventually("the re-export released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;
    }

    /// A claimed operation that panics still reports: `OperationFailed`
    /// with its stage, and the meeting is released.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_run_or_re_export_that_panics_posts_the_failure_and_releases_the_meeting() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let id = meeting.id;
        let rerun = Arc::new(pipeline(&store, Some(Arc::new(PanickingSummarizer))));
        let mut receiver = rerun.pipeline.current().dependencies().events.subscribe();
        let caller = rerun.clone();
        assert_eq!(call(move || caller.rerun_summary(id, "default")), Ok(()));
        let failed = |event: &MeetingEvent| matches!(event, MeetingEvent::OperationFailed { .. });
        assert_eq!(
            next_event(&mut receiver, failed).await,
            MeetingEvent::OperationFailed {
                meeting_id: id,
                operation: MeetingOperation::SummaryRerun,
                stage: PipelineStage::Summarize,
                failure: format!("summarize: {}", steno_pipeline::OPERATION_PANICKED),
            }
        );
        eventually("the re-run released its meeting", || {
            rerun.pipeline.current().in_flight().is_empty()
        })
        .await;

        let (reexport, _release, _asked) = held_re_exports(&store, true);
        let mut receiver = reexport
            .pipeline
            .current()
            .dependencies()
            .events
            .subscribe();
        let reexport = Arc::new(reexport);
        let caller = reexport.clone();
        assert_eq!(call(move || caller.redeliver(id)), Ok(()));
        assert_eq!(
            next_event(&mut receiver, failed).await,
            MeetingEvent::OperationFailed {
                meeting_id: id,
                operation: MeetingOperation::Reexport,
                stage: PipelineStage::Deliver,
                failure: format!("deliver: {}", steno_pipeline::OPERATION_PANICKED),
            }
        );
        eventually("the re-export released its meeting", || {
            reexport.pipeline.current().in_flight().is_empty()
        })
        .await;
    }

    /// Calls `process_again` as the host does, from a thread outside the
    /// runtime.
    fn process_again(
        service: &Arc<HostPipeline>,
        meeting_id: Uuid,
    ) -> Result<(), ProcessAgainRefusal> {
        let service = service.clone();
        on_own_thread(PATIENCE, "process_again returned", move || {
            service.process_again(meeting_id)
        })
    }

    /// `meeting` saved in `state` with a six-second call recorded under
    /// `audio`.
    fn recorded_meeting(
        store: &Store,
        audio: &std::path::Path,
        state: MeetingState,
    ) -> (steno_core::Meeting, steno_core::AudioAsset) {
        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        meeting.state = state;
        let asset = steno_pipeline::fixtures::two_lane_call(
            audio,
            meeting.id,
            AudioRetention::KeepDays(30),
        )
        .unwrap();
        store.save_meeting_with_asset(&meeting, &asset).unwrap();
        (meeting, asset)
    }

    /// "Process again" on a failed meeting whose master is on disk saves it
    /// queued before the call returns, and the run takes it to ready.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_runs_a_failed_meeting_with_its_master_to_ready() {
        let (dir, store) = temp_store();
        let service = Arc::new(pipeline(&store, None));
        let failed = MeetingState::Failed {
            reason: "transcribe: the model is not installed".to_owned(),
        };
        let (meeting, _) = recorded_meeting(&store, dir.path(), failed);
        assert_eq!(process_again(&service, meeting.id), Ok(()));
        assert_ne!(
            meeting_state(&store, meeting.id).kind(),
            steno_core::MeetingStateKind::Failed,
            "saved queued before the call returned"
        );
        service.pipeline.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, meeting.id), MeetingState::Ready);
    }

    /// Each refusal of `reprocess` reaches the host as the refusal the
    /// detail words; a store failure keeps its text.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_process_again_says_why() {
        let (dir, store) = temp_store();
        let (summarizer, release, asked) = held();
        let service = Arc::new(pipeline(&store, Some(Arc::new(summarizer))));
        let failed = || MeetingState::Failed {
            reason: "decode: unreadable".to_owned(),
        };

        assert_eq!(
            process_again(&service, Uuid::new_v4()),
            Err(ProcessAgainRefusal::MeetingGone)
        );

        let mut without_asset = sample_data::meeting();
        without_asset.id = Uuid::new_v4();
        without_asset.state = failed();
        store.save_meeting(&without_asset).unwrap();
        assert_eq!(
            process_again(&service, without_asset.id),
            Err(ProcessAgainRefusal::RecordingGone)
        );

        let (swept, asset) = recorded_meeting(&store, dir.path(), failed());
        std::fs::remove_file(steno_core::paths::file_url_path(&asset.url).unwrap()).unwrap();
        assert_eq!(
            process_again(&service, swept.id),
            Err(ProcessAgainRefusal::RecordingGone)
        );

        let (held_meeting, _) = recorded_meeting(&store, dir.path(), failed());
        let id = held_meeting.id;
        let caller = service.clone();
        assert_eq!(call(move || caller.rerun_summary(id, "default")), Ok(()));
        reached(&asked).await;
        assert_eq!(process_again(&service, id), Err(ProcessAgainRefusal::Busy));
        release.notify_one();
        eventually("the re-run released its meeting", || {
            service.pipeline.current().in_flight().is_empty()
        })
        .await;

        service.pipeline.quit();
        assert_eq!(
            process_again(&service, id),
            Err(ProcessAgainRefusal::Quitting)
        );

        assert_eq!(
            process_again_refusal(ReprocessError::Pipeline(PipelineFailure::new(
                PipelineStage::Decode,
                "the disk is full"
            ))),
            ProcessAgainRefusal::CouldNotStart("decode: the disk is full".to_owned())
        );
    }

    /// "Process again" is for a meeting it is offered for: a ready one is
    /// refused below the host too, so a stale detail cannot run it again.
    /// A ready meeting whose recording was swept is refused as not offered
    /// too, not as one whose recording is gone: the rule is checked before
    /// the files.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_refuses_a_ready_meeting() {
        let (dir, store) = temp_store();
        let service = Arc::new(pipeline(&store, None));
        let (ready, _) = recorded_meeting(&store, dir.path(), MeetingState::Ready);
        assert_eq!(
            process_again(&service, ready.id),
            Err(ProcessAgainRefusal::NotOffered)
        );
        let (swept, asset) = recorded_meeting(&store, dir.path(), MeetingState::Ready);
        std::fs::remove_file(steno_core::paths::file_url_path(&asset.url).unwrap()).unwrap();
        assert_eq!(
            process_again(&service, swept.id),
            Err(ProcessAgainRefusal::NotOffered)
        );
        service.pipeline.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, ready.id), MeetingState::Ready);
        assert_eq!(meeting_state(&store, swept.id), MeetingState::Ready);
    }

    /// A queued or processing meeting is already being processed; a
    /// recording one is not offered.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_on_a_queued_or_processing_meeting_is_busy() {
        let (dir, store) = temp_store();
        let service = Arc::new(pipeline(&store, None));
        for state in [MeetingState::Queued, MeetingState::Processing] {
            let (meeting, _) = recorded_meeting(&store, dir.path(), state.clone());
            assert_eq!(
                process_again(&service, meeting.id),
                Err(ProcessAgainRefusal::Busy),
                "{state:?}"
            );
            assert_eq!(meeting_state(&store, meeting.id), state, "unchanged");
        }
        let (recording, _) = recorded_meeting(&store, dir.path(), MeetingState::Recording);
        assert_eq!(
            process_again(&service, recording.id),
            Err(ProcessAgainRefusal::NotOffered)
        );
    }

    /// What every [`FakeSidecar`] of a test shares: the children (alive
    /// now, the most alive at once, started and stopped) and the gate the
    /// first transcription waits at for `open` after notifying `entered`.
    #[derive(Default)]
    struct Children {
        live: AtomicUsize,
        most: AtomicUsize,
        spawns: AtomicUsize,
        stops: AtomicUsize,
        gate_used: AtomicBool,
        entered: tokio::sync::Notify,
        open: tokio::sync::Notify,
    }

    impl Children {
        fn counts(&self) -> [usize; 4] {
            [&self.live, &self.most, &self.spawns, &self.stops].map(|n| n.load(Ordering::SeqCst))
        }
    }

    /// A speech engine that runs a pretend child, as the sidecar engine
    /// does: `prepare` or `transcribe` starts it when none runs, `release`
    /// stops it.
    struct FakeSidecar {
        inner: FakeSpeechEngine,
        children: Arc<Children>,
        child: AtomicBool,
    }

    impl FakeSidecar {
        /// A sidecar engine with no child yet, counted in `children`.
        fn new(children: &Arc<Children>) -> Self {
            FakeSidecar {
                inner: FakeSpeechEngine::default(),
                children: children.clone(),
                child: AtomicBool::new(false),
            }
        }

        fn start_child(&self) {
            if !self.child.swap(true, Ordering::SeqCst) {
                let live = self.children.live.fetch_add(1, Ordering::SeqCst) + 1;
                self.children.most.fetch_max(live, Ordering::SeqCst);
                self.children.spawns.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for FakeSidecar {
        fn id(&self) -> &str {
            self.inner.id()
        }

        fn supported_languages(&self) -> &std::collections::BTreeSet<LanguageTag> {
            self.inner.supported_languages()
        }

        async fn prepare(&self) -> BoundaryResult<()> {
            self.start_child();
            self.inner.prepare().await
        }

        async fn transcribe(
            &self,
            audio: &AudioBuffer16k,
            hint: Option<&LanguageTag>,
        ) -> BoundaryResult<Vec<RawSegment>> {
            self.start_child();
            let gate = &self.children;
            if !gate.gate_used.swap(true, Ordering::SeqCst) {
                gate.entered.notify_one();
                gate.open.notified().await;
            }
            self.inner.transcribe(audio, hint).await
        }

        async fn release(&self) -> BoundaryResult<()> {
            if self.child.swap(false, Ordering::SeqCst) {
                self.children.live.fetch_sub(1, Ordering::SeqCst);
                self.children.stops.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    /// A job on a pipeline a reload retired still transcribes in the
    /// sidecar while a reload goes to the in-process engine and back and a
    /// job on the new pipeline transcribes too: both run on the one
    /// sidecar engine and its one child, the new job's end does not stop
    /// the child under the retired one's, and the retired job's end stops
    /// it once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_while_a_job_transcribes_keeps_one_sidecar_child() {
        let (dir, store) = temp_store();
        let children = Arc::new(Children::default());
        // The runtime of each build, in order.
        let builds = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engines = Arc::new(SpeechEngines::with_builder(
            crate::speech::testing::setup(dir.path(), steno_speech::SpeechSettings::default()),
            Box::new({
                let (children, builds) = (children.clone(), builds.clone());
                move |runtime| -> Arc<dyn SpeechEngine> {
                    builds.lock().unwrap().push(runtime);
                    match runtime {
                        SpeechRuntime::OnnxSidecar => Arc::new(FakeSidecar::new(&children)),
                        SpeechRuntime::CoreMlInProcess => Arc::new(FakeSpeechEngine::default()),
                    }
                }
            }),
        ));
        // The runtime the stored engine id runs on, as the next build sees it.
        let runtime = Arc::new(std::sync::Mutex::new(SpeechRuntime::OnnxSidecar));
        let make: MakeDependencies = {
            let (store, engines, runtime) = (store.clone(), engines.clone(), runtime.clone());
            Arc::new(move || {
                let runtime = *runtime.lock().unwrap();
                Ok(BuiltPipeline {
                    dependencies: fake_dependencies(&store, "fake-engine")
                        .with_speech_engine(engines.engine(runtime)),
                    engine: BuiltEngine {
                        engine_id: "parakeet-v3".to_owned(),
                        runtime,
                    },
                })
            })
        };
        let current =
            CurrentPipeline::new(make().unwrap(), make, tokio::runtime::Handle::current());
        let switch_to = |next| {
            *runtime.lock().unwrap() = next;
            current.reload().unwrap();
            current.current()
        };

        let retired = current.current();
        let held = enqueue_call(dir.path(), &retired);
        tokio::time::timeout(PATIENCE, children.entered.notified())
            .await
            .expect("the first job is transcribing");

        let in_process = switch_to(SpeechRuntime::CoreMlInProcess);
        assert!(
            !same_engine(&in_process, &retired),
            "another runtime, another engine"
        );
        let back = switch_to(SpeechRuntime::OnnxSidecar);
        assert!(
            same_engine(&back, &retired),
            "back in the sidecar: the engine the retired job runs on"
        );
        let unchanged = switch_to(SpeechRuntime::OnnxSidecar);
        assert!(
            same_engine(&unchanged, &back),
            "a reload that keeps the runtime keeps the engine"
        );
        assert_eq!(
            *builds.lock().unwrap(),
            [SpeechRuntime::OnnxSidecar, SpeechRuntime::CoreMlInProcess],
            "one per runtime, each on its own"
        );

        let next = enqueue_call(dir.path(), &unchanged);
        unchanged.wait_until_idle().await;
        assert_eq!(meeting_state(&store, next), MeetingState::Ready);
        // [live, most at once, started, stopped]
        assert_eq!(
            children.counts(),
            [1, 1, 1, 0],
            "the new job ran on the retired job's child and left it running"
        );

        // The reload's own wait took the retired job's handle, so
        // `retired.wait_until_idle` would return at once: wait for the
        // meeting and the child instead.
        children.open.notify_one();
        eventually("the retired job finished and stopped the child", || {
            meeting_state(&store, held) == MeetingState::Ready
                && children.stops.load(Ordering::SeqCst) == 1
        })
        .await;
        assert_eq!(children.counts(), [0, 1, 1, 1]);
    }

    /// A build of `dependencies` with `parakeet-v3` in the speech sidecar.
    fn on_the_sidecar(dependencies: PipelineDependencies) -> BuiltPipeline {
        BuiltPipeline {
            dependencies,
            engine: BuiltEngine {
                engine_id: "parakeet-v3".to_owned(),
                runtime: SpeechRuntime::OnnxSidecar,
            },
        }
    }

    /// A `CurrentPipeline` whose every pipeline, reloads included, shares
    /// `engine` behind one gate that refuses until the returned flag is set,
    /// and one event bus; its reloads resume only once the flag is set, as
    /// the app's do.
    fn gated_current(
        store: &Arc<Store>,
        engine: Arc<dyn SpeechEngine>,
    ) -> (Arc<AtomicBool>, CurrentPipeline) {
        let (installed, check) = crate::model_gate::testing::flag(false);
        let shared = steno_pipeline::SharedSpeechEngine::new(Arc::new(
            crate::model_gate::GatedSpeechEngine::new(engine, check.clone()),
        ));
        let events = steno_pipeline::MeetingEventBus::new();
        let make: MakeDependencies = {
            let store = store.clone();
            Arc::new(move || {
                let mut dependencies =
                    fake_dependencies(&store, "fake-engine").with_speech_engine(shared.clone());
                dependencies.events = events.clone();
                Ok(on_the_sidecar(dependencies))
            })
        };
        let current =
            CurrentPipeline::new(make().unwrap(), make, tokio::runtime::Handle::current())
                .resuming_when(Arc::new(move |_| check()));
        (installed, current)
    }

    /// Enqueues a two-lane call in `audio` on `pipeline` as a meeting of
    /// its own; its id.
    fn enqueue_call(audio: &std::path::Path, pipeline: &ProcessingPipeline) -> Uuid {
        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        let asset =
            steno_pipeline::fixtures::two_lane_call(audio, meeting.id, AudioRetention::KeepForever)
                .unwrap();
        pipeline.enqueue(&meeting, &asset).unwrap();
        meeting.id
    }

    fn meeting_state(store: &Store, id: Uuid) -> MeetingState {
        store.meeting(id).unwrap().unwrap().state
    }

    /// Whether `a` and `b` run on the same speech engine and its claims.
    fn same_engine(a: &ProcessingPipeline, b: &ProcessingPipeline) -> bool {
        a.dependencies()
            .speech_engine
            .ptr_eq(&b.dependencies().speech_engine)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_re_export_returns_at_once_and_runs_in_the_background() {
        let (_dir, store) = temp_store();
        let meeting = ready_meeting(&store);
        let service = pipeline(&store, None);
        let mut receiver = service.pipeline.current().dependencies().events.subscribe();
        let id = meeting.id;
        assert_eq!(call(move || service.redeliver(id)), Ok(()));
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

    /// A reload while a retired pipeline still transcribes meeting M, then
    /// a model install's resume on the current pipeline: M is not started
    /// again there, so it is processed once (one LLM pass, one export).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resume_after_a_reload_does_not_run_an_in_flight_meeting_twice() {
        let (dir, store) = temp_store();
        let children = Arc::new(Children::default());
        let sidecar = Arc::new(std::sync::Mutex::new(None::<Arc<FakeSidecar>>));
        let engines = Arc::new(SpeechEngines::with_builder(
            crate::speech::testing::setup(dir.path(), steno_speech::SpeechSettings::default()),
            Box::new({
                let (children, sidecar) = (children.clone(), sidecar.clone());
                move |_runtime| -> Arc<dyn SpeechEngine> {
                    let engine = Arc::new(FakeSidecar::new(&children));
                    *sidecar.lock().unwrap() = Some(engine.clone());
                    engine
                }
            }),
        ));
        let make: MakeDependencies = {
            let (store, engines) = (store.clone(), engines.clone());
            Arc::new(move || {
                Ok(on_the_sidecar(
                    fake_dependencies(&store, "fake-engine")
                        .with_speech_engine(engines.engine(SpeechRuntime::OnnxSidecar)),
                ))
            })
        };
        let current =
            CurrentPipeline::new(make().unwrap(), make, tokio::runtime::Handle::current());
        let retired = current.current();
        let held = enqueue_call(dir.path(), &retired);
        tokio::time::timeout(PATIENCE, children.entered.notified())
            .await
            .expect("the first job is transcribing");
        current.reload().unwrap();
        current.resume_waiting();
        children.open.notify_one();
        eventually("the meeting is ready and every run is done", || {
            meeting_state(&store, held) == MeetingState::Ready
                && current.current().in_flight().is_empty()
                && retired.in_flight().is_empty()
        })
        .await;
        current.current().wait_until_idle().await;
        let engine = sidecar.lock().unwrap().clone().unwrap();
        let after_held = engine.inner.transcriptions.count();
        let control = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, control), MeetingState::Ready);
        let one_run = engine.inner.transcriptions.count() - after_held;
        assert_eq!(
            after_held, one_run,
            "the in-flight meeting started again on the new pipeline"
        );
    }

    /// A meeting a run left waiting for models, which the pipeline a
    /// reload retired processes again on its own (`process`, holding the
    /// meeting in the shared in-flight set) when an install's resume runs
    /// on the current pipeline: the resume finds the meeting held through
    /// the set every pipeline of the `CurrentPipeline` shares and skips
    /// it, so the meeting is processed once more, not twice.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resume_after_a_reload_skips_a_waiting_meeting_the_retired_pipeline_holds() {
        let (dir, store) = temp_store();
        let children = Arc::new(Children::default());
        let engine = Arc::new(FakeSidecar::new(&children));
        let (installed, current) = gated_current(&store, engine.clone());
        // The reload comes first, as its own resume would start a meeting
        // waiting by then; the run on the retired pipeline then leaves the
        // meeting waiting.
        let retired = current.current();
        current.reload().unwrap();
        let waiting = enqueue_call(dir.path(), &retired);
        eventually("the refused run left the meeting waiting", || {
            current.current().dependencies().model_waits.waiting() == vec![waiting]
        })
        .await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);

        installed.store(true, Ordering::SeqCst);
        let asset_id = store.asset(waiting).unwrap().unwrap().id;
        let held = tokio::spawn({
            let retired = retired.clone();
            async move { retired.process(asset_id).await }
        });
        tokio::time::timeout(PATIENCE, children.entered.notified())
            .await
            .expect("the retired pipeline is transcribing the meeting");
        let before = engine.inner.transcriptions.count();
        current.resume_waiting();
        children.open.notify_one();
        held.await.unwrap().unwrap();
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Ready);
        let after_held = engine.inner.transcriptions.count() - before;
        let control = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, control), MeetingState::Ready);
        let one_run = engine.inner.transcriptions.count() - before - after_held;
        assert_eq!(
            after_held, one_run,
            "the waiting meeting the retired pipeline held ran again on the new one"
        );
    }

    /// Models the `steno` command installed into the app's models
    /// directory leave Settings with no Download to press, so no install
    /// resumes a meeting waiting for them: the next reload does, on the
    /// new pipeline.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_resumes_a_meeting_waiting_for_models_installed_meanwhile() {
        let (dir, store) = temp_store();
        let (installed, current) = gated_current(&store, Arc::new(FakeSpeechEngine::default()));
        let waiting = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);

        installed.store(true, Ordering::SeqCst);
        current.reload().unwrap();
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Ready);
        assert_eq!(
            current.current().dependencies().model_waits.waiting(),
            Vec::<Uuid>::new()
        );
    }

    /// A settings save while the models are still missing leaves the
    /// meetings waiting for them waiting: the reload starts none of them,
    /// so no card leaves "Download the speech models in Settings" for
    /// `decode` and comes back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_with_the_models_still_missing_leaves_the_waiting_meetings_waiting() {
        a_resume_with_the_models_still_missing_leaves_the_waiting_meetings_waiting(|current| {
            current.reload().unwrap();
        })
        .await;
    }

    /// The same for an install that leaves another model missing (the
    /// diarizer's finished, Parakeet v3's not yet): its resume starts none.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_install_with_a_model_still_missing_leaves_the_waiting_meetings_waiting() {
        a_resume_with_the_models_still_missing_leaves_the_waiting_meetings_waiting(
            CurrentPipeline::resume_waiting,
        )
        .await;
    }

    async fn a_resume_with_the_models_still_missing_leaves_the_waiting_meetings_waiting(
        resume: impl FnOnce(&CurrentPipeline),
    ) {
        let (dir, store) = temp_store();
        let (_installed, current) = gated_current(&store, Arc::new(FakeSpeechEngine::default()));
        let waiting = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        let mut events = current.current().dependencies().events.subscribe();

        resume(&current);
        current.current().wait_until_idle().await;
        let mut progress = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let steno_core::MeetingEvent::Progress { progress: p, .. } = event {
                progress.push(p.stage);
            }
        }
        assert_eq!(progress, Vec::<PipelineStage>::new(), "no run started");
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);
        assert_eq!(
            current.current().dependencies().model_waits.waiting(),
            [waiting]
        );
    }

    /// A meeting a run left waiting for models is not "already being
    /// processed": "Process again" from a stale detail says to download
    /// the models, and the meeting keeps waiting, untouched, until an
    /// install resumes it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_again_on_a_meeting_waiting_for_models_says_to_download_them() {
        let (dir, store) = temp_store();
        let (installed, current) = gated_current(&store, Arc::new(FakeSpeechEngine::default()));
        let service = Arc::new(HostPipeline {
            pipeline: Arc::new(current),
            sweep: RetentionSweep::new(store.clone()),
            export_retries: Arc::new(ExportRetries::in_memory()),
        });
        let waiting = enqueue_call(dir.path(), &service.pipeline.current());
        service.pipeline.current().wait_until_idle().await;

        assert_eq!(
            process_again(&service, waiting),
            Err(ProcessAgainRefusal::ModelsMissing)
        );
        service.pipeline.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);
        assert_eq!(
            service
                .pipeline
                .current()
                .dependencies()
                .model_waits
                .waiting(),
            [waiting]
        );

        installed.store(true, Ordering::SeqCst);
        service.pipeline.resume_waiting();
        service.pipeline.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Ready);
    }

    /// Which model a Settings Remove takes away during a run.
    #[derive(Clone, Copy, Debug)]
    enum Removed {
        Speech,
        Diarizer,
    }

    /// A boundary behind the app's gate whose first load waits at a gate
    /// (`checked`, then `go`) after the gate's check let it through, then
    /// loads as the app's loaders do: refused with the store's or the
    /// diarizer's `NotInstalled` while `installed` does not hold.
    struct LoadsAfterAGate {
        installed: crate::model_gate::InstalledCheck,
        not_installed: fn() -> steno_core::protocols::BoxError,
        gate_used: AtomicBool,
        checked: tokio::sync::Notify,
        go: tokio::sync::Notify,
        speech: FakeSpeechEngine,
        diarizer: steno_core::testing::FakeDiarizer,
    }

    impl LoadsAfterAGate {
        /// The boundary `removed` names, refusing as its loader does.
        fn new(removed: Removed, installed: crate::model_gate::InstalledCheck) -> Self {
            LoadsAfterAGate {
                installed,
                not_installed: match removed {
                    Removed::Speech => || {
                        Box::new(steno_speech::SpeechError::NotInstalled {
                            asset: "parakeet".to_owned(),
                            directory: "/m".into(),
                            missing: vec!["encoder".to_owned()],
                        })
                    },
                    Removed::Diarizer => || {
                        Box::new(steno_diarize::DiarizeError::NotInstalled {
                            asset: steno_diarize::models::ASSET_ID.to_owned(),
                            directory: "/m".into(),
                            missing: vec![steno_diarize::models::EMBEDDING_FILE.to_owned()],
                        })
                    },
                },
                gate_used: AtomicBool::new(false),
                checked: tokio::sync::Notify::new(),
                go: tokio::sync::Notify::new(),
                speech: FakeSpeechEngine::default(),
                diarizer: steno_core::testing::FakeDiarizer::default(),
            }
        }

        async fn load(&self) -> BoundaryResult<()> {
            if !self.gate_used.swap(true, Ordering::SeqCst) {
                self.checked.notify_one();
                self.go.notified().await;
            }
            if (self.installed)() {
                Ok(())
            } else {
                Err((self.not_installed)())
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for LoadsAfterAGate {
        fn id(&self) -> &str {
            self.speech.id()
        }

        fn supported_languages(&self) -> &std::collections::BTreeSet<LanguageTag> {
            self.speech.supported_languages()
        }

        async fn prepare(&self) -> BoundaryResult<()> {
            self.load().await
        }

        async fn transcribe(
            &self,
            audio: &AudioBuffer16k,
            hint: Option<&LanguageTag>,
        ) -> BoundaryResult<Vec<RawSegment>> {
            self.load().await?;
            self.speech.transcribe(audio, hint).await
        }
    }

    #[async_trait]
    impl steno_core::Diarizer for LoadsAfterAGate {
        async fn prepare(&self) -> BoundaryResult<()> {
            self.load().await
        }

        async fn diarize(
            &self,
            audio: &AudioBuffer16k,
        ) -> BoundaryResult<steno_core::DiarizationResult> {
            self.load().await?;
            self.diarizer.diarize(audio).await
        }
    }

    /// Pipelines whose speech engine or diarizer (`removed`'s) is
    /// `boundary` behind the app's gate over `installed`.
    fn behind_the_gate(
        store: &Arc<Store>,
        boundary: &Arc<LoadsAfterAGate>,
        installed: &crate::model_gate::InstalledCheck,
        removed: Removed,
    ) -> MakeDependencies {
        let (store, boundary, installed) = (store.clone(), boundary.clone(), installed.clone());
        Arc::new(move || {
            let mut dependencies = fake_dependencies(&store, "fake-engine");
            match removed {
                Removed::Speech => {
                    dependencies =
                        dependencies.with_speech_engine(steno_pipeline::SharedSpeechEngine::new(
                            Arc::new(crate::model_gate::GatedSpeechEngine::new(
                                boundary.clone(),
                                installed.clone(),
                            )),
                        ));
                }
                Removed::Diarizer => {
                    dependencies.diarizer = Arc::new(crate::model_gate::GatedDiarizer::new(
                        boundary.clone(),
                        installed.clone(),
                    ));
                }
            }
            Ok(on_the_sidecar(dependencies))
        })
    }

    /// A Settings Remove of `removed`'s files while a run of a meeting
    /// with "delete after processing" is past the gate's check and before
    /// the load: the run is refused for missing models, the meeting stays
    /// `queued` with no failure reason and waits, its audio is kept (no
    /// retention stamp, the sweep takes nothing), and a reinstall's resume
    /// processes it.
    async fn a_remove_during_a_run_leaves_the_meeting_waiting_with_its_audio(removed: Removed) {
        use steno_host::services::SpeechModels as _;
        use steno_host::speech::ModelAsset;
        let (dir, store) = temp_store();
        let models_directory = dir.path().join("models");
        let models = Arc::new(crate::speech::testing::models_in(&models_directory));
        crate::speech::testing::install_every_model(&models);
        let (asset, installed): (ModelAsset, crate::model_gate::InstalledCheck) = match removed {
            Removed::Speech => (ModelAsset::ParakeetV3, {
                let models = models.clone();
                Arc::new(move || models.is_installed(ModelAsset::ParakeetV3))
            }),
            Removed::Diarizer => (ModelAsset::OfflineDiarizer, {
                let store = models.speech.clone();
                Arc::new(move || steno_diarize::models::installed(&store).is_ok())
            }),
        };
        let boundary = Arc::new(LoadsAfterAGate::new(removed, installed.clone()));
        let make = behind_the_gate(&store, &boundary, &installed, removed);
        let current = Arc::new(CurrentPipeline::new(
            make().unwrap(),
            make,
            tokio::runtime::Handle::current(),
        ));
        let settings_models = crate::model_gate::ResumingSpeechModels::resuming(
            crate::speech::testing::models_in(&models_directory),
            current.clone(),
        );

        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        let audio = steno_pipeline::fixtures::two_lane_call(
            dir.path(),
            meeting.id,
            AudioRetention::DeleteAfterProcessing,
        )
        .unwrap();
        current.current().enqueue(&meeting, &audio).unwrap();
        tokio::time::timeout(PATIENCE, boundary.checked.notified())
            .await
            .expect("the run is past the gate's check");
        settings_models.remove(asset).unwrap();
        assert!(!settings_models.is_installed(asset), "{removed:?} removed");
        boundary.go.notify_one();
        current.current().wait_until_idle().await;

        let master = steno_core::paths::file_url_path(&audio.url).unwrap();
        let stored = store.meeting(meeting.id).unwrap().unwrap();
        assert_eq!(stored.state, MeetingState::Queued, "{removed:?}");
        assert_eq!(
            current.current().dependencies().model_waits.waiting(),
            vec![meeting.id]
        );
        assert_eq!(store.asset(meeting.id).unwrap().unwrap().expires_at, None);
        let swept = steno_pipeline::RetentionSweep::new(store.clone())
            .run(Utc::now() + chrono::Duration::days(365))
            .unwrap();
        assert!(swept.is_empty(), "{swept:?}");
        assert!(master.is_file(), "the recording is kept");

        crate::speech::testing::install_every_model(&models);
        current.resume_waiting();
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, meeting.id), MeetingState::Ready);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_remove_of_the_speech_model_during_a_run_leaves_the_meeting_waiting() {
        a_remove_during_a_run_leaves_the_meeting_waiting_with_its_audio(Removed::Speech).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_remove_of_the_diarizer_during_a_run_leaves_the_meeting_waiting() {
        a_remove_during_a_run_leaves_the_meeting_waiting_with_its_audio(Removed::Diarizer).await;
    }

    /// A meeting a run left waiting for models before a reload is started
    /// by the app's model service once an install finished, on the
    /// pipeline the reload built: every pipeline a `CurrentPipeline`
    /// builds shares its waiting meetings.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_install_after_a_reload_resumes_the_meeting_the_retired_pipeline_left_waiting() {
        use steno_host::services::SpeechModels as _;
        let (dir, store) = temp_store();
        let (installed, check) = crate::model_gate::testing::flag(false);
        let make: MakeDependencies = {
            let store = store.clone();
            Arc::new(move || {
                let gated = crate::model_gate::GatedSpeechEngine::new(
                    Arc::new(FakeSpeechEngine::default()),
                    check.clone(),
                );
                Ok(on_the_sidecar(
                    fake_dependencies(&store, "fake-engine").with_speech_engine(
                        steno_pipeline::SharedSpeechEngine::new(Arc::new(gated)),
                    ),
                ))
            })
        };
        let current = Arc::new(CurrentPipeline::new(
            make().unwrap(),
            make,
            tokio::runtime::Handle::current(),
        ));
        let models = crate::model_gate::ResumingSpeechModels::resuming(
            steno_host::fakes::FakeSpeechModels::default(),
            current.clone(),
        );
        let waiting = enqueue_call(dir.path(), &current.current());
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Queued);
        current.reload().unwrap();
        installed.store(true, Ordering::SeqCst);
        models
            .download(steno_host::speech::ModelAsset::ParakeetV3, &mut |_, _| {})
            .unwrap();
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, waiting), MeetingState::Ready);
    }

    /// A speech engine whose first transcription decides its model is
    /// missing and waits at a gate (`entered`, then `open`) before it
    /// returns the refusal; every later call is refused at once.
    #[derive(Default)]
    struct RefusingAfterAGate {
        used: AtomicBool,
        entered: tokio::sync::Notify,
        open: tokio::sync::Notify,
        inner: FakeSpeechEngine,
    }

    #[async_trait]
    impl SpeechEngine for RefusingAfterAGate {
        fn id(&self) -> &str {
            self.inner.id()
        }

        fn supported_languages(&self) -> &std::collections::BTreeSet<LanguageTag> {
            self.inner.supported_languages()
        }

        async fn prepare(&self) -> BoundaryResult<()> {
            self.inner.prepare().await
        }

        async fn transcribe(
            &self,
            _audio: &AudioBuffer16k,
            _hint: Option<&LanguageTag>,
        ) -> BoundaryResult<Vec<RawSegment>> {
            if !self.used.swap(true, Ordering::SeqCst) {
                self.entered.notify_one();
                self.open.notified().await;
            }
            Err(Box::new(PipelineFailure::models_missing(
                PipelineStage::Transcribe,
            )))
        }
    }

    /// A run being refused on a pipeline a reload then retired, whose
    /// meeting an install's resume skipped because it was in flight,
    /// starts the meeting again on the current pipeline, built from the
    /// settings saved last, not on the retired one's dependencies (whose
    /// engine here still refuses).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_run_on_a_retired_pipeline_starts_its_meeting_again_on_the_current_one() {
        let (dir, store) = temp_store();
        let refusing = Arc::new(RefusingAfterAGate::default());
        let builds = Arc::new(AtomicUsize::new(0));
        let make: MakeDependencies = {
            let (store, refusing, builds) = (store.clone(), refusing.clone(), builds.clone());
            Arc::new(move || {
                let dependencies = fake_dependencies(&store, "fake-engine");
                Ok(on_the_sidecar(
                    if builds.fetch_add(1, Ordering::SeqCst) == 0 {
                        dependencies.with_speech_engine(steno_pipeline::SharedSpeechEngine::new(
                            refusing.clone(),
                        ))
                    } else {
                        dependencies
                    },
                ))
            })
        };
        let current =
            CurrentPipeline::new(make().unwrap(), make, tokio::runtime::Handle::current());
        let meeting = enqueue_call(dir.path(), &current.current());
        tokio::time::timeout(PATIENCE, refusing.entered.notified())
            .await
            .expect("the run is transcribing");
        current.reload().unwrap();
        current.resume_waiting();
        refusing.open.notify_one();
        eventually("the meeting is ready", || {
            meeting_state(&store, meeting) == MeetingState::Ready
        })
        .await;
        assert_eq!(
            current.current().dependencies().model_waits.waiting(),
            Vec::<Uuid>::new()
        );
    }

    /// An install's resume starts only the meetings a run left waiting for
    /// models: a queued meeting no run refused stays queued for the
    /// launch's recovery, which then processes it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_install_resumes_no_queued_meeting_a_run_did_not_leave_waiting() {
        let (dir, store) = temp_store();
        let current = current_pipeline(fake_dependencies(&store, "fake-engine"));
        let mut meeting = sample_data::meeting();
        meeting.id = Uuid::new_v4();
        meeting.state = MeetingState::Queued;
        let asset = steno_pipeline::fixtures::two_lane_call(
            dir.path(),
            meeting.id,
            AudioRetention::KeepForever,
        )
        .unwrap();
        store.save_meeting_with_asset(&meeting, &asset).unwrap();

        current.resume_waiting();
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, meeting.id), MeetingState::Queued);

        assert_eq!(current.current().resume_unfinished().unwrap(), [meeting.id]);
        current.current().wait_until_idle().await;
        assert_eq!(meeting_state(&store, meeting.id), MeetingState::Ready);
    }
}
