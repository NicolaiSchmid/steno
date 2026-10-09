//! The pipeline over core's fakes: the stage order, what each stage
//! persists, the no-LLM path, failure attribution, deferred retention,
//! launch recovery and the learned rates.
//! Swift: `Tests/StenoCoreTests/PipelineIntegrationTests.swift`,
//! `StageTests.swift`, `RetentionSweepTests.swift`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
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
    MeetingStateKind, ParticipantRole, PipelineStage, RawSegment, RecordingLayout,
    SpeakerAssignmentKind, Store, async_trait,
    paths::{file_url, file_url_path},
};
use steno_pipeline::crash_loop::{MAX_CRASHED_RUNS, TOO_MANY_CRASHED_RUNS};
use steno_pipeline::pipeline::{diarized_lane_after_transcription, tap_carried_no_conversation};
use steno_pipeline::{
    ClipProbe, ClipStep, ExportRetries, InFlight, LaneMerger, MeetingEventBus, MonotonicClock,
    PipelineDependencies, ProcessingPipeline, QuitLatch, ReprocessError, RetentionSweep,
    SharedSpeechEngine, StageRates,
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
    /// Holds the store, the audio and `export-retries.json`.
    dir: tempfile::TempDir,
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
        dir,
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
    /// Every transcription, and for ever, as a crash leaves the runs
    /// alive at it.
    EveryTranscription,
}

/// A fake engine whose first transcription or first release waits until
/// `open` is notified (or panics, with `panics`), or whose every
/// transcription hangs (`Gate::EveryTranscription`), logging each `prepare`
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
        if self.gate == Gate::EveryTranscription {
            self.entered
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return std::future::pending().await;
        }
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
    // Its own meeting id gives the asset an id that picks this run's log
    // lines out.
    let (meeting, asset) = processing_with_count(&world, 2);
    assert_eq!(pipeline.resume_unfinished().unwrap(), [meeting.id]);
    engine.wait_until_entered().await;

    let runs = runs_file(&asset);
    assert_eq!(read_runs(&asset), "3", "the run is counted");
    pipeline.quit();
    engine.open.notify_one();
    pipeline.wait_until_idle().await;
    assert_eq!(engine.inner.transcriptions.count(), 1, "the job failed");
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Processing);
    assert_eq!(
        read_runs(&asset),
        "2",
        "a run the exit stopped does not count, and the earlier crashes still do"
    );
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
    assert!(!runs.exists());
}

/// A run the exit took back that still makes the meeting ready clears the
/// count, earlier crashes included.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_the_exit_took_back_that_ends_ready_clears_the_count() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (meeting, asset) = processing_with_count(&world, 2);
    let (pipeline, engine) = gated_pipeline(&world, &QuitLatch::default());
    assert_eq!(pipeline.resume_unfinished().unwrap(), [meeting.id]);
    engine.wait_until_entered().await;
    pipeline.quit();
    assert_eq!(read_runs(&asset), "2", "taken back");
    engine.open.notify_one();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert!(!runs_file(&asset).exists());
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
    dependencies.diarizer = failing_diarizer(reason);
    ProcessingPipeline::new(dependencies)
}

/// A diarizer whose every run fails with `reason`.
fn failing_diarizer(reason: &str) -> Arc<FakeDiarizer> {
    Arc::new(FakeDiarizer {
        failure: Some(reason.to_owned()),
        ..FakeDiarizer::default()
    })
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
    // The merge's sweep keeps every clip the kept rows name.
    for speaker in &again.speakers {
        if let Some(url) = &speaker.sample_clip_url {
            assert!(file_url_path(url).unwrap().exists(), "{url}");
        }
    }
    assert_eq!(clip_files(&asset).len(), 2);
    assert_eq!(world.store.person(anna.id).unwrap().unwrap(), voice);
    assert_eq!(again.segments.len(), first.segments.len());
    for (segment, before) in again.segments.iter().zip(&first.segments) {
        assert_eq!(segment.speaker_id, before.speaker_id, "{segment:?}");
    }
}

/// The re-run's diarizer: the first run's two speakers, so the same ids,
/// with one-second clips where the first run's are one and a half seconds
/// long, so a clip's length tells which run wrote it.
fn rediarizing() -> Arc<FakeDiarizer> {
    Arc::new(FakeDiarizer::answering(|audio| {
        FakeDiarizer::round_robin(audio.duration(), 2, 1.0)
    }))
}

/// A pipeline over the world's dependencies with the re-run's diarizer,
/// and `probe` at the clip steps.
fn rerun_pipeline(world: &World, probe: Option<ClipProbe>) -> ProcessingPipeline {
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.diarizer = rediarizing();
    if let Some(probe) = probe {
        dependencies = dependencies.with_clip_probe(probe);
    }
    ProcessingPipeline::new(dependencies)
}

/// The meeting ready after the first run with its two speakers, "Speaker
/// 1" confirmed as Anna; its asset and the confirmed speaker's id.
async fn confirmed_call(world: &World) -> (AudioAsset, Uuid) {
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let confirmed = speaker(&world.store.speakers(meeting.id).unwrap(), "Speaker 1").id;
    world
        .store
        .confirm_speaker(confirmed, &sample_data::person(0, "Anna"))
        .unwrap();
    (asset, confirmed)
}

/// The samples of the 16 kHz mono WAV at `path`, which must be whole: its
/// header's data size is the rest of the file.
fn whole_clip_samples(path: &Path) -> u32 {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert!(bytes.len() >= 44, "{} has no header", path.display());
    assert_eq!(&bytes[36..40], b"data");
    let size = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
    assert_eq!(
        size as usize,
        bytes.len() - 44,
        "{} is torn",
        path.display()
    );
    size / 2
}

/// Asserts that "Speaker 1" is still confirmed as Anna and that every
/// speaker row of `meeting_id` with a clip names a whole WAV whose length
/// is the row's clip range: the clip of the run that wrote the row, or the
/// earlier clip a confirmed row given none kept. Returns the file names the
/// rows name.
fn assert_each_row_names_its_own_clip(store: &Store, meeting_id: Uuid) -> BTreeSet<OsString> {
    let speakers = store.speakers(meeting_id).unwrap();
    assert_eq!(
        speaker(&speakers, "Speaker 1").assignment,
        steno_core::SpeakerAssignment::Confirmed {
            person_id: sample_data::person(0, "Anna").id
        }
    );
    let mut named = BTreeSet::new();
    for speaker in speakers {
        let Some(url) = &speaker.sample_clip_url else {
            continue;
        };
        let path = file_url_path(url).unwrap();
        let range = speaker.sample_clip_range.unwrap();
        let seconds = f64::from(whole_clip_samples(&path)) / 16_000.0;
        assert!(
            (seconds - (range.upper - range.lower)).abs() < 1e-9,
            "{} names another run's clip",
            speaker.cluster_label
        );
        named.insert(path.file_name().unwrap().to_owned());
    }
    named
}

/// A probe that holds a run at [`ClipStep::Merging`], with the receiver
/// that hears when a run is held there and the sender that lets it go.
fn held_at_merging() -> (
    ClipProbe,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (reached, held_there) = std::sync::mpsc::channel();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let gate: ClipProbe = Arc::new(move |step| {
        if step == ClipStep::Merging {
            reached.send(()).unwrap();
            wait.lock().unwrap().recv().unwrap();
        }
        Ok(())
    });
    (gate, held_there, go)
}

/// The names of the files in the meeting's `speakers/`.
fn clip_files(asset: &AudioAsset) -> BTreeSet<OsString> {
    let layout = RecordingLayout::from_asset(asset).unwrap();
    std::fs::read_dir(layout.speakers_directory())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect()
}

/// The name of the file a clip URL names.
fn file_name_of(url: &str) -> OsString {
    file_url_path(url).unwrap().file_name().unwrap().to_owned()
}

/// A re-run that ends at any step of its sample clips, as a crash would,
/// leaves each speaker row naming a whole clip of the run that wrote the
/// row: the confirmed speaker keeps its earlier clip until the merge
/// commits the new one, which is on the disk by then, and the earlier clip
/// goes only after that commit. The next run sweeps what the ended one
/// left, and only that.
#[tokio::test(flavor = "multi_thread")]
async fn a_rerun_that_ends_at_any_clip_step_leaves_each_speaker_its_own_clip() {
    for (step, committed) in [
        (ClipStep::Written(0), false),
        (ClipStep::Merging, false),
        (ClipStep::Sweeping, true),
        (ClipStep::Removed(0), true),
    ] {
        let world = world(false, None, AudioRetention::KeepForever);
        let (asset, _) = confirmed_call(&world).await;
        let meeting = asset.meeting_id;
        let earlier = assert_each_row_names_its_own_clip(&world.store, meeting);
        assert_eq!(earlier.len(), 2);
        assert_eq!(clip_files(&asset), earlier);

        let crash: ClipProbe = Arc::new(move |reached| {
            assert_ne!(reached, step, "the app ends here");
            Ok(())
        });
        rerun_pipeline(&world, Some(crash))
            .process(asset.id)
            .await
            .unwrap_err();

        let named = assert_each_row_names_its_own_clip(&world.store, meeting);
        assert_eq!(named.len(), 2, "{step:?}");
        if committed {
            assert!(named.is_disjoint(&earlier), "{step:?}: the new clips");
        } else {
            assert_eq!(named, earlier, "{step:?}: the earlier clips");
        }
        let left = clip_files(&asset);
        assert!(named.is_subset(&left), "{step:?}");
        assert!(left.len() > named.len(), "{step:?}: files left to sweep");

        rerun_pipeline(&world, None)
            .process(asset.id)
            .await
            .unwrap();
        let swept = assert_each_row_names_its_own_clip(&world.store, meeting);
        assert!(swept.is_disjoint(&named), "{step:?}");
        assert_eq!(clip_files(&asset), swept, "{step:?}: only named clips stay");
    }
}

/// "Process again" whose clip write fails once one clip is on the disk
/// keeps the confirmed speaker's clip playable: the run removes the file it
/// wrote, falls back to the stored speakers, and the rows still name the
/// first run's clips.
#[tokio::test(flavor = "multi_thread")]
async fn process_again_whose_clip_write_fails_keeps_the_confirmed_clip_playable() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (asset, confirmed) = confirmed_call(&world).await;
    let meeting = asset.meeting_id;
    let before = world.store.speakers(meeting).unwrap();
    let earlier = assert_each_row_names_its_own_clip(&world.store, meeting);

    let failing: ClipProbe = Arc::new(|reached| match reached {
        ClipStep::Written(1) => Err(std::io::Error::other("the disk is full")),
        _ => Ok(()),
    });
    let pipeline = rerun_pipeline(&world, Some(failing));
    pipeline.reprocess(meeting).unwrap();
    pipeline.wait_until_idle().await;

    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    let after = world.store.speakers(meeting).unwrap();
    let kept = after.iter().find(|kept| kept.id == confirmed).unwrap();
    let was = before.iter().find(|was| was.id == confirmed).unwrap();
    assert_eq!(kept.sample_clip_url, was.sample_clip_url);
    assert_eq!(
        assert_each_row_names_its_own_clip(&world.store, meeting),
        earlier
    );
    assert_eq!(clip_files(&asset), earlier, "the failed write left nothing");
}

/// No sweep removes the clips of a run in flight: a re-run held after
/// writing its clips, before its merge, keeps them while another meeting's
/// run sweeps its own folder, and "Process again" on the held meeting is
/// refused, so no other run sweeps that folder. Once let go, the held run
/// names its clips and sweeps the earlier ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runs_uncommitted_clips_survive_every_other_runs_sweep() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (asset, _) = confirmed_call(&world).await;
    let meeting = asset.meeting_id;
    let earlier = clip_files(&asset);
    let (gate, held_there, go) = held_at_merging();
    let pipeline = rerun_pipeline(&world, Some(gate));
    let run = tokio::spawn(async move { pipeline.process(asset.id).await });
    tokio::task::spawn_blocking(move || held_there.recv().unwrap())
        .await
        .unwrap();
    let in_flight: BTreeSet<OsString> = clip_files(&asset).difference(&earlier).cloned().collect();
    assert_eq!(in_flight.len(), 2, "the held run wrote its clips");

    let other = enqueue_call(&world, &world.pipeline);
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, other), MeetingState::Ready);
    assert!(world.pipeline.reprocess(meeting).is_err());
    assert!(in_flight.is_subset(&clip_files(&asset)));

    go.send(()).unwrap();
    run.await.unwrap().unwrap();
    assert_eq!(
        assert_each_row_names_its_own_clip(&world.store, meeting),
        in_flight
    );
    assert_eq!(clip_files(&asset), in_flight);
}

/// A meeting whose master lies in another meeting's folder writes no clip
/// there and sweeps nothing, and the owner's re-run, which sweeps while the
/// guest's re-run is held before its merge, leaves the guest's files alone:
/// a clip of the guest's speaker that no row names stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_meeting_whose_master_lies_in_another_meetings_folder_leaves_its_clips_alone() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (owners, _) = confirmed_call(&world).await;
    let owner = owners.meeting_id;
    let mut guest = call_meeting(world.now);
    guest.id = Uuid::new_v4();
    let mut guests = owners.clone();
    guests.id = Uuid::new_v4();
    guests.meeting_id = guest.id;
    let before = clip_files(&owners);
    world.pipeline.enqueue(&guest, &guests).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, guest.id), MeetingState::Ready);
    let speakers = world.store.speakers(guest.id).unwrap();
    assert!(speakers.iter().all(|row| row.sample_clip_url.is_none()));
    assert_eq!(clip_files(&owners), before, "the guest wrote no clip here");
    // A clip of the guest's speaker that no row names, a fixed-name clip
    // as Swift writes it.
    let layout = RecordingLayout::from_asset(&owners).unwrap();
    let guests_clip = layout.sample_clip(speaker(&speakers, "Speaker 1").id);
    std::fs::write(&guests_clip, b"the guest's earlier clip").unwrap();

    let (gate, held_there, go) = held_at_merging();
    let held = rerun_pipeline(&world, Some(gate));
    held.reprocess(guest.id).unwrap();
    tokio::task::spawn_blocking(move || held_there.recv().unwrap())
        .await
        .unwrap();
    rerun_pipeline(&world, None)
        .process(owners.id)
        .await
        .unwrap();
    go.send(()).unwrap();
    held.wait_until_idle().await;

    assert_eq!(meeting_state(&world, guest.id), MeetingState::Ready);
    let guest_rows = world.store.speakers(guest.id).unwrap();
    assert!(guest_rows.iter().all(|row| row.sample_clip_url.is_none()));
    assert!(
        guests_clip.exists(),
        "another meeting's clip is never swept"
    );
    let mut expected = assert_each_row_names_its_own_clip(&world.store, owner);
    assert!(expected.is_disjoint(&before), "the owner's re-run swept");
    expected.insert(guests_clip.file_name().unwrap().to_owned());
    assert_eq!(clip_files(&owners), expected);
}

