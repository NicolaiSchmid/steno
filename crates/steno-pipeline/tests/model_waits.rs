//! A resume that lands while a refused run lets go of its meeting, a
//! refused run's restart, and the restart an exit stops. Their own test
//! binary: the gates are log lines, seen by the binary's global subscriber
//! (the first test, on a current-thread runtime) or by one its runtime's
//! worker threads alone use (the others), which no other test shares.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use steno_core::paths::{file_url, file_url_path};
use steno_core::protocols::BoundaryResult;
use steno_core::testing::{FakeDiarizer, FakeSpeechEngine, InMemorySpeakerMemory, sample_data};
use steno_core::{
    AudioAsset, AudioBuffer16k, AudioFormat, AudioLane, AudioRetention, Delivery,
    DeliveryDispatcher, LanguageTag, MeetingEvent, MeetingState, PipelineStage, RawSegment,
    SpeechEngine, Store, async_trait,
};
use steno_pipeline::{
    BACKGROUND_RUN_LOG, MeetingEventBus, PipelineDependencies, PipelineFailure, ProcessingPipeline,
};
use tracing_subscriber::layer::SubscriberExt as _;
use uuid::Uuid;

/// How long a test waits for a gate or for its runs to end.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// Reads the 16 kHz mono WAV fixtures.
struct WavDecoder;

#[async_trait]
impl steno_core::AudioDecoder for WavDecoder {
    async fn decode(&self, asset: &AudioAsset, lane: AudioLane) -> BoundaryResult<AudioBuffer16k> {
        let url = asset.sidecars_16k.get(&lane).unwrap_or(&asset.url);
        let bytes = std::fs::read(file_url_path(url).ok_or("not a file URL")?)?;
        Ok(AudioBuffer16k::new(
            bytes[44..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32768.0)
                .collect(),
        ))
    }

    fn mixdown_format(&self) -> AudioFormat {
        AudioFormat::Wav16kInt16
    }

    async fn mixdown(&self, asset: &AudioAsset, to: &Path) -> BoundaryResult<()> {
        std::fs::copy(file_url_path(&asset.url).ok_or("not a file URL")?, to)?;
        Ok(())
    }
}

/// Delivers to no destination.
struct NoDestinations;

#[async_trait]
impl DeliveryDispatcher for NoDestinations {
    async fn deliver_all(&self, _meeting_id: Uuid) -> Vec<Delivery> {
        Vec::new()
    }
}

/// A speech engine whose transcription is refused for missing models until
/// `installed` is set, as the app's gated engines refuse.
#[derive(Default)]
struct Uninstalled {
    installed: AtomicBool,
    inner: FakeSpeechEngine,
}

#[async_trait]
impl SpeechEngine for Uninstalled {
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
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        if !self.installed.load(Ordering::SeqCst) {
            return Err(Box::new(PipelineFailure::models_missing(
                PipelineStage::Transcribe,
            )));
        }
        self.inner.transcribe(audio, hint).await
    }
}

/// A speech engine whose first transcription decides its model is
/// missing, then waits at a gate (`entered`, then `open`) before it
/// returns the refusal, so a test can install and resume in between; every
/// later call transcribes.
#[derive(Default)]
struct HeldRefusal {
    refused: AtomicBool,
    entered: tokio::sync::Notify,
    open: tokio::sync::Notify,
    inner: FakeSpeechEngine,
}

#[async_trait]
impl SpeechEngine for HeldRefusal {
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
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        if !self.refused.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.open.notified().await;
            return Err(Box::new(PipelineFailure::models_missing(
                PipelineStage::Transcribe,
            )));
        }
        self.inner.transcribe(audio, hint).await
    }
}

/// A store with an audio folder in `dir`, and a pipeline over it with
/// `engine`.
fn pipeline_over(
    dir: &Path,
    engine: Arc<dyn SpeechEngine>,
) -> (Arc<Store>, std::path::PathBuf, ProcessingPipeline) {
    let audio = dir.join("audio");
    let store = Arc::new(Store::open(dir.join("steno.sqlite")).unwrap());
    let mut settings = store.settings().unwrap();
    settings.audio_folder = file_url(&audio, true);
    store.save_settings(&settings).unwrap();
    let pipeline = ProcessingPipeline::new(PipelineDependencies::new(
        Arc::new(WavDecoder),
        engine,
        Arc::new(FakeDiarizer::default()),
        Arc::new(InMemorySpeakerMemory::new(Vec::new())),
        Arc::new(NoDestinations),
        store.clone(),
        MeetingEventBus::new(),
    ));
    (store, audio, pipeline)
}

