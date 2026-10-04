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
    DeliveryStatus, Destination, Meeting, MeetingEvent, MeetingExport, MeetingSource, MeetingState,
    ParticipantRole, PipelineStage, RawSegment, RecordingLayout, SpeakerAssignmentKind, Store,
    async_trait,
    paths::{file_url, file_url_path},
};
use steno_pipeline::pipeline::{diarized_lane_after_transcription, tap_carried_no_conversation};
use steno_pipeline::{
    LaneMerger, MeetingEventBus, MonotonicClock, PipelineDependencies, ProcessingPipeline,
    RetentionSweep, StageRates,
};
use uuid::Uuid;

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
        let export = self.store.export(meeting_id).unwrap();
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
    decoder: Arc<WavDecoder>,
    diarizer: Arc<FakeDiarizer>,
    audio: PathBuf,
    vault: PathBuf,
    now: DateTime<Utc>,
}

/// `destination` builds the one fake destination over the vault path,
/// `None` runs without any.
fn world(
    summarizer: bool,
    destination: Option<fn(&Path) -> FakeDestination>,
    retention: AudioRetention,
) -> World {
    world_with(
        summarizer,
        destination,
        retention,
        FakeSpeechEngine::default(),
        FakeDiarizer::default(),
    )
}

/// [`world`] over the given speech engine and diarizer.
fn world_with(
    summarizer: bool,
    destination: Option<fn(&Path) -> FakeDestination>,
    retention: AudioRetention,
    engine: FakeSpeechEngine,
    diarizer: FakeDiarizer,
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
        .map(|make| Arc::new(make(&vault)) as Arc<dyn Destination>)
        .collect();
    let dispatcher = Arc::new(FakeDispatcher {
        store: store.clone(),
        destinations,
        now,
    });
    let people = store.persons().unwrap();
    let decoder = Arc::new(WavDecoder::default());
    let diarizer = Arc::new(diarizer);
    let mut dependencies = PipelineDependencies::new(
        decoder.clone(),
        Arc::new(engine),
        diarizer.clone(),
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
        decoder,
        diarizer,
        _dir: dir,
        store,
        events,
        audio,
        vault,
        now,
    }
}

/// Reads the 16 kHz mono WAV fixtures and logs the lanes it decoded; the
/// real decoders live in `steno-audio`.
#[derive(Default)]
struct WavDecoder {
    decodes: Mutex<Vec<AudioLane>>,
}

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
        self.decodes.lock().unwrap().push(lane);
        let url = asset.sidecars_16k.get(&lane).unwrap_or(&asset.url);
        let path = file_url_path(url).ok_or("not a file URL")?;
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
        let source = file_url_path(&asset.url).ok_or("not a file URL")?;
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
    steno_pipeline::fixtures::two_lane_call(audio, meeting_id, retention).unwrap()
}

/// Six seconds of digital silence over the asset's system sidecar: the tap
/// of a phone call held on speaker next to the Mac.
fn silence_the_tap(asset: &AudioAsset) {
    let layout = RecordingLayout::from_asset(asset).unwrap();
    steno_pipeline::fixtures::write_wav(&layout.sidecar(AudioLane::System), &vec![0; 6 * 16_000])
        .unwrap();
}

/// The fake engine hearing nothing in a silent buffer.
fn engine_deaf_to_silence() -> FakeSpeechEngine {
    FakeSpeechEngine {
        silent_below_peak: Some(1e-4),
        ..FakeSpeechEngine::default()
    }
}