/// A confirmed speaker that a re-run gives no clip keeps naming its
/// earlier clip, which stays whole and playable, and a confirmed speaker
/// the re-run drops keeps its file with no row naming it, through that
/// re-run and the next.
#[tokio::test(flavor = "multi_thread")]
async fn a_confirmed_speaker_a_rerun_gives_no_clip_keeps_its_clip_file() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (asset, anna) = confirmed_call(&world).await;
    let meeting = asset.meeting_id;
    let ben = speaker(&world.store.speakers(meeting).unwrap(), "Speaker 2").id;
    world
        .store
        .confirm_speaker(ben, &sample_data::person(1, "Ben"))
        .unwrap();
    let annas_clip = speaker(&world.store.speakers(meeting).unwrap(), "Speaker 1")
        .sample_clip_url
        .clone()
        .unwrap();
    let earlier = clip_files(&asset);
    assert_eq!(earlier.len(), 2);
    // One cluster without a clip: "Speaker 1" comes back without one, and
    // "Speaker 2" is dropped.
    let pipeline = rerun_with_clipless(&world, 1, &["Speaker 1"], 1.0);

    for run in 0..2 {
        pipeline.process(asset.id).await.unwrap();
        let named = assert_each_row_names_its_own_clip(&world.store, meeting);
        let speakers = world.store.speakers(meeting).unwrap();
        let kept = speaker(&speakers, "Speaker 1");
        assert_eq!(kept.id, anna);
        assert_eq!(
            kept.sample_clip_url.as_ref(),
            Some(&annas_clip),
            "run {run}"
        );
        assert_eq!(named.len(), 1, "run {run}: Anna still plays her clip");
        assert!(speakers.iter().all(|row| row.id != ben), "run {run}");
        assert_eq!(clip_files(&asset), earlier, "run {run}: both clips stay");
    }
}

/// A pipeline over the world's dependencies whose diarizer gives
/// `clusters` speakers of `turn`-second turns, and no clip to the ones
/// labelled in `clipless`.
fn rerun_with_clipless(
    world: &World,
    clusters: usize,
    clipless: &'static [&'static str],
    turn: f64,
) -> ProcessingPipeline {
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.diarizer = Arc::new(FakeDiarizer::answering(move |audio| {
        let mut result = FakeDiarizer::round_robin(audio.duration(), clusters, turn);
        for cluster in &mut result.clusters {
            if clipless.contains(&cluster.label.as_str()) {
                cluster.sample_clip_range = None;
            }
        }
        result
    }));
    ProcessingPipeline::new(dependencies)
}

/// Confirming a speaker as the person of a confirmed speaker without a clip
/// merges it into that row, which takes the merged speaker's clip under
/// that speaker's id. Re-runs that again give the confirmed row no clip,
/// and the merged speaker's id a clip of its own, leave the row naming the
/// clip it took, whole and playable.
#[tokio::test(flavor = "multi_thread")]
async fn a_merge_into_a_confirmed_speaker_without_a_clip_keeps_its_clip_through_reruns() {
    let world = world(false, None, AudioRetention::KeepForever);
    let first = rerun_with_clipless(&world, 2, &["Speaker 1"], 1.5);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    first.enqueue(&meeting, &asset).unwrap();
    first.wait_until_idle().await;
    let speakers = world.store.speakers(meeting.id).unwrap();
    let (kept, merged) = (
        speaker(&speakers, "Speaker 1").id,
        speaker(&speakers, "Speaker 2").id,
    );
    let anna = sample_data::person(0, "Anna");
    world.store.confirm_speaker(kept, &anna).unwrap();
    world.store.confirm_speaker(merged, &anna).unwrap();
    let took = world
        .store
        .speakers(meeting.id)
        .unwrap()
        .into_iter()
        .find(|row| row.id == kept)
        .unwrap()
        .sample_clip_url
        .unwrap();
    let took_path = file_url_path(&took).unwrap();
    assert!(
        file_name_of(&took)
            .to_string_lossy()
            .starts_with(&merged.to_string().to_uppercase()),
        "the clip stays under the merged speaker's id"
    );

    let rerun = rerun_with_clipless(&world, 2, &["Speaker 1"], 1.0);
    for run in 1..=2 {
        rerun.process(asset.id).await.unwrap();
        assert_each_row_names_its_own_clip(&world.store, meeting.id);
        let rows = world.store.speakers(meeting.id).unwrap();
        let row = rows.iter().find(|row| row.id == kept).unwrap();
        assert_eq!(row.sample_clip_url.as_ref(), Some(&took), "run {run}");
        assert!(rows.iter().any(|row| row.id == merged), "run {run}");
        assert_eq!(whole_clip_samples(&took_path), 24_000, "run {run}");
    }
}

/// A confirmed speaker a re-run drops keeps its file, and so it does when
/// a later re-run brings its id back unconfirmed and without a clip: only
/// a speaker the merge replaced or this run gave a clip has its files
/// swept.
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_confirmed_speaker_that_comes_back_without_a_clip_keeps_its_file() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (asset, _) = confirmed_call(&world).await;
    let meeting = asset.meeting_id;
    let speakers = world.store.speakers(meeting).unwrap();
    let ben = speaker(&speakers, "Speaker 2").id;
    world
        .store
        .confirm_speaker(ben, &sample_data::person(1, "Ben"))
        .unwrap();
    let bens_clip = file_name_of(
        speaker(&speakers, "Speaker 2")
            .sample_clip_url
            .as_ref()
            .unwrap(),
    );

    rerun_with_clipless(&world, 1, &[], 1.0)
        .process(asset.id)
        .await
        .unwrap();
    let rows = world.store.speakers(meeting).unwrap();
    assert!(rows.iter().all(|row| row.id != ben), "the re-run drops Ben");
    assert!(clip_files(&asset).contains(&bens_clip));

    rerun_with_clipless(&world, 2, &["Speaker 2"], 1.0)
        .process(asset.id)
        .await
        .unwrap();
    let rows = world.store.speakers(meeting).unwrap();
    let back = rows.iter().find(|row| row.id == ben).unwrap();
    assert!(!back.assignment.is_confirmed());
    assert_eq!(back.sample_clip_url, None);
    assert!(
        clip_files(&asset).contains(&bens_clip),
        "the file of the confirmed speaker stays"
    );
}

/// A confirmation the user makes while a re-run is held after writing its
/// clips, before its merge, is kept by the merge, and the sweep reads it in
/// the merge's transaction: given a clip, the speaker names the new one;
/// given none, it keeps naming its earlier clip; dropped, its earlier file
/// stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_confirmation_during_a_rerun_keeps_the_speakers_clip() {
    for (clusters, clipless) in [(2, &[][..]), (2, &["Speaker 2"][..]), (1, &[][..])] {
        let world = world(false, None, AudioRetention::KeepForever);
        let (asset, _) = confirmed_call(&world).await;
        let meeting = asset.meeting_id;
        let speakers = world.store.speakers(meeting).unwrap();
        let ben = speaker(&speakers, "Speaker 2").id;
        let bens_clip = speaker(&speakers, "Speaker 2").sample_clip_url.clone();
        let bens_file = file_name_of(bens_clip.as_ref().unwrap());
        let (gate, held_there, go) = held_at_merging();
        let pipeline = ProcessingPipeline::new(
            rerun_with_clipless(&world, clusters, clipless, 1.0)
                .dependencies()
                .clone()
                .with_clip_probe(gate),
        );
        let run = tokio::spawn(async move { pipeline.process(asset.id).await });
        tokio::task::spawn_blocking(move || held_there.recv().unwrap())
            .await
            .unwrap();
        world
            .store
            .confirm_speaker(ben, &sample_data::person(1, "Ben"))
            .unwrap();
        go.send(()).unwrap();
        run.await.unwrap().unwrap();

        let case = format!("{clusters} clusters, clipless {clipless:?}");
        let named = assert_each_row_names_its_own_clip(&world.store, meeting);
        let rows = world.store.speakers(meeting).unwrap();
        let files = clip_files(&asset);
        if let Some(row) = rows.iter().find(|row| row.id == ben) {
            assert!(row.assignment.is_confirmed(), "{case}");
            if clipless.is_empty() {
                assert_ne!(row.sample_clip_url, bens_clip, "{case}");
            } else {
                assert_eq!(row.sample_clip_url, bens_clip, "{case}");
            }
            assert_eq!(files, named, "{case}: only named clips stay");
        } else {
            let mut expected = named.clone();
            expected.insert(bens_file.clone());
            assert_eq!(files, expected, "{case}: the dropped file stays");
        }
    }
}

/// Retention that comes due while a re-run is held after writing its
/// clips, before its merge, removes nothing: the meeting is processing.
/// Once let go, the run commits rows that name clips on the disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_during_a_held_rerun_removes_nothing() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (mut asset, _) = confirmed_call(&world).await;
    asset.retention = AudioRetention::KeepDays(1);
    asset.expires_at = Some(world.now - Duration::hours(1));
    world.store.save_asset(&asset).unwrap();
    let earlier = clip_files(&asset);
    let (gate, held_there, go) = held_at_merging();
    let pipeline = rerun_pipeline(&world, Some(gate));
    let id = asset.id;
    let run = tokio::spawn(async move { pipeline.process(id).await });
    tokio::task::spawn_blocking(move || held_there.recv().unwrap())
        .await
        .unwrap();
    let in_flight: BTreeSet<OsString> = clip_files(&asset).difference(&earlier).cloned().collect();
    assert_eq!(in_flight.len(), 2);

    let removed = RetentionSweep::new(world.store.clone())
        .run(world.now)
        .unwrap();
    assert_eq!(removed, Vec::<PathBuf>::new());
    assert!(in_flight.is_subset(&clip_files(&asset)));
    go.send(()).unwrap();
    run.await.unwrap().unwrap();
    assert_eq!(
        assert_each_row_names_its_own_clip(&world.store, asset.meeting_id),
        in_flight
    );
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
    // The helper fails with `reason`, so its absence below means something.
    let error = steno_core::Diarizer::diarize(
        failing_diarizer(reason).as_ref(),
        &steno_core::AudioBuffer16k::new(vec![0.0; 16_000]),
    )
    .await
    .expect_err("the failing diarizer fails");
    assert!(error.to_string().contains(reason), "{error}");
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
        !runs_file(&asset).exists(),
        "a panic the process survives is not a run that ended with the app"
    );
}

/// The asset's `.processing-runs`.
fn runs_file(asset: &AudioAsset) -> PathBuf {
    RecordingLayout::from_asset(asset)
        .unwrap()
        .processing_runs()
}

/// A pipeline over `dependencies` as a new process builds it: with a quit
/// latch and an in-flight set of its own, so a run an earlier launch left
/// holding its meeting claims nothing in the new set.
fn new_process(dependencies: PipelineDependencies) -> ProcessingPipeline {
    ProcessingPipeline::new(
        dependencies
            .with_quit_latch(QuitLatch::default())
            .with_in_flight(InFlight::default()),
    )
}

