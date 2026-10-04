//! The pipeline over a `SidecarSpeechEngine` on the real
//! `steno-speech-sidecar` binary (its fake engine, so no models), the
//! engine the app runs Parakeet on off the Mac: the two-lane fixture call.
//! The child starts at the job's warm-up, transcribes both lanes, and is
//! gone before the diarizer runs; the next job starts a child of its own.
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
