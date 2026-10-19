//! The pipeline over core's fakes: stage order, what each stage persists,
//! the no-LLM path, failure attribution, retention, launch recovery and the
//! learned rates.

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
use steno_pipeline::pipeline::{diarized_lane_after_transcription, tap_carried_no_conversation};
use steno_pipeline::{
    ClipProbe, ClipStep, LaneMerger, LiveRecording, MeetingEventBus, MonotonicClock,
    PipelineDependencies, ProcessingPipeline, QuitLatch, REDELIVER_AFTER, ReprocessError,
    RetentionSweep, SharedSpeechEngine, StageRates,
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

/// The adapters' dispatcher reduced to one `Delivery` row per destination.
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
    /// Holds the store and the audio.
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

/// `destination` builds the one fake destination over the vault path.
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

/// Reads the 16 kHz mono WAV fixtures and logs the lanes it decoded.
#[derive(Default)]
struct WavDecoder {
    decodes: Mutex<Vec<AudioLane>>,
    /// What each decode reports it replaced by silence; none by default.
    damage: Mutex<steno_core::AudioDamage>,
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
        let mut buffer = steno_core::AudioBuffer16k::new(read_wav(&path)?);
        buffer.damage = *self.damage.lock().unwrap();
        Ok(buffer)
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

/// Six seconds of silence over the asset's system sidecar.
fn silence_the_tap(asset: &AudioAsset) {
    let layout = RecordingLayout::from_asset(asset).unwrap();
    steno_pipeline::fixtures::write_wav(&layout.sidecar(AudioLane::System), &vec![0; 6 * 16_000])
        .unwrap();
}

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

/// A call whose tap holds under 5 % of the mic's speech and under ten
/// seconds is diarized on the mic lane; otherwise the standard lane stays.
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
    // A mostly listening partner: 3 %, but ten seconds is a conversation.
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
    // The rule is for calls: an in-person asset keeps the last lane.
    assert_eq!(
        diarized_lane_after_transcription(MeetingSource::MacInPerson, &call, &silent_tap),
        Some(AudioLane::System)
    );
}

/// A call on speaker with a silent tap: the mic lane is diarized like a
/// room, nobody is "me", and the recording and lanes are untouched.
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

/// A silent tap with one voice on the mic keeps the mic as "me" and writes
/// no clip for a cluster never made a speaker.
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

/// A re-run after the tap went quiet removes the "me" participant the
/// first run created.
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

/// A cleaner that first runs `meddle` with the store, as the user can
/// while the cleanup waits, then marks every segment cleaned.
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

/// A pipeline with a [`MeddlingCleaner`] and `summarizer`.
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

fn speaker<'a>(speakers: &'a [steno_core::Speaker], label: &str) -> &'a steno_core::Speaker {
    speakers
        .iter()
        .find(|speaker| speaker.cluster_label == label)
        .unwrap_or_else(|| panic!("no speaker {label}"))
}

/// A [`FakeSummarizer`] that keeps the input each call saw.
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