/// A launch whose run of the meeting never ends: the run is left
/// mid-transcription, as a crash leaves it, and the pipeline is dropped.
async fn launch_that_crashes(world: &World, first: Option<(&Meeting, &AudioAsset)>) -> Vec<Uuid> {
    let engine = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let pipeline = new_process(with_engine(world, engine.clone()));
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
/// meeting again (`enqueue`) starts afresh, and a run that ends clears
/// the count.
#[tokio::test(flavor = "multi_thread")]
async fn launch_recovery_gives_up_on_a_meeting_whose_runs_ended_with_the_app() {
    assert!(TOO_MANY_CRASHED_RUNS.contains(&MAX_CRASHED_RUNS.to_string()));
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    let runs = runs_file(&asset);

    assert_eq!(
        launch_that_crashes(&world, Some((&meeting, &asset))).await,
        [meeting.id]
    );
    for count in 2..=MAX_CRASHED_RUNS {
        assert_eq!(
            launch_that_crashes(&world, None).await,
            [meeting.id],
            "launch {count}"
        );
        assert_eq!(read_runs(&asset), count.to_string());
    }
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Processing);

    assert_eq!(launch_that_crashes(&world, None).await, Vec::<Uuid>::new());
    assert_eq!(
        meeting_state(&world, meeting.id),
        MeetingState::Failed {
            reason: TOO_MANY_CRASHED_RUNS.to_owned()
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
async fn reprocess_refuses_a_meeting_that_is_not_finished_or_has_no_asset() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    let mut queued = meeting.clone();
    queued.state = MeetingState::Queued;
    world
        .store
        .save_meeting_with_asset(&queued, &asset)
        .unwrap();
    assert_eq!(
        world.pipeline.reprocess(meeting.id),
        Err(ReprocessError::Unfinished {
            meeting_id: meeting.id,
            state: MeetingStateKind::Queued
        })
    );

    let mut orphan = sample_data::meeting();
    orphan.id = Uuid::new_v4();
    orphan.state = MeetingState::Failed {
        reason: "x".to_owned(),
    };
    world.store.save_meeting(&orphan).unwrap();
    assert_eq!(
        world.pipeline.reprocess(orphan.id),
        Err(ReprocessError::NoAsset(orphan.id))
    );
    let unknown = Uuid::new_v4();
    assert_eq!(
        world.pipeline.reprocess(unknown),
        Err(ReprocessError::MeetingNotFound(unknown))
    );
}

/// `process_again` is `reprocess` for a meeting the app offers it for: a
/// ready one is refused under the pipeline's own read and stays as it was,
/// unclaimed; a failed one runs to ready.
#[tokio::test(flavor = "multi_thread")]
async fn process_again_refuses_a_ready_meeting_and_runs_a_failed_one() {
    let world = world(false, None, AudioRetention::KeepForever);
    let mut meeting = call_meeting(world.now);
    meeting.state = MeetingState::Ready;
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world
        .store
        .save_meeting_with_asset(&meeting, &asset)
        .unwrap();
    assert_eq!(
        world.pipeline.process_again(meeting.id),
        Err(ReprocessError::NotOffered(meeting.id))
    );
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert_eq!(world.pipeline.in_flight(), Vec::<Uuid>::new());

    world
        .store
        .set_state(
            meeting.id,
            MeetingState::Failed {
                reason: "decode: unreadable".to_owned(),
            },
            world.now,
        )
        .unwrap();
    world.pipeline.process_again(meeting.id).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
}

/// A ready meeting whose results may be incomplete is offered "Process
/// again" too: after a diarizer fallback stored the room row, the
/// pipeline's own read accepts it, and once a working diarizer replaced
/// the row the meeting is complete and refused again.
#[tokio::test(flavor = "multi_thread")]
async fn process_again_runs_a_ready_meeting_kept_incomplete() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    let fell_back = with_failing_diarizer(&world, "no model");
    let meeting = enqueue_call(&world, &fell_back);
    fell_back.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    assert!(has_room_row(&world, meeting));

    world.pipeline.process_again(meeting).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    assert!(!has_room_row(&world, meeting), "the diarizer worked");
    assert_eq!(
        world.pipeline.process_again(meeting),
        Err(ReprocessError::NotOffered(meeting))
    );
}

/// The retention sweep keeps the asset row when it removes a ready
/// meeting's files, so `reprocess` checks the disk: a meeting whose master
/// is gone is refused before anything is saved, so its transcript stays,
/// even with its sidecars there (the run ends by mixing the master down).
#[tokio::test(flavor = "multi_thread")]
async fn reprocess_refuses_a_meeting_whose_audio_is_gone() {
    let world = world(false, None, AudioRetention::KeepForever);
    let asset = ready_with_count(&world, 2);
    std::fs::remove_file(file_url_path(&asset.url).unwrap()).unwrap();
    let before = world.store.meeting(asset.meeting_id).unwrap().unwrap();
    assert_eq!(
        world.pipeline.reprocess(asset.meeting_id),
        Err(ReprocessError::AudioGone(asset.meeting_id))
    );
    assert_eq!(
        world.store.meeting(asset.meeting_id).unwrap().unwrap(),
        before
    );
    assert_eq!(read_runs(&asset), "2", "the count is untouched");
}

/// Two "Process again" at once start one run: the in-flight check and the
/// claim are one step, so the second call is refused (busy, or
/// queued by the first by the time it reads the meeting).
#[tokio::test(flavor = "multi_thread")]
async fn two_reprocess_calls_at_once_start_one_run() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    let mut ready = meeting.clone();
    ready.state = MeetingState::Ready;
    world.store.save_meeting_with_asset(&ready, &asset).unwrap();
    let runtime = tokio::runtime::Handle::current();
    for round in 0..50 {
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let calls: Vec<_> = (0..2)
            .map(|_| {
                let pipeline = world.pipeline.clone();
                let barrier = barrier.clone();
                let runtime = runtime.clone();
                std::thread::spawn(move || {
                    let _entered = runtime.enter();
                    barrier.wait();
                    pipeline.reprocess(meeting.id)
                })
            })
            .collect();
        let results: Vec<_> = calls.into_iter().map(|c| c.join().unwrap()).collect();
        let started = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(started, 1, "round {round}: {results:?}");
        for result in &results {
            assert!(
                matches!(
                    result,
                    Ok(()) | Err(ReprocessError::Busy(_) | ReprocessError::Unfinished { .. })
                ),
                "round {round}: {result:?}"
            );
        }
        world.pipeline.wait_until_idle().await;
        assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    }
}

/// `reprocess` is refused while another operation holds the meeting, and
/// so is `enqueue`.
#[tokio::test(flavor = "multi_thread")]
async fn reprocess_refuses_a_meeting_another_operation_holds() {
    let world = world(true, None, AudioRetention::KeepForever);
    let asset = ready_with_count(&world, 0);
    let meeting = world.store.meeting(asset.meeting_id).unwrap().unwrap();
    let held = world.pipeline.claim_redeliver(meeting.id).unwrap();
    assert_eq!(
        world.pipeline.reprocess(meeting.id),
        Err(ReprocessError::Busy(meeting.id))
    );
    assert!(world.pipeline.enqueue(&meeting, &asset).is_err());
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    drop(held);
    world.pipeline.reprocess(meeting.id).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
}

/// A store that fails saving the meeting queued: `reprocess` says so with
/// the store's failure, the meeting and its count are as they were, and
/// the claim is released, so a later call starts the run.
#[tokio::test(flavor = "multi_thread")]
async fn a_reprocess_whose_save_fails_is_refused_and_changes_nothing() {
    let world = world(false, None, AudioRetention::KeepForever);
    let asset = ready_with_count(&world, 2);
    let before = world.store.meeting(asset.meeting_id).unwrap().unwrap();
    world
        .store
        .write(|transaction| {
            Ok(transaction.execute_batch(
                "CREATE TEMP TRIGGER refuse_queued BEFORE UPDATE ON meeting \
                 WHEN NEW.state = 'queued' BEGIN SELECT RAISE(ABORT, 'refused'); END",
            )?)
        })
        .unwrap();
    match world.pipeline.reprocess(asset.meeting_id) {
        Err(ReprocessError::Pipeline(failure)) => {
            assert_eq!(failure.stage, PipelineStage::Decode);
        }
        other => panic!("expected the store's failure, got {other:?}"),
    }
    world.pipeline.wait_until_idle().await;
    assert_eq!(
        world.store.meeting(asset.meeting_id).unwrap().unwrap(),
        before
    );
    assert_eq!(read_runs(&asset), "2", "the count is untouched");

    world
        .store
        .write(|transaction| Ok(transaction.execute_batch("DROP TRIGGER temp.refuse_queued")?))
        .unwrap();
    world.pipeline.reprocess(asset.meeting_id).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, asset.meeting_id), MeetingState::Ready);
}

/// A reprocess that fails leaves no retention stamp from the run before
/// it, so the sweep keeps the audio a retry needs.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_reprocess_leaves_the_audio_to_no_sweep() {
    let world = world_with(
        false,
        None,
        AudioRetention::KeepForever,
        FakeSpeechEngine {
            failure: Some("the model refused".to_owned()),
            ..FakeSpeechEngine::default()
        },
        FakeDiarizer::default(),
    );
    let mut meeting = call_meeting(world.now);
    meeting.state = MeetingState::Ready;
    let mut asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepDays(7));
    asset.expires_at = Some(world.now + Duration::days(7));
    world
        .store
        .save_meeting_with_asset(&meeting, &asset)
        .unwrap();

    world.pipeline.reprocess(meeting.id).unwrap();
    world.pipeline.wait_until_idle().await;
    assert!(matches!(
        meeting_state(&world, meeting.id),
        MeetingState::Failed { .. }
    ));
    assert_eq!(
        world.store.asset(meeting.id).unwrap().unwrap().expires_at,
        None
    );
    RetentionSweep::new(world.store.clone())
        .run(world.now + Duration::days(8))
        .unwrap();
    assert!(file_url_path(&asset.url).unwrap().exists(), "the master");
    for sidecar in asset.sidecars_16k.values() {
        assert!(file_url_path(sidecar).unwrap().exists(), "a sidecar");
    }
}

/// A meeting's crashes are charged to it, not to the meetings waiting
/// behind it: of two unfinished meetings that both ran when the app first
/// went down, the one launch recovery resumes first (A) takes it down at
/// every launch after, while the other (B) waits for its turn uncounted. Launch recovery gives up on A
/// and processes B, which ends ready.
#[tokio::test(flavor = "multi_thread")]
async fn a_meeting_that_waits_its_turn_is_not_charged_for_another_meetings_crash() {
    let world = world(false, None, AudioRetention::KeepForever);
    let mut two = [
        processing_with_count(&world, 0),
        processing_with_count(&world, 0),
    ];
    // Launch recovery takes meetings of one start time by id.
    two.sort_by_key(|(meeting, _)| meeting.id);
    let [(a, a_asset), (b, b_asset)] = two;
    for launch in 1..=MAX_CRASHED_RUNS {
        let engine = Arc::new(GatedEngine::new(Gate::EveryTranscription));
        let pipeline = new_process(with_engine(&world, engine.clone()));
        assert_eq!(
            pipeline.resume_unfinished().unwrap(),
            [a.id, b.id],
            "launch {launch}"
        );
        // A is transcribing.
        engine.wait_until_entered().await;
        assert_eq!(read_runs(&a_asset), launch.to_string(), "launch {launch}");
        // Both ran at the first launch; B waits behind A after that.
        assert_eq!(read_runs(&b_asset), "1", "launch {launch}");
        // The crash: nothing ends, nothing quits.
        drop(pipeline);
    }

    let next_launch = new_process(world.pipeline.dependencies().clone());
    assert_eq!(next_launch.resume_unfinished().unwrap(), [b.id]);
    next_launch.wait_until_idle().await;
    assert_eq!(
        meeting_state(&world, a.id),
        MeetingState::Failed {
            reason: TOO_MANY_CRASHED_RUNS.to_owned()
        }
    );
    assert_eq!(meeting_state(&world, b.id), MeetingState::Ready);
    assert!(!runs_file(&b_asset).exists());
}

/// A count-0 meeting started at once and held mid-transcription, and a
/// count-1 meeting that goes alone: the alone run waits uncounted until the
/// first run ends, then runs; both end ready.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_started_at_once_holds_back_a_run_that_goes_alone() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (fresh, fresh_asset) = processing_with_count(&world, 0);
    let (counted, counted_asset) = processing_with_count(&world, 1);
    let engine = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let pipeline = new_process(with_engine(&world, engine.clone()));
    assert_eq!(
        pipeline.resume_unfinished().unwrap(),
        [fresh.id, counted.id]
    );
    engine.wait_until_entered().await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(read_runs(&fresh_asset), "1");
    assert_eq!(
        read_runs(&counted_asset),
        "1",
        "the alone run waits uncounted"
    );
    assert_eq!(meeting_state(&world, counted.id), MeetingState::Processing);
    engine.open.notify_one();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, fresh.id), MeetingState::Ready);
    assert_eq!(meeting_state(&world, counted.id), MeetingState::Ready);
    assert!(!runs_file(&fresh_asset).exists());
    assert!(!runs_file(&counted_asset).exists());
}

/// A quit while the alone run waits its turn: it never starts, its count
/// stays, and the next launch resumes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_quit_while_a_run_waits_its_turn_leaves_its_count() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (fresh, fresh_asset) = processing_with_count(&world, 0);
    let (counted, counted_asset) = processing_with_count(&world, 1);
    let engine = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let pipeline = new_process(with_engine(&world, engine.clone()));
    assert_eq!(
        pipeline.resume_unfinished().unwrap(),
        [fresh.id, counted.id]
    );
    engine.wait_until_entered().await;
    pipeline.quit();
    assert!(
        !runs_file(&fresh_asset).exists(),
        "the exit took the open run back"
    );
    engine.open.notify_one();
    pipeline.wait_until_idle().await;
    assert_eq!(
        read_runs(&counted_asset),
        "1",
        "the waiting run was never counted"
    );
    assert_eq!(meeting_state(&world, counted.id), MeetingState::Processing);
    assert!(!runs_file(&fresh_asset).exists());

    let next_launch = new_process(world.pipeline.dependencies().clone());
    let resumed = next_launch.resume_unfinished().unwrap();
    assert!(resumed.contains(&counted.id), "{resumed:?}");
    next_launch.wait_until_idle().await;
    assert_eq!(meeting_state(&world, counted.id), MeetingState::Ready);
    assert!(!runs_file(&counted_asset).exists());
}

/// Three alone meetings, the first two hung: only the first is counted;
/// the dispatcher serialises them oldest first.
#[tokio::test(flavor = "multi_thread")]
async fn alone_runs_are_serial_and_oldest_first() {
    let world = world(false, None, AudioRetention::KeepForever);
    let mut three = [
        processing_with_count(&world, 1),
        processing_with_count(&world, 1),
        processing_with_count(&world, 1),
    ];
    three.sort_by_key(|(meeting, _)| meeting.id);
    let engine = Arc::new(GatedEngine::new(Gate::EveryTranscription));
    let pipeline = new_process(with_engine(&world, engine.clone()));
    let ids: Vec<Uuid> = three.iter().map(|(m, _)| m.id).collect();
    assert_eq!(pipeline.resume_unfinished().unwrap(), ids);
    engine.wait_until_entered().await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let counts: Vec<String> = three.iter().map(|(_, a)| read_runs(a)).collect();
    assert_eq!(counts, ["2", "1", "1"]);
    drop(pipeline);
}

/// A meeting saved `processing` with `crashed` runs that ended with the
/// app already counted, as an earlier launch leaves it.
fn processing_with_count(world: &World, crashed: u32) -> (Meeting, AudioAsset) {
    let mut meeting = call_meeting(world.now);
    meeting.id = Uuid::new_v4();
    meeting.state = MeetingState::Processing;
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world
        .store
        .save_meeting_with_asset(&meeting, &asset)
        .unwrap();
    std::fs::write(runs_file(&asset), crashed.to_string()).unwrap();
    (meeting, asset)
}

/// A ready meeting with `crashed` runs left in its count; its asset.
fn ready_with_count(world: &World, crashed: u32) -> AudioAsset {
    let (mut meeting, asset) = processing_with_count(world, crashed);
    meeting.state = MeetingState::Ready;
    world.store.save_meeting(&meeting).unwrap();
    asset
}

