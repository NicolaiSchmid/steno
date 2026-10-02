//! Snapshot parity against the recorded bridge fixtures
//! (`apps/macos/web/fixtures/bridge/`): for every snapshot and reply
//! fixture, the store and service state that produces it is built, the
//! host's snapshot is encoded with `steno_bridge::json::to_canonical_string`
//! and compared byte for byte with the file.
//!
//! Where a fixture cannot be reproduced from any store or service state,
//! because `BridgeSamples.swift` wrote a hand-picked value the view models
//! never compute, the difference is listed as a [`Deviation`] on the
//! fixture: a JSON pointer, the value the host produces instead, and why.
//! The test applies the deviations to the fixture and then demands byte
//! equality, so everything not listed is proven equal, and the table the
//! test prints is the PR's parity table.

// The ported suites keep one Swift test per function, long as some are.
#![allow(clippy::too_many_lines)]

mod common;

use steno_host::services::{Handover as _, LoginItem as _};

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use serde_json::{Value, json};
use steno_bridge::{
    AssetIdParams, BridgeHost, BridgeTopic, ConfirmDestructiveParams, ExportUpdateParams,
    PermissionKind, PermissionState, RecordingRetention, RecordingState, RetentionMode,
    SetRetentionParams, SetStringParams, SpeakerOptionsParams, SummariesUpdateParams,
};
use steno_core::paths::file_url;
use steno_core::{AudioRetention, HandoverReceipt, HandoverState, LlmProvider, PipelineStage};
use steno_host::main_window::progress::{MeetingEvent, ProcessingProgress};
use steno_host::services::{
    AutoStopStatus, CodexModel, LaneLevels, LoginItemStatus, RecorderStatus, UpdateOutcome,
};
use steno_host::speech::ModelAsset;

/// One hand-written fixture value the host computes differently.
struct Deviation {
    /// JSON pointer into the fixture.
    pointer: &'static str,
    /// The host's value; `None` removes the key.
    host_value: Option<Value>,
    reason: &'static str,
}

fn deviation(pointer: &'static str, host_value: Value, reason: &'static str) -> Deviation {
    Deviation {
        pointer,
        host_value: Some(host_value),
        reason,
    }
}

fn removed(pointer: &'static str, reason: &'static str) -> Deviation {
    Deviation {
        pointer,
        host_value: None,
        reason,
    }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/macos/web/fixtures/bridge")
}

