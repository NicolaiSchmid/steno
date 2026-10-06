//! The synthetic meeting every adapter golden is rendered from: the port of
//! `Tests/StenoAdaptersTests/Support/FixtureMeeting.swift`. Sequential
//! UUIDs, fixed dates, invented text; `Tests/Fixtures/meetings/
//! produktstrategie.json` is its `StenoJSON` encoding as the Swift CLI
//! writes it; plus the golden, temp-folder and store helpers the test files
//! share.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone as _, Utc};
use chrono_tz::Tz;
use steno_adapters::rendering::{LinkStyle, RenderOptions};
use steno_core::*;
use uuid::Uuid;

/// `00000000-0000-0000-0000-000000000001` for `n == 1`; Swift's
/// `SampleData.uuid`.
pub fn uuid(n: u32) -> Uuid {
    let mut bytes = [0u8; 16];
    let [_, _, high, low] = n.to_be_bytes();
    bytes[14] = high;
    bytes[15] = low;
    Uuid::from_bytes(bytes)
}

pub fn epoch(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).unwrap()
}

pub const MEETING_ID: u32 = 1;
pub const ANNA_ID: u32 = 10;
pub const NICOLAI_ID: u32 = 11;
pub const SPEAKER_ME_ID: u32 = 20;
pub const SPEAKER_ONE_ID: u32 = 21;
pub const SPEAKER_TWO_ID: u32 = 22;

pub fn meeting_id() -> Uuid {
    uuid(MEETING_ID)
}
pub fn anna_id() -> Uuid {
    uuid(ANNA_ID)
}
pub fn nicolai_id() -> Uuid {
    uuid(NICOLAI_ID)
}
pub fn speaker_me_id() -> Uuid {
    uuid(SPEAKER_ME_ID)
}
pub fn speaker_one_id() -> Uuid {
    uuid(SPEAKER_ONE_ID)
}
pub fn speaker_two_id() -> Uuid {
    uuid(SPEAKER_TWO_ID)
}

/// 2026-09-24T12:00:00Z, 14:00 in Berlin.
pub fn started_at() -> DateTime<Utc> {
    epoch(1_790_251_200)
}
pub fn created_at() -> DateTime<Utc> {
    epoch(1_790_256_600)
}
pub fn updated_at() -> DateTime<Utc> {
    epoch(1_790_257_200)
}
/// 2026-10-01T12:00:00Z and 2026-10-15T12:00:00Z: noon, so the day is the
/// same in Berlin and UTC.
pub fn due_october_first() -> DateTime<Utc> {
    epoch(1_790_856_000)
}
pub fn due_october_fifteenth() -> DateTime<Utc> {
    epoch(1_792_065_600)
}

pub const TITLE: &str = "Produktstrategie: \"90/10\" & Roadmap für Q4";
pub const FOLDER_SLUG: &str = "2026-09-24-produktstrategie-90-10-roadmap-fuer-q4";
pub const FOLDER: &str = "Meetings/2026-09-24-produktstrategie-90-10-roadmap-fuer-q4";

pub const BERLIN: Tz = Tz::Europe__Berlin;

/// Plain names, no people, no tag, UTC: the `PLAIN` preset.
pub fn plain() -> RenderOptions {
    RenderOptions::PLAIN
}
pub fn plain_berlin() -> RenderOptions {
    RenderOptions {
        time_zone: BERLIN,
        ..RenderOptions::PLAIN
    }
}
/// What the Obsidian destination uses with a people folder, in Berlin, on
/// the Mac.
pub fn wikilink() -> RenderOptions {
    RenderOptions {
        link_style: LinkStyle::Wikilink,
        person_pages: true,
        task_tag: None,
        time_zone: BERLIN,
        platform: Platform::Macos,
    }
}
pub fn wikilink_utc() -> RenderOptions {
    RenderOptions {
        time_zone: Tz::UTC,
        ..wikilink()
    }
}
pub fn wikilink_tag() -> RenderOptions {
    RenderOptions {
        task_tag: Some("task".to_owned()),
        ..wikilink()
    }
}

pub fn export() -> MeetingExport {
    MeetingExport {
        schema_version: MeetingExport::CURRENT_SCHEMA_VERSION,
        meeting: meeting(),
        participants: participants(),
        speakers: speakers(),
        persons: persons(),
        segments: segments(),
        tasks: tasks(),
        decisions: decisions(),
        audio: Some(audio()),
    }
}

