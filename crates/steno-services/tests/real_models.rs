//! The opt-in acceptance over the real engines: the synthetic two-lane
//! fixture through the Parakeet engine the platform runs (the speech
//! sidecar off the Mac, `CoreML` on it) and the ONNX diarizer, the way
//! `steno process --engine parakeet-v3` wires them, asserting the shape of
//! the exported `meeting.json`. Set `STENO_MODEL_TESTS=1` (about 0.7 GB of
//! downloads on first run); `STENO_MODELS_DIR` keeps the models between
//! runs, `STENO_MODELS_MIRROR` names a mirror. Off the Mac the sidecar
//! binary must be built in the target directory (`cargo test --workspace`
//! builds it, as does `cargo build -p steno-speech-sidecar`).
//! Swift: `Tests/StenoEndToEndTests/RealModelsEndToEndTests.swift`.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use steno_adapters::ArtifactRenderer;
use steno_audio::SymphoniaAudioCodec;
use steno_core::testing::{FakeDestination, FakeSummarizer, PassthroughCleaner};
use steno_core::{
    AudioRetention, Destination, Meeting, MeetingExport, MeetingSource, MeetingState, StenoPaths,
    Store, TitleOrigin, paths::file_url,
};
use steno_pipeline::{
    MeetingEventBus, PipelineDependencies, ProcessingPipeline, StoreSpeakerMemory,
};
use steno_services::speech::SpeechSetup;

// One flow: the setup is most of it.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread")]
async fn the_synthetic_call_runs_through_the_real_engines_to_a_well_formed_export() {
    if std::env::var("STENO_MODEL_TESTS").as_deref() != Ok("1") {
        eprintln!(
            "set STENO_MODEL_TESTS=1 to run the pipeline over the fixture recording with the ONNX engines"
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let models = std::env::var_os("STENO_MODELS_DIR")
        .map_or_else(|| dir.path().join("models"), PathBuf::from);
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    let audio = dir.path().join("audio");
    let mut settings = store.settings().unwrap();
    settings.audio_folder = file_url(&audio, true);
    "parakeet-v3".clone_into(&mut settings.speech_engine_id);
    store.save_settings(&settings).unwrap();

    // The test binary sits in the target directory's `deps/`, the sidecar
    // binary one folder up.
    let exe = std::env::current_exe().unwrap();
    let target = exe.parent().and_then(std::path::Path::parent).unwrap();
    let setup = SpeechSetup {
        models_directory: models.clone(),
        settings: steno_services::speech::speech_settings(&StenoPaths::new(dir.path())),
        sidecar: steno_speech::SidecarConfig::new(
            target.join(steno_speech::sidecar::SIDECAR_BINARY),
        ),
    };
    let runtime = setup.runtime(&settings.speech_engine_id);
    let speech_store = setup.model_store();
    let engine = steno_services::speech::speech_engine(&settings.speech_engine_id, &setup);
    let diarizer = steno_services::speech::diarizer(&models);
    let vault = dir.path().join("vault");
    let destination: Arc<dyn Destination> = Arc::new(FakeDestination::new(&vault));
    let dispatcher = Arc::new(steno_adapters::DeliveryCoordinator::with_destinations(
        store.clone(),
        Box::new(move |_| vec![destination.clone()]),
        Box::new(Utc::now),
    ));
    let pipeline = ProcessingPipeline::new(
        PipelineDependencies::new(
            Arc::new(SymphoniaAudioCodec::new()),
            engine,
            diarizer,
            Arc::new(StoreSpeakerMemory::new(store.clone())),
            dispatcher,
            store.clone(),
            MeetingEventBus::new(),
        )
        .with_llm(
            Some(Arc::new(PassthroughCleaner::default())),
            Some(Arc::new(FakeSummarizer::default())),
        ),
    );

    let meeting_id = uuid::Uuid::new_v4();
    let now = Utc::now();
    let meeting = Meeting {
        id: meeting_id,
        title: "conversation".to_owned(),
        started_at: now - chrono::Duration::seconds(6),
        duration: 6.0,
        language: None,
        source: MeetingSource::MacCall,
        calendar_event_id: None,
        tags: Vec::new(),
        state: MeetingState::Queued,
        end_reason: None,
        title_origin: TitleOrigin::Default,
        template_id: Meeting::DEFAULT_TEMPLATE_ID.to_owned(),
        summary: None,
        scratchpad: String::new(),
        llm_usage: None,
        created_at: now,
        updated_at: now,
    };
    let asset =
        steno_pipeline::fixtures::two_lane_call(&audio, meeting_id, AudioRetention::KeepForever)
            .unwrap();
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;

    let stored = store.meeting(meeting_id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready, "{:?}", stored.state);
    let export = store.export(meeting_id).unwrap();
    let json = ArtifactRenderer.render_json(&export).unwrap();
    let decoded: MeetingExport = serde_json::from_slice(&json).unwrap();
    assert_eq!(
        decoded.schema_version,
        MeetingExport::CURRENT_SCHEMA_VERSION
    );
    assert_eq!(decoded.meeting.id, meeting_id);
    assert_eq!(decoded.meeting.state, MeetingState::Ready);
    assert_eq!(decoded.meeting.source, MeetingSource::MacCall);
    assert!(
        decoded.speakers.iter().any(|s| s.cluster_label == "Me"),
        "the mic lane has the me speaker"
    );
    assert!(
        decoded
            .audio
            .as_ref()
            .is_some_and(|a| a.mixdown_url.is_some())
    );
    assert!(
        decoded
            .speakers
            .iter()
            .filter(|s| s.cluster_label != "Me")
            .all(|s| s.embedding.is_none()),
        "embeddings never reach meeting.json"
    );
    // Synthetic tones carry no speech: the engine may return no segments,
    // and the shape is what this test pins, not the words.
    eprintln!(
        "[model-tests] {} segments, {} speakers: {:?}",
        decoded.segments.len(),
        decoded.speakers.len(),
        decoded
            .speakers
            .iter()
            .map(|s| &s.cluster_label)
            .collect::<Vec<_>>()
    );
    if runtime == steno_speech::SpeechRuntime::OnnxSidecar {
        assert!(speech_store.is_installed(&steno_speech::ModelAsset::parakeet_v3_fp32()));
    }
}