/// Cleanup writes text by segment and leaves the speakers alone, so edits
/// made while it ran stay and the summary sees the stored speakers.
#[tokio::test(flavor = "multi_thread")]
async fn a_speaker_named_during_cleanup_stays_named() {
    let world = world(false, None, AudioRetention::KeepForever);
    let summarizer = Arc::new(RecordingSummarizer::default());
    let pipeline = meddling(
        &world,
        // Confirms "Speaker 1" as Anna and clears the clip of "Speaker 2".
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

/// A title typed while the meeting processes is kept and used by the
/// summary.
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

/// A template picked while the meeting processes is kept and used by the
/// summary.
#[tokio::test(flavor = "multi_thread")]
async fn a_template_picked_while_processing_is_kept_and_used() {
    let world = world(false, None, AudioRetention::KeepForever);
    let pipeline = meddling(
        &world,
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

fn with_engine(world: &World, engine: Arc<dyn steno_core::SpeechEngine>) -> PipelineDependencies {
    world
        .pipeline
        .dependencies()
        .clone()
        .with_speech_engine(SharedSpeechEngine::new(engine))
}

/// Enqueues the two-lane call as its own meeting; its id.
fn enqueue_call(world: &World, pipeline: &ProcessingPipeline) -> Uuid {
    let mut meeting = call_meeting(world.now);
    meeting.id = Uuid::new_v4();
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    pipeline.enqueue(&meeting, &asset).unwrap();
    meeting.id
}

/// [`enqueue_call`] for a run refused for missing models, once it ended.
async fn enqueue_refused_call(world: &World, pipeline: &ProcessingPipeline) -> Uuid {
    let meeting = enqueue_call(world, pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(pipeline.dependencies().model_waits.waiting(), [meeting]);
    meeting
}

fn meeting_state(world: &World, id: Uuid) -> MeetingState {
    world.store.meeting(id).unwrap().unwrap().state
}

/// The speech engine is released once the lanes are transcribed, before
/// the diarizer runs.
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

/// What the `CoreML` engine fails with once the boundary caught an
/// Objective-C exception: an ordinary speech error.
const COREML_EXCEPTION: &str =
    "CoreML raised NSGenericException: Failed to allocate memory IOSurface object";

/// A caught `CoreML` exception fails the meeting like any speech failure:
/// every audio file stays, and Process again later makes it ready.
#[tokio::test(flavor = "multi_thread")]
async fn a_coreml_exception_fails_the_meeting_retryably_with_its_audio_kept() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    let raising_load = FakeSpeechEngine {
        prepare_failure: Some(COREML_EXCEPTION.to_owned()),
        ..FakeSpeechEngine::default()
    };
    let raising_call = FakeSpeechEngine {
        failure: Some(COREML_EXCEPTION.to_owned()),
        ..FakeSpeechEngine::default()
    };
    for (engine, stage) in [(raising_load, "decode"), (raising_call, "transcribe")] {
        let pipeline = ProcessingPipeline::new(with_engine(&world, Arc::new(engine)));
        let mut meeting = call_meeting(world.now);
        meeting.id = Uuid::new_v4();
        let asset = call_asset(
            &world.audio,
            meeting.id,
            AudioRetention::DeleteAfterProcessing,
        );
        let before = audio_files(&asset);
        pipeline.enqueue(&meeting, &asset).unwrap();
        pipeline.wait_until_idle().await;
        assert_eq!(
            meeting_state(&world, meeting.id),
            MeetingState::Failed {
                reason: format!("{stage}: {COREML_EXCEPTION}")
            }
        );
        let stored = world.store.asset(meeting.id).unwrap().unwrap();
        assert_eq!(stored.expires_at, None, "{stage}: no retention stamp");
        let _ = RetentionSweep::new(world.store.clone()).run(world.now + Duration::days(400));
        assert_eq!(
            audio_files(&asset),
            before,
            "{stage}: every audio file is kept"
        );

        world.pipeline.process_again(meeting.id).unwrap();
        world.pipeline.wait_until_idle().await;
        assert_eq!(
            meeting_state(&world, meeting.id),
            MeetingState::Ready,
            "{stage}"
        );
    }
}

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

/// A diarizer that does not load fails nothing: the meeting is ready with
/// one unknown room speaker.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_diarizer_does_not_load_keeps_its_transcript() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(FakeSpeechEngine::default());
    let mut dependencies = with_engine(&world, engine.clone()).with_speech_in_process(true);
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

/// Waits for [`MeetingEvent::WaitingForRecording`] for each of `meetings`.
async fn waiting_for_recording(receiver: &mut steno_pipeline::EventReceiver, meetings: &[Uuid]) {
    let mut left: BTreeSet<Uuid> = meetings.iter().copied().collect();
    tokio::time::timeout(PATIENCE, async {
        while !left.is_empty() {
            if let MeetingEvent::WaitingForRecording { meeting_id } = receiver.recv().await.unwrap()
            {
                left.remove(&meeting_id);
            }
        }
    })
    .await
    .expect("each meeting said it waits for the recording");
}

/// A pipeline running `engine` in this process with `live` as the
/// recorder's state.
fn in_process_beside(
    world: &World,
    engine: Arc<dyn steno_core::SpeechEngine>,
    live: &LiveRecording,
) -> ProcessingPipeline {
    ProcessingPipeline::new(
        with_engine(world, engine)
            .with_speech_in_process(true)
            .with_live_recording(live.clone()),
    )
}

async fn a_moment() {
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}

/// While recording, a job runs no in-process speech engine: it waits and
/// goes on once the recorder is idle.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_waits_for_the_recording_before_the_engine_in_this_process_loads() {
    let world = world(false, None, AudioRetention::KeepForever);
    let live = LiveRecording::default();
    live.set(true);
    let engine = Arc::new(FakeSpeechEngine::default());
    let pipeline = in_process_beside(&world, engine.clone(), &live);
    let mut receiver = world.events.subscribe();
    let meeting = enqueue_call(&world, &pipeline);
    waiting_for_recording(&mut receiver, &[meeting]).await;
    a_moment().await;
    assert_eq!(engine.preparations.count(), 0, "no load beside a recording");
    assert_eq!(engine.transcriptions.count(), 0);
    assert_eq!(meeting_state(&world, meeting), MeetingState::Queued);

    live.set(false);
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    assert_eq!(engine.preparations.count(), 1);
    assert_eq!(engine.transcriptions.count(), 2);
}

/// A sidecar engine does not wait for a recording.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_on_an_engine_in_a_child_runs_beside_a_recording() {
    let world = world(false, None, AudioRetention::KeepForever);
    let live = LiveRecording::default();
    live.set(true);
    let engine = Arc::new(FakeSpeechEngine::default());
    let dependencies = with_engine(&world, engine.clone()).with_live_recording(live);
    let pipeline = ProcessingPipeline::new(dependencies);
    let meeting = enqueue_call(&world, &pipeline);
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    assert_eq!(engine.transcriptions.count(), 2);
}

/// A fake engine whose first transcription waits for `open`.
struct GatedEngine {
    inner: FakeSpeechEngine,
    entered: std::sync::atomic::AtomicBool,
    open: tokio::sync::Notify,
}

impl GatedEngine {
    fn new(inner: FakeSpeechEngine) -> Self {
        GatedEngine {
            inner,
            entered: false.into(),
            open: tokio::sync::Notify::new(),
        }
    }

    async fn wait_until_entered(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !self.entered.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the gated call started");
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
        self.inner.prepare().await
    }

    async fn transcribe(
        &self,
        audio: &steno_core::AudioBuffer16k,
        hint: Option<&steno_core::LanguageTag>,
    ) -> steno_core::protocols::BoundaryResult<Vec<steno_core::RawSegment>> {
        if !self.entered.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.open.notified().await;
        }
        self.inner.transcribe(audio, hint).await
    }

    async fn release(&self) -> steno_core::protocols::BoundaryResult<()> {
        self.inner.release().await
    }
}

/// A job failing after quit leaves its meeting `queued`, warns nothing,
/// and the next launch processes it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_that_fails_after_the_pipeline_quits_is_resumed_at_the_next_launch() {
    let log = steno_pipeline::fixtures::CapturedLog::warnings();
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(GatedEngine::new(FakeSpeechEngine {
        failure: Some("the sidecar died mid-request".to_owned()),
        ..FakeSpeechEngine::default()
    }));
    let dependencies = with_engine(&world, engine.clone()).with_quit_latch(QuitLatch::default());
    let pipeline = ProcessingPipeline::new(dependencies);
    // Its own meeting id picks this run's log lines out.
    let (mut meeting, asset) = saved_processing(&world);
    meeting.state = MeetingState::Queued;
    world.store.save_meeting(&meeting).unwrap();
    assert_eq!(pipeline.resume_unfinished().unwrap(), [meeting.id]);
    engine.wait_until_entered().await;

    pipeline.quit();
    engine.open.notify_one();
    pipeline.wait_until_idle().await;
    assert_eq!(engine.inner.transcriptions.count(), 1, "the job failed");
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Queued);
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