pub fn meeting() -> Meeting {
    Meeting {
        id: meeting_id(),
        title: TITLE.to_owned(),
        started_at: started_at(),
        duration: 5400.0,
        language: Some("de".into()),
        source: MeetingSource::MacCall,
        calendar_event_id: Some("event-42".to_owned()),
        tags: vec!["Kunde ACME".to_owned(), "#q4".to_owned()],
        state: MeetingState::Ready,
        end_reason: None,
        title_origin: TitleOrigin::Default,
        template_id: Meeting::DEFAULT_TEMPLATE_ID.to_owned(),
        summary: Some(SummaryDocument {
            template_id: Meeting::DEFAULT_TEMPLATE_ID.to_owned(),
            language: Some("de".into()),
            sections: vec![
                SummarySection {
                    id: "executive-summary".to_owned(),
                    heading: "Executive Summary".to_owned(),
                    bullets: vec![
                        SummaryBullet {
                            lead: "Fokus".to_owned(),
                            text: "Speaker 1 setzt 90 Prozent auf den Kern und 10 auf Experimente."
                                .to_owned(),
                        },
                        SummaryBullet {
                            lead: "Protokoll".to_owned(),
                            text: "Speaker 2 verteilt das Protokoll nach dem Termin.".to_owned(),
                        },
                    ],
                },
                SummarySection {
                    id: "next-steps".to_owned(),
                    heading: "Nächste Schritte".to_owned(),
                    bullets: vec![SummaryBullet {
                        lead: String::new(),
                        text: "Angebot an ACME bis 1. Oktober; Roadmap-Folien bis Mitte Oktober."
                            .to_owned(),
                    }],
                },
            ],
        }),
        scratchpad:
            "Nachfassen wegen Budget.\n\n---\n\n## Summary\n\nNicht vergessen: Jérôme fragen."
                .to_owned(),
        llm_usage: Some(LlmUsage {
            prompt_tokens: 4200,
            completion_tokens: 900,
            requests: 5,
        }),
        created_at: created_at(),
        updated_at: updated_at(),
    }
}

/// Ordered by display name, as `Store::export` returns them.
pub fn participants() -> Vec<Participant> {
    vec![
        Participant {
            id: uuid(30),
            meeting_id: meeting_id(),
            person_id: Some(anna_id()),
            display_name: "Anna Müller".to_owned(),
            role: ParticipantRole::Them,
            email: Some("anna@example.com".to_owned()),
        },
        Participant {
            id: uuid(32),
            meeting_id: meeting_id(),
            person_id: None,
            display_name: "Jérôme Dupont".to_owned(),
            role: ParticipantRole::Them,
            email: None,
        },
        Participant {
            id: uuid(31),
            meeting_id: meeting_id(),
            person_id: Some(nicolai_id()),
            display_name: "Nicolai Schmid".to_owned(),
            role: ParticipantRole::Me,
            email: Some("nicolai@example.com".to_owned()),
        },
    ]
}

/// Ordered by display name.
pub fn persons() -> Vec<Person> {
    vec![
        Person {
            id: anna_id(),
            display_name: "Anna Müller".to_owned(),
            email: Some("anna@example.com".to_owned()),
            embedding: None,
            sample_count: 4,
            created_at: created_at(),
        },
        Person {
            id: nicolai_id(),
            display_name: "Nicolai Schmid".to_owned(),
            email: Some("nicolai@example.com".to_owned()),
            embedding: None,
            sample_count: 9,
            created_at: created_at(),
        },
    ]
}

/// Ordered by cluster label.
pub fn speakers() -> Vec<Speaker> {
    vec![
        Speaker {
            id: speaker_me_id(),
            meeting_id: meeting_id(),
            cluster_label: "Me".to_owned(),
            assignment: SpeakerAssignment::Confirmed {
                person_id: nicolai_id(),
            },
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: None,
            cluster_confidence: 1.0,
        },
        Speaker {
            id: speaker_one_id(),
            meeting_id: meeting_id(),
            cluster_label: "Speaker 1".to_owned(),
            assignment: SpeakerAssignment::Confirmed {
                person_id: anna_id(),
            },
            embedding: None,
            sample_clip_range: Some(TimeRange {
                lower: 4.5,
                upper: 14.5,
            }),
            sample_clip_url: None,
            cluster_confidence: 0.88,
        },
        Speaker {
            id: speaker_two_id(),
            meeting_id: meeting_id(),
            cluster_label: "Speaker 2".to_owned(),
            assignment: SpeakerAssignment::Unknown,
            embedding: None,
            sample_clip_range: Some(TimeRange {
                lower: 30.5,
                upper: 36.0,
            }),
            sample_clip_url: Some(format!(
                "file:///tmp/steno/{}/speakers/{}.wav",
                json::uuid_string(meeting_id()),
                json::uuid_string(speaker_two_id())
            )),
            cluster_confidence: 0.7,
        },
    ]
}

