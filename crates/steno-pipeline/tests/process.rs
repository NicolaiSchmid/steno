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
use steno_pipeline::runs::{MAX_UNSETTLED_RUNS, TOO_MANY_UNSETTLED_RUNS};
use steno_pipeline::{
    LaneMerger, MeetingEventBus, MonotonicClock, PipelineDependencies, ProcessingPipeline,
    QuitLatch, RetentionSweep, SharedSpeechEngine, StageRates,
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

/// A cleaner that first does what the user can while the cleanup pass
/// waits on the model (`meddle`, with the store), then returns every
/// segment's text marked as cleaned.
struct MeddlingCleaner {
    store: Arc<Store>,
    meddle: fn(&Store, &steno_core::CleanupInput),
}

#[async_trait]
impl steno_core::TranscriptCleaner for MeddlingCleaner {
    async fn clean(
        &self,
        input: &steno_core::CleanupInput,
    ) -> steno_core::protocols::BoundaryResult<steno_core::CleanupOutput> {
        (self.meddle)(&self.store, input);
        Ok(steno_core::CleanupOutput {
            segments: input
                .segments
                .iter()
                .map(|segment| {
                    let mut cleaned = segment.clone();
                    cleaned.text = format!("{} (cleaned)", segment.text);
                    cleaned
                })
                .collect(),
            failed_chunks: Vec::new(),
            usage: steno_core::LlmUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                requests: 1,
            },
        })
    }
}

/// A pipeline over the world whose cleaner is a [`MeddlingCleaner`] with
/// `meddle`, and whose summarizer is `summarizer`.
fn meddling(
    world: &World,
    meddle: fn(&Store, &steno_core::CleanupInput),
    summarizer: Arc<dyn steno_core::MeetingSummarizer>,
) -> ProcessingPipeline {
    let cleaner = MeddlingCleaner {
        store: world.store.clone(),
        meddle,
    };
    ProcessingPipeline::new(
        world
            .pipeline
            .dependencies()
            .clone()
            .with_llm(Some(Arc::new(cleaner)), Some(summarizer)),
    )
}

/// The speaker of `speakers` labelled `label`.
fn speaker<'a>(speakers: &'a [steno_core::Speaker], label: &str) -> &'a steno_core::Speaker {
    speakers
        .iter()
        .find(|speaker| speaker.cluster_label == label)
        .unwrap_or_else(|| panic!("no speaker {label}"))
}

/// A summarizer that keeps the input each call saw and answers as
/// [`FakeSummarizer`] does.
#[derive(Default)]
struct RecordingSummarizer(Mutex<Vec<steno_core::SummaryInput>>);

#[async_trait]
impl steno_core::MeetingSummarizer for RecordingSummarizer {
    async fn summarize(
        &self,
        input: &steno_core::SummaryInput,
    ) -> steno_core::protocols::BoundaryResult<steno_core::SummaryOutput> {
        self.0.lock().unwrap().push(input.clone());
        Ok(FakeSummarizer::output(
            input,
            FakeSummarizer::default().usage,
        ))
    }
}

/// The cleanup pass writes the cleaned text by segment and leaves the
/// speakers alone, so a speaker confirmed while it ran stays confirmed,
/// another speaker's clip cleared meanwhile stays cleared, and the summary
/// is made with the speakers as stored then.
#[tokio::test(flavor = "multi_thread")]
async fn a_speaker_named_during_cleanup_stays_named() {
    let world = world(false, None, AudioRetention::KeepForever);
    let summarizer = Arc::new(RecordingSummarizer::default());
    let pipeline = meddling(
        &world,
        // Confirms "Speaker 1" as Anna and clears the clip of "Speaker 2",
        // as the retention sweep can.
        |store, input| {
            let anna = sample_data::person(0, "Anna");
            store
                .confirm_speaker(speaker(&input.speakers, "Speaker 1").id, &anna)
                .unwrap();
            let second = speaker(&input.speakers, "Speaker 2");
            assert!(second.sample_clip_url.is_some());
            store
                .clear_sample_clips(second.meeting_id, &[second.id])
                .unwrap();
        },
        summarizer.clone(),
    );
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;

    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    let export = world.store.export(meeting).unwrap();
    let first = speaker(&export.speakers, "Speaker 1");
    let anna = steno_core::SpeakerAssignment::Confirmed {
        person_id: sample_data::person(0, "Anna").id,
    };
    assert_eq!(first.assignment, anna);
    assert_eq!(
        speaker(&export.speakers, "Speaker 2").sample_clip_url,
        None,
        "the speakers are not written again"
    );
    assert_eq!(export.segments.len(), 12);
    assert!(
        export
            .segments
            .iter()
            .all(|segment| segment.text == format!("{} (cleaned)", segment.raw_text))
    );
    assert!(
        export
            .segments
            .iter()
            .any(|segment| segment.speaker_id == Some(first.id)),
        "the segments keep their speakers"
    );
    let seen = summarizer.0.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0]
            .speakers
            .iter()
            .any(|speaker| speaker.id == first.id && speaker.assignment == anna),
        "the summary names the confirmed speaker"
    );
}

/// A title the user types while the meeting processes is kept, and the
/// summary is made with it: the stages' writes leave a title whose stored
/// origin is `user` alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_title_typed_while_processing_is_kept() {
    let world = world(false, None, AudioRetention::KeepForever);
    let summarizer = Arc::new(RecordingSummarizer::default());
    let pipeline = meddling(
        &world,
        |store, input| {
            store
                .rename(
                    input.segments[0].meeting_id,
                    "Typed while processing",
                    sample_data::started_at(),
                )
                .unwrap();
        },
        summarizer.clone(),
    );
    let mut meeting = call_meeting(world.now);
    // No calendar event, so the summary would replace a default title.
    meeting.calendar_event_id = None;
    meeting.title_origin = steno_core::TitleOrigin::Default;
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;

    let stored = world.store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready);
    assert!(stored.summary.is_some());
    assert_eq!(stored.title, "Typed while processing");
    assert_eq!(stored.title_origin, steno_core::TitleOrigin::User);
    assert_eq!(
        summarizer.0.lock().unwrap()[0].meeting.title,
        "Typed while processing"
    );
}