fn labels(export: &MeetingExport) -> Vec<&str> {
    export
        .speakers
        .iter()
        .map(|s| s.cluster_label.as_str())
        .collect()
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

// One flow, as the Swift test: every assertion reads the run before it.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread")]
async fn a_mac_call_runs_every_stage_to_ready_and_delivers() {
    let world = world(
        true,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::KeepDays(30),
    );
    let pipeline = &world.pipeline;
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

    let export = world.store.export(meeting.id).unwrap();
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
            .is_some_and(|url| file_url_path(url).is_some_and(|p| p.exists()))
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
            .is_some_and(|url| file_url_path(url).unwrap().exists())
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

/// A call whose tap holds less than 5 % of the mic's speech and under ten
/// seconds is diarized on the mic lane; one with a real partner, however
/// quiet, zero mic speech, or no tap at all keeps the standard lane.
/// Swift: `StageTests.aCallWhoseTapCarriedNoConversationIsDiarizedOnTheMicLane`.
#[test]
fn a_call_whose_tap_carried_no_conversation_is_diarized_on_the_mic_lane() {
    fn segments(durations: &[f64]) -> Vec<RawSegment> {
        let mut start = 0.0;
        durations
            .iter()
            .map(|duration| {
                let segment = RawSegment {
                    start,
                    end: start + duration,
                    text: "x".to_owned(),
                    language: None,
                    word_timings: None,
                };
                start += duration;
                segment
            })
            .collect()
    }
    let lanes = |mic: &[f64], system: &[f64]| {
        BTreeMap::from([
            (AudioLane::Mic, segments(mic)),
            (AudioLane::System, segments(system)),
        ])
    };
    let call = [AudioLane::Mic, AudioLane::System];
    let silent_tap = lanes(&[10.0, 20.0, 30.0], &[]);
    assert!(tap_carried_no_conversation(&silent_tap));
    assert_eq!(
        diarized_lane_after_transcription(MeetingSource::MacCall, &call, &silent_tap),
        Some(AudioLane::Mic)
    );
    // A chime on the tap: 2 s against 60 s is under 5 %.
    assert!(tap_carried_no_conversation(&lanes(
        &[10.0, 20.0, 30.0],
        &[2.0]
    )));
    // Exactly 5 % is a conversation; so is anything above.
    assert!(!tap_carried_no_conversation(&lanes(&[60.0], &[3.0])));
    let partner = lanes(&[30.0], &[30.0]);
    assert!(!tap_carried_no_conversation(&partner));
    // A partner who mostly listens: 60 s against 2000 s is 3 %, but ten
    // seconds of speech is a conversation.
    assert!(!tap_carried_no_conversation(&lanes(&[2000.0], &[60.0])));
    assert!(!tap_carried_no_conversation(&lanes(&[2000.0], &[10.0])));
    assert!(tap_carried_no_conversation(&lanes(&[2000.0], &[9.5])));
    assert_eq!(
        diarized_lane_after_transcription(MeetingSource::MacCall, &call, &partner),
        Some(AudioLane::System)
    );
    // Nothing on either lane, or no tap lane, never falls back.
    assert!(!tap_carried_no_conversation(&lanes(&[], &[])));
    assert!(!tap_carried_no_conversation(&BTreeMap::from([(
        AudioLane::Mic,
        segments(&[5.0])
    )])));
    assert_eq!(
        diarized_lane_after_transcription(
            MeetingSource::MacInPerson,
            &[AudioLane::Mixed],
            &BTreeMap::from([(AudioLane::Mixed, Vec::new())])
        ),
        Some(AudioLane::Mixed)
    );
    // The silent-tap rule is for calls: an in-person asset with the same
    // lanes keeps the last lane.
    assert_eq!(
        diarized_lane_after_transcription(MeetingSource::MacInPerson, &call, &silent_tap),
        Some(AudioLane::System)
    );
}

/// A phone call on speaker next to the Mac: the tap records silence and the
/// microphone hears both people. The mic lane is diarized like a room,
/// nobody is "me", the mic is decoded a second time for the diarizer, and
/// the kept recording and lanes are untouched. Swift:
/// `PipelineIntegrationTests.aCallWithASilentTapDiarizesTheMicLaneAndHasNoMe`.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_with_a_silent_tap_diarizes_the_mic_lane_and_has_no_me() {
    let world = world_with(
        false,
        None,
        AudioRetention::KeepDays(30),
        engine_deaf_to_silence(),
        FakeDiarizer::default(),
    );
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepDays(30));
    silence_the_tap(&asset);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;

    let export = world.store.export(meeting.id).unwrap();
    assert_eq!(export.meeting.state, MeetingState::Ready);
    assert_eq!(
        export.meeting.source,
        MeetingSource::MacCall,
        "it was a call, just not through the Mac"
    );
    assert_eq!(labels(&export), ["Speaker 1", "Speaker 2"]);
    assert_eq!(export.participants, [], "no \"me\" participant is invented");
    assert_eq!(export.segments.len(), 6);
    assert!(
        export
            .segments
            .iter()
            .all(|s| s.lane == AudioLane::Mic && s.speaker_id.is_some())
    );
    let used: std::collections::BTreeSet<_> = export
        .segments
        .iter()
        .filter_map(|s| s.speaker_id)
        .collect();
    let speakers: std::collections::BTreeSet<_> = export.speakers.iter().map(|s| s.id).collect();
    assert_eq!(used, speakers);
    assert_eq!(
        *world.decoder.decodes.lock().unwrap(),
        [AudioLane::Mic, AudioLane::System, AudioLane::Mic]
    );
    assert_eq!(world.diarizer.diarizations.entries(), [6.0]);
    assert_eq!(
        export.audio.unwrap().lanes,
        [AudioLane::Mic, AudioLane::System]
    );
}