/// Fourteen segments over both lanes: consecutive turns, a gap of more than
/// three seconds inside a turn, one zero-length segment, one without a
/// speaker, and texts that start with `#`, `-`, `>` and `1.` or carry `<`,
/// `&` and `-->`.
#[allow(clippy::too_many_lines)]
pub fn segments() -> Vec<TranscriptSegment> {
    let segment =
        |n: u32, start: f64, end: f64, speaker: Option<Uuid>, lane: AudioLane, text: &str| {
            TranscriptSegment {
                id: uuid(40 + n),
                meeting_id: meeting_id(),
                start,
                end,
                speaker_id: speaker,
                lane,
                text: text.to_owned(),
                raw_text: text.to_lowercase(),
            }
        };
    let me = Some(speaker_me_id());
    let one = Some(speaker_one_id());
    let two = Some(speaker_two_id());
    vec![
        segment(
            0,
            0.0,
            4.2,
            me,
            AudioLane::Mic,
            "Guten Morgen zusammen, fangen wir mit der Roadmap an.",
        ),
        segment(
            1,
            4.5,
            9.8,
            one,
            AudioLane::System,
            "Gern. Ich habe die Zahlen für Q4 dabei.",
        ),
        segment(
            2,
            10.0,
            15.2,
            one,
            AudioLane::System,
            "Der Kern bekommt 90 Prozent, der Rest 10.",
        ),
        segment(
            3,
            19.0,
            24.0,
            one,
            AudioLane::System,
            "Das ACME-Angebot muss bis zum 1. Oktober raus.",
        ),
        segment(
            4,
            24.5,
            30.0,
            me,
            AudioLane::Mic,
            "# Punkt eins: Budget <Kern> & Rest --> offen",
        ),
        segment(
            5,
            30.5,
            36.0,
            two,
            AudioLane::System,
            "Ich kann das Protokoll verteilen.",
        ),
        segment(6, 36.0, 36.0, two, AudioLane::System, "Okay."),
        segment(
            7,
            37.0,
            39.5,
            None,
            AudioLane::System,
            "- Notiz: Einwurf aus dem Off.",
        ),
        segment(
            8,
            40.0,
            46.0,
            me,
            AudioLane::Mic,
            "1. Wir starten im Oktober.",
        ),
        segment(
            9,
            46.5,
            52.0,
            one,
            AudioLane::System,
            "> Zitat aus dem Kundenbrief.",
        ),
        segment(
            10,
            52.5,
            58.0,
            me,
            AudioLane::Mic,
            "Jérôme, kannst du das Budget freigeben lassen?",
        ),
        segment(
            11,
            60.0,
            66.0,
            two,
            AudioLane::System,
            "Ja, ich kümmere mich darum.",
        ),
        segment(
            12,
            66.5,
            70.0,
            one,
            AudioLane::System,
            "Dann sind wir durch.",
        ),
        segment(
            13,
            70.5,
            72.0,
            me,
            AudioLane::Mic,
            "Danke, bis nächste Woche.",
        ),
    ]
}

/// Every priority, with and without due date and assignee, one done, one
/// with a line break in its text.
pub fn tasks() -> Vec<MeetingTask> {
    vec![
        MeetingTask {
            id: uuid(50),
            meeting_id: meeting_id(),
            text: "Angebot an ACME schicken".to_owned(),
            assignee_person_id: Some(anna_id()),
            assignee_name: Some("Anna".to_owned()),
            priority: TaskPriority::High,
            due_date: Some(due_october_first()),
            done: false,
        },
        MeetingTask {
            id: uuid(51),
            meeting_id: meeting_id(),
            text: "Roadmap-Folien aktualisieren".to_owned(),
            assignee_person_id: Some(nicolai_id()),
            assignee_name: Some("Nicolai Schmid".to_owned()),
            priority: TaskPriority::Normal,
            due_date: Some(due_october_fifteenth()),
            done: false,
        },
        MeetingTask {
            id: uuid(52),
            meeting_id: meeting_id(),
            text: "Protokoll verteilen".to_owned(),
            assignee_person_id: None,
            assignee_name: None,
            priority: TaskPriority::Low,
            due_date: None,
            done: true,
        },
        MeetingTask {
            id: uuid(53),
            meeting_id: meeting_id(),
            text: "Budget\nfreigeben lassen".to_owned(),
            assignee_person_id: None,
            assignee_name: Some("Jérôme Dupont".to_owned()),
            priority: TaskPriority::Normal,
            due_date: None,
            done: false,
        },
    ]
}

