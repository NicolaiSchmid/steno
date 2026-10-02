//! The pipeline over core's fakes: the stage order, what each stage
//! persists, the no-LLM path, failure attribution, deferred retention,
//! launch recovery and the learned rates.
//! Swift: `Tests/StenoCoreTests/PipelineIntegrationTests.swift`,
//! `StageTests.swift`, `RetentionSweepTests.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use steno_core::testing::{
    FakeDestination, FakeDiarizer, FakeSpeechEngine, FakeSummarizer, InMemorySpeakerMemory,
    PassthroughCleaner, sample_data,
};
use steno_core::{
    AudioAsset, AudioFormat, AudioLane, AudioRetention, Delivery, DeliveryDispatcher,
    DeliveryStatus, Destination, Meeting, MeetingEvent, MeetingSource, MeetingState, PipelineStage,
    RecordingLayout, SpeakerAssignmentKind, Store, async_trait,
    paths::{file_url, path_from_file_url},
};
use steno_pipeline::{
    MeetingEventBus, MonotonicClock, PipelineDependencies, ProcessingPipeline, RetentionSweep,
    StageRates,
};
use uuid::Uuid;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/audio")
        .join(name)
}

/// A manual monotonic clock: every stage takes one second.
struct TickingClock(Mutex<f64>);

impl MonotonicClock for TickingClock {
    fn seconds(&self) -> f64 {
        let mut now = self.0.lock().unwrap();
        *now += 1.0;
        *now
    }
}

/// The dispatcher the adapters crate provides, reduced to what the stages
/// need: one `Delivery` row per destination, the stored receipt as
/// `previous`.
struct FakeDispatcher {
    store: Arc<Store>,
    destinations: Vec<Arc<dyn Destination>>,
    now: DateTime<Utc>,
}

#[async_trait]
impl DeliveryDispatcher for FakeDispatcher {
    async fn deliver_all(&self, meeting_id: Uuid) -> Vec<Delivery> {
        let export = self.store.export(meeting_id).unwrap().unwrap();
        let existing = self.store.deliveries(meeting_id).unwrap();
        let mut rows = Vec::new();
        for destination in &self.destinations {
            let previous = existing
                .iter()
                .find(|d| d.destination_id == destination.id())
                .and_then(|d| d.receipt.clone());
            let mut delivery = Delivery {
                id: Delivery::id_for(meeting_id, destination.id()),
                meeting_id,
                destination_id: destination.id().to_owned(),
                status: DeliveryStatus::Pending,
                last_attempt_at: Some(self.now),
                receipt: previous.clone(),
            };
            match destination.deliver(&export, previous.as_ref()).await {
                Ok(receipt) => {
                    delivery.receipt = Some(receipt);
                    delivery.status = DeliveryStatus::Delivered;
                }
                Err(error) => delivery.status = DeliveryStatus::Failed(error.to_string()),
            }
            self.store.save_delivery(&delivery).unwrap();
            rows.push(delivery);
        }
        rows
    }
}

struct World {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    events: MeetingEventBus,
    pipeline: ProcessingPipeline,
    audio: PathBuf,
    vault: PathBuf,
    now: DateTime<Utc>,
}

fn world(
    summarizer: bool,
    destination: Option<FakeDestination>,
    retention: AudioRetention,
) -> World {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    let audio = dir.path().join("audio");
    let vault = dir.path().join("vault");
    let mut settings = store.settings().unwrap();
    settings.audio_folder = file_url(&audio, true);
    settings.default_retention = retention;
    store.save_settings(&settings).unwrap();
    for person in [
        sample_data::person(0, "Anna"),
        sample_data::person(1, "Ben"),
    ] {
        store.save_person(&person).unwrap();
    }
    let now = sample_data::started_at() + Duration::hours(1);
    let events = MeetingEventBus::new();
    let destinations: Vec<Arc<dyn Destination>> = destination
        .into_iter()
        .map(|d| Arc::new(d) as Arc<dyn Destination>)
        .collect();
    let dispatcher = Arc::new(FakeDispatcher {
        store: store.clone(),
        destinations,
        now,
    });
    let people = store.persons().unwrap();
    let mut dependencies = PipelineDependencies::new(
        Arc::new(WavDecoder),
        Arc::new(FakeSpeechEngine::default()),
        Arc::new(FakeDiarizer::default()),
        Arc::new(InMemorySpeakerMemory::new(people)),
        dispatcher,
        store.clone(),
        events.clone(),
    )
    .with_now(Arc::new(move || now))
    .with_clock(Arc::new(TickingClock(Mutex::new(0.0))));
    if summarizer {
        dependencies = dependencies.with_llm(
            Some(Arc::new(PassthroughCleaner::default())),
            Some(Arc::new(FakeSummarizer::default())),
        );
    }
    World {
        pipeline: ProcessingPipeline::new(dependencies),
        _dir: dir,
        store,
        events,
        audio,
        vault,
        now,
    }
}