/// Once quit, no job starts: `enqueue` saves the meeting `queued` and
/// spawns nothing, recovery starts nothing, and `process` is refused.
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

fn with_failing_diarizer(world: &World, reason: &str) -> ProcessingPipeline {
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.diarizer = failing_diarizer(reason);
    ProcessingPipeline::new(dependencies)
}

fn failing_diarizer(reason: &str) -> Arc<FakeDiarizer> {
    Arc::new(FakeDiarizer {
        failure: Some(reason.to_owned()),
        ..FakeDiarizer::default()
    })
}

/// A failing diarizer costs the labels, not the transcript: the mic stays
/// "me" and the tap goes to one unknown, nameable speaker.
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

/// A failing voice lookup leaves speakers unknown and the meeting ready.
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

/// A re-run whose diarizer fails keeps the stored speakers, confirmation
/// and voice included.
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

/// The re-run's diarizer: the same two speakers with one-second clips (the
/// first run's are 1.5 s), so a clip's length tells which run wrote it.
fn rediarizing() -> Arc<FakeDiarizer> {
    Arc::new(FakeDiarizer::answering(|audio| {
        FakeDiarizer::round_robin(audio.duration(), 2, 1.0)
    }))
}

/// A pipeline with the re-run's diarizer and `probe` at the clip steps.
fn rerun_pipeline(world: &World, probe: Option<ClipProbe>) -> ProcessingPipeline {
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.diarizer = rediarizing();
    if let Some(probe) = probe {
        dependencies = dependencies.with_clip_probe(probe);
    }
    ProcessingPipeline::new(dependencies)
}

