//! A resume that lands while a refused run lets go of its meeting. Its own
//! test binary: the gate is a log line, and the subscriber that runs the
//! resume at it is the binary's global one, which no other test shares.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use steno_core::paths::{file_url, file_url_path};
use steno_core::protocols::BoundaryResult;
use steno_core::testing::{FakeDiarizer, FakeSpeechEngine, InMemorySpeakerMemory, sample_data};
use steno_core::{
    AudioAsset, AudioBuffer16k, AudioFormat, AudioLane, AudioRetention, Delivery,
    DeliveryDispatcher, LanguageTag, MeetingState, PipelineStage, RawSegment, SpeechEngine, Store,
    async_trait,
};
use steno_pipeline::{
    BACKGROUND_RUN_LOG, MeetingEventBus, PipelineDependencies, PipelineFailure, ProcessingPipeline,
};
use tracing_subscriber::layer::SubscriberExt as _;
use uuid::Uuid;

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
    let audio = dir.path().join("audio");
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    let mut settings = store.settings().unwrap();
    settings.audio_folder = file_url(&audio, true);
    store.save_settings(&settings).unwrap();
    let engine = Arc::new(Uninstalled::default());
    let pipeline = ProcessingPipeline::new(PipelineDependencies::new(
        Arc::new(WavDecoder),
        engine.clone(),
        Arc::new(FakeDiarizer::default()),
        Arc::new(InMemorySpeakerMemory::new(Vec::new())),
        Arc::new(NoDestinations),
        store.clone(),
        MeetingEventBus::new(),
    ));
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

    let mut meeting = sample_data::meeting();
    meeting.id = Uuid::new_v4();
    meeting.state = MeetingState::Recording;
    let asset =
        steno_pipeline::fixtures::two_lane_call(&audio, meeting.id, AudioRetention::KeepForever)
            .unwrap();
    pipeline.enqueue(&meeting, &asset).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), gated.notified())
        .await
        .expect("the gate was reached");
    pipeline.wait_until_idle().await;
    assert_eq!(
        store.meeting(meeting.id).unwrap().unwrap().state,
        MeetingState::Ready
    );
}
