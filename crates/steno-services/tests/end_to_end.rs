//! The one real-pipeline test across crates. The LLM passes run against the
//! stub chat server on loopback, fed from `Tests/Fixtures/llm/responses/`;
//! delivery runs through the real `DeliveryCoordinator` and
//! `ObsidianFolderDestination` into a temp vault; speaker suggestions come
//! from the store-backed cosine memory (the engine and the diarizer stay
//! fakes). No models, no network.
//! Swift: `Tests/StenoEndToEndTests/EndToEndTests.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use steno_adapters::{ArtifactRenderer, DeliveryCoordinator, ObsidianFolderDestination};
use steno_audio::SymphoniaAudioCodec;
use steno_core::testing::{FakeDiarizer, FakeSpeechEngine, sample_data};
use steno_core::{
    AudioAsset, AudioFormat, AudioLane, AudioRetention, DeliveryStatus, Destination, LlmUsage,
    Meeting, MeetingEvent, MeetingExport, MeetingState, ObsidianSettings, PipelineStage,
    RecordingLayout, SpeakerAssignmentKind, Store,
    paths::{file_url, path_from_file_url},
};
use steno_llm::testing::{Scripts, StubChatServer};
use steno_llm::{
    LlmEndpoint, LlmMeetingSummarizer, LlmTranscriptCleaner, OpenAiCompatibleClient, RetryPolicy,
};
use steno_pipeline::{
    MeetingEventBus, PipelineDependencies, ProcessingPipeline, StoreSpeakerMemory,
};

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures")
        .join(relative)
}

fn cleanup_usage() -> LlmUsage {
    LlmUsage {
        prompt_tokens: 300,
        completion_tokens: 120,
        requests: 1,
    }
}

fn summary_usage() -> LlmUsage {
    LlmUsage {
        prompt_tokens: 900,
        completion_tokens: 250,
        requests: 1,
    }
}