/// The ready meeting with "Speaker 1" confirmed as Anna; its asset and
/// that speaker's id.
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

/// The samples of the whole 16 kHz mono WAV at `path`.
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

/// Asserts "Speaker 1" is still Anna and every speaker row's clip is a
/// whole WAV as long as its range; returns the file names the rows name.
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

fn clip_files(asset: &AudioAsset) -> BTreeSet<OsString> {
    let layout = RecordingLayout::from_asset(asset).unwrap();
    std::fs::read_dir(layout.speakers_directory())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect()
}

/// A re-run ended at any clip step, as by a crash, leaves every row naming
/// a whole clip; the next run sweeps only what the ended one left.
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

/// A re-run whose clip write fails removes the file it wrote and falls
/// back to the stored speakers, whose clips stay playable.
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

/// When the mic lane of a silent-tap call fails to diarize, it becomes
/// the one room speaker, not "me".
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

/// A diarizer fallback is logged at warn with meeting and stage only: the
/// reason can name the audio file.
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

/// A clock that panics once the meeting is stored `ready`, inside
/// `persist`.
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

/// A meeting `persist` marked ready stays ready and is delivered after a
/// later panic.
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

/// A failed background run is logged at warn with asset and stage only;
/// the reason stays with the meeting row and the debug level.
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
    // An id of its own picks this run's line out of the shared subscriber.
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
async fn a_second_operation_on_a_meeting_in_flight_is_refused() {
    let world = world(false, None, AudioRetention::KeepForever);
    let meeting = call_meeting(world.now);
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    let again = world.pipeline.enqueue(&meeting, &asset).unwrap_err();
    assert!(again.reason.contains("already being processed"));
    world.pipeline.wait_until_idle().await;
}

/// A decoder whose next `decode` panics while `panics` is set, as
/// symphonia's demuxer does on a crafted `co64` offset.
struct PanickingDecoder {
    inner: Arc<WavDecoder>,
    panics: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl steno_core::AudioDecoder for PanickingDecoder {
    async fn decode(
        &self,
        asset: &AudioAsset,
        lane: AudioLane,
    ) -> steno_core::protocols::BoundaryResult<steno_core::AudioBuffer16k> {
        assert!(
            !self.panics.swap(false, std::sync::atomic::Ordering::SeqCst),
            "the demuxer broke"
        );
        self.inner.decode(asset, lane).await
    }

    fn mixdown_format(&self) -> AudioFormat {
        self.inner.mixdown_format()
    }

    async fn mixdown(
        &self,
        asset: &AudioAsset,
        to: &Path,
    ) -> steno_core::protocols::BoundaryResult<()> {
        self.inner.mixdown(asset, to).await
    }
}

/// Every file in the asset's folder, with its bytes.
fn audio_files(asset: &AudioAsset) -> BTreeMap<PathBuf, Vec<u8>> {
    let folder = file_url_path(&asset.url).unwrap();
    std::fs::read_dir(folder.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

/// A decode panic fails the meeting and keeps every audio file, sweep
/// included; the worker lives on and the meeting can be reprocessed, on
/// the resume path too.
#[tokio::test(flavor = "multi_thread")]
async fn a_decoder_panic_fails_the_meeting_and_keeps_its_audio() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    let decoder = Arc::new(PanickingDecoder {
        inner: world.decoder.clone(),
        panics: std::sync::atomic::AtomicBool::new(true),
    });
    let mut dependencies = world.pipeline.dependencies().clone();
    dependencies.decoder = decoder.clone();
    let pipeline = ProcessingPipeline::new(dependencies);

    let mut meeting = call_meeting(world.now);
    meeting.id = Uuid::new_v4();
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    let before = audio_files(&asset);
    assert!(!before.is_empty());
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;
    let MeetingState::Failed { reason } = meeting_state(&world, meeting.id) else {
        panic!(
            "the panic fails the meeting: {:?}",
            meeting_state(&world, meeting.id)
        );
    };
    assert!(
        reason.contains(steno_pipeline::OPERATION_PANICKED),
        "{reason}"
    );
    assert_eq!(audio_files(&asset), before, "every audio file is kept");
    assert!(world.store.asset(meeting.id).unwrap().is_some());
    let _ = RetentionSweep::new(world.store.clone()).run(world.now + Duration::days(400));
    assert_eq!(audio_files(&asset), before, "the sweep keeps them too");

    let mut next = call_meeting(world.now);
    next.id = Uuid::new_v4();
    let next_asset = call_asset(&world.audio, next.id, AudioRetention::KeepForever);
    pipeline.enqueue(&next, &next_asset).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, next.id), MeetingState::Ready);