/// The asset's count as written.
fn read_runs(asset: &AudioAsset) -> String {
    std::fs::read_to_string(runs_file(asset)).unwrap()
}

/// A pipeline over its own latch and an engine whose first transcription
/// waits until it is let go.
fn gated_pipeline(world: &World, latch: &QuitLatch) -> (ProcessingPipeline, Arc<GatedEngine>) {
    let engine = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let pipeline =
        ProcessingPipeline::new(with_engine(world, engine.clone()).with_quit_latch(latch.clone()));
    (pipeline, engine)
}

/// A normal quit with a run held mid-transcription, never let go (the app
/// exits before the run ends, and nothing drops it): the exit takes the
/// run back at once, so the count is what it was before the run and the
/// earlier crashes still count. The next launch resumes the meeting.
#[tokio::test(flavor = "multi_thread")]
async fn a_quit_takes_back_a_run_held_mid_transcription() {
    let world = world(false, None, AudioRetention::KeepForever);
    let (meeting, asset) = processing_with_count(&world, 2);
    let runs = runs_file(&asset);
    let latch = QuitLatch::default();
    let (pipeline, engine) = gated_pipeline(&world, &latch);
    assert_eq!(pipeline.resume_unfinished().unwrap(), [meeting.id]);
    engine.wait_until_entered().await;
    assert_eq!(read_runs(&asset), "3", "the run is counted");

    pipeline.quit();
    assert_eq!(read_runs(&asset), "2", "the exit took its run back");
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Processing);

    let next_launch = new_process(world.pipeline.dependencies().clone());
    assert_eq!(next_launch.resume_unfinished().unwrap(), [meeting.id]);
    next_launch.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert!(!runs.exists());
}

/// The exit reaches every pipeline that shares the latch: a reload's
/// retired pipeline still finishing a resumed run, and the current one
/// with a meeting processed again. Both runs are taken back, the earlier
/// crash still counts, and `reprocess` after the exit is refused before it
/// saves or counts anything.
#[tokio::test(flavor = "multi_thread")]
async fn a_quit_takes_back_the_runs_of_every_pipeline_on_the_latch() {
    let world = world(false, None, AudioRetention::KeepForever);
    let latch = QuitLatch::default();
    let (retired, retired_engine) = gated_pipeline(&world, &latch);
    let (current, current_engine) = gated_pipeline(&world, &latch);
    let (resumed, resumed_asset) = processing_with_count(&world, 1);
    assert_eq!(retired.resume_unfinished().unwrap(), [resumed.id]);
    retired_engine.wait_until_entered().await;
    let again = ready_with_count(&world, 2);
    current.reprocess(again.meeting_id).unwrap();
    current_engine.wait_until_entered().await;
    assert_eq!(read_runs(&resumed_asset), "2");
    assert_eq!(read_runs(&again), "1");

    latch.set();
    assert_eq!(read_runs(&resumed_asset), "1");
    assert!(!runs_file(&again).exists());

    let late = ready_with_count(&world, 2);
    assert_eq!(
        current.reprocess(late.meeting_id),
        Err(ReprocessError::Quitting)
    );
    assert_eq!(read_runs(&late), "2", "nothing started, nothing counted");
    assert_eq!(meeting_state(&world, late.meeting_id), MeetingState::Ready);
}

/// A run that fails before any exit is the pipeline's to report: the
/// meeting is failed and the count goes, earlier crashes included.
#[tokio::test(flavor = "multi_thread")]
async fn an_ordinary_failure_clears_the_count() {
    let world = world_with(
        false,
        None,
        AudioRetention::KeepForever,
        FakeSpeechEngine {
            failure: Some("the model refused".to_owned()),
            ..FakeSpeechEngine::default()
        },
        FakeDiarizer::default(),
    );
    let (meeting, asset) = processing_with_count(&world, 2);
    assert_eq!(world.pipeline.resume_unfinished().unwrap(), [meeting.id]);
    world.pipeline.wait_until_idle().await;
    assert!(matches!(
        meeting_state(&world, meeting.id),
        MeetingState::Failed { .. }
    ));
    assert!(!runs_file(&asset).exists());
}

/// A panic the pipeline catches fails the meeting like any stage failure
/// and is reported as one: the count goes, earlier crashes included, so
/// the one run is never counted as a crash too.
#[tokio::test(flavor = "multi_thread")]
async fn a_caught_panic_clears_the_count() {
    let world = world(false, None, AudioRetention::KeepForever);
    let pipeline = ProcessingPipeline::new(with_engine(
        &world,
        Arc::new(PanickingEngine(std::collections::BTreeSet::new())),
    ));
    let (meeting, asset) = processing_with_count(&world, 2);
    assert_eq!(pipeline.resume_unfinished().unwrap(), [meeting.id]);
    pipeline.wait_until_idle().await;
    let MeetingState::Failed { reason } = meeting_state(&world, meeting.id) else {
        panic!("the panic fails the meeting");
    };
    assert!(
        reason.contains(steno_pipeline::OPERATION_PANICKED),
        "{reason}"
    );
    assert!(!runs_file(&asset).exists());
}

/// `reprocess` starts the count afresh: mid-run it holds this run only,
/// not the crashes before.
#[tokio::test(flavor = "multi_thread")]
async fn reprocess_counts_its_run_from_none() {
    let world = world(false, None, AudioRetention::KeepForever);
    let asset = ready_with_count(&world, 2);
    let (pipeline, engine) = gated_pipeline(&world, &QuitLatch::default());
    pipeline.reprocess(asset.meeting_id).unwrap();
    engine.wait_until_entered().await;
    assert_eq!(read_runs(&asset), "1");
    engine.open.notify_one();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, asset.meeting_id), MeetingState::Ready);
    assert!(!runs_file(&asset).exists());
}

/// A count that cannot be read never blocks processing: a file that is
/// not text, text that is not a number, and an empty file count as none.
#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_run_count_never_blocks_processing() {
    let world = world(false, None, AudioRetention::KeepForever);
    for corrupt in [&b"\xff not a count"[..], b"abc", b""] {
        let (meeting, asset) = processing_with_count(&world, 0);
        let runs = runs_file(&asset);
        std::fs::write(&runs, corrupt).unwrap();

        assert_eq!(
            world.pipeline.resume_unfinished().unwrap(),
            [meeting.id],
            "{corrupt:?}"
        );
        world.pipeline.wait_until_idle().await;
        assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
        assert!(!runs.exists());
    }
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
    let runs = runs_file(&given_up);
    std::fs::write(&runs, MAX_CRASHED_RUNS.to_string()).unwrap();

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

/// What the app does after `resume_unfinished` at launch: an export a
/// previous process left `pending` (it ended mid-delivery), or one that
/// failed more than a day before the launch, after it (a clock that ran
/// ahead) or was never attempted (a Swift row), is re-exported; a failure within the day, a meeting that is not
/// ready and an export that went through are left alone, and the count of
/// a meeting exported since its last failure is dropped.
#[tokio::test(flavor = "multi_thread")]
async fn exports_left_unfinished_are_re_exported_at_launch() {
    let world = world(
        false,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::KeepForever,
    );
    let pipeline = &world.pipeline;
    let ids: Vec<Uuid> = (0..8).map(|_| enqueue_call(&world, pipeline)).collect();
    pipeline.wait_until_idle().await;
    // The pipeline's clock reads the launch.
    let (launch, day) = (world.now, ExportRetries::INTERVAL);
    let failed = || DeliveryStatus::Failed("the vault was offline".to_owned());
    mark_delivery(&world, ids[0], DeliveryStatus::Pending, Some(launch));
    mark_delivery(
        &world,
        ids[1],
        failed(),
        Some(launch - day - Duration::seconds(1)),
    );
    mark_delivery(&world, ids[2], failed(), None);
    mark_delivery(&world, ids[3], failed(), Some(launch - day));
    mark_delivery(&world, ids[4], failed(), Some(launch - Duration::hours(1)));
    mark_delivery(&world, ids[5], DeliveryStatus::Pending, Some(launch));
    world
        .store
        .set_state(ids[5], MeetingState::Queued, launch)
        .unwrap();
    let exported = launch - day * 2;
    mark_delivery(&world, ids[6], DeliveryStatus::Delivered, Some(exported));
    write_retries(&world, ids[6], ExportRetries::LIMIT);
    mark_delivery(&world, ids[7], failed(), Some(launch + Duration::hours(1)));

    let retries = export_retries(&world);
    let mut owed = pipeline.redeliver_unfinished(&retries).unwrap();
    owed.sort();
    let mut expected = [&ids[..3], &ids[7..]].concat();
    expected.sort();
    assert_eq!(owed, expected);
    assert_eq!(retries.count(ids[6]), 0, "exported since its last failure");
    pipeline.wait_until_idle().await;
    for id in &expected {
        assert_eq!(delivery(&world, *id).status, DeliveryStatus::Delivered);
        assert_eq!(retries.count(*id), 0);
    }
    for id in &ids[3..5] {
        assert_eq!(delivery(&world, *id).status, failed());
    }
    assert_eq!(delivery(&world, ids[5]).status, DeliveryStatus::Pending);
    assert_eq!(delivery(&world, ids[6]).last_attempt_at, Some(exported));
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
}

/// The meeting's first delivery row.
fn delivery(world: &World, id: Uuid) -> Delivery {
    world.store.deliveries(id).unwrap().remove(0)
}

/// Stores the meeting's first delivery row as `status`, last attempted at
/// `attempt`.
fn mark_delivery(world: &World, id: Uuid, status: DeliveryStatus, attempt: Option<DateTime<Utc>>) {
    let mut row = delivery(world, id);
    row.status = status;
    row.last_attempt_at = attempt;
    world.store.save_delivery(&row).unwrap();
}

/// The world's `export-retries.json`, read as a launch reads it.
fn export_retries(world: &World) -> Arc<ExportRetries> {
    Arc::new(ExportRetries::new(retries_path(world)))
}

fn retries_path(world: &World) -> PathBuf {
    world.dir.path().join(ExportRetries::FILE_NAME)
}

/// Writes `count` launch re-exports in a row for `id` alone.
fn write_retries(world: &World, id: Uuid, count: u32) {
    std::fs::write(retries_path(world), format!("{{\"{id}\": {count}}}")).unwrap();
}

/// A destination whose every delivery fails.
fn offline(vault: &Path) -> FakeDestination {
    FakeDestination {
        deliver_failure: Some("the vault is offline".to_owned()),
        ..FakeDestination::new(vault)
    }
}

/// The world's pipeline as a launch at `at` builds it: its clock reads
/// `at`, and its one destination, `destination`, stamps `at` on the row.
fn launch_at(world: &World, at: DateTime<Utc>, destination: FakeDestination) -> ProcessingPipeline {
    launch_with(world, at, vec![destination])
}

/// [`launch_at`] with every destination in `destinations`.
fn launch_with(
    world: &World,
    at: DateTime<Utc>,
    destinations: Vec<FakeDestination>,
) -> ProcessingPipeline {
    let mut dependencies = world
        .pipeline
        .dependencies()
        .clone()
        .with_now(Arc::new(move || at));
    dependencies.dispatcher = Arc::new(FakeDispatcher {
        store: world.store.clone(),
        destinations: destinations
            .into_iter()
            .map(|destination| Arc::new(destination) as Arc<dyn Destination>)
            .collect(),
        now: at,
    });
    ProcessingPipeline::new(dependencies)
}

/// A failed export is retried by one launch a day at most, posting no
/// `OperationFailed`, and after three failed launches in a row it is left
/// until the user exports it again (`reset`); a row an exit left `pending`
/// on a meeting the launch stopped retrying is saved failed, and an export
/// that goes through starts the count from 0.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_export_is_retried_once_a_day_and_left_after_three_failed_launches() {
    let world = world(false, Some(offline), AudioRetention::KeepForever);
    let id = enqueue_call(&world, &world.pipeline);
    world.pipeline.wait_until_idle().await;
    let mut receiver = world.events.subscribe();
    assert!(matches!(
        delivery(&world, id).status,
        DeliveryStatus::Failed(_)
    ));
    let (day, hour) = (ExportRetries::INTERVAL, Duration::hours(1));
    let retries = export_retries(&world);
    let within_a_day = launch_at(&world, world.now + day - hour, offline(&world.vault));
    assert_eq!(
        within_a_day.redeliver_unfinished(&retries).unwrap(),
        Vec::<Uuid>::new()
    );

    let mut at = world.now;
    for failures in 1..=ExportRetries::LIMIT {
        at += day + hour;
        let launch = launch_at(&world, at, offline(&world.vault));
        assert_eq!(launch.redeliver_unfinished(&retries).unwrap(), [id]);
        launch.wait_until_idle().await;
        assert_eq!(delivery(&world, id).last_attempt_at, Some(at));
        assert_eq!(retries.count(id), failures);
        assert_eq!(
            launch.redeliver_unfinished(&retries).unwrap(),
            Vec::<Uuid>::new(),
            "once per launch"
        );
    }
    let relaunched = export_retries(&world);
    assert!(relaunched.stopped(id), "the count is on disk");
    at += day * 10;
    assert_eq!(
        launch_at(&world, at, offline(&world.vault))
            .redeliver_unfinished(&relaunched)
            .unwrap(),
        Vec::<Uuid>::new()
    );

    relaunched.reset(id);
    let launch = launch_at(&world, at, offline(&world.vault));
    assert_eq!(launch.redeliver_unfinished(&relaunched).unwrap(), [id]);
    launch.wait_until_idle().await;
    assert_eq!(relaunched.count(id), 1);
    assert!(
        !drain(&mut receiver)
            .iter()
            .any(|event| matches!(event, MeetingEvent::OperationFailed { .. })),
        "a launch re-export's failure is logged only"
    );

    // The last launch before the stop ended mid-export.
    write_retries(&world, id, ExportRetries::LIMIT);
    let stopped = export_retries(&world);
    mark_delivery(&world, id, DeliveryStatus::Pending, Some(at));
    at += hour;
    let launch = launch_at(&world, at, FakeDestination::new(&world.vault));
    assert_eq!(
        launch.redeliver_unfinished(&stopped).unwrap(),
        Vec::<Uuid>::new()
    );
    assert!(matches!(
        delivery(&world, id).status,
        DeliveryStatus::Failed(_)
    ));
    assert_eq!(delivery(&world, id).last_attempt_at, Some(at - hour));
    assert!(stopped.stopped(id), "the detail says it keeps failing");
    assert_eq!(launch.in_flight(), Vec::<Uuid>::new());

    stopped.reset(id);
    let launch = launch_at(&world, at + day, FakeDestination::new(&world.vault));
    assert_eq!(launch.redeliver_unfinished(&stopped).unwrap(), [id]);
    launch.wait_until_idle().await;
    assert_eq!(delivery(&world, id).status, DeliveryStatus::Delivered);
    assert_eq!(export_retries(&world).count(id), 0);
}