fn fixture(name: &str) -> Value {
    let path = fixtures_dir().join(format!("{name}.json"));
    serde_json::from_slice(&fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap()
}

fn apply(mut fixture: Value, deviations: &[Deviation]) -> Value {
    for deviation in deviations {
        let (parent, key) = deviation.pointer.rsplit_once('/').unwrap();
        let parent = fixture
            .pointer_mut(parent)
            .unwrap_or_else(|| panic!("no {parent} in the fixture"));
        match (&deviation.host_value, parent) {
            (Some(value), Value::Object(map)) => {
                map.insert(key.to_owned(), value.clone());
            }
            (None, Value::Object(map)) => {
                map.remove(key);
            }
            (Some(value), Value::Array(items)) => {
                items[key.parse::<usize>().unwrap()] = value.clone();
            }
            (None, Value::Array(items)) => {
                items.remove(key.parse::<usize>().unwrap());
            }
            _ => panic!("{} is not a container", deviation.pointer),
        }
    }
    fixture
}

/// Asserts byte equality between the host's value and the fixture after
/// its deviations, and records the row for the table.
fn assert_parity(name: &str, host: &Value, deviations: &[Deviation]) {
    let expected = apply(fixture(name), deviations);
    let expected = steno_bridge::json::to_canonical_string(&expected).unwrap();
    let actual = steno_bridge::json::to_canonical_string(host).unwrap();
    if expected != actual {
        let diff = similar_lines(&expected, &actual);
        panic!("{name}.json differs from the host's snapshot:\n{diff}");
    }
    let row = if deviations.is_empty() {
        format!("| `{name}` | exact | |")
    } else {
        let reasons: Vec<String> = deviations
            .iter()
            .map(|d| format!("`{}`: {}", d.pointer, d.reason))
            .collect();
        format!(
            "| `{name}` | {} deviation(s) | {} |",
            deviations.len(),
            reasons.join("; ")
        )
    };
    println!("{row}");
}

fn similar_lines(expected: &str, actual: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (left, right) in expected.lines().zip(actual.lines()) {
        if left != right {
            let _ = writeln!(out, "- {left}\n+ {right}");
        }
    }
    let (l, r) = (expected.lines().count(), actual.lines().count());
    if l != r {
        let _ = writeln!(out, "(fixture has {l} lines, host {r})");
    }
    out
}

fn sample_harness() -> Harness {
    Harness::builder()
        .seed(|store, fakes| {
            let folder =
                steno_core::paths::path_from_file_url(&store.settings().unwrap().audio_folder)
                    .unwrap();
            populate_sample(store, fakes, &folder);
            configure_llm(store, "qwen3-8b");
            set_retention(store, AudioRetention::KeepDays(30));
        })
        .build()
}

#[test]
fn app() {
    let harness = Harness::builder()
        .with_handover("steno-mac-7f3a", 52_431)
        .seed(|store, fakes| {
            let folder =
                steno_core::paths::path_from_file_url(&store.settings().unwrap().audio_folder)
                    .unwrap();
            populate_sample(store, fakes, &folder);
            let handover = fakes.handover.as_ref().unwrap();
            handover.pair(paired_phone());
            handover.start().unwrap();
        })
        .build();
    // The banner needs the list to have filled once.
    let _ = harness.snapshot(BridgeTopic::MeetingsList);
    assert_parity(
        "app",
        &harness.snapshot(BridgeTopic::App),
        &[
            deviation(
                "/setupBanner/title",
                json!("Summaries and export are off."),
                "the fixture's banner copy is not `SetupCopy`'s; the host says what is missing",
            ),
            deviation(
                "/setupBanner/body",
                json!(
                    "Steno has no LLM endpoint and no Obsidian vault yet, so meetings keep a raw transcript on this Mac."
                ),
                "as above",
            ),
        ],
    );
}

#[test]
fn recording_idle() {
    let harness = Harness::builder().build();
    assert_parity("recording", &harness.snapshot(BridgeTopic::Recording), &[]);
}

#[test]
fn recording_live() {
    let harness = Harness::builder().build();
    harness.fakes.recorder.set_status(RecorderStatus {
        state: RecordingState::Recording,
        started_at: Some(now()),
        mode: Some(steno_bridge::CaptureMode::Call),
        call_app: Some("Zoom".to_owned()),
        meeting_id: Some(uuid(MEETING)),
        levels: Some(LaneLevels {
            mic: 0.42,
            system: Some(0.18),
        }),
        auto_stop: Some(AutoStopStatus {
            app_name: Some("Zoom".to_owned()),
            remaining_seconds: 42.0,
            total_seconds: 60.0,
        }),
        denied_permissions: Vec::new(),
        warning: None,
        error: None,
    });
    assert_parity(
        "recording.live",
        &harness.snapshot(BridgeTopic::Recording),
        &[deviation(
            "/autoStop/reason",
            json!("Zoom closed the microphone."),
            "the host words the reason as `RecordingSnapshot.init` did; the fixture abbreviates",
        )],
    );
}

#[test]
fn progress() {
    let harness = Harness::builder().build();
    let meeting_id = uuid(0x02);
    harness.host.apply_meeting_event(&MeetingEvent::Progress {
        meeting_id,
        progress: ProcessingProgress {
            stage: PipelineStage::Decode,
            fraction: 0.05,
            next_fraction: 0.1,
            estimated_remaining_seconds: 180.0,
            is_estimate_seeded: false,
        },
    });
    harness.host.apply_meeting_event(&MeetingEvent::Progress {
        meeting_id,
        progress: ProcessingProgress {
            stage: PipelineStage::Transcribe,
            fraction: 0.62,
            next_fraction: 0.8,
            estimated_remaining_seconds: 95.0,
            is_estimate_seeded: false,
        },
    });
    assert_parity(
        "progress",
        &harness.snapshot(BridgeTopic::Progress),
        &[
            deviation(
                "/entries/0/stage",
                json!("transcribe"),
                "the host sends `PipelineStage`'s raw value (`stage.rawValue` in Swift); the fixture's gerund is not a stage",
            ),
            deviation(
                "/entries/0/title",
                json!("Transcribing…"),
                "the entry title carries the ellipsis (`ProcessingProgressModel.Entry.title`)",
            ),
        ],
    );
}

#[test]
fn meetings_list() {
    let harness = sample_harness();
    assert_parity(
        "meetings.list",
        &harness.snapshot(BridgeTopic::MeetingsList),
        &[
            deviation(
                "/groups/0/meetings/0/preview",
                json!(
                    "Fokus: Nicolai schlägt vor, 90 Prozent der Kapazität auf den Kern zu setzen und Nebenprojekte bis Q1 zu pausieren."
                ),
                "the preview is the summary's first bullet (`lead: text`); the fixture's sentence is not in its own detail summary",
            ),
            deviation(
                "/groups/0/meetings/0/speakers/0/colorIndex",
                json!(7),
                "`colorIndex` is the FNV fold of the person id; the fixture counts 0, 1, 2, 3",
            ),
            deviation(
                "/groups/0/meetings/0/speakers/1/colorIndex",
                json!(4),
                "as above",
            ),
            deviation(
                "/groups/0/meetings/0/speakers/2/colorIndex",
                json!(1),
                "as above",
            ),
            deviation(
                "/groups/0/meetings/0/speakers/3/colorIndex",
                json!(0),
                "as above (the speaker's own id)",
            ),
            deviation(
                "/groups/0/meetings/0/speakers/2/isConfirmed",
                json!(false),
                "the chip follows the speaker's assignment, and the fixture's own detail has Speaker 3 suggested",
            ),
            deviation(
                "/tags",
                json!([{"count": 1, "name": "investors"}, {"count": 1, "name": "q4"}, {"count": 1, "name": "strategie"}]),
                "tags count every listed meeting's tags, sorted by name (`tagCounts.keys.sorted()`); the fixture omits the failed meeting's `investors` and lists the rest in meeting order",
            ),
        ],
    );
}

#[test]
fn meeting_detail() {
    let harness = sample_harness();
    let _ = harness.snapshot(BridgeTopic::MeetingsList);
    assert_parity(
        "meeting.detail",
        &harness.snapshot(BridgeTopic::MeetingDetail),
        &[
            deviation(
                "/speakers/0/colorIndex",
                json!(7),
                "`colorIndex` is the FNV fold of the person id; the fixture counts 0, 1, 2, 3",
            ),
            deviation("/speakers/1/colorIndex", json!(4), "as above"),
            deviation("/speakers/2/colorIndex", json!(1), "as above"),
            deviation(
                "/speakers/3/colorIndex",
                json!(0),
                "as above (the speaker's own id)",
            ),
            deviation("/tasks/0/assigneeColorIndex", json!(4), "as above (Jérôme)"),
            deviation(
                "/tasks/1/assigneeColorIndex",
                json!(7),
                "as above (Nicolai)",
            ),
            deviation(
                "/templates",
                json!([
                    {"id": "default", "name": "Default"},
                    {"id": "customer-discovery", "name": "Customer Discovery"},
                    {"id": "daily-standup", "name": "Daily Standup"},
                    {"id": "interview", "name": "Interview"}
                ]),
                "the templates are the four bundled ones; the fixture names two that do not exist",
            ),
        ],
    );
}

#[test]
fn settings_general() {
    let harness = Harness::builder()
        .seed(|_, fakes| {
            *fakes.login_item.changes.lock().unwrap() = Vec::new();
            fakes.login_item.set_enabled(true).unwrap();
            *fakes.updater.last_check.lock().unwrap() = Some(date("2026-09-29T11:50:00.000Z"));
            *fakes.updater.outcome.lock().unwrap() = UpdateOutcome::UpToDate;
        })
        .build();
    assert_eq!(harness.fakes.login_item.status(), LoginItemStatus::Enabled);
    let host = harness.snapshot(BridgeTopic::SettingsGeneral);
    let templates = host["templates"].clone();
    let acknowledgements = host["acknowledgements"].clone();
    assert_eq!(templates.as_array().unwrap().len(), 4);
    assert_eq!(acknowledgements.as_array().unwrap().len(), 12);
    assert_parity(
        "settings.general",
        &host,
        &[
            deviation(
                "/templates",
                templates,
                "the templates are the four bundled ones with their own descriptions; the fixture names three that do not exist",
            ),
            deviation(
                "/acknowledgements",
                acknowledgements,
                "every speech model and the seven libraries; the fixture lists four",
            ),
        ],
    );
}

#[test]
fn settings_recording() {
    let harness = Harness::builder()
        .seed(|store, fakes| {
            let mut settings = store.settings().unwrap();
            settings.audio_folder = file_url(
                Path::new("/Users/nicolai/Library/Application Support/Steno/audio"),
                true,
            );
            store.save_settings(&settings).unwrap();
            *fakes.audio_devices.devices.lock().unwrap() = vec![
                steno_host::services::InputDevice {
                    uid: "BuiltInMicrophoneDevice".to_owned(),
                    name: "MacBook Pro Microphone".to_owned(),
                },
                steno_host::services::InputDevice {
                    uid: "AirPodsPro".to_owned(),
                    name: "Nicolai's AirPods Pro".to_owned(),
                },
            ];
            *fakes.folder_usage.bytes.lock().unwrap() = Ok(734_003_200);
            *fakes.pipeline.kept_forever.lock().unwrap() = 2;
        })
        .build();
    // Forever once (keeping two recordings), then back to 30 days: the
    // count stays, as the view model keeps it.
    harness
        .host
        .settings_recording_set_retention(SetRetentionParams {
            retention: RecordingRetention {
                mode: RetentionMode::KeepForever,
                days: 30,
            },
        })
        .unwrap();
    harness
        .host
        .settings_recording_set_retention(SetRetentionParams {
            retention: RecordingRetention {
                mode: RetentionMode::KeepDays,
                days: 30,
            },
        })
        .unwrap();
    assert_parity(
        "settings.recording",
        &harness.snapshot(BridgeTopic::SettingsRecording),
        &[],
    );
}

#[test]
fn settings_transcription() {
    let harness = Harness::builder()
        .seed(|_, fakes| {
            fakes
                .speech_models
                .install(ModelAsset::ParakeetV3, Some(485_000_000));
            *fakes.speech_models.progress.lock().unwrap() = vec![(0.35, "Downloading".to_owned())];
            *fakes.speech_models.download_failure.lock().unwrap() = Some("offline".to_owned());
        })
        .build();
    // The download's one progress report publishes the snapshot the fixture
    // describes; the host then records the failure.
    harness
        .host
        .settings_transcription_download(AssetIdParams {
            asset_id: "offlineDiarizer".to_owned(),
        })
        .unwrap();
    let published = harness.sink.all(BridgeTopic::SettingsTranscription);
    let mid_download = published
        .iter()
        .find(|snapshot| snapshot["assets"][1]["downloadFraction"] == json!(0.35))
        .expect("a snapshot during the download");
    let host_engines = mid_download["engines"].clone();
    assert_parity(
        "settings.transcription",
        mid_download,
        &[
            deviation(
                "/engineID",
                json!("parakeet-v3"),
                "the engine id is `Settings.speechEngineID`'s value; the fixture shortens it",
            ),
            deviation(
                "/engines",
                host_engines,
                "engines carry `SpeechSettingsViewModel.engineTitle` (\"Parakeet · fast · 25 languages\")",
            ),
            deviation(
                "/assets/0/name",
                json!("Speech recognition"),
                "asset names are `componentTitle`, what the component does",
            ),
            deviation(
                "/assets/0/detail",
                json!("Installed · 485 MB"),
                "asset details are `statusText`",
            ),
            deviation("/assets/1/name", json!("Speaker recognition"), "as above"),
            deviation("/assets/1/detail", json!("Downloading… 35%"), "as above"),
            deviation(
                "/subtitle",
                json!("Download needed"),
                "the sidebar says so until both models are installed",
            ),
        ],
    );
}

fn presets_deviation() -> Deviation {
    deviation(
        "/presets",
        json!([
            {"id": "lmStudio", "modelPlaceholder": "the model loaded in LM Studio", "needsAPIKey": false, "showsServerField": true, "title": "LM Studio on this Mac"},
            {"id": "ollama", "modelPlaceholder": "llama3.1", "needsAPIKey": false, "showsServerField": true, "title": "Ollama on this Mac"},
            {"id": "codex", "modelPlaceholder": "pick a model", "needsAPIKey": false, "showsServerField": false, "title": "ChatGPT (Codex)"},
            {"id": "openRouter", "modelPlaceholder": "openai/gpt-4.1-mini", "needsAPIKey": true, "showsServerField": false, "title": "OpenRouter"},
            {"id": "openAI", "modelPlaceholder": "gpt-4.1-mini", "needsAPIKey": true, "showsServerField": false, "title": "OpenAI"},
            {"id": "anthropic", "modelPlaceholder": "claude-sonnet-5", "needsAPIKey": true, "showsServerField": false, "title": "Anthropic"},
            {"id": "custom", "modelPlaceholder": "model name", "needsAPIKey": false, "showsServerField": true, "title": "Custom server"}
        ]),
        "the presets are `LLMPreset.allCases`, seven; the fixture lists three",
    )
}

#[test]
fn settings_summaries() {
    let harness = Harness::builder().build();
    assert_parity(
        "settings.summaries",
        &harness.snapshot(BridgeTopic::SettingsSummaries),
        &[presets_deviation()],
    );
}

#[test]
fn settings_summaries_codex() {
    let harness = Harness::builder()
        .seed(|store, fakes| {
            let mut settings = store.settings().unwrap();
            settings.llm_provider = LlmProvider::Codex;
            settings.codex_model = Some("gpt-5.1-codex".to_owned());
            settings.codex_confirmed_at = Some(date("2026-09-28T10:00:00.000Z"));
            store.save_settings(&settings).unwrap();
            *fakes.llm.codex_account.lock().unwrap() = Ok("nicolai@example.com (Plus)".to_owned());
            *fakes.llm.codex_models.lock().unwrap() = Ok(vec![
                CodexModel {
                    slug: "gpt-5.1-codex".to_owned(),
                    display_name: "GPT-5.1 Codex".to_owned(),
                    context_window: Some(272_000),
                },
                CodexModel {
                    slug: "gpt-5.1-codex-mini".to_owned(),
                    display_name: "GPT-5.1 Codex mini".to_owned(),
                    context_window: None,
                },
            ]);
            *fakes.llm.probe_result.lock().unwrap() =
                Ok("Connected. 3 models listed; structured output works.".to_owned());
        })
        .build();
    harness
        .host
        .settings_summaries_refresh_codex_models()
        .unwrap();
    harness.host.settings_summaries_test().unwrap();
    assert_parity(
        "settings.summaries.codex",
        &harness.snapshot(BridgeTopic::SettingsSummaries),
        &[presets_deviation()],
    );
}

#[test]
fn settings_export() {
    let harness = Harness::builder().build();
    harness
        .host
        .settings_export_update(ExportUpdateParams {
            people_folder: Some("People".to_owned()),
            include_audio: None,
            task_tag: Some("#steno".to_owned()),
        })
        .unwrap();
    assert_parity(
        "settings.export",
        &harness.snapshot(BridgeTopic::SettingsExport),
        &[],
    );
}

fn phone_harness() -> Harness {
    Harness::builder()
        .with_handover("steno-mac-7f3a", 52_431)
        .seed(|_, fakes| {
            let handover = fakes.handover.as_ref().unwrap();
            handover.pair(paired_phone());
            handover.start().unwrap();
            *fakes.qr.png_base64.lock().unwrap() = Some(
                fixture("settings.iphone.pairing")["pairing"]["qrPNGBase64"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        })
        .build()
}

#[test]
fn settings_iphone() {
    let harness = phone_harness();
    assert_parity(
        "settings.iphone",
        &harness.snapshot(BridgeTopic::SettingsPhone),
        &[deviation(
            "/subtitle",
            json!("1 iPhone paired"),
            "the sidebar counts paired phones (`SettingsOverviewViewModel.subtitles`); the fixture names the phone",
        )],
    );
}

#[test]
fn settings_iphone_pairing() {
    let harness = phone_harness();
    harness.host.settings_phone_begin_pairing().unwrap();
    *harness
        .fakes
        .handover
        .as_ref()
        .unwrap()
        .receipts
        .lock()
        .unwrap() = vec![HandoverReceipt {
        recording_id: uuid(0x29),
        device_id: uuid(PHONE),
        state: HandoverState::Receiving,
        byte_count: 25_000_000,
        sha256: vec![0; 32],
        chunk_size: 500_000,
        received_chunks: (0..25).collect(),
        created_at: now(),
        updated_at: now(),
    }];
    harness.host.phones_changed();
    assert_parity(
        "settings.iphone.pairing",
        &harness.snapshot(BridgeTopic::SettingsPhone),
        &[deviation(
            "/subtitle",
            json!("1 iPhone paired"),
            "the sidebar counts paired phones (`SettingsOverviewViewModel.subtitles`); the fixture names the phone",
        )],
    );
}

const RETENTION_SENTENCE: &str = "Each recording is deleted 30 days after it was processed and exported. Transcripts, summaries and exports are never deleted by this rule. Change this any time in Settings > Audio.";

#[test]
fn onboarding_permissions() {
    let harness = Harness::builder()
        .seed(|store, fakes| {
            set_retention(store, AudioRetention::KeepDays(30));
            for kind in PermissionKind::ALL {
                fakes.permissions.set(*kind, PermissionState::Unknown);
            }
            fakes
                .permissions
                .set(PermissionKind::Microphone, PermissionState::Granted);
            fakes
                .permissions
                .answer(PermissionKind::SystemAudio, PermissionState::Unknown);
        })
        .build();
    harness
        .host
        .onboarding_skip(steno_bridge::PermissionKindParams {
            kind: PermissionKind::LocalNetwork,
        })
        .unwrap();
    // The prompt is up: the fake calls back while the host waits on it,
    // which is when the page sees `isRequesting`.
    harness.sink.clear();
    harness
        .host
        .onboarding_request(steno_bridge::PermissionKindParams {
            kind: PermissionKind::SystemAudio,
        })
        .unwrap();
    let published = harness.sink.all(BridgeTopic::Onboarding);
    assert_eq!(
        published.len(),
        2,
        "one publish while requesting, one after"
    );
    assert_parity(
        "onboarding",
        &published[0],
        &[
            removed(
                "/summaries",
                "page 1 never carries the Summaries form (`OnboardingSnapshot.init` sends it on page 2 only)",
            ),
            removed("/vault", "as above for the vault row"),
            deviation(
                "/retentionSentence",
                json!(RETENTION_SENTENCE),
                "the sentence is the Audio footnote plus the Settings pointer; the fixture drops the footnote's second sentence",
            ),
        ],
    );
}

#[test]
fn onboarding_setup() {
    let harness = Harness::builder()
        .choose(Some("/Users/nicolai/Notes/Work Vault"))
        .seed(|store, fakes| {
            set_retention(store, AudioRetention::KeepDays(30));
            fakes
                .permissions
                .set(PermissionKind::LocalNetwork, PermissionState::Unknown);
            *fakes.export_validator.failure.lock().unwrap() = Some(
                "The Obsidian vault at /Users/nicolai/Notes/Work Vault does not exist.".to_owned(),
            );
        })
        .build();
    harness
        .host
        .onboarding_skip(steno_bridge::PermissionKindParams {
            kind: PermissionKind::LocalNetwork,
        })
        .unwrap();
    harness
        .host
        .settings_summaries_update(SummariesUpdateParams {
            base_url: None,
            model: Some("qwen3-8b".to_owned()),
            context_tokens: None,
            api_key: None,
        })
        .unwrap();
    harness.host.onboarding_save_summaries().unwrap();
    let reply = harness.host.onboarding_choose_vault().unwrap();
    assert_eq!(
        reply.path.as_deref(),
        Some("/Users/nicolai/Notes/Work Vault")
    );
    assert_parity(
        "onboarding.setup",
        &harness.snapshot(BridgeTopic::Onboarding),
        &[
            deviation(
                "/retentionSentence",
                json!(RETENTION_SENTENCE),
                "the sentence is the Audio footnote plus the Settings pointer; the fixture drops the footnote's second sentence",
            ),
            Deviation {
                pointer: "/summaries/presets",
                host_value: presets_deviation().host_value,
                reason: presets_deviation().reason,
            },
        ],
    );
}

#[test]
fn speakers_options_reply() {
    let harness = sample_harness();
    let _ = harness.snapshot(BridgeTopic::MeetingDetail);
    let reply = harness
        .host
        .speakers_options(SpeakerOptionsParams {
            speaker_id: uuid(SPEAKER_ANNA),
            query: "an".to_owned(),
        })
        .unwrap();
    let host = serde_json::to_value(&reply).unwrap();
    assert_parity(
        "speakers.options.reply",
        &host,
        &[deviation(
            "/options",
            json!([
                {"detail": "Sounds like", "kind": "person", "label": "Anna", "personID": "00000000-0000-0000-0000-00000000000C"},
                {"kind": "create", "label": "an"}
            ]),
            "the rows are `SpeakerOptions.build`'s: the voice match tagged \"Sounds like\", a create row named after the query; the fixture's \"Suggested, 87% match\", \"Add “…”\" and the unknown row are labels the host never produces",
        )],
    );
}

#[test]
fn reply_confirm() {
    let harness = Harness::builder().confirm(true).build();
    let reply = harness
        .host
        .ui_confirm_destructive(ConfirmDestructiveParams {
            title: "Delete this meeting?".to_owned(),
            message: "The recording, transcript and summary are removed.".to_owned(),
            confirm_title: "Delete".to_owned(),
        })
        .unwrap();
    assert_parity("reply.confirm", &serde_json::to_value(reply).unwrap(), &[]);
}

#[test]
fn reply_chosen_path() {
    let harness = Harness::builder()
        .choose(Some("/Users/nicolai/Notes"))
        .build();
    let reply = harness.host.settings_export_choose_vault().unwrap();
    assert_parity(
        "reply.chosenPath",
        &serde_json::to_value(reply).unwrap(),
        &[],
    );
    let _ = SetStringParams {
        value: String::new(),
    };
}