    pipeline.reprocess(meeting.id).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);

    // A meeting left queued at the last quit, resumed at launch.
    decoder
        .panics
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (mut resumed, resumed_asset) = saved_processing(&world);
    resumed.state = MeetingState::Queued;
    world.store.save_meeting(&resumed).unwrap();
    let before = audio_files(&resumed_asset);
    assert_eq!(pipeline.resume_unfinished().unwrap(), [resumed.id]);
    pipeline.wait_until_idle().await;
    assert!(matches!(
        meeting_state(&world, resumed.id),
        MeetingState::Failed { .. }
    ));
    assert_eq!(audio_files(&resumed_asset), before);
    pipeline.reprocess(resumed.id).unwrap();
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, resumed.id), MeetingState::Ready);
}

/// `reprocess` takes only a ready or failed meeting with its asset.
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

/// `process_again` refuses a ready meeting, leaving it unclaimed, and runs
/// a failed one to ready.
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

/// A ready meeting with a diarizer fallback's room row is offered Process
/// again, and refused once a working diarizer replaced the row.
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

/// `reprocess` refuses a meeting whose master is gone before saving
/// anything, even with its sidecars there.
#[tokio::test(flavor = "multi_thread")]
async fn reprocess_refuses_a_meeting_whose_audio_is_gone() {
    let world = world(false, None, AudioRetention::KeepForever);
    let asset = saved_ready(&world);
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
}

/// A failed reprocess leaves no retention stamp from the run before it.
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

/// A meeting saved `processing`, as a quit leaves it.
fn saved_processing(world: &World) -> (Meeting, AudioAsset) {
    let mut meeting = call_meeting(world.now);
    meeting.id = Uuid::new_v4();
    meeting.state = MeetingState::Processing;
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world
        .store
        .save_meeting_with_asset(&meeting, &asset)
        .unwrap();
    (meeting, asset)
}

/// A ready meeting; its asset.
fn saved_ready(world: &World) -> AudioAsset {
    let (mut meeting, asset) = saved_processing(world);
    meeting.state = MeetingState::Ready;
    world.store.save_meeting(&meeting).unwrap();
    asset
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

/// A meeting the last process left `processing` fails at launch with its
/// recording stamp cleared, so a recording that crashes the app is not read
/// at every launch; a `queued` one still runs.
#[tokio::test(flavor = "multi_thread")]
async fn launch_recovery_fails_a_meeting_left_processing_and_runs_a_queued_one() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    let (crashed, mut crashed_asset) = saved_processing(&world);
    crashed_asset.expires_at = Some(world.now);
    world.store.save_asset(&crashed_asset).unwrap();
    let (mut queued, _) = saved_processing(&world);
    queued.state = MeetingState::Queued;
    world.store.save_meeting(&queued).unwrap();

    assert_eq!(world.pipeline.resume_unfinished().unwrap(), [queued.id]);
    world.pipeline.wait_until_idle().await;
    assert_eq!(
        meeting_state(&world, crashed.id),
        MeetingState::Failed {
            reason: steno_pipeline::CLOSED_WHILE_PROCESSING.to_owned()
        }
    );
    assert_eq!(
        world.store.asset(crashed.id).unwrap().unwrap().expires_at,
        None
    );
    assert_eq!(meeting_state(&world, queued.id), MeetingState::Ready);
}

/// A decode that replaced parts by silence keeps the recording forever
/// under a finite rule.
#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_recording_is_kept_forever() {
    let world = world(false, None, AudioRetention::DeleteAfterProcessing);
    *world.decoder.damage.lock().unwrap() = steno_core::AudioDamage {
        parts: 3,
        seconds: 0.07,
        salvaged: false,
    };
    let meeting = call_meeting(world.now);
    let asset = call_asset(
        &world.audio,
        meeting.id,
        AudioRetention::DeleteAfterProcessing,
    );
    world.pipeline.enqueue(&meeting, &asset).unwrap();
    world.pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
    let asset = world.store.asset(meeting.id).unwrap().unwrap();
    assert_eq!(asset.retention, AudioRetention::KeepForever);
    assert_eq!(asset.expires_at, None);
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
        // The CLI checks the manifest hash; here the bytes are compared.
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