/// An export that goes through to one destination and fails at the other
/// is not delivered: the launches stop after three, although each wrote a
/// `Delivered` row.
#[tokio::test(flavor = "multi_thread")]
async fn launches_stop_an_export_that_fails_at_one_of_two_destinations() {
    let world = world(false, Some(offline), AudioRetention::KeepForever);
    let id = enqueue_call(&world, &world.pipeline);
    world.pipeline.wait_until_idle().await;
    let two = || {
        vec![
            offline(&world.vault),
            FakeDestination {
                id: "works".to_owned(),
                ..FakeDestination::new(&world.vault)
            },
        ]
    };
    let retries = export_retries(&world);
    let mut at = world.now;
    for launches in 1..=ExportRetries::LIMIT {
        at += ExportRetries::INTERVAL + Duration::hours(1);
        let launch = launch_with(&world, at, two());
        assert_eq!(launch.redeliver_unfinished(&retries).unwrap(), [id]);
        launch.wait_until_idle().await;
        let rows = world.store.deliveries(id).unwrap();
        assert!(matches!(rows[0].status, DeliveryStatus::Failed(_)));
        assert_eq!(rows[1].status, DeliveryStatus::Delivered);
        assert_eq!(retries.count(id), launches);
    }
    at += ExportRetries::INTERVAL + Duration::hours(1);
    let relaunched = export_retries(&world);
    assert!(relaunched.stopped(id));
    assert_eq!(
        launch_with(&world, at, two())
            .redeliver_unfinished(&relaunched)
            .unwrap(),
        Vec::<Uuid>::new()
    );
}

/// A meeting another operation holds when its turn comes is skipped at
/// launch, and once the pipeline quits nothing is started.
#[tokio::test(flavor = "multi_thread")]
async fn launch_re_exports_skip_a_meeting_in_flight_and_stop_once_quitting() {
    let world = world(
        false,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::KeepForever,
    );
    let pipeline = &world.pipeline;
    let id = enqueue_call(&world, pipeline);
    pipeline.wait_until_idle().await;
    mark_delivery(&world, id, DeliveryStatus::Pending, Some(world.now));
    let retries = export_retries(&world);

    let held = pipeline.claim_redeliver(id).unwrap();
    assert_eq!(pipeline.redeliver_unfinished(&retries).unwrap(), [id]);
    pipeline.wait_until_idle().await;
    assert_eq!(delivery(&world, id).status, DeliveryStatus::Pending);
    assert_eq!(retries.count(id), 0, "a refused claim is not counted");
    drop(held);
    pipeline.quit();
    assert_eq!(
        pipeline.redeliver_unfinished(&retries).unwrap(),
        Vec::<Uuid>::new()
    );
}