/// Enqueues a two-lane call in `audio` on `pipeline`; its meeting id.
fn enqueue_call(audio: &Path, pipeline: &ProcessingPipeline) -> Uuid {
    let mut meeting = sample_data::meeting();
    meeting.id = Uuid::new_v4();
    meeting.state = MeetingState::Recording;
    let asset =
        steno_pipeline::fixtures::two_lane_call(audio, meeting.id, AudioRetention::KeepForever)
            .unwrap();
    pipeline.enqueue(&meeting, &asset).unwrap();
    meeting.id
}

/// A layer that hands every event of the background runs' target to
/// `on_message`, on the thread that logs it.
struct OnMessage<F>(F);

impl<S, F> tracing_subscriber::Layer<S> for OnMessage<F>
where
    S: tracing::Subscriber,
    F: Fn(&str) + Send + Sync + 'static,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        if event.metadata().target() != BACKGROUND_RUN_LOG {
            return;
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        (self.0)(&message.0);
    }
}

/// An install's resume that lands after a refused run decided its meeting
/// waits and before the run let the meeting go. The gate is the background
/// task's "waits for the models" line, which it logs before it ends, on
/// this thread (a current-thread runtime): the resume runs right there,
/// and the task ends in the same poll, before the test goes on. The
/// meeting is started once the run is done, rather than left waiting with
/// every model installed. (`wait_until_idle` takes the task's entry out of
/// the running ones, so the test waits for the gate before it calls it.)
#[tokio::test(flavor = "current_thread")]
async fn a_resume_while_a_refused_run_lets_go_of_its_meeting_still_starts_it() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Arc::new(Uninstalled::default());
    let (store, audio, pipeline) = pipeline_over(dir.path(), engine.clone());
    let gated = Arc::new(tokio::sync::Notify::new());
    let resumed = Arc::new(AtomicBool::new(false));
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(std::io::sink)
            .with_max_level(tracing::Level::INFO)
            .finish()
            .with(OnMessage({
                let (pipeline, engine, gated) = (pipeline.clone(), engine.clone(), gated.clone());
                move |message: &str| {
                    if message.contains("processing waits for the models")
                        && !resumed.swap(true, Ordering::SeqCst)
                    {
                        engine.installed.store(true, Ordering::SeqCst);
                        pipeline.resume_waiting().unwrap();
                        gated.notify_one();
                    }
                }
            })),
    )
    .unwrap();

    let meeting_id = enqueue_call(&audio, &pipeline);
    tokio::time::timeout(PATIENCE, gated.notified())
        .await
        .expect("the gate was reached");
    pipeline.wait_until_idle().await;
    assert_eq!(
        store.meeting(meeting_id).unwrap().unwrap().state,
        MeetingState::Ready
    );
}