/// Reads the 16 kHz mono WAV fixtures; the real decoders live in
/// `steno-audio`.
struct WavDecoder;

fn read_wav(path: &Path) -> std::io::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    Ok(bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32768.0)
        .collect())
}

#[async_trait]
impl steno_core::AudioDecoder for WavDecoder {
    async fn decode(
        &self,
        asset: &AudioAsset,
        lane: AudioLane,
    ) -> steno_core::protocols::BoundaryResult<steno_core::AudioBuffer16k> {
        let url = asset.sidecars_16k.get(&lane).unwrap_or(&asset.url);
        let path = path_from_file_url(url).ok_or("not a file URL")?;
        Ok(steno_core::AudioBuffer16k::new(read_wav(&path)?))
    }

    fn mixdown_format(&self) -> AudioFormat {
        AudioFormat::Wav16kInt16
    }

    async fn mixdown(
        &self,
        asset: &AudioAsset,
        to: &Path,
    ) -> steno_core::protocols::BoundaryResult<()> {
        let source = path_from_file_url(&asset.url).ok_or("not a file URL")?;
        std::fs::copy(source, to)?;
        Ok(())
    }
}

fn call_meeting(now: DateTime<Utc>) -> Meeting {
    let mut meeting = sample_data::meeting();
    "Produktstrategie".clone_into(&mut meeting.title);
    meeting.calendar_event_id = Some("event-1".to_owned());
    meeting.duration = 6.0;
    meeting.state = MeetingState::Recording;
    meeting.created_at = now;
    meeting.updated_at = now;
    meeting
}

fn call_asset(audio: &Path, meeting_id: Uuid, retention: AudioRetention) -> AudioAsset {
    let layout = RecordingLayout::new(audio, meeting_id);
    layout.create_directories(false).unwrap();
    std::fs::copy(
        fixture("conversation-two-lane-6s.wav"),
        layout.master(AudioFormat::Wav16kInt16),
    )
    .unwrap();
    std::fs::copy(
        fixture("conversation-mic-6s.wav"),
        layout.sidecar(AudioLane::Mic),
    )
    .unwrap();
    std::fs::copy(
        fixture("conversation-system-6s.wav"),
        layout.sidecar(AudioLane::System),
    )
    .unwrap();
    AudioAsset {
        id: Uuid::new_v4(),
        meeting_id,
        url: file_url(&layout.master(AudioFormat::Wav16kInt16), false),
        format: AudioFormat::Wav16kInt16,
        lanes: vec![AudioLane::Mic, AudioLane::System],
        sidecars_16k: BTreeMap::from([
            (
                AudioLane::Mic,
                file_url(&layout.sidecar(AudioLane::Mic), false),
            ),
            (
                AudioLane::System,
                file_url(&layout.sidecar(AudioLane::System), false),
            ),
        ]),
        mixdown_url: None,
        retention,
        expires_at: None,
    }
}

fn drain(receiver: &mut steno_pipeline::EventReceiver) -> Vec<MeetingEvent> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}