/// Launch re-export: a `pending` export, or a failure older than a day,
/// in the future or never attempted, is re-exported; recent failures,
/// unready meetings and delivered exports are left.
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
    let (launch, day) = (world.now, REDELIVER_AFTER);
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
    mark_delivery(&world, ids[7], failed(), Some(launch + Duration::hours(1)));

    let mut owed = pipeline.redeliver_unfinished().unwrap();
    owed.sort();
    let mut expected = [&ids[..3], &ids[7..]].concat();
    expected.sort();
    assert_eq!(owed, expected);
    pipeline.wait_until_idle().await;
    for id in &expected {
        assert_eq!(delivery(&world, *id).status, DeliveryStatus::Delivered);
    }
    for id in &ids[3..5] {
        assert_eq!(delivery(&world, *id).status, failed());
    }
    assert_eq!(delivery(&world, ids[5]).status, DeliveryStatus::Pending);
    assert_eq!(delivery(&world, ids[6]).last_attempt_at, Some(exported));
    assert_eq!(pipeline.in_flight(), Vec::<Uuid>::new());
}

fn delivery(world: &World, id: Uuid) -> Delivery {
    world.store.deliveries(id).unwrap().remove(0)
}

/// Stores the first delivery row as `status`, last attempted at `attempt`.
fn mark_delivery(world: &World, id: Uuid, status: DeliveryStatus, attempt: Option<DateTime<Utc>>) {
    let mut row = delivery(world, id);
    row.status = status;
    row.last_attempt_at = attempt;
    world.store.save_delivery(&row).unwrap();
}

/// The pipeline a launch at `at` builds, with `destination` stamping `at`.
fn launch_at(world: &World, at: DateTime<Utc>, destination: FakeDestination) -> ProcessingPipeline {
    let mut dependencies = world
        .pipeline
        .dependencies()
        .clone()
        .with_now(Arc::new(move || at));
    dependencies.dispatcher = Arc::new(FakeDispatcher {
        store: world.store.clone(),
        destinations: vec![Arc::new(destination)],
        now: at,
    });
    ProcessingPipeline::new(dependencies)
}

/// A dispatcher that records the meeting's state and summary template at
/// `mark_pending` and delivers nothing.
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

/// `persist` marks the export pending before the meeting ready, so an exit
/// before delivery leaves rows the next launch re-exports.
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

/// How long a test waits for a held fake to be reached.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// Where [`Uninstalled`] refuses while its models are missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    SpeechPrepare,
    Transcribe,
    DiarizerPrepare,
    Diarize,
}

/// Fakes that refuse at one point with `PipelineFailure::models_missing`
/// until `installed` is set.
struct Uninstalled {
    refusal: Refusal,
    installed: std::sync::atomic::AtomicBool,
    engine: FakeSpeechEngine,
    diarizer: FakeDiarizer,
}

impl Uninstalled {
    fn refusing(refusal: Refusal) -> Arc<Self> {
        Arc::new(Uninstalled {
            refusal,
            installed: false.into(),
            engine: FakeSpeechEngine::default(),
            diarizer: FakeDiarizer::default(),
        })
    }