/// A silent tap with one voice on the mic is the user alone (headphones,
/// the tap permission missing): the standard rules stand, the mic is "me",
/// and no clip is written for a cluster that was never made a speaker.
/// Swift: `PipelineIntegrationTests.aSilentTapWithOneVoiceOnTheMicKeepsTheMicAsMe`.
#[tokio::test(flavor = "multi_thread")]
async fn a_silent_tap_with_one_voice_on_the_mic_keeps_the_mic_as_me() {
    let world = world_with(
        false,
        None,
        AudioRetention::KeepDays(30),
        engine_deaf_to_silence(),
        FakeDiarizer {
            cluster_count: 1,
            ..FakeDiarizer::default()
        },
    );
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepDays(30));
    silence_the_tap(&asset);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;

    let export = world.store.export(meeting.id).unwrap();
    assert_eq!(export.meeting.state, MeetingState::Ready);
    assert_eq!(labels(&export), ["Me"]);
    assert_eq!(
        export
            .participants
            .iter()
            .map(|p| p.role)
            .collect::<Vec<_>>(),
        [ParticipantRole::Me]
    );
    let me = LaneMerger::me_speaker_id(meeting.id);
    assert_eq!(export.segments.len(), 6);
    assert!(
        export
            .segments
            .iter()
            .all(|s| s.lane == AudioLane::Mic && s.speaker_id == Some(me))
    );
    let layout = RecordingLayout::from_asset(&asset).unwrap();
    assert!(!layout.speakers_directory().exists());
}

/// Processing again after the tap went quiet: the "me" participant the
/// first run created goes with the "me" speaker, so the export does not
/// advertise a participant nobody speaks as. Swift:
/// `PipelineIntegrationTests.aRerunThatFallsBackToTheMicLaneRemovesThePipelinesMeParticipant`.
#[tokio::test(flavor = "multi_thread")]
async fn a_rerun_that_falls_back_to_the_mic_lane_removes_the_pipelines_me_participant() {
    let world = world_with(
        false,
        None,
        AudioRetention::KeepDays(30),
        engine_deaf_to_silence(),
        FakeDiarizer::default(),
    );
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepDays(30));
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let first = world.store.export(meeting.id).unwrap();
    assert_eq!(
        first
            .participants
            .iter()
            .map(|p| p.role)
            .collect::<Vec<_>>(),
        [ParticipantRole::Me]
    );
    assert_eq!(labels(&first), ["Me", "Speaker 1", "Speaker 2"]);

    silence_the_tap(&asset);
    world.pipeline.process(asset.id).await.unwrap();
    let second = world.store.export(meeting.id).unwrap();
    assert_eq!(second.meeting.state, MeetingState::Ready);
    assert_eq!(second.participants, []);
    assert_eq!(labels(&second), ["Speaker 1", "Speaker 2"]);
    assert!(second.segments.iter().all(|s| s.speaker_id.is_some()));
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