fn call_asset(audio: &Path, meeting_id: uuid::Uuid) -> AudioAsset {
    let layout = RecordingLayout::new(audio, meeting_id);
    layout.create_directories(false).unwrap();
    std::fs::copy(
        fixture("audio/conversation-two-lane-6s.wav"),
        layout.master(AudioFormat::Wav16kInt16),
    )
    .unwrap();
    std::fs::copy(
        fixture("audio/conversation-mic-6s.wav"),
        layout.sidecar(AudioLane::Mic),
    )
    .unwrap();
    std::fs::copy(
        fixture("audio/conversation-system-6s.wav"),
        layout.sidecar(AudioLane::System),
    )
    .unwrap();
    AudioAsset {
        id: steno_core::derived_uuid(meeting_id, "asset"),
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
        retention: AudioRetention::KeepDays(30),
        expires_at: None,
    }
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
async fn a_mac_call_fixture_lands_in_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    let now: DateTime<Utc> = sample_data::started_at() + Duration::hours(1);
    let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
    let audio = dir.path().join("audio");
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    let mut settings = store.settings().unwrap();
    settings.audio_folder = file_url(&audio, true);
    settings.obsidian = Some(ObsidianSettings {
        vault_path: vault.to_string_lossy().into_owned(),
        people_folder: Some("People".to_owned()),
        include_audio: true,
        task_tag: Some("task".to_owned()),
    });
    store.save_settings(&settings).unwrap();
    for person in [
        sample_data::person(0, "Jérôme"),
        sample_data::person(1, "Nicolai"),
    ] {
        store.save_person(&person).unwrap();
    }

    // The real cleaner and summarizer on the loopback stub: the cleanup
    // answer echoes every segment capitalised, the summary answer is the
    // canned analysis in `llm/responses/e2e-summary.json`.
    let server = StubChatServer::start().await.unwrap();
    let summary_body = std::fs::read_to_string(fixture("llm/responses/e2e-summary.json")).unwrap();
    let echo = Scripts.cleanup_echo(cleanup_usage(), |_, text| {
        let mut chars = text.chars();
        chars
            .next()
            .map(|first| format!("{}{}.", first.to_uppercase(), chars.as_str()))
    });
    server.respond(Arc::new(move |request| {
        if request.purpose.as_deref() == Some("summary") {
            Some(Scripts.completion(
                &summary_body,
                Some("stop"),
                Some(summary_usage()),
                "stub-model",
            ))
        } else {
            echo(request)
        }
    }));
    let endpoint = LlmEndpoint::new(server.base_url().clone(), "stub-model");
    let client: Arc<dyn steno_core::LanguageModel> = Arc::new(
        OpenAiCompatibleClient::new(endpoint.clone(), None)
            .with_retry(RetryPolicy::with_max_attempts(1)),
    );
    let cleaner = Arc::new(LlmTranscriptCleaner::new(client.clone(), endpoint.clone()));
    let summarizer = Arc::new(LlmMeetingSummarizer::new(client, endpoint, chrono_tz::UTC));

    // The stored settings decide the destination, as in the app; the time
    // zone is pinned so the folder names hold on every machine.
    let berlin: Tz = "Europe/Berlin".parse().unwrap();
    let dispatcher = Arc::new(DeliveryCoordinator::with_destinations(
        store.clone(),
        Box::new(move |settings: &steno_core::Settings| {
            settings
                .obsidian
                .iter()
                .map(|obsidian| {
                    Arc::new(ObsidianFolderDestination::new(obsidian.clone(), berlin))
                        as Arc<dyn Destination>
                })
                .collect()
        }),
        Box::new(move || now),
    ));
    let events = MeetingEventBus::new();
    let mut receiver = events.subscribe();
    let pipeline = ProcessingPipeline::new(
        PipelineDependencies::new(
            Arc::new(SymphoniaAudioCodec::new()),
            Arc::new(FakeSpeechEngine::default()),
            Arc::new(FakeDiarizer::default()),
            Arc::new(StoreSpeakerMemory::new(store.clone())),
            dispatcher,
            store.clone(),
            events.clone(),
        )
        .with_llm(Some(cleaner), Some(summarizer))
        .with_now(Arc::new(move || now)),
    );

    let mut meeting: Meeting = sample_data::meeting();
    "Produktstrategie".clone_into(&mut meeting.title);
    meeting.calendar_event_id = Some("event-1".to_owned());
    meeting.duration = 6.0;
    meeting.state = MeetingState::Recording;
    let asset = call_asset(&audio, meeting.id);
    pipeline.enqueue(&meeting, &asset).unwrap();
    pipeline.wait_until_idle().await;

    let stored = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.state, MeetingState::Ready, "{:?}", stored.state);
    assert_eq!(stored.title, "Produktstrategie", "a calendar title is kept");
    let purposes: Vec<String> = server
        .requests()
        .iter()
        .filter_map(|r| r.purpose.clone())
        .collect();
    assert_eq!(purposes, ["cleanup", "summary"]);
    assert_eq!(stored.llm_usage, Some(cleanup_usage() + summary_usage()));
    assert_eq!(
        stored
            .summary
            .as_ref()
            .unwrap()
            .sections
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["executive-summary", "full-summary"]
    );

    let deliveries = store.deliveries(meeting.id).unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(
        deliveries[0].destination_id,
        ObsidianFolderDestination::DESTINATION_ID
    );
    assert_eq!(deliveries[0].status, DeliveryStatus::Delivered);
    assert_eq!(deliveries[0].last_attempt_at, Some(now));
    let receipt = deliveries[0].receipt.clone().unwrap();
    let folder = "Meetings/2026-09-29-produktstrategie";
    let slug = "2026-09-29-produktstrategie";
    assert_eq!(receipt.folder, folder);
    assert_eq!(
        receipt
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect::<Vec<_>>(),
        [
            format!("{folder}/{slug} - Tasks.md"),
            format!("{folder}/{slug} - Transcript.md"),
            format!("{folder}/{slug}.md"),
            format!("{folder}/audio.wav"),
            format!("{folder}/meeting.json"),
            format!("{folder}/transcript.vtt"),
            "People/Jérôme.md".to_owned(),
            "People/Nicolai.md".to_owned(),
        ]
    );
    for file in &receipt.files {
        let data = std::fs::read(vault.join(&file.relative_path)).unwrap();
        assert_eq!(
            file.sha256,
            steno_adapters::sha256(&data),
            "{}",
            file.relative_path
        );
    }

    let json = std::fs::read(vault.join(format!("{folder}/meeting.json"))).unwrap();
    let export: MeetingExport = serde_json::from_slice(&json).unwrap();
    assert_eq!(
        json,
        ArtifactRenderer.render_json(&export).unwrap(),
        "meeting.json is the StenoJSON encoding"
    );
    let current = store.export(meeting.id).unwrap().unwrap();
    assert_eq!(export.meeting, current.meeting);
    assert_eq!(export.segments, current.segments);
    assert_eq!(export.tasks, current.tasks);
    assert!(
        export.audio.as_ref().unwrap().expires_at.is_none()
            && current.audio.as_ref().unwrap().expires_at.is_some(),
        "delivery precedes the retention stage, which sets the expiry afterwards"
    );
    assert_eq!(export.schema_version, MeetingExport::CURRENT_SCHEMA_VERSION);
    assert_eq!(export.segments.len(), 12);
    assert!(
        export
            .segments
            .iter()
            .all(|s| s.raw_text.starts_with("fake segment"))
    );
    assert!(export.segments.iter().all(|s| {
        let mut chars = s.raw_text.chars();
        let expected = chars
            .next()
            .map(|first| format!("{}{}.", first.to_uppercase(), chars.as_str()))
            .unwrap();
        s.text == expected
    }));
    assert_eq!(
        export
            .tasks
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>(),
        ["Budgetzahlen prüfen."]
    );
    assert_eq!(
        export
            .decisions
            .iter()
            .map(|d| d.text.as_str())
            .collect::<Vec<_>>(),
        ["Der Kern wird priorisiert."]
    );
    assert_eq!(
        export
            .speakers
            .iter()
            .map(|s| s.cluster_label.as_str())
            .collect::<Vec<_>>(),
        ["Me", "Speaker 1", "Speaker 2"]
    );
    // The fake diarizer's axis embeddings match the pre-enrolled people
    // through the real cosine memory: every "them" speaker is suggested.
    let them: Vec<_> = export
        .speakers
        .iter()
        .filter(|s| s.cluster_label != "Me")
        .collect();
    assert_eq!(them.len(), 2);
    assert!(
        them.iter()
            .all(|s| s.assignment.kind() == SpeakerAssignmentKind::Suggested),
        "{them:?}"
    );
    let suggested: std::collections::BTreeSet<_> = them
        .iter()
        .filter_map(|s| s.assignment.person_id())
        .collect();
    let people: std::collections::BTreeSet<_> =
        store.persons().unwrap().iter().map(|p| p.id).collect();
    assert_eq!(
        suggested, people,
        "each cluster is suggested to its own person"
    );
    // The stub's analysis names "Speaker 1" from a quote; the summarize
    // stage persists it for the review sheet.
    let speaker_one = export
        .speakers
        .iter()
        .find(|s| s.cluster_label == "Speaker 1")
        .unwrap();
    let suggestions = store.name_suggestions(meeting.id).unwrap();
    assert_eq!(suggestions.len(), 1, "{suggestions:?}");
    assert_eq!(suggestions[0].speaker_id, speaker_one.id);
    assert_eq!(suggestions[0].name.as_deref(), Some("Jérôme"));
    let mixdown = path_from_file_url(
        current
            .audio
            .as_ref()
            .unwrap()
            .mixdown_url
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(vault.join(format!("{folder}/audio.wav"))).unwrap(),
        std::fs::read(&mixdown).unwrap(),
        "the mixdown is copied byte for byte"
    );
    let note = std::fs::read_to_string(vault.join(format!("{folder}/{slug}.md"))).unwrap();
    assert!(note.contains("Produktstrategie"), "{note}");

    // Re-export through the one entry point overwrites Steno's files and
    // leaves the user's alone.
    let notes = vault.join(format!("{folder}/notes.md"));
    std::fs::write(&notes, "mine\n").unwrap();
    pipeline.redeliver(meeting.id).await.unwrap();
    let again = store.deliveries(meeting.id).unwrap()[0]
        .receipt
        .clone()
        .unwrap();
    assert_eq!(again.folder, receipt.folder);
    assert_eq!(
        again
            .files
            .iter()
            .map(|f| &f.relative_path)
            .collect::<Vec<_>>(),
        receipt
            .files
            .iter()
            .map(|f| &f.relative_path)
            .collect::<Vec<_>>()
    );
    for (before, after) in receipt.files.iter().zip(&again.files) {
        if !before.relative_path.ends_with("meeting.json") {
            assert_eq!(
                before.sha256, after.sha256,
                "{} is byte-identical",
                before.relative_path
            );
        }
    }
    assert_ne!(
        again.files[4].sha256, receipt.files[4].sha256,
        "meeting.json changed: the retention stage set expiresAt after the first delivery"
    );
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "mine\n");
    assert_eq!(
        server.request_count(),
        2,
        "a re-export never re-runs the LLM"
    );

    let mut collected = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        collected.push(event);
    }
    assert_eq!(
        collected.len(),
        14,
        "eleven stage starts (transcribe once per lane), one review request, one retention applied, one re-export: {collected:#?}"
    );
    assert_eq!(
        stages(&collected),
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
            PipelineStage::Deliver,
        ]
    );
    assert!(matches!(
        &collected[9],
        MeetingEvent::SpeakersNeedReview { meeting_id, speaker_ids } if *meeting_id == meeting.id && speaker_ids.len() == 3
    ));
    assert_eq!(
        collected[12],
        MeetingEvent::RetentionApplied {
            meeting_id: meeting.id
        }
    );
}
