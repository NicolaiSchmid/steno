//! The pipeline over a `SidecarSpeechEngine` on the real
//! `steno-speech-sidecar` binary (its fake engine, so no models), the
//! engine the app runs Parakeet on off the Mac: the two-lane fixture call.
//! The child starts at the job's warm-up, transcribes both lanes, and is
//! gone before the diarizer runs; the next job starts a child of its own.
//!
//! The diarizer runs in the same engine's child: a child that dies while
//! it diarizes fails that stage only.
//!
//! The binary is the one in the target directory, which
//! `cargo test --workspace` builds (as does
//! `cargo build -p steno-speech-sidecar`).

use std::ffi::OsString;
use std::sync::{Arc, Mutex};

use steno_audio::SymphoniaAudioCodec;
use steno_core::testing::{FakeDiarizer, InMemorySpeakerMemory, sample_data};
use steno_core::{AudioRetention, MeetingState, SpeechEngine, Store, paths::file_url};
use steno_pipeline::{MeetingEventBus, PipelineDependencies, ProcessingPipeline};
use steno_speech::{ModelStore, SidecarConfig, SidecarSpeechEngine};

mod common;

#[tokio::test(flavor = "multi_thread")]
async fn each_job_starts_the_sidecar_and_frees_it_once_its_lanes_are_transcribed() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    let audio = dir.path().join("audio");
    let mut settings = store.settings().unwrap();
    settings.audio_folder = file_url(&audio, true);
    store.save_settings(&settings).unwrap();

    let mut config = SidecarConfig::new(common::sidecar_binary());
    config.args = vec![OsString::from("--fake-engine")];
    let engine = Arc::new(SidecarSpeechEngine::with_assets(
        ModelStore::new(dir.path().join("models")),
        config,
        Vec::new(),
    ));
    // What the diarizer saw of the child: its pid, when one ran.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let diarizer = {
        let (engine, seen) = (engine.clone(), seen.clone());
        FakeDiarizer::answering(move |audio| {
            seen.lock().unwrap().push(engine.pid());
            FakeDiarizer::round_robin(audio.duration(), 2, 1.5)
        })
    };
    let pipeline = ProcessingPipeline::new(PipelineDependencies::new(
        Arc::new(SymphoniaAudioCodec::new()),
        engine.clone() as Arc<dyn SpeechEngine>,
        Arc::new(diarizer),
        Arc::new(InMemorySpeakerMemory::new(Vec::new())),
        Arc::new(steno_adapters::DeliveryCoordinator::new(store.clone())),
        store.clone(),
        MeetingEventBus::new(),
    ));

    for job in 1..=2 {
        let mut meeting = sample_data::meeting();
        meeting.id = uuid::Uuid::new_v4();
        meeting.duration = 6.0;
        meeting.state = MeetingState::Recording;
        let asset = steno_pipeline::fixtures::two_lane_call(
            &audio,
            meeting.id,
            AudioRetention::KeepForever,
        )
        .unwrap();
        pipeline.enqueue(&meeting, &asset).unwrap();
        pipeline.wait_until_idle().await;

        let stored = store.meeting(meeting.id).unwrap().unwrap();
        assert_eq!(stored.state, MeetingState::Ready, "{:?}", stored.state);
        assert!(
            !store.segments(meeting.id).unwrap().is_empty(),
            "the child's transcripts reached the store"
        );
        assert_eq!(engine.spawns(), job, "one child per job");
        assert_eq!(engine.pid(), None, "no child outlives the job");
    }
    assert_eq!(
        *seen.lock().unwrap(),
        [None, None],
        "the child was gone before each job's diarization"
    );
}

/// The diarizer the app runs, in the same sidecar engine as speech: each
/// job's child transcribes and is released, then a child of the
/// diarizer's own diarizes and stops. The first job's diarizing child
/// aborts (as ONNX Runtime would): its diarize stage fails, the job still
/// ends `ready` with the fallback's speakers, and the app runs on. The
/// second job diarizes in a new child.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_dies_while_it_diarizes_fails_that_stage_and_the_next_job_diarizes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    let audio = dir.path().join("audio");
    let mut settings = store.settings().unwrap();
    settings.audio_folder = file_url(&audio, true);
    store.save_settings(&settings).unwrap();

    let models = ModelStore::new(dir.path().join("models"));
    // Files of the manifest's sizes, which the fake engine never opens.
    let asset = steno_diarize::models::asset();
    let folder = models.directory(&asset);
    std::fs::create_dir_all(&folder).unwrap();
    for file in &asset.files {
        std::fs::File::create(folder.join(&file.name))
            .unwrap()
            .set_len(file.size)
            .unwrap();
    }
    let marker = dir.path().join("faulted");
    let mut config = SidecarConfig::new(common::sidecar_binary());
    config.args = [
        "--fake-engine",
        "--fault",
        "abort-diarizing",
        "--fault-once",
        marker.to_str().unwrap(),
    ]
    .map(OsString::from)
    .to_vec();
    let engine = Arc::new(SidecarSpeechEngine::with_assets(models, config, Vec::new()));
    let pipeline = ProcessingPipeline::new(PipelineDependencies::new(
        Arc::new(SymphoniaAudioCodec::new()),
        engine.clone() as Arc<dyn SpeechEngine>,
        steno_services::speech::diarizer_in(engine.clone(), steno_diarize::Install::Never),
        Arc::new(InMemorySpeakerMemory::new(Vec::new())),
        Arc::new(steno_adapters::DeliveryCoordinator::new(store.clone())),
        store.clone(),
        MeetingEventBus::new(),
    ));

    for job in 1..=2u64 {
        let mut meeting = sample_data::meeting();
        meeting.id = uuid::Uuid::new_v4();
        meeting.duration = 6.0;
        meeting.state = MeetingState::Recording;
        let asset = steno_pipeline::fixtures::two_lane_call(
            &audio,
            meeting.id,
            AudioRetention::KeepForever,
        )
        .unwrap();
        pipeline.enqueue(&meeting, &asset).unwrap();
        pipeline.wait_until_idle().await;

        let stored = store.meeting(meeting.id).unwrap().unwrap();
        assert_eq!(stored.state, MeetingState::Ready, "{:?}", stored.state);
        assert!(
            !store.segments(meeting.id).unwrap().is_empty(),
            "the child's transcripts reached the store"
        );
        let speakers = store.speakers(meeting.id).unwrap();
        let diarized = speakers
            .iter()
            .any(|speaker| speaker.embedding.is_some() && speaker.sample_clip_range.is_some());
        assert_eq!(diarized, job == 2, "job {job}: {speakers:?}");
        // One child to transcribe, one to diarize.
        assert_eq!(engine.spawns(), 2 * job);
        assert_eq!(engine.pid(), None, "no child outlives the job");
    }
    assert!(marker.exists(), "the first diarizing child aborted");
}