/// A run without a summarizer (the LLM was turned off since) keeps the
/// summary, tasks, decisions and name suggestions an earlier run wrote.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_without_a_summarizer_keeps_the_earlier_summary() {
    let world = world(true, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let summarized = world.store.export(meeting.id).unwrap();
    assert!(summarized.meeting.summary.is_some());
    assert_eq!(summarized.tasks.len(), 1);
    assert_eq!(summarized.decisions.len(), 1);
    // The fake summarizer guesses no names; a model's guess, stored.
    let decisions: Vec<String> = summarized
        .decisions
        .iter()
        .map(|decision| decision.text.clone())
        .collect();
    world
        .store
        .replace_summary(
            &summarized.meeting,
            &summarized.tasks,
            &decisions,
            &[steno_core::SpeakerNameSuggestion {
                speaker_id: speaker(&summarized.speakers, "Speaker 1").id,
                name: Some("Ben".to_owned()),
                confidence: 0.7,
                evidence: "was greeted by name".to_owned(),
            }],
        )
        .unwrap();
    let suggestions = world.store.name_suggestions(meeting.id).unwrap();
    assert_eq!(suggestions.len(), 1);

    ProcessingPipeline::new(world.pipeline.dependencies().clone().with_llm(None, None))
        .process(asset.id)
        .await
        .unwrap();

    let again = world.store.export(meeting.id).unwrap();
    assert_eq!(again.meeting.state, MeetingState::Ready);
    assert_eq!(again.meeting.summary, summarized.meeting.summary);
    assert_eq!(again.tasks, summarized.tasks);
    assert_eq!(again.decisions, summarized.decisions);
    assert_eq!(
        world.store.name_suggestions(meeting.id).unwrap(),
        suggestions
    );
}

/// A template picked while the meeting processes is kept, and the summary
/// is made with it: the run read the meeting before the pick and never
/// writes the old template back.
#[tokio::test(flavor = "multi_thread")]
async fn a_template_picked_while_processing_is_kept_and_used() {
    let world = world(false, None, AudioRetention::KeepForever);
    let pipeline = meddling(
        &world,
        // Picks another template, as the user's `meeting.setTemplate` can
        // while the meeting is in flight.
        |store, input| {
            store
                .update_meeting(
                    input.segments[0].meeting_id,
                    sample_data::started_at(),
                    |meeting| {
                        "interview".clone_into(&mut meeting.template_id);
                        Ok(())
                    },
                )
                .unwrap();
        },
        Arc::new(FakeSummarizer::default()),
    );
    let meeting = call_meeting(world.now);
    assert_eq!(meeting.template_id, "default");
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;

    let stored = world.store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready);
    assert_eq!(stored.template_id, "interview");
    assert_eq!(stored.summary.unwrap().template_id, "interview");
}

/// A summary re-run does not store the template it ran with: the host
/// stores the user's pick before it starts a re-run, so a re-run started
/// with "daily-standup" never writes it back over "interview", a pick
/// stored since. The summary records which template made it.
#[tokio::test(flavor = "multi_thread")]
async fn a_summary_rerun_leaves_a_later_pick_alone() {
    let world = world(true, None, AudioRetention::KeepForever);
    let meeting = enqueue_call(&world, &world.pipeline);
    world.pipeline.wait_until_idle().await;
    world
        .store
        .update_meeting(meeting, world.now, |stored| {
            "interview".clone_into(&mut stored.template_id);
            Ok(())
        })
        .unwrap();
    world
        .pipeline
        .rerun_summary(meeting, "daily-standup")
        .await
        .unwrap();
    let stored = world.store.meeting(meeting).unwrap().unwrap();
    assert_eq!(stored.template_id, "interview");
    assert_eq!(stored.summary.unwrap().template_id, "daily-standup");
}

/// The world's dependencies with `engine` as the speech engine.
fn with_engine(world: &World, engine: Arc<dyn steno_core::SpeechEngine>) -> PipelineDependencies {
    world
        .pipeline
        .dependencies()
        .clone()
        .with_speech_engine(SharedSpeechEngine::new(engine))
}

/// Enqueues the two-lane call as a meeting of its own; its id.
fn enqueue_call(world: &World, pipeline: &ProcessingPipeline) -> Uuid {
    let mut meeting = call_meeting(world.now);
    meeting.id = Uuid::new_v4();
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    meeting.id
}

fn meeting_state(world: &World, id: Uuid) -> MeetingState {
    world.store.meeting(id).unwrap().unwrap().state
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
    let mut dependencies = with_engine(&world, engine.clone());
    dependencies.diarizer = Arc::new(diarizer);
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
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
    let pipeline = ProcessingPipeline::new(with_engine(&world, failing.clone()));
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(
        meeting_state(&world, meeting),
        MeetingState::Failed {
            reason: "transcribe: no model".to_owned()
        }
    );
    assert_eq!(failing.releases.count(), 1);
}

/// A lane that cannot be decoded fails the job in `decode`, after the
/// engine was loaded; the engine is released all the same.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_lane_cannot_be_decoded_releases_the_engine_too() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(FakeSpeechEngine::default());
    let pipeline = ProcessingPipeline::new(with_engine(&world, engine.clone()));
    let mut meeting = call_meeting(world.now);
    meeting.id = Uuid::new_v4();
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    let layout = RecordingLayout::from_asset(&asset).unwrap();
    std::fs::remove_file(layout.sidecar(AudioLane::System)).unwrap();
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    let MeetingState::Failed { reason } = meeting_state(&world, meeting.id) else {
        panic!("the job failed");
    };
    assert!(reason.starts_with("decode: "), "{reason}");
    assert_eq!(engine.preparations.count(), 1);
    assert_eq!(engine.releases.count(), 1);
}

/// A diarizer whose models cannot load.
struct UnloadableDiarizer;

#[async_trait]
impl steno_core::Diarizer for UnloadableDiarizer {
    async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
        Err("no diarizer model".into())
    }

    async fn diarize(
        &self,
        _audio: &steno_core::AudioBuffer16k,
    ) -> steno_core::protocols::BoundaryResult<steno_core::DiarizationResult> {
        Err("never loaded".into())
    }
}

/// A diarizer that does not load fails neither the warm-up nor the job:
/// the lanes are transcribed, the meeting is ready with one unknown room
/// speaker, and the engine is released after the last lane as usual.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_diarizer_does_not_load_keeps_its_transcript() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(FakeSpeechEngine::default());
    let mut dependencies = with_engine(&world, engine.clone());
    dependencies.diarizer = Arc::new(UnloadableDiarizer);
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    assert_eq!(world.store.segments(meeting).unwrap().len(), 12);
    assert_eq!(engine.preparations.count(), 1);
    assert_eq!(engine.transcriptions.count(), 2);
    assert_eq!(engine.releases.count(), 1);
}