/// The speech engine is released once a job's lanes are transcribed,
/// before the diarizer runs, so the speech sidecar's working set is back
/// before the stages after transcription.
#[tokio::test(flavor = "multi_thread")]
async fn the_engine_is_released_after_the_last_lane_before_diarization() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(FakeSpeechEngine::default());
    // (transcriptions, releases) as the diarizer saw them.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let diarizer = {
        let (engine, seen) = (engine.clone(), seen.clone());
        FakeDiarizer::answering(move |audio| {
            seen.lock()
                .unwrap()
                .push((engine.transcriptions.count(), engine.releases.count()));
            FakeDiarizer::round_robin(audio.duration(), 2, 1.5)
        })
    };
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.speech_engine = engine.clone();
    dependencies.diarizer = Arc::new(diarizer);
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(
        world.store.meeting(meeting.id).unwrap().unwrap().state,
        MeetingState::Ready
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [(2, 1)],
        "both lanes, then the release"
    );
    assert_eq!(engine.preparations.count(), 1);
    assert_eq!(engine.releases.count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_transcription_fails_releases_the_engine_too() {
    let world = world(false, None, AudioRetention::KeepForever);
    let failing = Arc::new(FakeSpeechEngine {
        failure: Some("no model".to_owned()),
        ..FakeSpeechEngine::default()
    });
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.speech_engine = failing.clone();
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(
        world.store.meeting(meeting.id).unwrap().unwrap().state,
        MeetingState::Failed {
            reason: "transcribe: no model".to_owned()
        }
    );
    assert_eq!(failing.releases.count(), 1);
}

/// Which call of a [`GatedEngine`] waits until its `open` is notified.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gate {
    FirstTranscription,
    FirstRelease,
}

/// A fake engine whose first transcription or first release waits until
/// `open` is notified, logging each `prepare` and the start and end of
/// each `release`.
struct GatedEngine {
    inner: FakeSpeechEngine,
    gate: Gate,
    /// Set once the gated call has started.
    entered: std::sync::atomic::AtomicBool,
    open: tokio::sync::Notify,
    log: Mutex<Vec<&'static str>>,
}

impl GatedEngine {
    fn new(gate: Gate) -> Self {
        GatedEngine {
            inner: FakeSpeechEngine::default(),
            gate,
            entered: false.into(),
            open: tokio::sync::Notify::new(),
            log: Mutex::new(Vec::new()),
        }
    }

    /// Waits until the gated call has started.
    async fn wait_until_entered(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !self.entered.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the gated call started");
    }

    /// Waits for `open` on the first call through `gate`.
    async fn pass(&self, gate: Gate) {
        if self.gate == gate && !self.entered.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.open.notified().await;
        }
    }

    fn log(&self) -> Vec<&'static str> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait]
impl steno_core::SpeechEngine for GatedEngine {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn supported_languages(&self) -> &std::collections::BTreeSet<steno_core::LanguageTag> {
        self.inner.supported_languages()
    }