    fn install(&self) {
        self.installed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn check(
        &self,
        at: Refusal,
        stage: PipelineStage,
    ) -> steno_core::protocols::BoundaryResult<()> {
        if at == self.refusal && !self.installed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Box::new(steno_pipeline::PipelineFailure::models_missing(
                stage,
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl steno_core::SpeechEngine for Uninstalled {
    fn id(&self) -> &str {
        self.engine.id()
    }

    fn supported_languages(&self) -> &std::collections::BTreeSet<steno_core::LanguageTag> {
        self.engine.supported_languages()
    }

    async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
        self.check(Refusal::SpeechPrepare, PipelineStage::Decode)?;
        self.engine.prepare().await
    }

    async fn transcribe(
        &self,
        audio: &steno_core::AudioBuffer16k,
        hint: Option<&steno_core::LanguageTag>,
    ) -> steno_core::protocols::BoundaryResult<Vec<RawSegment>> {
        self.check(Refusal::Transcribe, PipelineStage::Transcribe)?;
        self.engine.transcribe(audio, hint).await
    }
}

struct UninstalledDiarizer(Arc<Uninstalled>);

#[async_trait]
impl steno_core::Diarizer for UninstalledDiarizer {
    async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
        self.0
            .check(Refusal::DiarizerPrepare, PipelineStage::Diarize)?;
        self.0.diarizer.prepare().await
    }

    async fn diarize(
        &self,
        audio: &steno_core::AudioBuffer16k,
    ) -> steno_core::protocols::BoundaryResult<steno_core::DiarizationResult> {
        self.0.check(Refusal::Diarize, PipelineStage::Diarize)?;
        self.0.diarizer.diarize(audio).await
    }
}

/// A run refused for a missing model, at any of the four gates, leaves the
/// meeting `queued` with no failure reason and posts `ModelsMissing`; once
/// installed, `resume_unfinished` takes it to `ready`.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_refused_for_missing_models_stays_queued_and_resumes_once_they_are_installed() {
    for refusal in [
        Refusal::SpeechPrepare,
        Refusal::Transcribe,
        Refusal::DiarizerPrepare,
        Refusal::Diarize,
    ] {
        let world = world(false, None, AudioRetention::KeepForever);
        let gate = Uninstalled::refusing(refusal);
        let mut dependencies = with_engine(&world, gate.clone());
        dependencies.diarizer = Arc::new(UninstalledDiarizer(gate.clone()));
        let pipeline = ProcessingPipeline::new(dependencies);
        let mut events = world.events.subscribe();
        let meeting = enqueue_call(&world, &pipeline);
        pipeline.wait_until_idle().await;
        assert_eq!(
            meeting_state(&world, meeting),
            MeetingState::Queued,
            "{refusal:?}"
        );
        let row = world.store.meeting(meeting).unwrap().unwrap();
        assert_eq!(row.state.failure_reason(), None, "{refusal:?}");
        let posted = drain(&mut events);
        assert!(
            posted.contains(&MeetingEvent::ModelsMissing {
                meeting_id: meeting
            }),
            "{refusal:?}: {posted:?}"
        );
        assert!(pipeline.in_flight().is_empty(), "{refusal:?}");

        gate.install();
        assert_eq!(pipeline.resume_unfinished().unwrap(), [meeting]);
        pipeline.wait_until_idle().await;
        assert_eq!(
            meeting_state(&world, meeting),
            MeetingState::Ready,
            "{refusal:?}"
        );
    }
}

/// `process` returns the refusal itself, its kind intact through the
/// boxed error, so Process again can tell it from a failure.
#[tokio::test(flavor = "multi_thread")]
async fn process_returns_the_models_missing_refusal() {
    let world = world(false, None, AudioRetention::KeepForever);
    let gate = Uninstalled::refusing(Refusal::Transcribe);
    let pipeline = ProcessingPipeline::new(with_engine(&world, gate));
    let mut meeting = call_meeting(world.now);
    meeting.id = Uuid::new_v4();
    let asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepForever);
    world
        .store
        .save_meeting_with_asset(&meeting, &asset)
        .unwrap();
    let failure = pipeline.process(asset.id).await.unwrap_err();
    assert!(failure.is_models_missing());
    assert_eq!(failure.stage, PipelineStage::Transcribe);
    assert_eq!(failure.reason, "Download the speech models in Settings");
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Queued);
}

/// Process again on a failed meeting with models missing parks it like a
/// refused run, its retention stamp gone; the install's resume processes
/// it and any meeting already waiting.
#[tokio::test(flavor = "multi_thread")]
async fn process_again_with_models_missing_parks_the_failed_meeting_with_its_audio() {
    let world = world(false, None, AudioRetention::KeepDays(30));
    let gate = Uninstalled::refusing(Refusal::Transcribe);
    let pipeline = ProcessingPipeline::new(with_engine(&world, gate.clone()));
    let already = enqueue_refused_call(&world, &pipeline).await;
    let mut meeting = call_meeting(world.now);
    meeting.state = MeetingState::Failed {
        reason: "decode: unreadable".to_owned(),
    };
    let mut asset = call_asset(&world.audio, meeting.id, AudioRetention::KeepDays(30));
    asset.expires_at = Some(world.now);
    world
        .store
        .save_meeting_with_asset(&meeting, &asset)
        .unwrap();

    pipeline.process_again(meeting.id).unwrap();
    pipeline.wait_until_idle().await;
    let row = world.store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(row.state, MeetingState::Queued);
    assert_eq!(row.state.failure_reason(), None);
    let mut both = vec![already, meeting.id];
    both.sort();
    assert_eq!(pipeline.dependencies().model_waits.waiting(), both);
    let stored = world.store.asset(meeting.id).unwrap().unwrap();
    assert_eq!(stored.expires_at, None);
    assert!(file_url_path(&stored.url).unwrap().exists());
    for lane in stored.sidecars_16k.values() {
        assert!(file_url_path(lane).unwrap().exists());
    }

    gate.install();
    let mut resumed = pipeline.resume_waiting().unwrap();
    resumed.sort();
    assert_eq!(resumed, both);
    pipeline.wait_until_idle().await;
    assert_eq!(meeting_state(&world, already), MeetingState::Ready);
    assert_eq!(meeting_state(&world, meeting.id), MeetingState::Ready);
}