/// A dispatcher that waits in `deliver_all` until released, counting the
/// calls waiting.
#[derive(Default)]
struct HeldDispatcher {
    asked: tokio::sync::Notify,
    release: tokio::sync::Notify,
    waiting: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl DeliveryDispatcher for HeldDispatcher {
    async fn deliver_all(&self, _meeting_id: Uuid) -> Vec<Delivery> {
        use std::sync::atomic::Ordering::SeqCst;
        self.waiting.fetch_add(1, SeqCst);
        self.asked.notify_one();
        self.release.notified().await;
        self.waiting.fetch_sub(1, SeqCst);
        Vec::new()
    }
}

/// The launch re-exports one meeting at a time: the second meeting's
/// export starts only once the first one's is released.
#[tokio::test(flavor = "multi_thread")]
async fn launch_re_exports_run_one_at_a_time() {
    use std::sync::atomic::Ordering::SeqCst;
    let world = world(
        false,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::KeepForever,
    );
    let ids: Vec<Uuid> = (0..2)
        .map(|_| enqueue_call(&world, &world.pipeline))
        .collect();
    world.pipeline.wait_until_idle().await;
    for id in &ids {
        mark_delivery(&world, *id, DeliveryStatus::Pending, Some(world.now));
    }
    let held = Arc::new(HeldDispatcher::default());
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.dispatcher = held.clone();
    let pipeline = ProcessingPipeline::new(dependencies);

    let owed = pipeline
        .redeliver_unfinished(&export_retries(&world))
        .unwrap();
    assert_eq!(owed.len(), 2);
    for _ in &owed {
        held.asked.notified().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(held.waiting.load(SeqCst), 1, "one export at a time");
        assert_eq!(pipeline.in_flight().len(), 1);
        held.release.notify_one();
    }
    pipeline.wait_until_idle().await;
    assert_eq!(held.waiting.load(SeqCst), 0);
}

/// `wait_until_idle` waits for the launch's re-exports too, so the app's
/// reload and the CLI see them finished.
#[tokio::test(flavor = "multi_thread")]
async fn waiting_until_idle_waits_for_the_launch_re_exports() {
    let world = world(
        false,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::KeepForever,
    );
    let id = enqueue_call(&world, &world.pipeline);
    world.pipeline.wait_until_idle().await;
    mark_delivery(&world, id, DeliveryStatus::Pending, Some(world.now));
    let held = Arc::new(HeldDispatcher::default());
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.dispatcher = held.clone();
    let pipeline = ProcessingPipeline::new(dependencies);

    assert_eq!(
        pipeline
            .redeliver_unfinished(&export_retries(&world))
            .unwrap(),
        [id]
    );
    held.asked.notified().await;
    let waiter = tokio::spawn({
        let pipeline = pipeline.clone();
        async move { pipeline.wait_until_idle().await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!waiter.is_finished(), "returned while a re-export ran");
    held.release.notify_one();
    waiter.await.unwrap();
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
}

/// Pipelines over dependencies that share their in-flight set (the services'
/// reloads) refuse a meeting another of them holds, so a reload's new
/// pipeline never runs a second operation on a meeting the retired one is
/// still delivering.
#[tokio::test(flavor = "multi_thread")]
async fn pipelines_that_share_the_in_flight_set_refuse_each_others_meetings() {
    let world = world(false, None, AudioRetention::KeepForever);
    let id = enqueue_call(&world, &world.pipeline);
    world.pipeline.wait_until_idle().await;
    let in_flight = InFlight::default();
    let sharing = || {
        let dependencies = world.pipeline.dependencies().clone();
        ProcessingPipeline::new(dependencies.with_in_flight(in_flight.clone()))
    };
    let (retired, replacement) = (sharing(), sharing());

    let held = retired.claim_redeliver(id).unwrap();
    assert_eq!(replacement.in_flight(), vec![id]);
    let refused = replacement.claim_redeliver(id).map(drop).unwrap_err();
    assert!(
        refused.reason.contains("already being processed"),
        "{refused}"
    );
    let stored = world.store.meeting(id).unwrap().unwrap();
    let asset = world.store.asset(id).unwrap().unwrap();
    assert!(replacement.enqueue(&stored, &asset).is_err());
    assert_eq!(replacement.reprocess(id), Err(ReprocessError::Busy(id)));
    drop(held);
    assert_eq!(replacement.in_flight(), Vec::<Uuid>::new());
}

/// A processing run on a retired pipeline holds its asset in the shared
/// in-flight set, so the replacement starts no second run of the meeting,
/// even when the row reads `ready` again (a reprocess is refused as busy,
/// an enqueue too), and starts one once the retired run has ended.
#[tokio::test(flavor = "multi_thread")]
async fn a_reprocess_is_refused_while_a_retired_pipeline_runs_the_meeting() {
    let world = world(false, None, AudioRetention::KeepForever);
    let asset = ready_with_count(&world, 0);
    let id = asset.meeting_id;
    let in_flight = InFlight::default();
    let engine = Arc::new(GatedEngine::new(Gate::FirstTranscription));
    let retired = ProcessingPipeline::new(
        with_engine(&world, engine.clone()).with_in_flight(in_flight.clone()),
    );
    let replacement = ProcessingPipeline::new(
        world
            .pipeline
            .dependencies()
            .clone()
            .with_in_flight(in_flight.clone()),
    );

    retired.reprocess(id).unwrap();
    engine.wait_until_entered().await;
    world
        .store
        .set_state(id, MeetingState::Ready, world.now)
        .unwrap();
    assert_eq!(replacement.reprocess(id), Err(ReprocessError::Busy(id)));
    let meeting = world.store.meeting(id).unwrap().unwrap();
    assert!(replacement.enqueue(&meeting, &asset).is_err());

    engine.open.notify_one();
    retired.wait_until_idle().await;
    assert_eq!(meeting_state(&world, id), MeetingState::Ready);
    replacement.reprocess(id).unwrap();
    replacement.wait_until_idle().await;
    assert_eq!(meeting_state(&world, id), MeetingState::Ready);
    assert_eq!(replacement.in_flight(), Vec::<Uuid>::new());
}

/// A dispatcher that records the meeting's state and its summary's
/// template when the export is marked pending, and delivers nothing, as if
/// the app ended right after.
struct MarkedPendingOnly {
    store: Arc<Store>,
    seen: Mutex<Vec<(MeetingState, Option<String>)>>,
}

#[async_trait]
impl DeliveryDispatcher for MarkedPendingOnly {
    async fn deliver_all(&self, _meeting_id: Uuid) -> Vec<Delivery> {
        Vec::new()
    }

    fn mark_pending(&self, meeting_id: Uuid) {
        let meeting = self.store.meeting(meeting_id).unwrap().unwrap();
        self.seen.lock().unwrap().push((
            meeting.state,
            meeting.summary.map(|summary| summary.template_id),
        ));
    }
}

/// `persist` marks the export pending before it marks the meeting ready,
/// and a summary re-run once the new summary is saved, so an exit before
/// the delivery leaves rows the next launch re-exports.
#[tokio::test(flavor = "multi_thread")]
async fn the_export_is_marked_pending_before_the_meeting_is_ready() {
    let world = world(true, None, AudioRetention::KeepForever);
    let dispatcher = Arc::new(MarkedPendingOnly {
        store: world.store.clone(),
        seen: Mutex::new(Vec::new()),
    });
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.dispatcher = dispatcher.clone();
    let pipeline = ProcessingPipeline::new(dependencies);
    let id = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, id), MeetingState::Ready);
    assert_eq!(
        *dispatcher.seen.lock().unwrap(),
        [(MeetingState::Processing, Some("default".to_owned()))]
    );

    pipeline.rerun_summary(id, "daily-standup").await.unwrap();
    assert_eq!(
        dispatcher.seen.lock().unwrap().last(),
        Some(&(MeetingState::Ready, Some("daily-standup".to_owned())))
    );
}

/// A dispatcher that saves the row `pending`, stamped now, as the
/// coordinator does before `destination.deliver`, and then panics
/// (`panics`) or returns as if the process had been killed there.
struct EndsMidExport {
    store: Arc<Store>,
    now: DateTime<Utc>,
    panics: bool,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl DeliveryDispatcher for EndsMidExport {
    async fn deliver_all(&self, meeting_id: Uuid) -> Vec<Delivery> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut row = self.store.deliveries(meeting_id).unwrap().remove(0);
        row.status = DeliveryStatus::Pending;
        row.last_attempt_at = Some(self.now);
        self.store.save_delivery(&row).unwrap();
        assert!(!self.panics, "the destination panicked mid-export");
        Vec::new()
    }
}

/// An export that ends with its row `pending`, by a panic in the
/// destination or an exit mid-export, counts as a launch re-export that
/// did not deliver every row: the launches stop after three, the row is
/// failed after every launch, the detail says the export keeps failing,
/// and no `OperationFailed` is posted.
async fn launches_stop_an_export_that_ends_mid_delivery(panics: bool) {
    let world = world(false, Some(offline), AudioRetention::KeepForever);
    let id = enqueue_call(&world, &world.pipeline);
    world.pipeline.wait_until_idle().await;
    assert!(matches!(
        delivery(&world, id).status,
        DeliveryStatus::Failed(_)
    ));
    let mut receiver = world.events.subscribe();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut at = world.now;
    let mut seen = Vec::new();
    for _ in 0..(ExportRetries::LIMIT + 3) {
        at += ExportRetries::INTERVAL + Duration::hours(1);
        let retries = export_retries(&world);
        let mut dependencies = world
            .pipeline
            .dependencies()
            .clone()
            .with_now(Arc::new(move || at));
        dependencies.dispatcher = Arc::new(EndsMidExport {
            store: world.store.clone(),
            now: at,
            panics,
            calls: calls.clone(),
        });
        let launch = ProcessingPipeline::new(dependencies);
        let owed = launch.redeliver_unfinished(&retries).unwrap();
        launch.wait_until_idle().await;
        let row = delivery(&world, id);
        seen.push(format!(
            "owed={} count={} stopped={} status={:?}",
            owed.len(),
            export_retries(&world).count(id),
            export_retries(&world).stopped(id),
            row.status
        ));
        assert!(
            matches!(row.status, DeliveryStatus::Failed(_)),
            "a launch left the row unfailed: {seen:#?}"
        );
    }
    assert!(
        calls.load(std::sync::atomic::Ordering::SeqCst) <= ExportRetries::LIMIT as usize,
        "launches kept re-exporting: {seen:#?}"
    );
    assert!(
        export_retries(&world).stopped(id),
        "the detail never says it keeps failing: {seen:#?}"
    );
    assert!(
        !drain(&mut receiver)
            .iter()
            .any(|event| matches!(event, MeetingEvent::OperationFailed { .. })),
        "a launch re-export's failure is logged only"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn launches_stop_an_export_whose_destination_panics() {
    launches_stop_an_export_that_ends_mid_delivery(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn launches_stop_an_export_the_process_was_killed_in() {
    launches_stop_an_export_that_ends_mid_delivery(false).await;
}

/// A dispatcher that holds `held`'s export until released and delivers
/// every other meeting at once, recording each `deliver_all` call.
struct HoldsOne {
    store: Arc<Store>,
    held: Uuid,
    asked: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl DeliveryDispatcher for HoldsOne {
    async fn deliver_all(&self, meeting_id: Uuid) -> Vec<Delivery> {
        self.calls.lock().unwrap().push(meeting_id);
        if meeting_id == self.held {
            self.asked.notify_one();
            self.release.notified().await;
        }
        let mut rows = self.store.deliveries(meeting_id).unwrap();
        for row in &mut rows {
            row.status = DeliveryStatus::Delivered;
            self.store.save_delivery(row).unwrap();
        }
        rows
    }
}

/// A meeting the user exported again while the launch re-exported an
/// earlier one is not exported a second time when its turn comes, and is
/// not counted.
#[tokio::test(flavor = "multi_thread")]
async fn a_launch_skips_a_meeting_the_user_exported_meanwhile() {
    let world = world(
        false,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::KeepForever,
    );
    for _ in 0..2 {
        let id = enqueue_call(&world, &world.pipeline);
        world.pipeline.wait_until_idle().await;
        mark_delivery(&world, id, DeliveryStatus::Pending, Some(world.now));
    }
    // In the launch's order.
    let ids = world
        .store
        .meetings_with_unfinished_deliveries(world.now, ExportRetries::INTERVAL)
        .unwrap();
    assert_eq!(ids.len(), 2);
    let dispatcher = Arc::new(HoldsOne {
        store: world.store.clone(),
        held: ids[0],
        asked: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: Mutex::new(Vec::new()),
    });
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.dispatcher = dispatcher.clone();
    let pipeline = ProcessingPipeline::new(dependencies);
    let retries = export_retries(&world);

    assert_eq!(pipeline.redeliver_unfinished(&retries).unwrap(), ids);
    dispatcher.asked.notified().await;
    pipeline.claim_redeliver(ids[1]).unwrap().await.unwrap();
    assert_eq!(delivery(&world, ids[1]).status, DeliveryStatus::Delivered);
    dispatcher.release.notify_one();
    pipeline.wait_until_idle().await;

    assert_eq!(*dispatcher.calls.lock().unwrap(), ids);
    assert_eq!(retries.count(ids[1]), 0);
}

/// A dispatcher that returns without writing any row, as a re-export that
/// fails before `deliver_all` stamps the row (a settings, asset or
/// stage-rate read error) leaves it.
struct WritesNothing {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl DeliveryDispatcher for WritesNothing {
    async fn deliver_all(&self, _meeting_id: Uuid) -> Vec<Delivery> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Vec::new()
    }
}

/// A `pending` row never attempted (as `mark_pending` writes it) that a
/// launch re-export leaves unstamped is saved failed with the launch's
/// time, so it waits a day like any failure instead of being retried at
/// every launch.
#[tokio::test(flavor = "multi_thread")]
async fn a_row_failed_without_an_attempt_waits_a_day() {
    let world = world(
        false,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::KeepForever,
    );
    let id = enqueue_call(&world, &world.pipeline);
    world.pipeline.wait_until_idle().await;
    mark_delivery(&world, id, DeliveryStatus::Pending, None);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for hours in 0..3 {
        let at = world.now + Duration::hours(hours);
        let mut dependencies = world
            .pipeline
            .dependencies()
            .clone()
            .with_now(Arc::new(move || at));
        dependencies.dispatcher = Arc::new(WritesNothing {
            calls: calls.clone(),
        });
        let launch = ProcessingPipeline::new(dependencies);
        launch
            .redeliver_unfinished(&export_retries(&world))
            .unwrap();
        launch.wait_until_idle().await;
        let row = delivery(&world, id);
        assert!(matches!(row.status, DeliveryStatus::Failed(_)));
        assert_eq!(row.last_attempt_at, Some(world.now));
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(export_retries(&world).count(id), 1);
}

/// The stored asset's retention stamp.
fn expires_at(world: &World, asset: &AudioAsset) -> Option<DateTime<Utc>> {
    world
        .store
        .asset_by_id(asset.id)
        .unwrap()
        .unwrap()
        .expires_at
}

/// A meeting whose diarizer failed is ready with one unknown room speaker,
/// but its speakers can still be found from the recording: the automatic
/// retention keeps it unstamped, even under "delete after processing",
/// until a run with speakers succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn a_diarizer_fallback_keeps_the_recording_until_a_run_finds_the_speakers() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    let pipeline = with_failing_diarizer(&world, "no model");
    let meeting = call_meeting(world.now);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert_eq!(
        expires_at(&world, &asset),
        None,
        "the speakers still need the recording"
    );

    // A run whose diarizer works stamps it as the rule says.
    world.pipeline.reprocess(meeting.id).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert_eq!(expires_at(&world, &asset), Some(world.now));
}

/// The user's own rule is applied as chosen, also while the speakers are
/// incomplete.
#[tokio::test(flavor = "multi_thread")]
async fn a_rule_the_user_applies_stamps_a_meeting_whose_diarizer_failed() {
    let world = world(false, None, AudioRetention::KeepForever);
    let pipeline = with_failing_diarizer(&world, "no model");
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    pipeline
        .apply_retention(meeting.id, AudioRetention::DeleteAfterProcessing)
        .await
        .unwrap();
    assert_eq!(expires_at(&world, &asset), Some(world.now));
}

/// Runs a call of `duration` seconds whose every lane comes out with no
/// transcript, under "delete after processing", over `diarizer`, and
/// tells the meeting and whether its recording was kept unstamped. No
/// segment names the room, so no room row is stored even when the
/// diarizer fails, and the detail reads kept-incomplete exactly when the
/// recording is kept.
async fn silent_call(duration: f64, diarizer: FakeDiarizer) -> (Uuid, bool) {
    let world = world_with(
        false,
        None,
        AudioRetention::DeleteAfterProcessing,
        FakeSpeechEngine {
            silent_below_peak: Some(f32::MAX),
            ..FakeSpeechEngine::default()
        },
        diarizer,
    );
    let mut meeting = call_meeting(world.now);
    // An id of its own picks this run's log lines out.
    meeting.id = Uuid::new_v4();
    meeting.duration = duration;
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert_eq!(world.store.segments(meeting.id).unwrap(), Vec::new());
    assert!(
        !has_room_row(&world, meeting.id),
        "{duration} s: a room row with no segment"
    );
    let kept = expires_at(&world, &asset).is_none();
    assert_eq!(
        detail_reads_kept_incomplete(&world, meeting.id),
        kept,
        "{duration} s"
    );
    (meeting.id, kept)
}

/// [`silent_call`] with a working diarizer.
async fn silent_call_is_kept(duration: f64) -> bool {
    silent_call(duration, FakeDiarizer::default()).await.1
}

/// Whether the diarizer fallback's room row is stored for the meeting.
fn has_room_row(world: &World, meeting_id: Uuid) -> bool {
    let room = steno_core::room_speaker_id(meeting_id);
    world
        .store
        .speakers(meeting_id)
        .unwrap()
        .iter()
        .any(|speaker| speaker.id == room)
}

/// A first run whose diarizer fails over a call with both lanes silent
/// stores no room row, since no segment names it: the empty-lane arm
/// alone decides, so 30 s is stamped and 31 s is kept.
#[tokio::test(flavor = "multi_thread")]
async fn a_first_run_fallback_over_an_empty_call_stores_no_room_row() {
    let log = steno_pipeline::fixtures::CapturedLog::warnings();
    for (duration, kept) in [(30.0, false), (31.0, true)] {
        let (meeting, was_kept) = silent_call(
            duration,
            FakeDiarizer {
                failure: Some("no model".to_owned()),
                ..FakeDiarizer::default()
            },
        )
        .await;
        assert_eq!(was_kept, kept, "{duration} s");
        // The diarizer ran and failed, so the fallback was taken.
        let text = log.text();
        assert!(
            text.lines()
                .any(|line| line.contains(&meeting.to_string())
                    && line.contains("stage=\"diarize\"")),
            "{duration} s: no diarize failure logged: {text}"
        );
    }
}

/// A recording longer than half a minute that came out with no transcript
/// at all most likely failed to transcribe: the automatic retention keeps
/// it; a short one is stamped as usual.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_recording_with_no_transcript_keeps_its_recording() {
    for (duration, kept) in [(31.0, true), (6.0, false)] {
        assert_eq!(silent_call_is_kept(duration).await, kept, "{duration} s");
    }
}

/// Records, for every commit from now on, whether the asset holds a stamp
/// then and the connection's `synchronous` level (2 is FULL).
fn stamp_commits(world: &World, asset: &AudioAsset) -> Arc<Mutex<Vec<(bool, i64)>>> {
    let commits: Arc<Mutex<Vec<(bool, i64)>>> = Arc::default();
    let seen = commits.clone();
    let asset_id = steno_core::store::convert::DbUuid(asset.id);
    world.store.probe_commits(move |connection| {
        let stamp: Option<String> = connection
            .query_row(
                "SELECT expiresAt FROM audioAsset WHERE id = ?1",
                [&asset_id],
                |row| row.get(0),
            )
            .unwrap_or(None);
        let level = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        seen.lock().unwrap().push((stamp.is_some(), level));
    });
    commits
}

/// The level of the first recorded commit whose stamp is `stamped`.
fn first_level(commits: &Mutex<Vec<(bool, i64)>>, stamped: bool) -> Option<i64> {
    commits
        .lock()
        .unwrap()
        .iter()
        .find(|(stamp, _)| *stamp == stamped)
        .map(|(_, level)| *level)
}

/// The retention stamp, which lets the sweep delete the recording, commits
/// under `synchronous = FULL`: that commit also syncs the transcript and
/// summary written before it, so a power loss cannot leave the recording
/// gone and its results rolled back. That the commit then survives a power
/// loss is SQLite's and cannot be tested.
#[tokio::test(flavor = "multi_thread")]
async fn the_retention_stamp_commits_durably() {
    let world = world(true, None, AudioRetention::DeleteAfterProcessing);
    let meeting = call_meeting(world.now);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    let commits = stamp_commits(&world, &asset);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(expires_at(&world, &asset), Some(world.now));
    assert_eq!(
        first_level(&commits, true),
        Some(2),
        "the first commit that holds the stamp ran under FULL"
    );
}

/// The keep that clears a stamp is what stops the sweep, so it commits as
/// durably as the stamp: a power loss never brings the stamp back for the
/// sweep to delete what the user kept.
#[tokio::test(flavor = "multi_thread")]
async fn the_keep_that_clears_a_stamp_commits_durably() {
    let world = world(false, None, AudioRetention::KeepDays(30));
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepDays(30));
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    assert!(expires_at(&world, &asset).is_some());

    let commits = stamp_commits(&world, &asset);
    world
        .pipeline
        .apply_retention(meeting.id, AudioRetention::KeepForever)
        .await
        .unwrap();
    assert_eq!(expires_at(&world, &asset), None);
    assert_eq!(
        first_level(&commits, false),
        Some(2),
        "the first commit without the stamp ran under FULL"
    );
}

/// Settings' Keep forever for every recording clears every stamp, and
/// the clear of each recording is as durable as the first, in one write
/// ([`Store::keep_forever`]).
#[tokio::test(flavor = "multi_thread")]
async fn keep_forever_for_every_recording_commits_durably() {
    let world = world(false, None, AudioRetention::KeepDays(30));
    let first = call_meeting(world.now);
    let first_asset = call_asset(&world.audio, first.id, AudioRetention::KeepDays(30));
    let mut second = call_meeting(world.now);
    second.id = Uuid::new_v4();
    second.calendar_event_id = None;
    let second_asset = call_asset(&world.audio, second.id, AudioRetention::KeepDays(30));
    for (meeting, asset) in [(&first, &first_asset), (&second, &second_asset)] {
        world.pipeline.enqueue(meeting, asset).unwrap();
        world.pipeline.wait_until_idle().await;
        assert!(expires_at(&world, asset).is_some());
    }
    // The ids sort the assets either way round; probe the one
    // `keep_forever` writes last.
    let last = if first_asset.id > second_asset.id {
        &first_asset
    } else {
        &second_asset
    };
    let commits = stamp_commits(&world, last);
    assert_eq!(
        RetentionSweep::new(world.store.clone()).keep_all().unwrap(),
        2
    );
    for asset in [&first_asset, &second_asset] {
        let stored = world.store.asset_by_id(asset.id).unwrap().unwrap();
        assert_eq!(stored.retention, AudioRetention::KeepForever);
        assert_eq!(stored.expires_at, None);
    }
    assert_eq!(first_level(&commits, false), Some(2));
}

/// A meeting whose diarizer failed, run under "delete after processing"
/// by the pipeline returned with it, its one export failed; `summarizer`
/// as in [`world`].
async fn a_fallback_whose_export_failed(
    summarizer: bool,
) -> (World, ProcessingPipeline, Meeting, AudioAsset) {
    let world = world(
        summarizer,
        Some(|vault| FakeDestination {
            fail_until: 1,
            ..FakeDestination::new(vault)
        }),
        AudioRetention::DeleteAfterProcessing,
    );
    let pipeline = with_failing_diarizer(&world, "no model");
    let meeting = call_meeting(world.now);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    (world, pipeline, meeting, asset)
}

/// A fallback meeting whose export failed is exported again: once every
/// delivery succeeded, the re-export still keeps the recording unstamped,
/// and the sweep deletes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_reexport_of_a_diarizer_fallback_keeps_the_recording() {
    let (world, pipeline, meeting, asset) = a_fallback_whose_export_failed(false).await;
    assert!(matches!(
        world.store.deliveries(meeting.id).unwrap()[0].status,
        DeliveryStatus::Failed(_)
    ));
    pipeline.redeliver(meeting.id).await.unwrap();
    assert_eq!(
        world.store.deliveries(meeting.id).unwrap()[0].status,
        DeliveryStatus::Delivered
    );
    assert_eq!(
        expires_at(&world, &asset),
        None,
        "the speakers still need it"
    );
    let sweep = RetentionSweep::new(world.store.clone());
    assert_eq!(
        sweep.run(world.now + Duration::days(3650)).unwrap(),
        Vec::<PathBuf>::new()
    );
    assert!(file_url_path(&asset.url).unwrap().exists());
}

/// The launch's re-export of the same meeting keeps it unstamped too.
#[tokio::test(flavor = "multi_thread")]
async fn a_launch_reexport_of_a_diarizer_fallback_keeps_the_recording() {
    let (world, _pipeline, meeting, asset) = a_fallback_whose_export_failed(false).await;
    let at = world.now + Duration::days(2);
    let launch = launch_at(&world, at, FakeDestination::new(&world.vault));
    let retries = export_retries(&world);
    assert_eq!(launch.redeliver_unfinished(&retries).unwrap(), [meeting.id]);
    launch.wait_until_idle().await;
    assert_eq!(
        world.store.deliveries(meeting.id).unwrap()[0].status,
        DeliveryStatus::Delivered
    );
    assert_eq!(expires_at(&world, &asset), None);
}

/// A summary re-run delivers again and then asks for the stamp: a
/// fallback meeting stays unstamped.
#[tokio::test(flavor = "multi_thread")]
async fn a_summary_rerun_of_a_diarizer_fallback_keeps_the_recording() {
    let (world, pipeline, meeting, asset) = a_fallback_whose_export_failed(true).await;
    pipeline.rerun_summary(meeting.id, "default").await.unwrap();
    assert_eq!(
        world.store.deliveries(meeting.id).unwrap()[0].status,
        DeliveryStatus::Delivered
    );
    assert_eq!(expires_at(&world, &asset), None);
}

/// A run that fails after `persist` marked the meeting ready is delivered
/// and asks for the deferred stamp: a fallback meeting stays unstamped.
#[tokio::test(flavor = "multi_thread")]
async fn a_failure_after_the_ready_write_keeps_a_fallback_recording() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    let meeting = call_meeting(world.now);
    let clock = PanickingOnceReady {
        store: world.store.clone(),
        meeting_id: meeting.id,
        ticks: TickingClock(Mutex::new(0.0)),
        panicked: std::sync::atomic::AtomicBool::new(false),
    };
    let mut dependencies = world
        .pipeline
        .dependencies()
        .clone()
        .with_clock(Arc::new(clock));
    dependencies.diarizer = failing_diarizer("no model");
    let pipeline = ProcessingPipeline::new(dependencies);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert_eq!(expires_at(&world, &asset), None);
}

/// A re-run whose diarizer fails on a meeting with diarized speakers keeps
/// them, and every new segment falls inside a stored span (the same
/// audio): those speakers are as complete as the earlier working
/// diarization, so no room row is stored, every segment keeps a speaker,
/// and the run is stamped as the rule says.
#[tokio::test(flavor = "multi_thread")]
async fn a_rerun_whose_diarizer_fails_stamps_when_the_stored_speakers_cover_every_segment() {
    let world = world(
        false,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::DeleteAfterProcessing,
    );
    let meeting = call_meeting(world.now);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(expires_at(&world, &asset), Some(world.now));
    let first = world.store.export(meeting.id).unwrap();

    let pipeline = with_failing_diarizer(&world, "no model");
    pipeline.reprocess(meeting.id).unwrap();
    pipeline.wait_until_idle().await;
    let again = world.store.export(meeting.id).unwrap();
    assert_eq!(again.meeting.state, MeetingState::Ready);
    assert_eq!(labels(&again), labels(&first), "no room row");
    assert!(
        again
            .speaker(steno_core::room_speaker_id(meeting.id))
            .is_none()
    );
    assert!(
        again
            .segments
            .iter()
            .all(|segment| segment.speaker_id.is_some())
    );
    assert!(!detail_reads_kept_incomplete(&world, meeting.id));
    assert_eq!(
        expires_at(&world, &asset),
        Some(world.now),
        "normal stamping"
    );
}

/// The bound itself: an empty transcript over exactly 30 s is stamped,
/// just over it is kept.
#[tokio::test(flavor = "multi_thread")]
async fn the_empty_lane_bound_is_exclusive() {
    for (duration, kept) in [(30.0, false), (30.5, true)] {
        assert_eq!(silent_call_is_kept(duration).await, kept, "{duration} s");
    }
}

/// A call whose mic lane came out empty while the tap carried the other
/// side: past the bound the recording is kept (the mic may have failed to
/// transcribe, or stayed muted); within it, stamped.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_with_an_empty_mic_lane_keeps_its_recording_past_the_bound() {
    for (duration, kept) in [(31.0, true), (30.0, false)] {
        let world = world_with(
            false,
            None,
            AudioRetention::DeleteAfterProcessing,
            engine_deaf_to_silence(),
            FakeDiarizer::default(),
        );
        let mut meeting = call_meeting(world.now);
        meeting.duration = duration;
        let asset = call_asset(
            &world.audio,
            meeting.id,
            AudioRetention::DeleteAfterProcessing,
        );
        let layout = RecordingLayout::from_asset(&asset).unwrap();
        steno_pipeline::fixtures::write_wav(&layout.sidecar(AudioLane::Mic), &vec![0; 6 * 16_000])
            .unwrap();
        world.pipeline.enqueue(&meeting, &asset).unwrap();
        world.pipeline.wait_until_idle().await;
        assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
        let lanes: BTreeSet<AudioLane> = world
            .store
            .segments(meeting.id)
            .unwrap()
            .iter()
            .map(|segment| segment.lane)
            .collect();
        assert_eq!(lanes, BTreeSet::from([AudioLane::System]));
        assert_eq!(expires_at(&world, &asset).is_none(), kept, "{duration} s");
    }
}

/// A phone recording whose metadata announced no length, over `samples`.
fn phone_recording(world: &World, samples: &[i16]) -> (Meeting, AudioAsset) {
    let mut meeting = call_meeting(world.now);
    meeting.source = MeetingSource::Phone;
    meeting.duration = 0.0;
    let mut asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    asset.lanes = vec![AudioLane::Mixed];
    asset.sidecars_16k.clear();
    steno_pipeline::fixtures::write_wav(&file_url_path(&asset.url).unwrap(), samples).unwrap();
    (meeting, asset)
}

/// A phone meeting that announced no duration gets the length the run
/// decoded: with a transcript it is stamped as usual, and a long one with
/// no transcript at all keeps its recording.
#[tokio::test(flavor = "multi_thread")]
async fn a_phone_meeting_without_a_duration_gets_the_decoded_length() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    let (meeting, asset) = phone_recording(
        &world,
        &steno_pipeline::fixtures::conversation(6.0, &[AudioLane::Mixed]),
    );
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let stored = world.store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready);
    assert_eq!(stored.duration, 6.0);
    assert_ne!(world.store.segments(meeting.id).unwrap(), Vec::new());
    assert_eq!(expires_at(&world, &asset), Some(world.now));

    let world = world_with(
        false,
        None,
        AudioRetention::DeleteAfterProcessing,
        engine_deaf_to_silence(),
        FakeDiarizer::default(),
    );
    let (meeting, asset) = phone_recording(&world, &vec![0; 31 * 16_000]);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let stored = world.store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready);
    assert_eq!(stored.duration, 31.0);
    assert_eq!(world.store.segments(meeting.id).unwrap(), Vec::new());
    assert_eq!(expires_at(&world, &asset), None);
}