pub fn decision_texts() -> Vec<String> {
    vec![
        "90/10-Aufteilung wird umgesetzt.".to_owned(),
        "Roadmap-Review am 15. Oktober.".to_owned(),
    ]
}

pub fn decisions() -> Vec<Decision> {
    decision_texts()
        .into_iter()
        .enumerate()
        .map(|(index, text)| Decision {
            id: derived_uuid(meeting_id(), &format!("decision-{index}")),
            meeting_id: meeting_id(),
            text,
        })
        .collect()
}

pub fn audio() -> AudioAsset {
    let folder = format!("file:///tmp/steno/{}", json::uuid_string(meeting_id()));
    AudioAsset {
        id: uuid(70),
        meeting_id: meeting_id(),
        url: format!("{folder}/recording.caf"),
        format: AudioFormat::Caf48kFloat32,
        lanes: vec![AudioLane::Mic, AudioLane::System],
        sidecars_16k: BTreeMap::from([
            (AudioLane::Mic, format!("{folder}/mic.wav")),
            (AudioLane::System, format!("{folder}/system.wav")),
        ]),
        mixdown_url: Some(format!("{folder}/audio.m4a")),
        retention: AudioRetention::KeepDays(30),
        expires_at: Some(epoch(1_792_849_200)),
    }
}

/// The bytes of the test mixdown: 0 to 99.
pub fn mixdown_bytes() -> Vec<u8> {
    (0..100).collect()
}

/// The export with its mixdown pointing at a fresh 100-byte file under
/// `directory`, for delivery tests that copy audio.
pub fn export_with_audio_in(directory: &Path) -> MeetingExport {
    let mut export = export();
    let mixdown = directory.join("audio.m4a");
    std::fs::write(&mixdown, mixdown_bytes()).unwrap();
    export.audio.as_mut().unwrap().mixdown_url = Some(steno_core::paths::file_url(&mixdown, false));
    export
}

/// The repository root, from the crate's manifest directory.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A file under `Tests/Fixtures/`.
pub fn fixture(relative: &str) -> Vec<u8> {
    let path = repo_root().join("Tests/Fixtures").join(relative);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The folder note golden `variant` (`plain-utc`, `wikilink-berlin`) for a
/// call recorded on `platform`: the Mac's in `snapshots/obsidian`, shared
/// with the Swift renderer; the others in `snapshots/platforms/<platform>`.
pub fn folder_note_golden(platform: Platform, variant: &str) -> String {
    match platform {
        Platform::Macos => format!("snapshots/obsidian/folder-note-{variant}.md"),
        Platform::Windows | Platform::Linux => {
            format!("snapshots/platforms/{platform}/folder-note-{variant}.md")
        }
    }
}

pub fn fixture_text(relative: &str) -> String {
    String::from_utf8(fixture(relative)).unwrap()
}

/// `actual` equals the golden at `Tests/Fixtures/<relative>`, with a
/// readable diff when it does not.
pub fn assert_matches_golden(actual: &str, relative: &str) {
    use std::fmt::Write as _;
    let expected = fixture_text(relative);
    if actual != expected {
        let mut report = format!("{relative} differs from the golden:\n");
        for (line, (left, right)) in expected.lines().zip(actual.lines()).enumerate() {
            if left != right {
                let _ = writeln!(
                    report,
                    "line {}:
  golden: {left}
  actual: {right}",
                    line + 1
                );
            }
        }
        if expected.lines().count() != actual.lines().count() {
            let _ = writeln!(
                report,
                "golden has {} lines, actual {}",
                expected.lines().count(),
                actual.lines().count()
            );
        }
        panic!("{report}");
    }
}

/// Writes the fixture meeting into `store` the way the pipeline does:
/// persons, the meeting with its participants and asset, the transcript,
/// the summary with its tasks and decisions.
pub fn populate(store: &Store) -> MeetingExport {
    let export = export();
    for person in &export.persons {
        store.save_person(person).unwrap();
    }
    store
        .save_meeting_with_participants(&export.meeting, &export.participants)
        .unwrap();
    store.save_asset(export.audio.as_ref().unwrap()).unwrap();
    store
        .replace_transcript(&export.meeting, &export.segments, &export.speakers)
        .unwrap();
    store
        .replace_summary(&export.meeting, &export.tasks, &decision_texts(), &[])
        .unwrap();
    export
}

pub fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("steno-adapters-{label}-"))
        .tempdir()
        .unwrap()
}

/// The names in a directory, sorted.
pub fn list(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