/// A speech engine whose models cannot load; it logs its releases.
struct UnloadableEngine(FakeSpeechEngine);

#[async_trait]
impl steno_core::SpeechEngine for UnloadableEngine {
    fn id(&self) -> &str {
        steno_core::SpeechEngine::id(&self.0)
    }

    fn supported_languages(&self) -> &std::collections::BTreeSet<steno_core::LanguageTag> {
        steno_core::SpeechEngine::supported_languages(&self.0)
    }

    async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
        Err("no speech model".into())
    }

    async fn transcribe(
        &self,
        audio: &steno_core::AudioBuffer16k,
        hint: Option<&steno_core::LanguageTag>,
    ) -> steno_core::protocols::BoundaryResult<Vec<steno_core::RawSegment>> {
        steno_core::SpeechEngine::transcribe(&self.0, audio, hint).await
    }

    async fn release(&self) -> steno_core::protocols::BoundaryResult<()> {
        steno_core::SpeechEngine::release(&self.0).await
    }
}

/// A warm-up whose speech engine does not load fails the job in `decode`
/// before anything is transcribed, and still releases the engine.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_warm_up_fails_releases_the_engine_too() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(UnloadableEngine(FakeSpeechEngine::default()));
    let pipeline = ProcessingPipeline::new(with_engine(&world, engine.clone()));
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(
        meeting_state(&world, meeting),
        MeetingState::Failed {
            reason: "decode: no speech model".to_owned()
        }
    );
    assert_eq!(engine.0.transcriptions.count(), 0);
    assert_eq!(engine.0.releases.count(), 1);
}

/// Which call of a [`GatedEngine`] is gated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gate {
    FirstTranscription,
    FirstRelease,
}

/// A fake engine whose first transcription or first release waits until
/// `open` is notified (or panics, with `panics`), logging each `prepare`
/// and the start and end of each `release`.
struct GatedEngine {
    inner: FakeSpeechEngine,
    gate: Gate,
    panics: bool,
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
            panics: false,
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

    /// The first call through `gate` panics or waits for `open`.
    async fn pass(&self, gate: Gate) {
        if self.gate == gate && !self.entered.swap(true, std::sync::atomic::Ordering::SeqCst) {
            assert!(!self.panics, "the engine broke");
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
    let pipeline = ProcessingPipeline::new(with_engine(&world, engine.clone()));

    let slow = enqueue_call(&world, &pipeline);
    engine.wait_until_entered().await;

    let fast = enqueue_call(&world, &pipeline);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while meeting_state(&world, fast) != MeetingState::Ready {
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
    assert_eq!(meeting_state(&world, slow), MeetingState::Ready);
    assert_eq!(engine.inner.releases.count(), 1);
}

/// Two pipelines over one engine and its claims, as the services build
/// them across a reload: a job on the new one that finishes while a job
/// on the retired one still transcribes leaves the engine loaded; the
/// retired one's job, the last, releases it once.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_on_another_pipeline_over_the_engine_keeps_it_loaded_too() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let shared = with_engine(&world, engine.clone());
    let retired = ProcessingPipeline::new(shared.clone());
    let current = ProcessingPipeline::new(shared);
    assert!(
        retired
            .dependencies()
            .speech_engine
            .ptr_eq(&current.dependencies().speech_engine)
    );

    let slow = enqueue_call(&world, &retired);
    engine.wait_until_entered().await;
    let fast = enqueue_call(&world, &current);
    current.wait_until_idle().await;
    assert_eq!(meeting_state(&world, fast), MeetingState::Ready);
    assert_eq!(
        engine.inner.releases.count(),
        0,
        "the retired pipeline's job still needs the engine"
    );

    engine.open.notify_one();
    retired.wait_until_idle().await;
    assert_eq!(meeting_state(&world, slow), MeetingState::Ready);
    assert_eq!(engine.inner.releases.count(), 1);
}

/// A job that claims the engine while another job's release runs waits
/// for the release and prepares again after it, never during it.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_starts_during_a_release_prepares_again_after_it() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(GatedEngine::new(Gate::FirstRelease));
    let pipeline = ProcessingPipeline::new(with_engine(&world, engine.clone()));

    let first = enqueue_call(&world, &pipeline);
    engine.wait_until_entered().await;

    let second = enqueue_call(&world, &pipeline);
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
    for meeting in [first, second] {
        assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    }
}

/// The same across a reload: a job on another pipeline over the engine
/// that claims it while the retired pipeline's job releases it waits for
/// the release and prepares after it, as the lock is the engine's.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_on_another_pipeline_that_starts_during_a_release_prepares_after_it() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(GatedEngine::new(Gate::FirstRelease));
    let shared = with_engine(&world, engine.clone());
    let retired = ProcessingPipeline::new(shared.clone());
    let current = ProcessingPipeline::new(shared);

    let first = enqueue_call(&world, &retired);
    engine.wait_until_entered().await;
    let second = enqueue_call(&world, &current);
    // Time enough for the second job's warm-up, were it not held.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(engine.log(), ["prepare", "release"]);

    engine.open.notify_one();
    retired.wait_until_idle().await;
    current.wait_until_idle().await;
    assert_eq!(
        engine.log(),
        [
            "prepare", "release", "released", "prepare", "release", "released"
        ]
    );
    for meeting in [first, second] {
        assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    }
}

/// A recording's diarizer warm-up during a job's release waits for the
/// release, as a job's warm-up does: the diarizer never loads while the
/// speech engine frees its models.
#[tokio::test(flavor = "multi_thread")]
async fn the_diarizer_warm_up_waits_for_a_release() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(GatedEngine::new(Gate::FirstRelease));
    let diarizer = Arc::new(FakeDiarizer::default());
    let mut dependencies = with_engine(&world, engine.clone());
    dependencies.diarizer = diarizer.clone();
    let pipeline = ProcessingPipeline::new(dependencies);

    let meeting = enqueue_call(&world, &pipeline);
    engine.wait_until_entered().await;
    assert_eq!(diarizer.preparations.count(), 1, "the job's own warm-up");
    let warm_up = tokio::spawn({
        let pipeline = pipeline.clone();
        async move { pipeline.warm_up_diarizer().await }
    });
    // Time enough for the diarizer's warm-up, were it not held.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(diarizer.preparations.count(), 1);
    assert_eq!(engine.log(), ["prepare", "release"]);

    engine.open.notify_one();
    warm_up.await.unwrap().unwrap();
    assert_eq!(diarizer.preparations.count(), 2);
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
}