/// Under the hold a run is claimed and saved `queued` but reads nothing,
/// and a second hold is refused; once dropped the run goes to `ready`.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_hold_keeps_a_queued_run_from_starting_until_it_drops() {
    let world = world(false, None, AudioRetention::KeepForever);
    let engine = Arc::new(FakeSpeechEngine::default());
    let pipeline = ProcessingPipeline::new(with_engine(&world, engine.clone()));
    let in_flight = pipeline.dependencies().in_flight.clone();
    let hold = in_flight.try_hold().expect("idle");
    let meeting = enqueue_call(&world, &pipeline);
    assert!(!in_flight.is_idle(), "the queued run holds its asset");
    assert!(in_flight.try_hold().is_none(), "one hold at a time");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(meeting_state(&world, meeting), MeetingState::Queued);
    assert_eq!(engine.transcriptions.count(), 0);
    assert_eq!(world.decoder.decodes.lock().unwrap().len(), 0);

    drop(hold);
    tokio::time::timeout(PATIENCE, pipeline.wait_until_idle())
        .await
        .unwrap();
    assert_eq!(meeting_state(&world, meeting), MeetingState::Ready);
    assert!(engine.transcriptions.count() > 0);
}

fn expires_at(world: &World, asset: &AudioAsset) -> Option<DateTime<Utc>> {
    world
        .store
        .asset_by_id(asset.id)
        .unwrap()
        .unwrap()
        .expires_at
}

/// A diarizer fallback meeting stays unstamped, even under "delete after
/// processing", until a run with speakers succeeds.
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

/// Runs a `duration`-second call with empty lanes under "delete after
/// processing"; the meeting and whether it was kept unstamped.
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

/// An empty transcript over more than 30 s likely failed, so it is kept;
/// a short one is stamped.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_recording_with_no_transcript_keeps_its_recording() {
    for (duration, kept) in [(31.0, true), (6.0, false)] {
        assert_eq!(silent_call_is_kept(duration).await, kept, "{duration} s");
    }
}

/// Records for each commit whether the asset is stamped and the
/// `synchronous` level (2 is FULL).
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

fn first_level(commits: &Mutex<Vec<(bool, i64)>>, stamped: bool) -> Option<i64> {
    commits
        .lock()
        .unwrap()
        .iter()
        .find(|(stamp, _)| *stamp == stamped)
        .map(|(_, level)| *level)
}

/// The retention stamp commits under `synchronous = FULL`, syncing the
/// results written before it, so a power loss cannot leave the recording
/// gone and its results rolled back.
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

/// A keep that clears a stamp commits as durably as the stamp.
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

/// [`Store::keep_forever`] clears every stamp durably in one write.
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
    // Probe the asset `keep_forever` writes last.
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

/// The bound: an empty transcript over exactly 30 s is stamped, just over
/// is kept.
#[tokio::test(flavor = "multi_thread")]
async fn the_empty_lane_bound_is_exclusive() {
    for (duration, kept) in [(30.0, false), (30.5, true)] {
        assert_eq!(silent_call_is_kept(duration).await, kept, "{duration} s");
    }
}

/// A phone recording with no announced length, over `samples`.
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

/// A phone meeting with no announced duration uses the decoded length.
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

/// Rewrites the system sidecar with `seconds` of the conversation, giving
/// a re-run speech no stored speaker's span covers.
fn extend_the_tap(asset: &AudioAsset, seconds: f64) {
    let layout = RecordingLayout::from_asset(asset).unwrap();
    steno_pipeline::fixtures::write_wav(
        &layout.sidecar(AudioLane::System),
        &steno_pipeline::fixtures::conversation(seconds, &[AudioLane::System]),
    )
    .unwrap();
}

/// Whether the detail's recording line reads `KeptIncomplete`, mirroring
/// steno-host's `recording_status` over the stored rows.
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

/// A call processed under "delete after processing" with one working
/// destination, its tap then extended to eight seconds.
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

/// A failed re-run diarizer gives uncovered new speech to the room row,
/// and every later step leaves the recording unstamped and kept.
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
    assert_eq!(launch.redeliver_unfinished().unwrap(), [meeting.id]);
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

/// Confirmed and renamed speakers keep every segment they cover by
/// midpoint or overlap; only uncovered speech goes to the room row.
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

/// The room row feeds no voice, and a speaker merged into it gives the
/// person exactly that speaker's embedding; the room row stays.
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

    // The next meeting, matched against the stored voices, never meets Cleo.
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

    // A re-run failing the same way returns and posts the failure.
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

/// The room row beside stored speakers takes the next free "Speaker N"
/// everywhere: export, note and the summary's label map.
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