    async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
        self.log.lock().unwrap().push("prepare");
        self.inner.prepare().await
    }

    async fn transcribe(
        &self,
        audio: &steno_core::AudioBuffer16k,
        hint: Option<&steno_core::LanguageTag>,
    ) -> steno_core::protocols::BoundaryResult<Vec<steno_core::RawSegment>> {
        self.pass(Gate::FirstTranscription).await;
        self.inner.transcribe(audio, hint).await
    }

    async fn release(&self) -> steno_core::protocols::BoundaryResult<()> {
        self.log.lock().unwrap().push("release");
        self.pass(Gate::FirstRelease).await;
        let released = self.inner.release().await;
        self.log.lock().unwrap().push("released");
        released
    }
}

/// Two jobs on one engine: the one that finishes while the other is still
/// transcribing leaves the engine loaded; the last one releases it.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_leaves_the_engine_loaded_while_another_still_transcribes() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.speech_engine = engine.clone();
    let pipeline = ProcessingPipeline::new(dependencies);
    let patience = std::time::Duration::from_secs(5);

    let slow = call_meeting(world.now);
    let slow_asset = call_asset(&world.audio, slow.id, AudioRetention::KeepForever);
    pipeline.enqueue(&slow, &slow_asset).unwrap();
    engine.wait_until_entered().await;

    let mut fast = call_meeting(world.now);
    fast.id = Uuid::new_v4();
    let fast_asset = call_asset(&world.audio, fast.id, AudioRetention::KeepForever);
    pipeline.enqueue(&fast, &fast_asset).unwrap();
    tokio::time::timeout(patience, async {
        while world.store.meeting(fast.id).unwrap().unwrap().state != MeetingState::Ready {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the second job finished while the first waited");
    assert_eq!(
        engine.inner.releases.count(),
        0,
        "the first job still needs the engine"
    );

    engine.open.notify_one();
    pipeline.wait_until_idle().await;
    assert_eq!(
        world.store.meeting(slow.id).unwrap().unwrap().state,
        MeetingState::Ready
    );
    assert_eq!(engine.inner.releases.count(), 1);
}

/// A job that claims the engine while another job's release runs waits
/// for the release and prepares again after it, never during it.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_starts_during_a_release_prepares_again_after_it() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(GatedEngine::new(Gate::FirstRelease));
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.speech_engine = engine.clone();
    let pipeline = ProcessingPipeline::new(dependencies);

    let first = call_meeting(world.now);
    let first_asset = call_asset(&world.audio, first.id, AudioRetention::KeepForever);
    pipeline.enqueue(&first, &first_asset).unwrap();
    engine.wait_until_entered().await;

    let mut second = call_meeting(world.now);
    second.id = Uuid::new_v4();
    let second_asset = call_asset(&world.audio, second.id, AudioRetention::KeepForever);
    pipeline.enqueue(&second, &second_asset).unwrap();
    // Time enough for the second job's warm-up, were it not held.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(engine.log(), ["prepare", "release"]);

    engine.open.notify_one();
    pipeline.wait_until_idle().await;
    assert_eq!(
        engine.log(),
        [
            "prepare", "release", "released", "prepare", "release", "released"
        ]
    );
    for meeting in [first.id, second.id] {
        assert_eq!(
            world.store.meeting(meeting).unwrap().unwrap().state,
            MeetingState::Ready
        );
    }
}

/// An engine whose first transcription panics; the later ones are the
/// fake's.
struct PanicsOnceEngine {
    inner: FakeSpeechEngine,
    panicked: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl steno_core::SpeechEngine for PanicsOnceEngine {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn supported_languages(&self) -> &std::collections::BTreeSet<steno_core::LanguageTag> {
        self.inner.supported_languages()
    }

    async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
        self.inner.prepare().await
    }

    async fn transcribe(
        &self,
        audio: &steno_core::AudioBuffer16k,
        hint: Option<&steno_core::LanguageTag>,
    ) -> steno_core::protocols::BoundaryResult<Vec<steno_core::RawSegment>> {
        if !self
            .panicked
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            panic!("the engine broke");
        }
        self.inner.transcribe(audio, hint).await
    }

    async fn release(&self) -> steno_core::protocols::BoundaryResult<()> {
        self.inner.release().await
    }
}