/// [`GatedEngine`] whose second `prepare` also waits, until `open` is
/// notified: the first is the first job's warm-up.
struct SecondPrepareHeld {
    inner: Arc<GatedEngine>,
    prepares: std::sync::atomic::AtomicUsize,
    entered: tokio::sync::Notify,
    open: tokio::sync::Notify,
}

#[async_trait]
impl steno_core::SpeechEngine for SecondPrepareHeld {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn supported_languages(&self) -> &std::collections::BTreeSet<steno_core::LanguageTag> {
        self.inner.supported_languages()
    }

    async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
        if self
            .prepares
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            self.entered.notify_one();
            self.open.notified().await;
        }
        self.inner.prepare().await
    }

    async fn transcribe(
        &self,
        audio: &steno_core::AudioBuffer16k,
        hint: Option<&steno_core::LanguageTag>,
    ) -> steno_core::protocols::BoundaryResult<Vec<steno_core::RawSegment>> {
        self.inner.transcribe(audio, hint).await
    }

    async fn release(&self) -> steno_core::protocols::BoundaryResult<()> {
        self.inner.release().await
    }
}

/// A job whose lanes end while a recording's warm-up holds the lock
/// leaves the engine loaded when a second job claims it meanwhile: the
/// count is checked again under the lock, so only the second job's end
/// releases it.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_claimed_while_a_finisher_waits_on_a_warm_up_keeps_the_engine_loaded() {
    let world = world(false, None, AudioRetention::KeepForever);
    let gated = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let engine = Arc::new(SecondPrepareHeld {
        inner: gated.clone(),
        prepares: 0.into(),
        entered: tokio::sync::Notify::new(),
        open: tokio::sync::Notify::new(),
    });
    let pipeline = ProcessingPipeline::new(with_engine(&world, engine.clone()));

    let first = enqueue_call(&world, &pipeline);
    gated.wait_until_entered().await;
    let warm_up = tokio::spawn({
        let pipeline = pipeline.clone();
        async move { pipeline.warm_up().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), engine.entered.notified())
        .await
        .expect("the warm-up holds the lock");
    // The first job's lanes end; it waits on the lock to release.
    gated.open.notify_one();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    // The second job claims the engine and waits on the lock to warm up.
    let second = enqueue_call(&world, &pipeline);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(gated.log(), ["prepare"]);

    engine.open.notify_one();
    warm_up.await.unwrap().unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(
        gated.log(),
        ["prepare", "prepare", "prepare", "release", "released"],
        "only the last job's end releases the engine"
    );
    for meeting in [first, second] {
        assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    }
}

/// A job whose lanes end while another job holds a claim does not wait on
/// the lock at all: it finishes while a recording's warm-up still holds
/// it, and the engine stays loaded for the other job.
#[tokio::test(flavor = "multi_thread")]
async fn a_finisher_that_sees_another_claim_does_not_wait_on_a_warm_up() {
    let world = world(false, None, AudioRetention::KeepForever);
    let gated = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let engine = Arc::new(SecondPrepareHeld {
        inner: gated.clone(),
        prepares: 0.into(),
        entered: tokio::sync::Notify::new(),
        open: tokio::sync::Notify::new(),
    });
    let pipeline = ProcessingPipeline::new(with_engine(&world, engine.clone()));

    let first = enqueue_call(&world, &pipeline);
    gated.wait_until_entered().await;
    let warm_up = tokio::spawn({
        let pipeline = pipeline.clone();
        async move { pipeline.warm_up().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), engine.entered.notified())
        .await
        .expect("the warm-up holds the lock");
    // The second job claims the engine and waits on the lock to warm up.
    let second = enqueue_call(&world, &pipeline);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    // The first job's lanes end; it finishes under the held lock.
    gated.open.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while meeting_state(&world, first) != MeetingState::Ready {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first job finished while the warm-up held the lock");
    assert_eq!(gated.log(), ["prepare"]);

    engine.open.notify_one();
    warm_up.await.unwrap().unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(
        gated.log(),
        ["prepare", "prepare", "prepare", "release", "released"],
        "only the last job's end releases the engine"
    );
    assert_eq!(meeting_state(&world, second), MeetingState::Ready);
}

/// A job that panics gives its claim back without a release, so the next
/// job on the same pipeline still releases the engine after its lanes.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_panics_gives_its_claim_on_the_engine_back() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(GatedEngine {
        panics: true,
        ..GatedEngine::new(Gate::FirstTranscription)
    });
    let pipeline = ProcessingPipeline::new(with_engine(&world, engine.clone()));
    enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(
        engine.inner.releases.count(),
        0,
        "a panic only drops the claim"
    );

    let next = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, next), MeetingState::Ready);
    assert_eq!(engine.inner.releases.count(), 1);
}

/// A job that fails once the pipeline has quit (the exit signalled the
/// speech sidecar with the app) leaves its meeting `processing`, warns
/// nothing, and the next launch processes it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_fails_after_the_pipeline_quits_is_resumed_at_the_next_launch() {
    let log = steno_pipeline::fixtures::CapturedLog::warnings();
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(GatedEngine {
        inner: FakeSpeechEngine {
            failure: Some("the sidecar died mid-request".to_owned()),
            ..FakeSpeechEngine::default()
        },
        ..GatedEngine::new(Gate::FirstTranscription)
    });
    let dependencies = with_engine(&world, engine.clone()).with_quit_latch(QuitLatch::default());
    let pipeline = ProcessingPipeline::new(dependencies);
    let mut meeting = call_meeting(world.now);
    meeting.id = Uuid::new_v4();
    let mut asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    // An id of its own picks this run's log lines out.
    asset.id = Uuid::new_v4();
    pipeline.enqueue(&meeting, &asset).unwrap();
    engine.wait_until_entered().await;

    let runs = RecordingLayout::from_asset(&asset)
        .unwrap()
        .processing_runs();
    assert_eq!(
        std::fs::read_to_string(&runs).unwrap(),
        "1",
        "the run is counted"
    );
    pipeline.quit();
    engine.open.notify_one();
    pipeline.wait_until_idle().await;
    assert_eq!(engine.inner.transcriptions.count(), 1, "the job failed");
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Processing);
    assert!(!runs.exists(), "a run the exit stopped does not count");
    assert!(
        !log.text().contains(&asset.id.to_string()),
        "debug only: {}",
        log.text()
    );

    let next_launch = ProcessingPipeline::new(
        world
            .pipeline
            .dependencies()
            .clone()
            .with_quit_latch(QuitLatch::default()),
    );
    assert_eq!(next_launch.resume_unfinished().unwrap(), [meeting.id]);
    next_launch.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
}