/// The one-lane variant: an empty phone recording whose diarizer fails
/// stores no room row, and its decoded length decides, so 30 s is stamped
/// and 31 s is kept.
#[tokio::test(flavor = "multi_thread")]
async fn a_first_run_fallback_over_an_empty_phone_recording_stores_no_room_row() {
    for (seconds, kept) in [(30, false), (31, true)] {
        let world = world_with(
            false,
            None,
            AudioRetention::DeleteAfterProcessing,
            engine_deaf_to_silence(),
            FakeDiarizer {
                failure: Some("no model".to_owned()),
                ..FakeDiarizer::default()
            },
        );
        let (meeting, asset) = phone_recording(&world, &vec![0; seconds * 16_000]);
        world.pipeline.enqueue(&meeting, &asset).unwrap();
        world.pipeline.wait_until_idle().await;
        assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
        assert_eq!(world.store.segments(meeting.id).unwrap(), Vec::new());
        assert!(
            !has_room_row(&world, meeting.id),
            "{seconds} s: a room row with no segment"
        );
        assert_eq!(expires_at(&world, &asset).is_none(), kept, "{seconds} s");
    }
}

/// Rewrites the system sidecar with `seconds` of the conversation: the
/// fake engine then yields one-second tap segments up to `seconds`, so a
/// re-run past the first run's six seconds has speech that no stored
/// speaker's span covers (`cluster_covering` returns `None` for it).
fn extend_the_tap(asset: &AudioAsset, seconds: f64) {
    let layout = RecordingLayout::from_asset(asset).unwrap();
    steno_pipeline::fixtures::write_wav(
        &layout.sidecar(AudioLane::System),
        &steno_pipeline::fixtures::conversation(seconds, &[AudioLane::System]),
    )
    .unwrap();
}

/// What the meeting detail's recording line reads for a ready meeting
/// (`MeetingDetailViewModel::recording_status`, steno-host): `true` for
/// `KeptIncomplete`. steno-host is not a dev-dependency of the pipeline,
/// so this mirrors its ladder over the stored rows.
fn detail_reads_kept_incomplete(world: &World, meeting_id: Uuid) -> bool {
    let export = world.store.export(meeting_id).unwrap();
    let asset = export.audio.clone().expect("an asset");
    export.meeting.state == MeetingState::Ready
        && file_url_path(&asset.url).unwrap().exists()
        && asset.retention != AudioRetention::KeepForever
        && asset.expires_at.is_none()
        && world
            .store
            .deliveries(meeting_id)
            .unwrap()
            .iter()
            .all(|row| row.status == DeliveryStatus::Delivered)
        && steno_core::results_need_the_audio(
            &export.meeting,
            &export.speakers,
            &export.segments,
            &asset,
        )
}

/// A call processed with a working diarizer under "delete after
/// processing" and one working destination (stamped at `world.now`), its
/// tap then extended to eight seconds.
async fn a_diarized_call_with_new_speech(
    summarizer: bool,
) -> (World, Meeting, AudioAsset, MeetingExport) {
    let world = world(
        summarizer,
        Some(|vault| FakeDestination::new(vault)),
        AudioRetention::DeleteAfterProcessing,
    );
    let meeting = call_meeting(world.now);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(expires_at(&world, &asset), Some(world.now));
    let first = world.store.export(meeting.id).unwrap();
    assert_eq!(labels(&first), ["Me", "Speaker 1", "Speaker 2"]);
    extend_the_tap(&asset, 8.0);
    (world, meeting, asset, first)
}

/// A re-run whose diarizer fails keeps
/// the stored speakers, and the tap's new speech (6..8 s), which no stored
/// span covers, goes to the room row: the stored mark. The re-run, then a
/// re-export, a summary re-run and a launch re-export all leave the
/// recording unstamped, the detail reads keptIncomplete at every step, and
/// the sweep deletes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_fell_back_rerun_with_new_speech_keeps_the_recording_through_every_later_stamp() {
    let (world, meeting, asset, first) = a_diarized_call_with_new_speech(true).await;
    let persons = world.store.persons().unwrap();
    let pipeline = with_failing_diarizer(&world, "no model");
    pipeline.reprocess(meeting.id).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    assert_eq!(expires_at(&world, &asset), None, "after the re-run");

    pipeline.redeliver(meeting.id).await.unwrap();
    assert_eq!(expires_at(&world, &asset), None, "after the re-export");
    assert!(
        detail_reads_kept_incomplete(&world, meeting.id),
        "after the re-export"
    );

    pipeline.rerun_summary(meeting.id, "default").await.unwrap();
    assert_eq!(expires_at(&world, &asset), None, "after the summary re-run");
    assert!(
        detail_reads_kept_incomplete(&world, meeting.id),
        "after the summary re-run"
    );

    mark_delivery(&world, meeting.id, DeliveryStatus::Pending, None);
    let launch = launch_at(
        &world,
        world.now + Duration::days(2),
        FakeDestination::new(&world.vault),
    );
    assert_eq!(
        launch
            .redeliver_unfinished(&export_retries(&world))
            .unwrap(),
        [meeting.id]
    );
    launch.wait_until_idle().await;
    assert_eq!(
        world.store.deliveries(meeting.id).unwrap()[0].status,
        DeliveryStatus::Delivered
    );
    assert_eq!(
        expires_at(&world, &asset),
        None,
        "after the launch re-export"
    );
    assert!(
        detail_reads_kept_incomplete(&world, meeting.id),
        "after the launch re-export"
    );
    let sweep = RetentionSweep::new(world.store.clone());
    assert_eq!(
        sweep.run(world.now + Duration::days(3650)).unwrap(),
        Vec::<PathBuf>::new()
    );
    assert!(file_url_path(&asset.url).unwrap().exists());

    // The mark is the room row, and it holds exactly the new speech.
    let again = world.store.export(meeting.id).unwrap();
    let room = steno_core::room_speaker_id(meeting.id);
    let row = again.speaker(room).expect("the room row is stored");
    assert_eq!(row.assignment, steno_core::SpeakerAssignment::Unknown);
    assert_eq!(row.embedding, None);
    for segment in &again.segments {
        if segment.lane == AudioLane::System && segment.start >= 6.0 {
            assert_eq!(segment.speaker_id, Some(room), "{segment:?}");
        } else {
            let before = first
                .segments
                .iter()
                .find(|before| {
                    before.lane == segment.lane && (before.start - segment.start).abs() < 1e-9
                })
                .expect("the first run had this span");
            assert_eq!(segment.speaker_id, before.speaker_id, "{segment:?}");
        }
    }
    assert_eq!(
        world.store.persons().unwrap(),
        persons,
        "no voice was touched"
    );
}