/// A job that panics gives its claim back without a release, so the next
/// job on the same pipeline still releases the engine after its lanes.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_panics_gives_its_claim_on_the_engine_back() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(PanicsOnceEngine {
        inner: FakeSpeechEngine::default(),
        panicked: false.into(),
    });
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.speech_engine = engine.clone();
    let pipeline = ProcessingPipeline::new(dependencies);
    let panicking = call_meeting(world.now);
    let asset = call_asset(&world.audio, panicking.id, AudioRetention::KeepForever);
    pipeline.enqueue(&panicking, &asset).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(
        engine.inner.releases.count(),
        0,
        "a panic only drops the claim"
    );

    let mut next = call_meeting(world.now);
    next.id = Uuid::new_v4();
    let asset = call_asset(&world.audio, next.id, AudioRetention::KeepForever);
    pipeline.enqueue(&next, &asset).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(
        world.store.meeting(next.id).unwrap().unwrap().state,
        MeetingState::Ready
    );
    assert_eq!(engine.inner.releases.count(), 1);
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
    assert_eq!(
        stored.state,
        MeetingState::Failed {
            reason: "diarize: no model".to_owned()
        },
        "the stage is named once"
    );
    assert!(
        world.store.segments(meeting.id).unwrap().is_empty(),
        "nothing after the failed stage was persisted"
    );
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
}