/// Once the pipeline has quit, no job starts: `enqueue` saves the meeting
/// `queued` with its asset and spawns no task (its debug line says so),
/// launch recovery starts nothing, and a direct `process` is refused
/// before it claims the meeting.
#[tokio::test(flavor = "multi_thread")]
async fn no_job_starts_once_the_pipeline_quits() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(FakeSpeechEngine::default());
    let dependencies = with_engine(&world, engine.clone()).with_quit_latch(QuitLatch::default());
    let pipeline = ProcessingPipeline::new(dependencies);
    pipeline.quit();

    let mut meeting = call_meeting(world.now);
    meeting.id = Uuid::new_v4();
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    // `enqueue` decides on this thread; a spawned task would log elsewhere.
    let log = steno_pipeline::fixtures::CapturedLog::default();
    let on_this_thread = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_max_level(tracing::Level::DEBUG)
            .finish(),
    );
    pipeline.enqueue(&meeting, &asset).unwrap();
    drop(on_this_thread);
    let text = log.text();
    assert!(
        text.lines()
            .any(|line| line.contains("not started: the app is quitting")
                && line.contains(&asset.id.to_string())),
        "{text}"
    );
    assert_eq!(pipeline.resume_unfinished().unwrap(), Vec::<Uuid>::new());
    let refused = pipeline.process(asset.id).await.unwrap_err();
    assert_eq!(refused.reason, "the app is quitting");
    pipeline.wait_until_idle().await;

    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Queued);
    assert!(
        world.store.asset(meeting.id).unwrap().is_some(),
        "the asset"
    );
    assert_eq!(engine.preparations.count(), 0, "nothing ran");
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_stage_marks_the_meeting_failed_with_its_name() {
    let world = world(false, None, AudioRetention::KeepForever);
    let pipeline = ProcessingPipeline::new(with_engine(
        &world,
        Arc::new(FakeSpeechEngine {
            failure: Some("no model".to_owned()),
            ..FakeSpeechEngine::default()
        }),
    ));
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    let stored = world.store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(
        stored.state,
        MeetingState::Failed {
            reason: "transcribe: no model".to_owned()
        },
        "the stage is named once"
    );
    assert!(
        world.store.segments(meeting.id).unwrap().is_empty(),
        "nothing after the failed stage was persisted"
    );
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
}

/// A pipeline over the world whose diarizer fails with `reason`.
fn with_failing_diarizer(world: &World, reason: &str) -> ProcessingPipeline {
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.diarizer = Arc::new(FakeDiarizer {
        failure: Some(reason.to_owned()),
        ..FakeDiarizer::default()
    });
    ProcessingPipeline::new(dependencies)
}

/// A diarizer that fails costs the speaker labels, not the transcript:
/// the meeting is ready, the mic lane stays "me" and the tap's segments
/// all go to one unknown speaker, without an embedding, that the user can
/// still name.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_diarizer_keeps_the_transcript_with_one_room_speaker() {
    let world = world(true, None, AudioRetention::KeepForever);
    let pipeline = with_failing_diarizer(&world, "no model");
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;

    let stored = world.store.meeting(meeting).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready);
    assert!(stored.summary.is_some(), "the later stages ran");
    let export = world.store.export(meeting).unwrap();
    assert_eq!(labels(&export), ["Me", "Speaker 1"]);
    let room = speaker(&export.speakers, "Speaker 1");
    assert_eq!(room.assignment.kind(), SpeakerAssignmentKind::Unknown);
    assert_eq!(room.embedding, None);
    assert_ne!(
        room.id,
        steno_core::derived_uuid(meeting, "speaker-Speaker 1"),
        "a later run that diarizes does not inherit its confirmation"
    );
    assert_eq!(export.segments.len(), 12);
    for segment in &export.segments {
        let expected = match segment.lane {
            AudioLane::System => Some(room.id),
            _ => Some(LaneMerger::me_speaker_id(meeting)),
        };
        assert_eq!(segment.speaker_id, expected, "{:?}", segment.lane);
    }
}

/// A speaker memory whose lookups fail.
struct BrokenSpeakerMemory;

#[async_trait]
impl steno_core::SpeakerMemory for BrokenSpeakerMemory {
    async fn candidates(
        &self,
        _embedding: &steno_core::Embedding,
        _limit: usize,
    ) -> steno_core::protocols::BoundaryResult<Vec<steno_core::SpeakerMatch>> {
        Err("the voice index is unreadable".into())
    }
}

/// A failing voice lookup leaves the diarized speakers unknown and the
/// meeting ready.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_speaker_match_keeps_the_speakers_unknown() {
    let world = world(false, None, AudioRetention::KeepForever);
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.speaker_memory = Arc::new(BrokenSpeakerMemory);
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;

    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    let export = world.store.export(meeting).unwrap();
    assert_eq!(labels(&export), ["Me", "Speaker 1", "Speaker 2"]);
    assert!(
        export
            .speakers
            .iter()
            .filter(|speaker| speaker.cluster_label != "Me")
            .all(|speaker| speaker.assignment.kind() == SpeakerAssignmentKind::Unknown)
    );
    assert_eq!(export.segments.len(), 12);
}

/// A diarizer failure once the app quits does not fall back: the run
/// stops without persisting, so the meeting is processed again, speakers
/// and all, at the next launch.
#[tokio::test(flavor = "multi_thread")]
async fn a_diarizer_failure_during_the_exit_leaves_the_meeting_for_the_next_launch() {
    let world = world(false, None, AudioRetention::KeepForever);
    let mut dependencies = world.pipeline.dependencies().clone();
    let latch = dependencies.quit_latch.clone();
    dependencies.diarizer = Arc::new(FakeDiarizer::answering(move |audio| {
        latch.set();
        // Two clusters under one label: the stage fails.
        let mut result = FakeDiarizer::round_robin(audio.duration(), 2, 1.5);
        for cluster in &mut result.clusters {
            "Speaker 1".clone_into(&mut cluster.label);
        }
        result
    }));
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;

    assert_eq!(meeting_state(&world, meeting), MeetingState::Processing);
    assert_eq!(world.store.segments(meeting).unwrap(), Vec::new());
}