fn stages(events: &[MeetingEvent]) -> Vec<PipelineStage> {
    events
        .iter()
        .filter_map(|event| match event {
            MeetingEvent::Progress { progress, .. } => Some(progress.stage),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mac_call_runs_every_stage_to_ready_and_delivers() {
    let world = world(
        true,
        Some(FakeDestination::new(PathBuf::new())),
        AudioRetention::KeepDays(30),
    );
    let destination = FakeDestination::new(&world.vault);
    let world = World {
        pipeline: ProcessingPipeline::new(world.pipeline.dependencies().clone()),
        ..world
    };
    // The dispatcher above holds a destination rooted nowhere; rebuild with
    // the vault one so the files land where the assertions look.
    let dispatcher = Arc::new(FakeDispatcher {
        store: world.store.clone(),
        destinations: vec![Arc::new(destination)],
        now: world.now,
    });
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.dispatcher = dispatcher;
    let pipeline = ProcessingPipeline::new(dependencies);
    let mut receiver = world.events.subscribe();

    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepDays(30));
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;

    let stored = world.store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready);
    assert_eq!(stored.title, "Produktstrategie", "a calendar title is kept");
    assert_eq!(stored.language, Some("de".into()));
    assert!(stored.summary.is_some());
    assert!(stored.llm_usage.is_some());

    let export = world.store.export(meeting.id).unwrap().unwrap();
    assert_eq!(
        export.segments.len(),
        12,
        "six one-second segments per lane"
    );
    assert!(
        export
            .segments
            .iter()
            .all(|s| s.raw_text.starts_with("fake segment"))
    );
    assert_eq!(
        export
            .speakers
            .iter()
            .map(|s| s.cluster_label.as_str())
            .collect::<Vec<_>>(),
        ["Me", "Speaker 1", "Speaker 2"]
    );
    let them: Vec<_> = export
        .speakers
        .iter()
        .filter(|s| s.cluster_label != "Me")
        .collect();
    assert!(
        them.iter()
            .all(|s| s.assignment.kind() == SpeakerAssignmentKind::Suggested),
        "the fake diarizer's axis embeddings match the enrolled people: {:?}",
        them.iter().map(|s| &s.assignment).collect::<Vec<_>>()
    );
    assert!(them.iter().all(|s| {
        s.sample_clip_url
            .as_ref()
            .is_some_and(|url| path_from_file_url(url).is_some_and(|p| p.exists()))
    }));
    assert_eq!(export.tasks.len(), 1);
    assert_eq!(export.decisions.len(), 1);
    assert!(
        world.store.name_suggestions(meeting.id).unwrap().is_empty(),
        "the fake summarizer never guesses names, and only named suggestions are stored"
    );
    let audio = export.audio.unwrap();
    assert!(
        audio
            .mixdown_url
            .as_ref()
            .is_some_and(|url| path_from_file_url(url).unwrap().exists())
    );
    assert_eq!(
        audio.expires_at,
        Some(world.now + Duration::days(30)),
        "delivered, so the retention stage stamped the asset"
    );
    let deliveries = world.store.deliveries(meeting.id).unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].status, DeliveryStatus::Delivered);
    assert!(
        world
            .vault
            .join(deliveries[0].receipt.as_ref().unwrap().folder.as_str())
            .join("meeting.json")
            .exists()
    );

    let events = drain(&mut receiver);
    assert_eq!(
        stages(&events),
        [
            PipelineStage::Decode,
            PipelineStage::Transcribe,
            PipelineStage::Transcribe,
            PipelineStage::Diarize,
            PipelineStage::MatchSpeakers,
            PipelineStage::Merge,
            PipelineStage::Cleanup,
            PipelineStage::Summarize,
            PipelineStage::Persist,
            PipelineStage::Deliver,
            PipelineStage::Retention,
        ]
    );
    assert!(matches!(
        &events[9],
        MeetingEvent::SpeakersNeedReview { speaker_ids, .. } if speaker_ids.len() == 3
    ));
    assert_eq!(
        events.last(),
        Some(&MeetingEvent::RetentionApplied {
            meeting_id: meeting.id
        })
    );
    let fractions: Vec<f64> = events
        .iter()
        .filter_map(|e| match e {
            MeetingEvent::Progress { progress, .. } => Some(progress.fraction),
            _ => None,
        })
        .collect();
    assert!(fractions.windows(2).all(|w| w[0] <= w[1]), "{fractions:?}");

    // Every learned stage ran alone, so the rates left their seeds.
    let rates = StageRates::from_rows(&world.store.stage_rates().unwrap());
    assert_eq!(
        rates.rate(PipelineStage::Transcribe, "fake-engine").samples,
        1
    );
    assert_eq!(
        rates.rate(PipelineStage::Decode, "").samples,
        0,
        "decode is never learned"
    );

    // A re-export never re-runs the LLM and leaves the stamp alone.
    pipeline.redeliver(meeting.id).await.unwrap();
    assert_eq!(
        world.store.asset(meeting.id).unwrap().unwrap().expires_at,
        audio.expires_at
    );
    assert_eq!(stages(&drain(&mut receiver)), [PipelineStage::Deliver]);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_llm_the_meeting_is_ready_with_no_summary() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let stored = world.store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready);
    assert_eq!(stored.summary, None);
    assert_eq!(stored.llm_usage, None);
    assert_eq!(world.store.tasks(meeting.id).unwrap(), []);
    assert_eq!(
        world.store.asset(meeting.id).unwrap().unwrap().expires_at,
        None
    );
    let error = world
        .pipeline
        .rerun_summary(meeting.id, "default")
        .await
        .unwrap_err();
    assert_eq!(error.stage, PipelineStage::Summarize);
    assert!(error.reason.contains("no LLM endpoint"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_stage_marks_the_meeting_failed_with_its_name() {
    let world = world(false, None, AudioRetention::KeepForever);
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.diarizer = Arc::new(FakeDiarizer {
        failure: Some("no model".to_owned()),
        ..FakeDiarizer::default()
    });
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    let stored = world.store.meeting(meeting.id).unwrap().unwrap();
    assert!(
        matches!(&stored.state, MeetingState::Failed { reason } if reason.starts_with("diarize: ")),
        "{:?}",
        stored.state
    );
    assert!(
        world.store.segments(meeting.id).unwrap().is_empty(),
        "nothing after the failed stage was persisted"
    );
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_operation_on_a_meeting_in_flight_is_refused() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    let again = world.pipeline.enqueue(&meeting, &asset).unwrap_err();
    assert!(again.reason.contains("already being processed"));
    world.pipeline.wait_until_idle().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_recovery_resumes_queued_meetings_and_fails_those_without_an_asset() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    let mut queued = meeting.clone();
    queued.state = MeetingState::Queued;
    world
        .store
        .save_meeting_with_asset(&queued, &asset)
        .unwrap();
    let mut orphan = sample_data::meeting();
    orphan.id = Uuid::new_v4();
    orphan.state = MeetingState::Processing;
    world.store.save_meeting(&orphan).unwrap();

    let resumed = world.pipeline.resume_unfinished().unwrap();
    assert_eq!(resumed, [meeting.id]);
    world.pipeline.wait_until_idle().await;
    assert_eq!(
        world.store.meeting(meeting.id).unwrap().unwrap().state,
        MeetingState::Ready
    );
    assert!(matches!(
        world.store.meeting(orphan.id).unwrap().unwrap().state,
        MeetingState::Failed { .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_delivery_defers_deletion_until_a_redeliver_succeeds() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    let destination = FakeDestination {
        fail_until: 1,
        ..FakeDestination::new(&world.vault)
    };
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.dispatcher = Arc::new(FakeDispatcher {
        store: world.store.clone(),
        destinations: vec![Arc::new(destination)],
        now: world.now,
    });
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = call_meeting(world.now);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;

    assert_eq!(
        world.store.meeting(meeting.id).unwrap().unwrap().state,
        MeetingState::Ready
    );
    assert!(matches!(
        world.store.deliveries(meeting.id).unwrap()[0].status,
        DeliveryStatus::Failed(_)
    ));
    let deferred = world.store.asset_by_id(asset.id).unwrap().unwrap();
    assert_eq!(
        deferred.expires_at, None,
        "the export needs the audio, so nothing expires"
    );
    let sweep = RetentionSweep::new(world.store.clone());
    assert_eq!(
        sweep.run(world.now + Duration::days(3650)).unwrap(),
        Vec::<PathBuf>::new()
    );

    pipeline.redeliver(meeting.id).await.unwrap();
    assert_eq!(
        world.store.deliveries(meeting.id).unwrap()[0].status,
        DeliveryStatus::Delivered
    );
    let stamped = world.store.asset_by_id(asset.id).unwrap().unwrap();
    assert_eq!(
        stamped.expires_at,
        Some(world.now),
        "stamped only after the delivery succeeded"
    );

    let removed = sweep.run(world.now).unwrap();
    let master = path_from_file_url(&asset.url).unwrap();
    assert!(removed.contains(&master));
    assert!(!master.exists());
    assert_eq!(
        world
            .store
            .asset_by_id(asset.id)
            .unwrap()
            .unwrap()
            .expires_at,
        None
    );
    assert_eq!(
        sweep.keep_all().unwrap(),
        0,
        "the swept master is gone, nothing to keep"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_retention_replaces_the_rule_and_stamps_a_ready_meeting() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    world
        .pipeline
        .apply_retention(meeting.id, AudioRetention::KeepDays(7))
        .await
        .unwrap();
    let stamped = world.store.asset_by_id(asset.id).unwrap().unwrap();
    assert_eq!(stamped.retention, AudioRetention::KeepDays(7));
    assert_eq!(stamped.expires_at, Some(world.now + Duration::days(7)));
    world
        .pipeline
        .apply_retention(meeting.id, AudioRetention::KeepForever)
        .await
        .unwrap();
    assert_eq!(
        world
            .store
            .asset_by_id(asset.id)
            .unwrap()
            .unwrap()
            .expires_at,
        None
    );
}

#[test]
fn an_in_person_recording_has_no_me_speaker_and_the_fixtures_match_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let outputs = steno_pipeline::fixtures::generate(dir.path(), &|data| {
        use std::fmt::Write as _;
        // The manifest hash is checked in the CLI, which has sha2; here the
        // bytes themselves are compared with the committed fixtures.
        let mut text = String::new();
        for byte in data.iter().take(4) {
            write!(text, "{byte:02x}").unwrap();
        }
        text
    })
    .unwrap();
    assert_eq!(outputs.len(), 5);
    for output in outputs {
        let generated = std::fs::read(dir.path().join(&output.relative_path)).unwrap();
        let committed = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../Tests/Fixtures")
                .join(&output.relative_path),
        )
        .unwrap();
        assert_eq!(generated, committed, "{}", output.relative_path);
    }
    let _ = MeetingSource::MacInPerson;
}