/// A failed background run is logged at warn with its asset and stage
/// only: a stage's reason can name the audio file or quote the model, and
/// stays with the meeting row and the debug level.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_background_run_warns_with_its_stage_not_its_reason() {
    let log = steno_pipeline::fixtures::CapturedLog::warnings();
    let world = world(false, None, AudioRetention::KeepForever);
    let mut dependencies = world.pipeline.dependencies().clone();
    let reason = "cannot open /Users/someone/Audio/meeting/mic.caf";
    dependencies.diarizer = Arc::new(FakeDiarizer {
        failure: Some(reason.to_owned()),
        ..FakeDiarizer::default()
    });
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = call_meeting(world.now);
    // Other tests in this binary log through the same subscriber; an id
    // of its own picks this run's line out.
    let mut asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    asset.id = Uuid::new_v4();
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;

    let text = log.text();
    let line = text
        .lines()
        .find(|line| line.contains(&asset.id.to_string()))
        .unwrap_or_else(|| panic!("no line for the asset: {text}"));
    assert!(line.contains("processing failed"), "{line}");
    assert!(line.contains("stage=\"diarize\""), "{line}");
    assert!(!text.contains(reason), "{text}");
    assert!(
        !text.contains(&world.audio.display().to_string()),
        "no audio path: {text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_summary_names_its_stage_once() {
    let world = world(false, None, AudioRetention::KeepForever);
    let dependencies = world.pipeline.dependencies().clone().with_llm(
        Some(Arc::new(PassthroughCleaner::default())),
        Some(Arc::new(FakeSummarizer {
            failure: Some("HTTP 401".to_owned()),
            ..FakeSummarizer::default()
        })),
    );
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(
        world.store.meeting(meeting.id).unwrap().unwrap().state,
        MeetingState::Failed {
            reason: "summarize: HTTP 401".to_owned()
        }
    );

    // A re-run that fails the same way leaves the state alone, returns the
    // failure and posts it for the window.
    let mut receiver = world.events.subscribe();
    let error = pipeline
        .rerun_summary(meeting.id, "default")
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "summarize: HTTP 401");
    assert_eq!(
        drain(&mut receiver)
            .into_iter()
            .filter(|event| matches!(event, MeetingEvent::OperationFailed { .. }))
            .collect::<Vec<_>>(),
        [MeetingEvent::OperationFailed {
            meeting_id: meeting.id,
            operation: steno_core::MeetingOperation::SummaryRerun,
            stage: PipelineStage::Summarize,
            failure: "summarize: HTTP 401".to_owned(),
        }]
    );
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_row_fails_for_the_stage_of_the_operation_and_names_the_row() {
    let world = world(true, None, AudioRetention::KeepForever);
    let unknown = Uuid::new_v4();
    let rerun = world
        .pipeline
        .rerun_summary(unknown, "default")
        .await
        .unwrap_err();
    assert_eq!(
        rerun.to_string(),
        format!("summarize: meeting {unknown} not found")
    );
    let redeliver = world.pipeline.redeliver(unknown).await.unwrap_err();
    assert_eq!(
        redeliver.to_string(),
        format!("deliver: meeting {unknown} not found")
    );
    let processed = world.pipeline.process(unknown).await.unwrap_err();
    assert_eq!(
        processed.to_string(),
        format!("decode: audio asset {unknown} not found")
    );
    let kept = world
        .pipeline
        .apply_retention(unknown, AudioRetention::KeepForever)
        .await
        .unwrap_err();
    assert_eq!(
        kept.to_string(),
        format!("retention: meeting {unknown} has no audio asset")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claimed_operation_holds_the_meeting_until_it_runs_or_is_dropped() {
    let world = world(true, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;

    let claimed = world.pipeline.claim_redeliver(meeting.id).unwrap();
    assert_eq!(world.pipeline.in_flight(), [meeting.id]);
    let refused = world
        .pipeline
        .claim_rerun_summary(meeting.id, "default")
        .err()
        .unwrap();
    assert_eq!(
        refused.to_string(),
        format!(
            "summarize: meeting {} is already being processed",
            meeting.id
        )
    );
    drop(claimed);
    assert_eq!(world.pipeline.in_flight(), Vec::<Uuid>::new());

    let claimed = world
        .pipeline
        .claim_rerun_summary(meeting.id, "default")
        .unwrap();
    claimed.await.unwrap();
    assert_eq!(world.pipeline.in_flight(), Vec::<Uuid>::new());
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

/// An engine that panics mid-run, standing in for any bug in a stage.
struct PanickingEngine(std::collections::BTreeSet<steno_core::LanguageTag>);

#[async_trait]
impl steno_core::SpeechEngine for PanickingEngine {
    fn id(&self) -> &'static str {
        "panicking-engine"
    }

    fn supported_languages(&self) -> &std::collections::BTreeSet<steno_core::LanguageTag> {
        &self.0
    }

    async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
        Ok(())
    }

    async fn transcribe(
        &self,
        _audio: &steno_core::AudioBuffer16k,
        _hint: Option<&steno_core::LanguageTag>,
    ) -> steno_core::protocols::BoundaryResult<Vec<steno_core::RawSegment>> {
        panic!("the engine broke");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_panics_releases_its_asset_and_its_meeting() {
    let world = world(false, None, AudioRetention::KeepForever);
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.speech_engine = Arc::new(PanickingEngine(std::collections::BTreeSet::new()));
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    // Without waiting for idle (which drops the entries itself), the same
    // asset can be enqueued again once the panicked run is gone.
    let enqueued = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if pipeline.enqueue(&meeting, &asset).is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(enqueued.is_ok(), "the panicked run left its entry behind");
    pipeline.wait_until_idle().await;
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
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
async fn a_failed_delivery_defers_deletion_until_a_re_export_succeeds() {
    let world = world(
        false,
        Some(|vault| FakeDestination {
            fail_until: 1,
            ..FakeDestination::new(vault)
        }),
        AudioRetention::DeleteAfterProcessing,
    );
    let pipeline = &world.pipeline;
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
        pipeline.in_flight(),
        Vec::<Uuid>::new(),
        "the re-export released its meeting"
    );
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
    let master = file_url_path(&asset.url).unwrap();
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
fn the_generated_fixtures_match_the_committed_files() {
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
}