/// A re-run whose diarizer fails keeps the speakers the first run stored:
/// a confirmation stays, with the person's voice, and the new transcript's
/// segments point at the kept speakers.
#[tokio::test(flavor = "multi_thread")]
async fn a_rerun_whose_diarizer_fails_keeps_the_confirmed_speakers() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let first = world.store.export(meeting.id).unwrap();
    let confirmed = speaker(&first.speakers, "Speaker 1");
    let anna = sample_data::person(0, "Anna");
    world.store.confirm_speaker(confirmed.id, &anna).unwrap();
    let voice = world.store.person(anna.id).unwrap().unwrap();
    assert_eq!(voice.sample_count, 1);

    with_failing_diarizer(&world, "no model")
        .process(asset.id)
        .await
        .unwrap();

    let again = world.store.export(meeting.id).unwrap();
    assert_eq!(again.meeting.state, MeetingState::Ready);
    assert_eq!(labels(&again), labels(&first));
    let kept = again
        .speakers
        .iter()
        .find(|kept| kept.id == confirmed.id)
        .expect("the confirmed speaker is kept");
    assert_eq!(
        kept.assignment,
        steno_core::SpeakerAssignment::Confirmed { person_id: anna.id }
    );
    assert_eq!(kept.embedding, confirmed.embedding);
    assert_eq!(kept.sample_clip_url, confirmed.sample_clip_url);
    assert_eq!(world.store.person(anna.id).unwrap().unwrap(), voice);
    assert_eq!(again.segments.len(), first.segments.len());
    for (segment, before) in again.segments.iter().zip(&first.segments) {
        assert_eq!(segment.speaker_id, before.speaker_id, "{segment:?}");
    }
}

/// A call whose tap carried no conversation diarizes its mic lane, which
/// is the room. When that diarizer fails, the mic lane becomes the one
/// room speaker, not "me", so the other party's words are never the
/// user's.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_diarizer_on_the_mic_lane_makes_it_the_room() {
    let world = world_with(
        false,
        None,
        AudioRetention::KeepForever,
        engine_deaf_to_silence(),
        FakeDiarizer::default(),
    );
    let pipeline = with_failing_diarizer(&world, "no model");
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    silence_the_tap(&asset);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;

    let export = world.store.export(meeting.id).unwrap();
    assert_eq!(export.meeting.state, MeetingState::Ready);
    assert_eq!(labels(&export), ["Speaker 1"]);
    assert_eq!(export.participants, [], "no \"me\" participant");
    let room = export.speakers[0].id;
    assert_eq!(export.segments.len(), 6);
    assert!(
        export
            .segments
            .iter()
            .all(|segment| segment.lane == AudioLane::Mic && segment.speaker_id == Some(room))
    );
}

/// A re-run of a call whose mic lane was diarized as the room keeps the
/// room's stored speakers on the mic lane when its diarizer fails: a
/// confirmation stays, and the mic never goes back to "me".
#[tokio::test(flavor = "multi_thread")]
async fn a_rerun_of_a_mic_room_whose_diarizer_fails_keeps_its_speakers() {
    let world = world_with(
        false,
        None,
        AudioRetention::KeepForever,
        engine_deaf_to_silence(),
        FakeDiarizer::default(),
    );
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    silence_the_tap(&asset);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let first = world.store.export(meeting.id).unwrap();
    assert_eq!(labels(&first), ["Speaker 1", "Speaker 2"]);
    let anna = sample_data::person(0, "Anna");
    let confirmed = speaker(&first.speakers, "Speaker 1").id;
    world.store.confirm_speaker(confirmed, &anna).unwrap();

    with_failing_diarizer(&world, "no model")
        .process(asset.id)
        .await
        .unwrap();

    let again = world.store.export(meeting.id).unwrap();
    assert_eq!(again.meeting.state, MeetingState::Ready);
    assert_eq!(labels(&again), labels(&first));
    assert_eq!(again.participants, [], "no \"me\" participant");
    assert_eq!(
        speaker(&again.speakers, "Speaker 1").assignment,
        steno_core::SpeakerAssignment::Confirmed { person_id: anna.id }
    );
    assert_eq!(again.segments.len(), first.segments.len());
    for (segment, before) in again.segments.iter().zip(&first.segments) {
        assert_eq!(segment.lane, AudioLane::Mic);
        assert_eq!(segment.speaker_id, before.speaker_id, "{segment:?}");
    }
}

/// When a re-run diarizes the other lane than the run before (the tap
/// went quiet, so the mic is now the room) and its diarizer fails, the
/// stored speakers keep their rows and confirmations but none of the new
/// lane's words: those go to one unknown room speaker.
#[tokio::test(flavor = "multi_thread")]
async fn a_rerun_on_the_other_lane_whose_diarizer_fails_gives_it_the_room_speaker() {
    let world = world_with(
        false,
        None,
        AudioRetention::KeepForever,
        engine_deaf_to_silence(),
        FakeDiarizer::default(),
    );
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let first = world.store.export(meeting.id).unwrap();
    assert_eq!(labels(&first), ["Me", "Speaker 1", "Speaker 2"]);
    let anna = sample_data::person(0, "Anna");
    let confirmed = speaker(&first.speakers, "Speaker 1").id;
    world.store.confirm_speaker(confirmed, &anna).unwrap();

    silence_the_tap(&asset);
    with_failing_diarizer(&world, "no model")
        .process(asset.id)
        .await
        .unwrap();

    let again = world.store.export(meeting.id).unwrap();
    assert_eq!(again.meeting.state, MeetingState::Ready);
    let kept = again
        .speakers
        .iter()
        .find(|kept| kept.id == confirmed)
        .expect("the confirmed speaker keeps its row");
    assert_eq!(
        kept.assignment,
        steno_core::SpeakerAssignment::Confirmed { person_id: anna.id }
    );
    let room = steno_core::derived_uuid(meeting.id, "speaker-room");
    assert!(again.speakers.iter().any(|speaker| speaker.id == room));
    assert_ne!(again.segments, Vec::new());
    assert!(
        again
            .segments
            .iter()
            .all(|segment| segment.lane == AudioLane::Mic && segment.speaker_id == Some(room)),
        "{:?}",
        again.segments
    );
}