/// The room row beside stored speakers takes the next free "Speaker N",
/// never a stored label: in the export (its display name, every speaker
/// once in the participants), in the delivered note and in the labels the
/// summary pass maps back to ids (`steno_llm::labels::SpeakerLabels`
/// keeps one id per lower-cased label).
#[tokio::test(flavor = "multi_thread")]
async fn the_room_row_beside_stored_speakers_takes_the_next_free_label() {
    let (world, meeting, _asset, _first) = a_diarized_call_with_new_speech(false).await;
    let summarizer = Arc::new(RecordingSummarizer::default());
    let mut dependencies = world.pipeline.dependencies().clone().with_llm(
        Some(Arc::new(PassthroughCleaner::default())),
        Some(summarizer.clone()),
    );
    dependencies.diarizer = failing_diarizer("no model");
    let pipeline = ProcessingPipeline::new(dependencies);
    pipeline.reprocess(meeting.id).unwrap();
    pipeline.wait_until_idle().await;

    let again = world.store.export(meeting.id).unwrap();
    let room = steno_core::room_speaker_id(meeting.id);
    let row = again.speaker(room).expect("the room row is stored");
    assert_eq!(row.cluster_label, "Speaker 3");
    assert_eq!(again.display_name_for_speaker(room), "Speaker 3");
    let names: BTreeSet<String> = again
        .speakers
        .iter()
        .map(|speaker| again.display_name_for_speaker(speaker.id))
        .collect();
    assert_eq!(
        names.len(),
        again.speakers.len(),
        "no two speakers share a name"
    );

    let folder = world.store.deliveries(meeting.id).unwrap()[0]
        .receipt
        .clone()
        .unwrap()
        .folder;
    let note = std::fs::read_to_string(world.vault.join(folder).join("meeting.json")).unwrap();
    assert!(
        note.contains("Speaker 3"),
        "the delivered note carries the room's label"
    );

    let input = summarizer
        .0
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("summarized");
    let mut by_label: BTreeMap<String, Uuid> = BTreeMap::new();
    for speaker in &input.speakers {
        assert!(
            by_label
                .insert(speaker.cluster_label.to_lowercase(), speaker.id)
                .is_none(),
            "label {} twice in the summary's label map",
            speaker.cluster_label
        );
    }
    assert_eq!(by_label.get("speaker 3"), Some(&room));
    assert_eq!(
        by_label.get("speaker 1"),
        Some(&speaker(&again.speakers, "Speaker 1").id)
    );
}

/// A label merged away is not reused: the room row takes the label after
/// the highest stored one, so a kept summary that still names the merged
/// voice never points at the room's speech.
#[tokio::test(flavor = "multi_thread")]
async fn the_room_label_skips_every_stored_label() {
    let (world, meeting, _asset, first) = a_diarized_call_with_new_speech(false).await;
    let anna = sample_data::person(0, "Anna");
    world
        .store
        .confirm_speaker(speaker(&first.speakers, "Speaker 2").id, &anna)
        .unwrap();
    // Speaker 1 to the same person merges it into Speaker 2.
    world
        .store
        .confirm_speaker(speaker(&first.speakers, "Speaker 1").id, &anna)
        .unwrap();
    let merged = world.store.export(meeting.id).unwrap();
    assert_eq!(labels(&merged), ["Me", "Speaker 2"]);

    let pipeline = with_failing_diarizer(&world, "no model");
    pipeline.reprocess(meeting.id).unwrap();
    pipeline.wait_until_idle().await;
    let again = world.store.export(meeting.id).unwrap();
    let row = again
        .speaker(steno_core::room_speaker_id(meeting.id))
        .expect("the room row is stored");
    assert_eq!(
        row.cluster_label, "Speaker 3",
        "after the highest stored label"
    );
    let stored: BTreeSet<String> = labels(&merged)
        .iter()
        .map(|label| label.to_lowercase())
        .collect();
    assert!(
        !stored.contains(&row.cluster_label.to_lowercase()),
        "{}",
        row.cluster_label
    );
}

/// A confirmed speaker and a renamed one (confirmed to a person typed in
/// as new) keep every segment they cover, by midpoint or by overlap: the
/// re-run's 1.4 s segments put [5.6, 7.0] over the end of the renamed
/// speaker's stored [5, 6], so the overlap fallback gives it that segment,
/// and only [7, 8], which overlaps nothing stored, goes to the room row.
#[tokio::test(flavor = "multi_thread")]
async fn confirmed_and_renamed_speakers_keep_their_segments_on_a_fell_back_rerun() {
    let (world, meeting, _asset, first) = a_diarized_call_with_new_speech(false).await;
    let anna = sample_data::person(0, "Anna");
    let one = speaker(&first.speakers, "Speaker 1").id;
    let two = speaker(&first.speakers, "Speaker 2").id;
    world.store.confirm_speaker(one, &anna).unwrap();
    let dora = steno_core::Person {
        embedding: None,
        sample_count: 0,
        ..sample_data::person(7, "Dora")
    };
    world.store.confirm_speaker(two, &dora).unwrap();

    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.diarizer = failing_diarizer("no model");
    dependencies.speech_engine = SharedSpeechEngine::new(Arc::new(FakeSpeechEngine {
        segment_seconds: 1.4,
        ..FakeSpeechEngine::default()
    }));
    let pipeline = ProcessingPipeline::new(dependencies);
    pipeline.reprocess(meeting.id).unwrap();
    pipeline.wait_until_idle().await;

    let again = world.store.export(meeting.id).unwrap();
    assert_eq!(again.meeting.state, MeetingState::Ready);
    let room = steno_core::room_speaker_id(meeting.id);
    assert_eq!(
        again.speaker(one).unwrap().assignment,
        steno_core::SpeakerAssignment::Confirmed { person_id: anna.id }
    );
    assert_eq!(
        again.speaker(two).unwrap().assignment,
        steno_core::SpeakerAssignment::Confirmed { person_id: dora.id }
    );
    let tap: Vec<(f64, Option<Uuid>)> = again
        .segments
        .iter()
        .filter(|segment| segment.lane == AudioLane::System)
        .map(|segment| (segment.start, segment.speaker_id))
        .collect();
    let expected = [
        (0.0, Some(one)),
        (1.4, Some(two)),
        (2.8, Some(one)),
        (4.2, Some(one)),
        (5.6, Some(two)),
        (7.0, Some(room)),
    ];
    assert_eq!(tap.len(), expected.len(), "{tap:?}");
    for ((start, id), (want_start, want_id)) in tap.iter().zip(expected) {
        assert!((start - want_start).abs() < 1e-6, "{tap:?}");
        assert_eq!(*id, want_id, "the segment at {start} s");
    }
}

/// The room row feeds no voice: confirmed to a person, it gives them no
/// embedding (the voice is computed only from rows with one), so the next
/// meeting's matching never suggests them; a diarized speaker confirmed to
/// the same person merges into the room row (`merge_speaker_rows`) and
/// the voice is then exactly that speaker's own embedding, what a plain
/// confirmation of it gives, with nothing from the room's segments; the
/// room row and so the mark stay.
#[tokio::test(flavor = "multi_thread")]
async fn the_room_row_feeds_no_voice() {
    let (world, meeting, asset, first) = a_diarized_call_with_new_speech(false).await;
    let pipeline = with_failing_diarizer(&world, "no model");
    pipeline.reprocess(meeting.id).unwrap();
    pipeline.wait_until_idle().await;
    let room = steno_core::room_speaker_id(meeting.id);
    assert!(
        world
            .store
            .export(meeting.id)
            .unwrap()
            .speaker(room)
            .is_some()
    );

    let cleo = steno_core::Person {
        embedding: None,
        sample_count: 0,
        ..sample_data::person(9, "Cleo")
    };
    world.store.confirm_speaker(room, &cleo).unwrap();
    let voice = world.store.person(cleo.id).unwrap().unwrap();
    assert_eq!((voice.embedding.clone(), voice.sample_count), (None, 0));

    // The next meeting, matched against the stored voices, never meets
    // Cleo.
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.speaker_memory =
        Arc::new(steno_pipeline::StoreSpeakerMemory::new(world.store.clone()));
    let next_pipeline = ProcessingPipeline::new(dependencies);
    let mut next = call_meeting(world.now);
    next.id = Uuid::new_v4();
    next.calendar_event_id = None;
    let next_asset = call_asset(&world.audio, next.id, AudioRetention::KeepForever);
    next_pipeline.enqueue(&next, &next_asset).unwrap();
    next_pipeline.wait_until_idle().await;
    for speaker in world.store.speakers(next.id).unwrap() {
        assert_ne!(speaker.person_id(), Some(cleo.id), "{speaker:?}");
    }

    // A diarized speaker confirmed to Cleo merges into the room row.
    let two = speaker(&first.speakers, "Speaker 2");
    world.store.confirm_speaker(two.id, &cleo).unwrap();
    let after = world.store.export(meeting.id).unwrap();
    assert!(after.speaker(two.id).is_none(), "merged into the room row");
    let row = after.speaker(room).expect("the room row and the mark stay");
    assert_eq!(
        row.embedding, two.embedding,
        "the merge copied its embedding"
    );
    let voice = world.store.person(cleo.id).unwrap().unwrap();
    assert_eq!(voice.sample_count, 1);
    assert_eq!(
        voice.embedding,
        steno_core::Embedding::mean(&[two.embedding.clone().unwrap()]),
        "only the diarized speaker's own embedding"
    );
    assert!(detail_reads_kept_incomplete(&world, meeting.id));
    assert_eq!(expires_at(&world, &asset), None);
}

/// A first-run fallback re-run whose diarizer fails again: the stored room
/// row is the only owner, covers the whole recording again (the new speech
/// included), and no second room row is pushed (a duplicate id would fail
/// `insert_speaker` and so the run).
#[tokio::test(flavor = "multi_thread")]
async fn a_rerun_of_a_first_run_fallback_keeps_one_room_row() {
    let (world, pipeline, meeting, asset) = a_fallback_whose_export_failed(false).await;
    extend_the_tap(&asset, 8.0);
    pipeline.reprocess(meeting.id).unwrap();
    pipeline.wait_until_idle().await;
    let again = world.store.export(meeting.id).unwrap();
    assert_eq!(again.meeting.state, MeetingState::Ready);
    assert_eq!(labels(&again), ["Me", "Speaker 1"]);
    let room = steno_core::room_speaker_id(meeting.id);
    let tap: Vec<_> = again
        .segments
        .iter()
        .filter(|segment| segment.lane == AudioLane::System)
        .collect();
    assert_eq!(tap.len(), 8);
    assert!(tap.iter().all(|segment| segment.speaker_id == Some(room)));
    pipeline.redeliver(meeting.id).await.unwrap();
    assert_eq!(
        world.store.deliveries(meeting.id).unwrap()[0].status,
        DeliveryStatus::Delivered
    );
    assert_eq!(expires_at(&world, &asset), None);
    assert!(detail_reads_kept_incomplete(&world, meeting.id));
}

/// A room row stored for the other lane (the first run diarized the mic,
/// its tap silent) that the user confirmed takes none of this lane's
/// segments when a re-run diarizes the tap and fails again: a confirmed
/// row never gets segments it did not own. The row stays, so the meeting
/// stays kept.
#[tokio::test(flavor = "multi_thread")]
async fn a_confirmed_room_row_of_the_other_lane_takes_no_tap_segment() {
    let world = world_with(
        false,
        None,
        AudioRetention::DeleteAfterProcessing,
        engine_deaf_to_silence(),
        FakeDiarizer::default(),
    );
    let meeting = call_meeting(world.now);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    silence_the_tap(&asset);
    let pipeline = with_failing_diarizer(&world, "no model");
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    let room = steno_core::room_speaker_id(meeting.id);
    let first = world.store.export(meeting.id).unwrap();
    assert!(
        first
            .segments
            .iter()
            .filter(|segment| segment.lane == AudioLane::Mic)
            .all(|segment| segment.speaker_id == Some(room)),
        "the mic was the room"
    );
    let anna = sample_data::person(0, "Anna");
    world.store.confirm_speaker(room, &anna).unwrap();

    extend_the_tap(&asset, 6.0);
    pipeline.reprocess(meeting.id).unwrap();
    pipeline.wait_until_idle().await;
    let again = world.store.export(meeting.id).unwrap();
    assert_eq!(again.meeting.state, MeetingState::Ready);
    assert!(
        again
            .segments
            .iter()
            .any(|segment| segment.lane == AudioLane::System)
    );
    for segment in again
        .segments
        .iter()
        .filter(|segment| segment.lane == AudioLane::System)
    {
        assert_ne!(segment.speaker_id, Some(room), "{segment:?}");
    }
    assert_eq!(
        again.speaker(room).unwrap().assignment,
        steno_core::SpeakerAssignment::Confirmed { person_id: anna.id }
    );
    assert_eq!(expires_at(&world, &asset), None);
}

/// Only a failed diarizer has a room fallback: the tap's segments a
/// working diarizer's clusters miss (here the last two seconds) keep no
/// speaker, no room row is stored, and the recording is stamped as the rule
/// says.
#[tokio::test(flavor = "multi_thread")]
async fn a_working_diarizer_leaves_what_its_clusters_miss_without_a_speaker() {
    let world = world_with(
        false,
        None,
        AudioRetention::DeleteAfterProcessing,
        FakeSpeechEngine::default(),
        FakeDiarizer::answering(|_| FakeDiarizer::round_robin(4.0, 2, 1.5)),
    );
    let meeting = call_meeting(world.now);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    let export = world.store.export(meeting.id).unwrap();
    assert_eq!(export.meeting.state, MeetingState::Ready);
    assert_eq!(labels(&export), ["Me", "Speaker 1", "Speaker 2"]);
    let missed: Vec<_> = export
        .segments
        .iter()
        .filter(|segment| segment.lane == AudioLane::System && segment.start >= 4.0)
        .collect();
    assert_eq!(missed.len(), 2, "{:?}", export.segments);
    assert!(missed.iter().all(|segment| segment.speaker_id.is_none()));
    assert_eq!(expires_at(&world, &asset), Some(world.now));
}