/// An install's resume while a run is being refused skips the meeting,
/// which is in flight, so the run starts it again instead of leaving it
/// waiting. The restart's log line is the gate: it holds the run's worker
/// thread there until the test lets go. `wait_until_idle` called then
/// does not return, as the refused run is still among the background
/// runs until it has started the meeting again, and once let go it waits
/// for the restart too: the meeting is ready when it returns.
#[test]
fn wait_until_idle_waits_for_a_refused_run_to_start_its_meeting_again() {
    let reached = Arc::new(tokio::sync::Notify::new());
    let (open, opened) = std::sync::mpsc::channel::<()>();
    let opened = std::sync::Mutex::new(opened);
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_writer(std::io::sink)
            .with_max_level(tracing::Level::INFO)
            .finish()
            .with(OnMessage({
                let reached = reached.clone();
                move |message: &str| {
                    if message.contains("a resume ran during the refused run") {
                        reached.notify_one();
                        // Ends when the test lets go or drops `open`.
                        let _ = opened.lock().unwrap().recv();
                    }
                }
            })),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .on_thread_start(move || std::mem::forget(tracing::dispatcher::set_default(&dispatch)))
        .build()
        .unwrap();
    // Dropped before the runtime, so a failing assert unblocks the gate.
    let open = open;
    runtime.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(HeldRefusal::default());
        let (store, audio, pipeline) = pipeline_over(dir.path(), engine.clone());
        let meeting_id = enqueue_call(&audio, &pipeline);
        tokio::time::timeout(PATIENCE, engine.entered.notified())
            .await
            .expect("the run is transcribing");
        assert_eq!(
            pipeline.resume_waiting().unwrap(),
            Vec::<Uuid>::new(),
            "the meeting is in flight"
        );
        engine.open.notify_one();
        tokio::time::timeout(PATIENCE, reached.notified())
            .await
            .expect("the refused run is starting its meeting again");

        let mut idle = std::pin::pin!(pipeline.wait_until_idle());
        let pending = std::future::poll_fn(|context| {
            std::task::Poll::Ready(idle.as_mut().poll(context).is_pending())
        })
        .await;
        assert!(
            pending,
            "wait_until_idle returned while the refused run was starting its meeting again"
        );
        open.send(()).unwrap();
        tokio::time::timeout(PATIENCE, idle)
            .await
            .expect("every run is done");
        assert_eq!(
            store.meeting(meeting_id).unwrap().unwrap().state,
            MeetingState::Ready
        );
        assert_eq!(
            pipeline.dependencies().model_waits.waiting(),
            Vec::<Uuid>::new()
        );
    });
}

/// The app quits after a refused run decided its meeting waits and while
/// an install's resume had skipped it (it was in flight): the run neither
/// starts the meeting again nor posts its progress, and the meeting stays
/// `queued`, recorded nowhere, for the next launch's recovery. The gate is
/// the run's "waits for the models" line, logged between the refusal and
/// the restart, on a worker thread this test's runtime alone uses.
#[test]
fn a_refused_run_the_exit_overtakes_does_not_start_its_meeting_again() {
    let quitting = Arc::new(std::sync::Mutex::new(None::<ProcessingPipeline>));
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_writer(std::io::sink)
            .with_max_level(tracing::Level::INFO)
            .finish()
            .with(OnMessage({
                let quitting = quitting.clone();
                move |message: &str| {
                    if message.contains("processing waits for the models")
                        && let Some(pipeline) = quitting.lock().unwrap().take()
                    {
                        pipeline.quit();
                    }
                }
            })),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .on_thread_start(move || std::mem::forget(tracing::dispatcher::set_default(&dispatch)))
        .build()
        .unwrap();
    runtime.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(HeldRefusal::default());
        let (store, audio, pipeline) = pipeline_over(dir.path(), engine.clone());
        *quitting.lock().unwrap() = Some(pipeline.clone());
        let meeting_id = enqueue_call(&audio, &pipeline);
        tokio::time::timeout(PATIENCE, engine.entered.notified())
            .await
            .expect("the run is transcribing");
        assert_eq!(
            pipeline.resume_waiting().unwrap(),
            Vec::<Uuid>::new(),
            "the meeting is in flight"
        );
        let mut events = pipeline.dependencies().events.subscribe();
        engine.open.notify_one();
        tokio::time::timeout(PATIENCE, pipeline.wait_until_idle())
            .await
            .expect("every run is done");
        assert!(quitting.lock().unwrap().is_none(), "the gate was reached");
        let mut posted = Vec::new();
        while let Ok(event) = events.try_recv() {
            posted.push(event);
        }
        assert!(
            posted.contains(&MeetingEvent::ModelsMissing { meeting_id }),
            "{posted:?}"
        );
        assert!(
            !posted.iter().any(|event| matches!(
                event,
                MeetingEvent::Progress { progress, .. } if progress.stage == PipelineStage::Decode
            )),
            "the exit posted the meeting's progress again: {posted:?}"
        );
        assert_eq!(
            store.meeting(meeting_id).unwrap().unwrap().state,
            MeetingState::Queued
        );
        assert_eq!(
            pipeline.dependencies().model_waits.waiting(),
            Vec::<Uuid>::new()
        );
    });
}