/// A re-run whose diarizer fails again keeps the room speaker the first
/// failure made, with the name the user gave it.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_diarizer_failure_keeps_the_named_room_speaker() {
    let world = world(false, None, AudioRetention::KeepForever);
    let pipeline = with_failing_diarizer(&world, "no model");
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    let room = steno_core::derived_uuid(meeting, "speaker-room");
    let anna = sample_data::person(0, "Anna");
    world.store.confirm_speaker(room, &anna).unwrap();
    let asset = world.store.asset(meeting).unwrap().unwrap();

    pipeline.process(asset.id).await.unwrap();

    let again = world.store.export(meeting).unwrap();
    assert_eq!(labels(&again), ["Me", "Speaker 1"]);
    assert_eq!(
        speaker(&again.speakers, "Speaker 1").assignment,
        steno_core::SpeakerAssignment::Confirmed { person_id: anna.id }
    );
    assert!(
        again
            .segments
            .iter()
            .filter(|segment| segment.lane == AudioLane::System)
            .all(|segment| segment.speaker_id == Some(room))
    );
}

/// A diarizer failure the run goes on after is logged at warn with its
/// meeting and stage only: the reason can name the audio file.
#[tokio::test(flavor = "multi_thread")]
async fn a_diarizer_failure_warns_with_its_stage_not_its_reason() {
    let log = steno_pipeline::fixtures::CapturedLog::warnings();
    let world = world(false, None, AudioRetention::KeepForever);
    let reason = "cannot read /Users/someone/Audio/meeting/system.caf";
    let pipeline = with_failing_diarizer(&world, reason);
    // An id of its own picks this run's line out.
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);

    let text = log.text();
    let line = text
        .lines()
        .find(|line| line.contains(&meeting.to_string()))
        .unwrap_or_else(|| panic!("no line for the meeting: {text}"));
    assert!(line.contains("stage failed"), "{line}");
    assert!(line.contains("stage=\"diarize\""), "{line}");
    assert!(!text.contains(reason), "{text}");
}

/// A clock that panics once the meeting is stored `ready`: a panic inside
/// `persist`, after its ready write.
struct PanickingOnceReady {
    store: Arc<Store>,
    meeting_id: Uuid,
    ticks: TickingClock,
    panicked: std::sync::atomic::AtomicBool,
}

impl MonotonicClock for PanickingOnceReady {
    fn seconds(&self) -> f64 {
        let ready = self
            .store
            .meeting(self.meeting_id)
            .unwrap()
            .is_some_and(|meeting| meeting.state == MeetingState::Ready);
        if ready
            && !self
                .panicked
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            panic!("the clock broke after the ready write");
        }
        self.ticks.seconds()
    }
}

/// A meeting `persist` marked ready stays ready, and is delivered, even
/// when the run panics after that write.
#[tokio::test(flavor = "multi_thread")]
async fn a_panic_after_the_ready_write_leaves_the_meeting_ready() {
    let world = world(
        false,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::KeepForever,
    );
    let meeting = call_meeting(world.now);
    let clock = PanickingOnceReady {
        store: world.store.clone(),
        meeting_id: meeting.id,
        ticks: TickingClock(Mutex::new(0.0)),
        panicked: std::sync::atomic::AtomicBool::new(false),
    };
    let pipeline = ProcessingPipeline::new(
        world
            .pipeline
            .dependencies()
            .clone()
            .with_clock(Arc::new(clock)),
    );
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    let deliveries = world.store.deliveries(meeting.id).unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].status, DeliveryStatus::Delivered);
}

/// A failed background run is logged at warn with its asset and stage
/// only: a stage's reason can name the audio file or quote the model, and
/// stays with the meeting row and the debug level.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_background_run_warns_with_its_stage_not_its_reason() {
    let log = steno_pipeline::fixtures::CapturedLog::warnings();
    let world = world(false, None, AudioRetention::KeepForever);
    let reason = "cannot open /Users/someone/Audio/meeting/mic.caf";
    let pipeline = ProcessingPipeline::new(with_engine(
        &world,
        Arc::new(FakeSpeechEngine {
            failure: Some(reason.to_owned()),
            ..FakeSpeechEngine::default()
        }),
    ));
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
    assert!(line.contains("stage=\"transcribe\""), "{line}");
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
    let pipeline = ProcessingPipeline::new(with_engine(
        &world,
        Arc::new(PanickingEngine(std::collections::BTreeSet::new())),
    ));
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    // Without waiting for idle (which drops the entries itself), the same
    // asset can be enqueued again once the panicked run is gone.
    let enqueued = tokio::time::timeout(std::time::Duration::from_secs(30), async {
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
    assert!(
        !RecordingLayout::from_asset(&asset)
            .unwrap()
            .processing_runs()
            .exists(),
        "a panic the process survives is not a run that ended with the app"
    );
}

/// A launch whose run of the meeting never ends: the run is left
/// mid-transcription, as a crash leaves it, and the pipeline is dropped.
async fn launch_that_crashes(world: &World, first: Option<(&Meeting, &AudioAsset)>) -> Vec<Uuid> {
    let engine = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let pipeline = ProcessingPipeline::new(
        with_engine(world, engine.clone()).with_quit_latch(QuitLatch::default()),
    );
    let resumed = match first {
        Some((meeting, asset)) => {
            pipeline.enqueue(meeting, asset).unwrap();
            vec![meeting.id]
        }
        None => pipeline.resume_unfinished().unwrap(),
    };
    if !resumed.is_empty() {
        engine.wait_until_entered().await;
    }
    resumed
}

/// Processing that takes the app down is not retried for ever: each run is
/// counted in the meeting's folder, a run that ended with the app leaves
/// its count, and once three have, launch recovery marks the meeting
/// failed with a reason the user sees and keeps the audio. Processing the
/// meeting again (`enqueue`) starts afresh, and a run that settles clears
/// the count.
#[tokio::test(flavor = "multi_thread")]
async fn launch_recovery_gives_up_on_a_meeting_whose_runs_ended_with_the_app() {
    assert!(TOO_MANY_UNSETTLED_RUNS.contains(&MAX_UNSETTLED_RUNS.to_string()));
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    let layout = RecordingLayout::from_asset(&asset).unwrap();
    let runs = layout.processing_runs();

    assert_eq!(
        launch_that_crashes(&world, Some((&meeting, &asset))).await,
        [meeting.id]
    );
    for count in 2..=MAX_UNSETTLED_RUNS {
        assert_eq!(
            launch_that_crashes(&world, None).await,
            [meeting.id],
            "launch {count}"
        );
        assert_eq!(std::fs::read_to_string(&runs).unwrap(), count.to_string());
    }
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Processing);

    assert_eq!(launch_that_crashes(&world, None).await, Vec::<Uuid>::new());
    assert_eq!(
        meeting_state(&world, meeting.id),
        MeetingState::Failed {
            reason: TOO_MANY_UNSETTLED_RUNS.to_owned()
        }
    );
    assert!(!runs.exists(), "the count went with the meeting's run");
    assert!(
        file_url_path(&asset.url).unwrap().exists(),
        "the audio is kept"
    );
    for sidecar in asset.sidecars_16k.values() {
        assert!(
            file_url_path(sidecar).unwrap().exists(),
            "the sidecars are kept"
        );
    }

    // Processed again: the count starts afresh and the run clears it.
    std::fs::write(&runs, "7").unwrap();
    world.pipeline.reprocess(meeting.id).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert!(!runs.exists());
}

