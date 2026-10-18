//! The pipeline over a `SidecarSpeechEngine` on the real
//! `steno-speech-sidecar` binary (its fake engine, so no models), which
//! `cargo test --workspace` builds: each job's child transcribes, is gone
//! before the diarizer runs, and a child that dies while it diarizes fails
//! that stage only.

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

/// The diarizer in the same sidecar engine as speech: the first job's
/// diarizing child aborts, its stage fails and the job still ends `ready`
/// with the fallback's speakers; the second job diarizes in a new child.
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
            AudioRetention::DeleteAfterProcessing,
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
        // The fallback's room speaker keeps the recording unstamped.
        let room = speakers
            .iter()
            .any(|speaker| speaker.id == steno_core::room_speaker_id(meeting.id));
        assert_eq!(room, job == 1, "job {job}: {speakers:?}");
        let stamp = store.asset(meeting.id).unwrap().unwrap().expires_at;
        assert_eq!(stamp.is_none(), job == 1, "job {job}: {stamp:?}");
        // One child to transcribe, one to diarize.
        assert_eq!(engine.spawns(), 2 * job);
        assert_eq!(engine.pid(), None, "no child outlives the job");
    }
    assert!(marker.exists(), "the first diarizing child aborted");
}

/// A diarizer that fails as `SidecarDiarizer` does when the sidecar's
/// child dies, boxed as the boundary boxes it.
struct FailingInTheSidecar(SidecarFailure);

/// One way the child fails, made fresh for each job.
type SidecarFailure = fn() -> steno_speech::SidecarError;

#[steno_core::async_trait]
impl steno_core::Diarizer for FailingInTheSidecar {
    async fn prepare(&self) -> steno_core::protocols::BoundaryResult<()> {
        Ok(())
    }

    async fn diarize(
        &self,
        _audio: &steno_core::AudioBuffer16k,
    ) -> steno_core::protocols::BoundaryResult<steno_core::DiarizationResult> {
        Err(Box::new(steno_diarize::DiarizeError::Sidecar(
            steno_speech::SpeechError::Sidecar((self.0)()),
        )))
    }
}

/// Each way the sidecar's child fails while it diarizes (a crash, the
/// deadline, the memory ceiling) reaches the diarizer fallback: the job
/// ends `ready` with the room speaker, and the recording is kept as
/// possibly incomplete.
#[tokio::test(flavor = "multi_thread")]
async fn every_sidecar_diarize_failure_keeps_the_recording_incomplete() {
    let failures: [(&str, SidecarFailure); 4] = [
        ("crash", || steno_speech::SidecarError::Crashed {
            status: "signal: 6 (SIGABRT)".to_owned(),
            stderr: String::new(),
        }),
        ("oomd kill", || steno_speech::SidecarError::Crashed {
            status: "signal: 9 (SIGKILL)".to_owned(),
            stderr: String::new(),
        }),
        ("deadline", || steno_speech::SidecarError::Timeout {
            after: std::time::Duration::from_secs(600),
        }),
        ("ceiling", || steno_speech::SidecarError::MemoryCeiling {
            rss_bytes: 7 << 30,
            ceiling_bytes: 6 << 30,
        }),
    ];
    for (name, failure) in failures {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
        let audio = dir.path().join("audio");
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&audio, true);
        settings.default_retention = AudioRetention::DeleteAfterProcessing;
        store.save_settings(&settings).unwrap();
        let diarizer = Arc::new(steno_services::model_gate::GatedDiarizer::new(
            Arc::new(FailingInTheSidecar(failure)),
            Arc::new(|| true),
        ));
        let pipeline = ProcessingPipeline::new(PipelineDependencies::new(
            Arc::new(SymphoniaAudioCodec::new()),
            Arc::new(steno_core::testing::FakeSpeechEngine::default()),
            diarizer,
            Arc::new(InMemorySpeakerMemory::new(Vec::new())),
            Arc::new(steno_adapters::DeliveryCoordinator::new(store.clone())),
            store.clone(),
            MeetingEventBus::new(),
        ));
        let mut meeting = sample_data::meeting();
        meeting.id = uuid::Uuid::new_v4();
        meeting.duration = 6.0;
        meeting.state = MeetingState::Recording;
        let asset = steno_pipeline::fixtures::two_lane_call(
            &audio,
            meeting.id,
            AudioRetention::DeleteAfterProcessing,
        )
        .unwrap();
        pipeline.enqueue(&meeting, &asset).unwrap();
        pipeline.wait_until_idle().await;

        let export = store.export(meeting.id).unwrap();
        assert_eq!(export.meeting.state, MeetingState::Ready, "{name}");
        let room = steno_core::room_speaker_id(meeting.id);
        assert!(
            export.speakers.iter().any(|speaker| speaker.id == room),
            "{name}: the room speaker is stored: {:?}",
            export.speakers
        );
        assert_eq!(
            store.asset(meeting.id).unwrap().unwrap().expires_at,
            None,
            "{name}: kept"
        );
        let files = steno_host::fakes::FakeFileSystem::default();
        files.create(steno_core::paths::file_url_path(&asset.url).unwrap());
        let mut detail =
            steno_host::main_window::MeetingDetailViewModel::new(meeting.id, Some(&settings));
        detail.reload(&store, &files, &steno_host::fakes::FakePipeline::default());
        assert_eq!(
            detail.recording_status(),
            Some(steno_host::main_window::RecordingStatus::KeptIncomplete),
            "{name}"
        );
    }
}