/// `reprocess` takes a ready or failed meeting only, and one with its
/// asset.
#[tokio::test(flavor = "multi_thread")]
async fn reprocess_refuses_a_meeting_that_is_not_settled_or_has_no_asset() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    let mut queued = meeting.clone();
    queued.state = MeetingState::Queued;
    world
        .store
        .save_meeting_with_asset(&queued, &asset)
        .unwrap();
    let refused = world.pipeline.reprocess(meeting.id).unwrap_err();
    assert_eq!(refused.stage, PipelineStage::Decode);
    assert!(refused.reason.contains("is queued"), "{}", refused.reason);

    let mut orphan = sample_data::meeting();
    orphan.id = Uuid::new_v4();
    orphan.state = MeetingState::Failed {
        reason: "x".to_owned(),
    };
    world.store.save_meeting(&orphan).unwrap();
    let refused = world.pipeline.reprocess(orphan.id).unwrap_err();
    assert!(
        refused.reason.contains("no audio asset"),
        "{}",
        refused.reason
    );
    assert!(world.pipeline.reprocess(Uuid::new_v4()).is_err());
}

/// Runs below the limit are resumed, and a run that settles clears them.
#[tokio::test(flavor = "multi_thread")]
async fn launch_recovery_resumes_a_meeting_below_the_limit_and_clears_its_count() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    let mut processing = meeting.clone();
    processing.state = MeetingState::Processing;
    world
        .store
        .save_meeting_with_asset(&processing, &asset)
        .unwrap();
    let runs = RecordingLayout::from_asset(&asset)
        .unwrap()
        .processing_runs();
    std::fs::write(&runs, (MAX_UNSETTLED_RUNS - 1).to_string()).unwrap();

    assert_eq!(world.pipeline.resume_unfinished().unwrap(), [meeting.id]);
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert!(!runs.exists());
}

/// A count that cannot be read never blocks processing: a corrupt file
/// counts as none.
#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_run_count_never_blocks_processing() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    let mut processing = meeting.clone();
    processing.state = MeetingState::Processing;
    world
        .store
        .save_meeting_with_asset(&processing, &asset)
        .unwrap();
    let runs = RecordingLayout::from_asset(&asset)
        .unwrap()
        .processing_runs();
    std::fs::write(&runs, b"\xff not a count").unwrap();

    assert_eq!(world.pipeline.resume_unfinished().unwrap(), [meeting.id]);
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert!(!runs.exists());
}

/// A meeting launch recovery gave up on keeps its audio through the
/// retention sweep, even with an expired stamp an earlier run left on its
/// asset; the same stamp on a ready meeting is swept, so the failure never
/// makes the audio go sooner.
#[tokio::test(flavor = "multi_thread")]
async fn a_meeting_given_up_on_keeps_its_audio_through_the_sweep() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    let stamped = |id: Uuid, state: MeetingState| {
        let mut meeting = call_meeting(world.now);
        meeting.id = id;
        meeting.state = state;
        let mut asset = call_asset(&world.audio, id, AudioRetention::DeleteAfterProcessing);
        asset.expires_at = Some(world.now - Duration::hours(1));
        world
            .store
            .save_meeting_with_asset(&meeting, &asset)
            .unwrap();
        asset
    };
    let given_up = stamped(Uuid::new_v4(), MeetingState::Processing);
    let ready = stamped(Uuid::new_v4(), MeetingState::Ready);
    let runs = RecordingLayout::from_asset(&given_up)
        .unwrap()
        .processing_runs();
    std::fs::write(&runs, MAX_UNSETTLED_RUNS.to_string()).unwrap();

    assert_eq!(
        world.pipeline.resume_unfinished().unwrap(),
        Vec::<Uuid>::new()
    );
    assert!(matches!(
        meeting_state(&world, given_up.meeting_id),
        MeetingState::Failed { .. }
    ));
    assert_eq!(
        world
            .store
            .asset(given_up.meeting_id)
            .unwrap()
            .unwrap()
            .expires_at,
        None
    );
    let removed = RetentionSweep::new(world.store.clone())
        .run(world.now + Duration::days(3_650))
        .unwrap();
    let ready_master = file_url_path(&ready.url).unwrap();
    assert!(
        removed.contains(&ready_master),
        "the ready meeting's audio is swept"
    );
    assert!(!ready_master.exists());
    let given_up_master = file_url_path(&given_up.url).unwrap();
    assert!(
        given_up_master.exists(),
        "the given-up meeting keeps its master"
    );
    for sidecar in given_up.sidecars_16k.values() {
        assert!(file_url_path(sidecar).unwrap().exists());
    }
}

/// A run that panics fails its meeting, named by the stage it was in, so
/// the user can delete it or process it again, and the next launch does
/// not resume it.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_panics_fails_its_meeting() {
    let world = world(false, None, AudioRetention::KeepForever);
    let pipeline = ProcessingPipeline::new(with_engine(
        &world,
        Arc::new(PanickingEngine(std::collections::BTreeSet::new())),
    ));
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(
        meeting_state(&world, meeting),
        MeetingState::Failed {
            reason: format!("transcribe: {}", steno_pipeline::OPERATION_PANICKED)
        }
    );
    assert_eq!(pipeline.resume_unfinished().unwrap(), Vec::<Uuid>::new());
    world.store.delete_meeting(meeting).unwrap();
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
