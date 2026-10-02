//! The summary pass over the stub server: the single-shot path and its
//! post-processing, repair, truncation, templates, assignees, dates,
//! suggestions, language, usage, and the map and reduce path on the
//! 60-minute fixture.
//! Swift: `SummaryTests`, `SummaryContractTests`,
//! `NamesTasksLanguageUsageTests`, `MapReduceTests`, `MapReduceEdgeTests`.

mod common;

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use common::*;
use serde_json::json;
use steno_core::{
    LanguageTag, LlmFinishReason, LlmResponse, LlmUsage, MeetingSummarizer, SummaryBullet,
    SummaryInput, SummaryTemplate, TaskPriority, TranscriptCleaner,
};
use steno_llm::inputs::{cleanup_input, summary_input};
use steno_llm::labels::SpeakerLabels;
use steno_llm::summary::{DraftBullet, DraftSection, NotesTopic};
use steno_llm::testing::{StubChatServer, scripts};
use steno_llm::{
    AnalysisDraft, ChunkNotes, DraftSpeakerName, DraftTask, LlmEndpoint, LlmError,
    LlmMeetingSummarizer, LlmTranscriptCleaner, OpenAiCompatibleClient, RetryPolicy,
    StructuredOutputDecoder, SummaryPromptBuilder, TokenBudget, TranscriptChunker,
};

fn summarizer(server: &StubChatServer, context_tokens: i64) -> LlmMeetingSummarizer<Utc> {
    let endpoint = LlmEndpoint {
        context_tokens,
        ..LlmEndpoint::new(server.base_url().clone(), "stub-model")
    };
    let client = OpenAiCompatibleClient::new(endpoint.clone(), None).with_retry(RetryPolicy::NONE);
    LlmMeetingSummarizer::new(Arc::new(client), endpoint, Utc)
}

fn default_input() -> SummaryInput {
    summary_input(&standup(), SummaryTemplate::bundled_with_id("default"))
}

fn usage(prompt: i64, completion: i64) -> LlmUsage {
    LlmUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        requests: 1,
    }
}

fn empty_draft() -> AnalysisDraft {
    AnalysisDraft {
        title: String::new(),
        sections: Vec::new(),
        decisions: Vec::new(),
        tasks: Vec::new(),
        speaker_names: Vec::new(),
    }
}

fn section(id: &str, heading: &str, bullets: &[(&str, &str)]) -> DraftSection {
    DraftSection {
        id: id.to_owned(),
        heading: heading.to_owned(),
        bullets: bullets
            .iter()
            .map(|(lead, text)| DraftBullet {
                lead: (*lead).to_owned(),
                text: (*text).to_owned(),
            })
            .collect(),
    }
}

fn speaker_name(
    label: &str,
    name: Option<&str>,
    confidence: f64,
    evidence: &str,
) -> DraftSpeakerName {
    DraftSpeakerName {
        speaker_label: label.to_owned(),
        name: name.map(str::to_owned),
        confidence,
        evidence: evidence.to_owned(),
    }
}

fn task(
    text: &str,
    assignee: Option<&str>,
    priority: TaskPriority,
    due: Option<&str>,
) -> DraftTask {
    DraftTask {
        text: text.to_owned(),
        assignee: assignee.map(str::to_owned),
        priority,
        due_date: due.map(str::to_owned),
    }
}

fn midnight(year: i32, month: u32, day: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
}

fn purposes(server: &StubChatServer) -> Vec<String> {
    server
        .requests()
        .iter()
        .map(|r| r.purpose.clone().unwrap_or_default())
        .collect()
}

// One canned answer, every post-processing rule checked against it.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn single_shot_builds_the_summary_output_from_the_draft() {
    let server = StubChatServer::start().await.unwrap();
    let reported = usage(1_500, 400);
    server.enqueue([scripts.completion(
        &canned("summary-default-standup"),
        Some("stop"),
        Some(reported),
        "stub-model",
    )]);
    let input = default_input();
    let output = summarizer(&server, 32_000).summarize(&input).await.unwrap();

    assert_eq!(server.request_count(), 1);
    let chat = server.requests()[0].chat.clone().unwrap();
    assert_eq!(server.requests()[0].purpose.as_deref(), Some("summary"));
    assert_eq!(chat.temperature, Some(0.2));
    assert_eq!(
        chat.response_format.unwrap().json_schema.unwrap().name,
        "meeting_analysis"
    );
    assert_eq!(output.usage, reported);
    assert_eq!(
        output.title,
        "Daily Standup: Onboarding, CI und Obsidian-Export"
    );
    assert_eq!(output.language, Some(de()));
    assert_eq!(output.summary.template_id, "default");
    assert_eq!(output.summary.language, Some(de()));

    // Sections: template order, unknown `wrap-up` dropped, empty optional
    // `open-questions` dropped, translated heading kept.
    let ids: Vec<&str> = output
        .summary
        .sections
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(ids, ["executive-summary", "full-summary"]);
    assert_eq!(
        output.summary.sections[1].heading,
        "Vollständige Zusammenfassung"
    );
    assert!(
        output
            .summary
            .sections
            .iter()
            .flat_map(|s| s.bullets.iter())
            .all(|b| !b.lead.is_empty())
    );
    assert!(
        output.summary.sections[0].bullets[0]
            .text
            .contains("Speaker 2")
    );

    assert_eq!(output.decisions.len(), 2, "duplicate decisions collapse");
    assert_eq!(output.tasks.len(), 4, "the blank task is dropped");
    assert_eq!(
        output.tasks[0].due_date, None,
        "\"next Friday\" is not a date"
    );
    assert_eq!(output.tasks[0].priority, TaskPriority::High);
    assert_eq!(
        output.tasks[0].assignee_name.as_deref(),
        Some("Speaker 2"),
        "an unconfirmed speaker keeps its label"
    );
    assert_eq!(output.tasks[0].assignee_person_id, None);
    assert_eq!(output.tasks[1].assignee_name.as_deref(), Some("Jérôme"));
    assert_eq!(
        output.tasks[1].assignee_person_id,
        Some(sample_uuid(PERSON_JEROME))
    );
    assert_eq!(
        output.tasks[1].due_date,
        Some(input.meeting.started_at - chrono::TimeDelta::hours(9)),
        "midnight UTC"
    );
    assert_eq!(
        output.tasks[2].assignee_name.as_deref(),
        Some("Jérôme"),
        "case-insensitive"
    );
    assert_eq!(output.tasks[2].due_date, Some(midnight(2026, 9, 30)));
    assert_eq!(output.tasks[3].priority, TaskPriority::Low);
    assert_eq!(
        output.tasks[3].assignee_person_id,
        Some(sample_uuid(PERSON_MARA))
    );
    assert!(
        output
            .tasks
            .iter()
            .all(|t| t.meeting_id == input.meeting.id)
    );
    assert!(output.tasks.iter().all(|t| !t.done));
    let ids: std::collections::HashSet<_> = output.tasks.iter().map(|t| t.id).collect();
    assert_eq!(ids.len(), 4);
    assert_eq!(
        output.tasks[0].id,
        steno_core::derived_uuid(input.meeting.id, "task-0")
    );

    assert_eq!(output.speaker_names.len(), 1);
    assert_eq!(
        output.speaker_names[0].speaker_id,
        sample_uuid(STANDUP_SPEAKER_TWO)
    );
    assert_eq!(output.speaker_names[0].name.as_deref(), Some("Nicolai"));
    assert_eq!(output.speaker_names[0].confidence, 0.9);
}

#[tokio::test]
async fn an_invalid_answer_is_repaired_once_then_fails() {
    let server = StubChatServer::start().await.unwrap();
    let draft: AnalysisDraft = serde_json::from_str(&canned("summary-default-standup")).unwrap();
    server.enqueue([
        scripts.text("{\"title\": \"x\", \"sections\": [}"),
        scripts.fenced(&draft),
    ]);
    let output = summarizer(&server, 32_000)
        .summarize(&default_input())
        .await
        .unwrap();
    assert_eq!(output.summary.sections.len(), 2);
    assert_eq!(purposes(&server), ["summary", "summary-repair"]);
    let repair = server.requests().pop().unwrap().chat.unwrap();
    assert!(
        repair.messages[1]
            .content
            .contains("Validation error: malformed JSON")
    );
    assert!(
        repair.messages[1]
            .content
            .contains("{\"title\": \"x\", \"sections\": [}")
    );
    assert_eq!(output.usage.requests, 2);

    server.enqueue([scripts.text("nope"), scripts.text("still nope")]);
    let error = summarizer(&server, 32_000)
        .summarize(&default_input())
        .await
        .unwrap_err();
    assert!(
        matches!(downcast::<LlmError>(&error), LlmError::InvalidJson(_)),
        "{error}"
    );
    assert_eq!(server.request_count(), 4);
}

#[tokio::test]
async fn truncated_answer_fails_without_repair() {
    let server = StubChatServer::start().await.unwrap();
    server.enqueue([scripts.truncated("{\"title\": \"cut")]);
    let error = summarizer(&server, 32_000)
        .summarize(&default_input())
        .await
        .unwrap_err();
    assert_eq!(downcast::<LlmError>(&error), LlmError::Truncated);
    assert_eq!(server.request_count(), 1);
}

#[test]
fn required_sections_survive_empty_and_headings_fall_back_to_the_template() {
    let draft = AnalysisDraft {
        title: "  ".to_owned(),
        sections: vec![
            section("open-questions", " ", &[("Zeitplan:", " Oktober? ")]),
            section("executive-summary", "", &[("", "")]),
        ],
        decisions: vec![" a ".to_owned(), "A".to_owned(), String::new()],
        ..empty_draft()
    };
    let input = default_input();
    let output = draft.summary_output(&input, LlmUsage::ZERO, 0.3);
    assert_eq!(
        output.title, input.meeting.title,
        "a blank title keeps the meeting's"
    );
    let ids: Vec<&str> = output
        .summary
        .sections
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(ids, ["executive-summary", "open-questions"]);
    assert_eq!(output.summary.sections[0].bullets.len(), 0);
    assert_eq!(output.summary.sections[0].heading, "Executive Summary");
    assert_eq!(output.summary.sections[1].heading, "Open Questions");
    assert_eq!(
        output.summary.sections[1].bullets,
        [SummaryBullet {
            lead: "Zeitplan".to_owned(),
            text: "Oktober?".to_owned()
        }]
    );
    assert_eq!(output.decisions, ["a"]);
}

/// Small models in prompt-only mode sometimes split one section into two
/// blocks with the same id; both halves are kept, in order, under the first
/// heading the model wrote.
#[test]
fn a_section_the_model_split_in_two_is_merged() {
    let draft = AnalysisDraft {
        title: "T".to_owned(),
        sections: vec![
            section("executive-summary", "", &[("A", "a")]),
            section("full-summary", "  ", &[("B", "b")]),
            section("executive-summary", "Kurz", &[("C", "c")]),
            section("full-summary", "Lang", &[("D", "d")]),
        ],
        ..empty_draft()
    };
    let output = draft.summary_output(&default_input(), LlmUsage::ZERO, 0.3);
    let ids: Vec<&str> = output
        .summary
        .sections
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(ids, ["executive-summary", "full-summary"]);
    let leads = |i: usize| -> Vec<&str> {
        output.summary.sections[i]
            .bullets
            .iter()
            .map(|b| b.lead.as_str())
            .collect()
    };
    assert_eq!(leads(0), ["A", "C"]);
    assert_eq!(
        output.summary.sections[0].heading, "Kurz",
        "the first non-blank heading"
    );
    assert_eq!(leads(1), ["B", "D"]);
    assert_eq!(output.summary.sections[1].heading, "Lang");
}

#[test]
fn assignee_resolution_order() {
    let input = summary_input(&customer_call(), None);
    let labels = SpeakerLabels::new(&input.speakers);
    let resolve = |name: Option<&str>| AnalysisDraft::resolve_assignee(name, &input, &labels);
    assert_eq!(resolve(None), None);
    assert_eq!(resolve(Some("  ")), None);
    assert_eq!(resolve(Some("petra vogel")).unwrap().name, "Petra Vogel");
    assert_eq!(
        resolve(Some("Petra")).unwrap().name,
        "Petra Vogel",
        "unique first name"
    );
    assert_eq!(resolve(Some("Tom")).unwrap().name, "Tom Berger");
    assert_eq!(
        resolve(Some("Nicolai")).unwrap().person_id,
        Some(sample_uuid(PERSON_NICOLAI))
    );
    assert_eq!(
        resolve(Some("me")).unwrap().name,
        "Nicolai",
        "the Me speaker is a confirmed person"
    );
    let speaker_one = resolve(Some("Speaker 1")).unwrap();
    assert_eq!(
        speaker_one.name, "Speaker 1",
        "unresolved speaker keeps its label"
    );
    assert_eq!(speaker_one.person_id, None);
    assert_eq!(
        resolve(Some("Jérôme")).unwrap().person_id,
        Some(sample_uuid(PERSON_JEROME)),
        "known person, not a participant"
    );
    let somebody = resolve(Some("Somebody Else")).unwrap();
    assert_eq!(somebody.name, "Somebody Else");
    assert_eq!(somebody.person_id, None);
}

#[test]
fn due_dates_are_strict() {
    let parse = AnalysisDraft::parse_due_date;
    assert_eq!(parse(Some("2026-09-26")), Some(midnight(2026, 9, 26)));
    assert!(parse(Some(" 2026-09-26 ")).is_some());
    assert_eq!(parse(Some("next Friday")), None);
    assert_eq!(parse(Some("2026-9-26")), None);
    assert_eq!(parse(Some("26.09.2026")), None);
    assert_eq!(parse(Some("2026-13-01")), None);
    assert_eq!(parse(Some("2026-02-30")), None);
    assert_eq!(parse(None), None);
    assert_eq!(parse(Some("")), None);
}

#[test]
fn speaker_suggestions_are_filtered_clamped_and_one_per_speaker() {
    let speakers = standup().speakers;
    let labels = SpeakerLabels::new(&speakers);
    let drafts = [
        speaker_name("speaker 3", Some("Jérôme"), 1.7, " quote "),
        speaker_name("Speaker 2", Some("Nico"), 0.4, "a"),
        speaker_name("Speaker 2", Some("Nicolai"), 0.8, "b"),
        speaker_name("Speaker 1", Some("Mara"), 0.29, "c"),
        speaker_name("Speaker 1", Some(""), 0.9, "d"),
        speaker_name("Speaker 7", Some("Ghost"), 0.9, "e"),
    ];
    let result = AnalysisDraft::suggestions(&drafts, &labels, &speakers, 0.3);
    assert_eq!(
        result,
        [
            steno_core::SpeakerNameSuggestion {
                speaker_id: sample_uuid(STANDUP_SPEAKER_TWO),
                name: Some("Nicolai".to_owned()),
                confidence: 0.8,
                evidence: "b".to_owned()
            },
            steno_core::SpeakerNameSuggestion {
                speaker_id: sample_uuid(STANDUP_SPEAKER_THREE),
                name: Some("Jérôme".to_owned()),
                confidence: 1.0,
                evidence: "quote".to_owned()
            },
        ]
    );
}

// Contract beyond the default template

/// A draft that fills every section of `template` in reverse order, with
/// one extra section the template does not know.
fn full_draft(template: &SummaryTemplate) -> AnalysisDraft {
    let mut sections: Vec<DraftSection> = template
        .sections
        .iter()
        .rev()
        .map(|s| {
            section(
                &s.id,
                &format!("Überschrift {}", s.id),
                &[("Punkt", &format!("Speaker 1 sagt etwas zu {}.", s.id))],
            )
        })
        .collect();
    sections.push(section("not-a-section", "Nope", &[("x", "y")]));
    AnalysisDraft {
        title: "Titel: Untertitel".to_owned(),
        sections,
        decisions: vec!["Entschieden.".to_owned()],
        tasks: vec![task(
            "Tun",
            Some("Mara"),
            TaskPriority::Normal,
            Some("2026-10-01"),
        )],
        speaker_names: Vec::new(),
    }
}

#[tokio::test]
async fn every_template_round_trips_its_section_ids() {
    let standup = standup();
    for id in SummaryTemplate::BUNDLED_IDS {
        let template = SummaryTemplate::bundled_with_id(id).unwrap().clone();
        let input = summary_input(&standup, Some(&template));
        let builder = SummaryPromptBuilder::new(template.clone(), Utc);
        let ids: Vec<&str> = template.sections.iter().map(|s| s.id.as_str()).collect();

        // Prompt: the section list names exactly the template's ids in order.
        let system = builder.build_single_shot(&input).messages[0]
            .content
            .clone();
        let listed: Vec<&str> = system
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("- ")?;
                let dash = rest.find(" — \"")?;
                Some(&rest[..dash])
            })
            .collect();
        assert_eq!(listed, ids, "{id}");
        for section in &template.sections {
            assert!(
                system.contains(&section.instructions),
                "{id}/{} instructions",
                section.id
            );
        }
        // Schema: the id enum is the same list, for single shot and reduce.
        let id_enum = &builder.draft_schema().json_value()["properties"]["sections"]["items"]["properties"]
            ["id"]["enum"];
        assert_eq!(
            *id_enum,
            serde_json::Value::Array(ids.iter().map(|i| json!(i)).collect()),
            "{id}"
        );
        let quoted = ids
            .iter()
            .map(|i| format!("\"{i}\""))
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(builder.draft_schema().prompt_text().contains(&quoted));
        assert!(
            builder.build_reduce(&input, &[]).messages[0]
                .content
                .contains(&quoted)
        );

        // Post-processing: template order restored, the unknown id dropped,
        // the model's headings kept.
        let server = StubChatServer::start().await.unwrap();
        server.enqueue([scripts.json(&full_draft(&template), Some(usage(10, 5)))]);
        let output = summarizer(&server, 32_000).summarize(&input).await.unwrap();
        assert_eq!(output.summary.template_id, id);
        let out_ids: Vec<&str> = output
            .summary
            .sections
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(out_ids, ids, "{id}");
        let headings: Vec<String> = output
            .summary
            .sections
            .iter()
            .map(|s| s.heading.clone())
            .collect();
        let expected: Vec<String> = ids.iter().map(|i| format!("Überschrift {i}")).collect();
        assert_eq!(headings, expected);
        assert_eq!(output.title, "Titel: Untertitel");
        assert_eq!(
            output.tasks[0].assignee_person_id,
            Some(sample_uuid(PERSON_MARA))
        );
        assert!(!output.summary.plain_text().contains("Nope"));
    }
}

#[test]
fn required_sections_per_template_survive_an_empty_draft() {
    let standup = standup();
    for template in SummaryTemplate::bundled() {
        let input = summary_input(&standup, Some(template));
        let output = empty_draft().summary_output(&input, LlmUsage::ZERO, 0.3);
        let required: Vec<&str> = template
            .sections
            .iter()
            .filter(|s| s.required)
            .map(|s| s.id.as_str())
            .collect();
        let ids: Vec<&str> = output
            .summary
            .sections
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, required, "{}", template.id);
        assert!(output.summary.sections.iter().all(|s| s.bullets.is_empty()));
        let headings: Vec<&str> = output
            .summary
            .sections
            .iter()
            .map(|s| s.heading.as_str())
            .collect();
        let expected: Vec<&str> = template
            .sections
            .iter()
            .filter(|s| s.required)
            .map(|s| s.heading.as_str())
            .collect();
        assert_eq!(headings, expected, "{}", template.id);
    }
}

#[tokio::test]
async fn an_unknown_priority_is_a_decode_failure_repaired_once_with_the_path() {
    let server = StubChatServer::start().await.unwrap();
    let good = canned("summary-default-standup");
    let broken = good.replace("\"priority\": \"high\"", "\"priority\": \"urgent\"");
    assert_ne!(broken, good);
    server.enqueue([scripts.text(&broken), scripts.text(&good)]);
    let output = summarizer(&server, 32_000)
        .summarize(&default_input())
        .await
        .unwrap();
    assert_eq!(purposes(&server), ["summary", "summary-repair"]);
    let repair = server.requests().pop().unwrap().chat.unwrap();
    assert!(repair.messages[1].content.contains("Validation error: "));
    assert!(
        repair.messages[1].content.contains("tasks.[0].priority"),
        "{}",
        repair.messages[1].content
    );
    assert!(
        repair.messages[1]
            .content
            .contains("\"priority\": \"urgent\"")
    );
    assert_eq!(
        repair.response_format.unwrap().json_schema.unwrap().name,
        "meeting_analysis"
    );
    assert_eq!(output.tasks[0].priority, TaskPriority::High);
    assert_eq!(output.usage.requests, 2);
}

#[test]
fn a_missing_key_or_wrong_type_names_the_path() {
    let decode = |text: &str| {
        StructuredOutputDecoder::decode::<AnalysisDraft>(&LlmResponse {
            text: text.to_owned(),
            finish_reason: LlmFinishReason::Stop,
            usage: None,
            model: None,
        })
        .unwrap_err()
    };
    assert_eq!(
        decode(
            "{\"title\":\"t\",\"language\":\"de\",\"sections\":[],\"decisions\":[],\"tasks\":[]}"
        ),
        LlmError::InvalidJson("missing key speakerNames at root".to_owned())
    );
    let LlmError::InvalidJson(detail) = decode(
        "{\"title\":\"t\",\"language\":\"de\",\"sections\":[{\"id\":\"executive-summary\",\"heading\":\"h\",\"bullets\":\"none\"}],\"decisions\":[],\"tasks\":[],\"speakerNames\":[]}",
    ) else {
        panic!("expected InvalidJson");
    };
    assert!(
        detail.starts_with("expected a sequence at sections.[0].bullets"),
        "{detail}"
    );
    let LlmError::InvalidJson(confidence) = decode(
        "{\"title\":\"t\",\"language\":\"de\",\"sections\":[],\"decisions\":[],\"tasks\":[],\"speakerNames\":[{\"speakerLabel\":\"Speaker 1\",\"name\":null,\"confidence\":\"high\",\"evidence\":\"\"}]}",
    ) else {
        panic!("expected InvalidJson");
    };
    assert!(
        confidence.contains("speakerNames.[0].confidence"),
        "{confidence}"
    );
}

#[test]
fn nullable_fields_decode_as_none() {
    let json = "{\"title\":\"t\",\"sections\":[],\"decisions\":[],\n \"tasks\":[{\"text\":\"x\",\"assignee\":null,\"priority\":\"low\",\"dueDate\":null}],\n \"speakerNames\":[{\"speakerLabel\":\"Speaker 1\",\"name\":null,\"confidence\":0.1,\"evidence\":\"q\"}]}";
    let draft: AnalysisDraft = StructuredOutputDecoder::decode(&LlmResponse {
        text: json.to_owned(),
        finish_reason: LlmFinishReason::Stop,
        usage: None,
        model: None,
    })
    .unwrap();
    assert_eq!(draft.tasks[0].assignee, None);
    assert_eq!(draft.tasks[0].due_date, None);
    assert_eq!(draft.speaker_names[0].name, None);
    let output = draft.summary_output(&default_input(), LlmUsage::ZERO, 0.3);
    assert_eq!(output.tasks[0].assignee_name, None);
    assert_eq!(output.tasks[0].due_date, None);
    assert_eq!(
        output.speaker_names.len(),
        0,
        "a suggestion without a name is dropped"
    );
}

#[test]
fn the_meeting_date_line_follows_the_summarizer_time_zone() {
    fn date_line<Tz: TimeZone>(input: &SummaryInput, zone: Tz) -> String {
        SummaryPromptBuilder::new(input.template.clone(), zone)
            .build_single_shot(input)
            .messages[0]
            .content
            .lines()
            .find(|line| line.starts_with("Meeting date: "))
            .unwrap()
            .to_owned()
    }
    // The fixture starts at 09:00 UTC on Thursday 2026-09-24.
    let input = default_input();
    assert_eq!(input.meeting.started_at.timestamp(), 1_790_240_400);
    assert!(date_line(&input, Utc).starts_with("Meeting date: 2026-09-24 (Thursday)."));
    assert!(
        date_line(&input, chrono_tz::Europe::Berlin)
            .starts_with("Meeting date: 2026-09-24 (Thursday).")
    );
    assert!(
        date_line(&input, chrono_tz::Pacific::Honolulu)
            .starts_with("Meeting date: 2026-09-23 (Wednesday).")
    );
    assert!(
        date_line(&input, chrono_tz::Pacific::Kiritimati)
            .starts_with("Meeting date: 2026-09-24 (Thursday).")
    );
    // Map and reduce carry the same line.
    let builder = SummaryPromptBuilder::new(input.template.clone(), Utc);
    let chunk = TranscriptChunker::default().chunk(&input.segments, Some(&de()))[0].clone();
    assert!(
        builder.build_map(&input, &chunk, 1, 1_500).messages[0]
            .content
            .contains("Meeting date: 2026-09-24 (Thursday).")
    );
    assert!(
        builder.build_reduce(&input, &[]).messages[0]
            .content
            .contains("Meeting date: 2026-09-24 (Thursday).")
    );
}

/// The instruction to translate the section headings follows the language
/// line only where headings follow in the prompt.
#[test]
fn the_headings_rule_appears_only_where_headings_follow() {
    let input = default_input();
    let builder = SummaryPromptBuilder::new(input.template.clone(), Utc);
    let rule = SummaryPromptBuilder::<Utc>::headings_rule("German");
    assert_eq!(
        rule,
        "Translate the section headings given below into German."
    );
    assert!(
        builder.build_single_shot(&input).messages[0]
            .content
            .contains(&rule)
    );
    assert!(
        builder.build_reduce(&input, &[]).messages[0]
            .content
            .contains(&rule)
    );
    let chunk = TranscriptChunker::default().chunk(&input.segments, Some(&de()))[0].clone();
    let map = builder.build_map(&input, &chunk, 1, 1_500).messages[0]
        .content
        .clone();
    assert!(!map.contains("Translate the section headings"));
    assert!(!map.contains("Sections, in this order"));
    assert!(map.contains("Output language: German. Write the title, every heading"));
}

// Names, tasks, language, usage

fn draft_with(speaker_names: Vec<DraftSpeakerName>, tasks: Vec<DraftTask>) -> AnalysisDraft {
    AnalysisDraft {
        title: "T".to_owned(),
        sections: vec![section(
            "executive-summary",
            "Executive Summary",
            &[("Lead", "Speaker 2 sagt etwas.")],
        )],
        decisions: Vec::new(),
        tasks,
        speaker_names,
    }
}

fn input_with_language(language: Option<LanguageTag>) -> SummaryInput {
    let mut export = standup();
    export.meeting.language = language;
    summary_input(&export, SummaryTemplate::bundled_with_id("default"))
}

#[test]
fn speaker_two_maps_to_its_uuid_and_weak_suggestions_are_dropped() {
    let names = vec![
        speaker_name("Speaker 2", Some("Nicolai"), 0.85, "q"),
        speaker_name("SPEAKER 1", Some("Mara"), 0.29, "q"),
        speaker_name("Speaker 3", Some("Jérôme"), 0.3, "q"),
    ];
    let output = draft_with(names.clone(), Vec::new()).summary_output(
        &input_with_language(Some(de())),
        LlmUsage::ZERO,
        0.3,
    );
    let ids: Vec<_> = output.speaker_names.iter().map(|s| s.speaker_id).collect();
    assert_eq!(
        ids,
        [
            sample_uuid(STANDUP_SPEAKER_TWO),
            sample_uuid(STANDUP_SPEAKER_THREE)
        ]
    );
    let found: Vec<Option<&str>> = output
        .speaker_names
        .iter()
        .map(|s| s.name.as_deref())
        .collect();
    assert_eq!(found, [Some("Nicolai"), Some("Jérôme")]);
    let strict = draft_with(names, Vec::new()).summary_output(
        &input_with_language(Some(de())),
        LlmUsage::ZERO,
        0.5,
    );
    let found: Vec<Option<&str>> = strict
        .speaker_names
        .iter()
        .map(|s| s.name.as_deref())
        .collect();
    assert_eq!(found, [Some("Nicolai")]);
}

#[test]
fn suggestions_never_rename_a_speaker() {
    let input = input_with_language(Some(de()));
    let output = draft_with(
        vec![speaker_name("Speaker 2", Some("Nicolai"), 1.0, "q")],
        Vec::new(),
    )
    .summary_output(&input, LlmUsage::ZERO, 0.3);
    assert_eq!(
        output.summary.sections[0].bullets[0].text,
        "Speaker 2 sagt etwas."
    );
    assert_eq!(
        input.speakers[1].assignment,
        steno_core::SpeakerAssignment::Unknown,
        "the input is a value; nothing was renamed"
    );
    assert_eq!(output.speaker_names[0].evidence, "q");
}

#[test]
fn tasks_carry_owner_priority_and_date() {
    let tasks = vec![
        task(
            "Angebot schicken",
            Some("Nicolai"),
            TaskPriority::High,
            Some("2026-10-02"),
        ),
        task("Retro planen", None, TaskPriority::Low, None),
        task(
            "Doku schreiben",
            Some("Speaker 3"),
            TaskPriority::Normal,
            Some("Freitag"),
        ),
    ];
    let output = draft_with(Vec::new(), tasks).summary_output(
        &input_with_language(Some(de())),
        LlmUsage::ZERO,
        0.3,
    );
    let priorities: Vec<_> = output.tasks.iter().map(|t| t.priority).collect();
    assert_eq!(
        priorities,
        [TaskPriority::High, TaskPriority::Low, TaskPriority::Normal]
    );
    assert_eq!(
        output.tasks[0].assignee_person_id,
        Some(sample_uuid(PERSON_NICOLAI))
    );
    assert_eq!(output.tasks[0].due_date, Some(midnight(2026, 10, 2)));
    assert_eq!(output.tasks[1].assignee_name, None);
    assert_eq!(output.tasks[1].assignee_person_id, None);
    assert_eq!(
        output.tasks[2].assignee_name.as_deref(),
        Some("Jérôme"),
        "a confirmed speaker resolves to its person"
    );
    assert_eq!(
        output.tasks[2].assignee_person_id,
        Some(sample_uuid(PERSON_JEROME))
    );
    assert_eq!(output.tasks[2].due_date, None);
    assert!(output.tasks.iter().all(|t| !t.done));
}

#[test]
fn meeting_language_drives_prompt_and_output() {
    let cases = [
        (Some(de()), "German", "de"),
        (None, "English", "en"),
        (Some(LanguageTag::from("de-CH")), "German", "de-CH"),
    ];
    for (language, name, tag) in cases {
        let input = input_with_language(language.clone());
        let builder = SummaryPromptBuilder::new(input.template.clone(), Utc);
        let single = builder.build_single_shot(&input);
        assert!(
            single.messages[0]
                .content
                .contains(&format!("Output language: {name}."))
        );
        let chunk =
            TranscriptChunker::default().chunk(&input.segments, language.as_ref())[0].clone();
        let map = builder.build_map(&input, &chunk, 1, 1_500);
        assert!(
            map.messages[0]
                .content
                .contains(&format!("Output language: {name}."))
        );
        let cleanup = steno_llm::CleanupPromptBuilder::default().build(
            &chunk,
            language.as_ref(),
            &steno_llm::cleanup::glossary(&cleanup_input(&standup())),
            &SpeakerLabels::new(&input.speakers),
        );
        assert!(
            cleanup.messages[0]
                .content
                .contains(&format!("Meeting language: {name}."))
        );
        let output = draft_with(Vec::new(), Vec::new()).summary_output(&input, LlmUsage::ZERO, 0.3);
        assert_eq!(
            output.summary.language,
            Some(LanguageTag::from(tag)),
            "the renderer always gets a language"
        );
        assert_eq!(
            output.language, input.meeting.language,
            "the pipeline stores the tag as elected"
        );
    }
    assert_eq!(
        draft_with(Vec::new(), Vec::new())
            .summary_output(&input_with_language(None), LlmUsage::ZERO, 0.3)
            .language,
        None
    );
}

#[tokio::test]
async fn usage_is_the_sum_of_every_call_including_repairs_and_missing_usage() {
    let server = StubChatServer::start().await.unwrap();
    let good = canned("summary-default-standup");
    server.enqueue([
        scripts.completion("broken {", Some("stop"), Some(usage(700, 3)), "stub-model"),
        scripts.completion(&good, Some("stop"), None, "stub-model"),
    ]);
    let output = summarizer(&server, 32_000)
        .summarize(&input_with_language(Some(de())))
        .await
        .unwrap();
    assert_eq!(
        output.usage,
        LlmUsage {
            prompt_tokens: 700,
            completion_tokens: 3,
            requests: 2
        },
        "a body without usage still counts as a request"
    );

    let cleanup_server = StubChatServer::start().await.unwrap();
    cleanup_server.respond(scripts.cleanup_echo(usage(40, 20), |_, text| Some(text.to_owned())));
    let endpoint = LlmEndpoint::new(cleanup_server.base_url().clone(), "stub-model");
    let cleaner = LlmTranscriptCleaner::new(
        Arc::new(OpenAiCompatibleClient::new(endpoint.clone(), None).with_retry(RetryPolicy::NONE)),
        endpoint,
    )
    .with_chunker(TranscriptChunker::new(150, 220, 3));
    let pass_one = cleaner.clean(&cleanup_input(&standup())).await.unwrap();
    let calls = i64::try_from(cleanup_server.request_count()).unwrap();
    assert_eq!(
        pass_one.usage,
        LlmUsage {
            prompt_tokens: 40 * calls,
            completion_tokens: 20 * calls,
            requests: calls
        }
    );
    let meeting_total = pass_one.usage + output.usage;
    assert_eq!(meeting_total.requests, calls + 2);
    assert_eq!(meeting_total.prompt_tokens, 40 * calls + 700);
}

// Map and reduce

fn call_input() -> SummaryInput {
    summary_input(
        &customer_call(),
        SummaryTemplate::bundled_with_id("default"),
    )
}

/// Notes for map requests, the canned draft for everything else.
fn responder() -> steno_llm::testing::Responder {
    let notes = canned("notes-customer-call");
    let draft = canned("summary-default-standup");
    Arc::new(move |request| {
        let text = if request.purpose.as_deref() == Some("summary-map") {
            &notes
        } else {
            &draft
        };
        Some(scripts.completion(text, Some("stop"), Some(usage(1_000, 100)), "stub-model"))
    })
}

fn notes(chunk_index: i64, points: usize, point_length: usize) -> ChunkNotes {
    ChunkNotes {
        chunk_index,
        topics: vec![NotesTopic {
            topic: format!("Thema {chunk_index}"),
            points: vec!["wort ".repeat(point_length / 5); points],
        }],
        decisions: Vec::new(),
        task_candidates: Vec::new(),
        speaker_cues: Vec::new(),
    }
}

#[tokio::test]
async fn at_8k_context_issues_map_calls_then_one_reduce() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(responder());
    let summarizer = summarizer(&server, 8_000);
    let output = summarizer.summarize(&call_input()).await.unwrap();

    let purposes = purposes(&server);
    let map_count = purposes.iter().filter(|p| *p == "summary-map").count();
    assert!((6..=12).contains(&map_count), "{map_count} map calls");
    assert_eq!(purposes.last().map(String::as_str), Some("summary-reduce"));
    assert!(
        purposes[..purposes.len() - 1]
            .iter()
            .all(|p| p == "summary-map")
    );
    assert_eq!(server.request_count(), map_count + 1);
    assert!(server.max_in_flight() <= 2);

    let reduce = server.requests().pop().unwrap().chat.unwrap();
    let user = &reduce.messages[1].content;
    assert!(user.starts_with(&format!("Notes from {map_count} parts, in order (JSON):")));
    for index in 0..map_count {
        assert!(
            user.contains(&format!("\"chunkIndex\" : {index}")),
            "chunk {index} in the reduce input"
        );
    }
    assert_eq!(
        reduce.response_format.unwrap().json_schema.unwrap().name,
        "meeting_analysis"
    );
    assert_eq!(reduce.max_tokens, Some(2_000), "a quarter of 8k");
    let maps: Vec<_> = server
        .requests()
        .into_iter()
        .take(map_count)
        .filter_map(|r| r.chat)
        .collect();
    assert!(maps.iter().all(|m| {
        m.response_format
            .as_ref()
            .and_then(|f| f.json_schema.as_ref())
            .map(|s| s.name.as_str())
            == Some("chunk_notes")
    }));
    // Each map call may spend the chunk's share of the input budget on its
    // notes, not the 1 500 ceiling.
    let budget = summarizer.budget_for(&call_input(), None);
    let notes_tokens = budget.map_notes_output_tokens(map_count);
    assert!((256..1_500).contains(&notes_tokens), "{notes_tokens}");
    assert!(notes_tokens * i64::try_from(map_count).unwrap() <= budget.input_budget());
    assert!(maps.iter().all(|m| m.max_tokens == Some(notes_tokens)));
    let rule = format!(
        "at most {} points in total",
        SummaryPromptBuilder::<Utc>::max_notes_points(notes_tokens)
    );
    assert!(
        maps.iter().all(|m| m.messages[0].content.contains(&rule)),
        "{rule}"
    );
    for part in 1..=map_count {
        let asked = maps
            .iter()
            .filter(|m| {
                m.messages[0]
                    .content
                    .contains(&format!("part {part} of {map_count}"))
            })
            .count();
        assert_eq!(asked, 1, "part {part}");
    }

    let calls = i64::try_from(map_count + 1).unwrap();
    assert_eq!(
        output.usage,
        LlmUsage {
            prompt_tokens: 1_000 * calls,
            completion_tokens: 100 * calls,
            requests: calls
        }
    );
    assert_eq!(output.summary.sections[0].id, "executive-summary");
    assert_eq!(
        output.title,
        "Daily Standup: Onboarding, CI und Obsidian-Export"
    );
}

#[tokio::test]
async fn at_32k_context_one_call_suffices() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(responder());
    let output = summarizer(&server, 32_000)
        .summarize(&call_input())
        .await
        .unwrap();
    assert_eq!(purposes(&server), ["summary"]);
    let chat = server.requests()[0].chat.clone().unwrap();
    assert_eq!(chat.max_tokens, Some(4_096));
    assert_eq!(output.usage.requests, 1);
    let user = &chat.messages[1].content;
    assert!(user.starts_with("Transcript:\n"));
    assert_eq!(user.lines().count(), customer_call().segments.len() + 1);
}

#[tokio::test]
async fn at_4k_context_throws_transcript_too_long_before_any_call() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(responder());
    let error = summarizer(&server, 4_000)
        .summarize(&call_input())
        .await
        .unwrap_err();
    let LlmError::TranscriptTooLong {
        estimated_tokens,
        budget,
    } = downcast::<LlmError>(&error)
    else {
        panic!("expected TranscriptTooLong, got {error}");
    };
    assert!(estimated_tokens > 12_000);
    assert!(budget < 2_500);
    assert_eq!(
        server.requests().len(),
        0,
        "the transcript is untouched and nothing was sent"
    );
}

#[tokio::test]
async fn a_failed_map_call_propagates() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(Arc::new(|_| Some(scripts.server_error(500))));
    let error = summarizer(&server, 8_000)
        .summarize(&call_input())
        .await
        .unwrap_err();
    assert_eq!(
        downcast::<LlmError>(&error),
        LlmError::Http {
            status: 500,
            body: "The server had an error".to_owned()
        }
    );
}

#[test]
fn map_and_reduce_prompts_match_their_goldens() {
    let input = default_input();
    let builder = SummaryPromptBuilder::new(input.template.clone(), Utc);
    let chunks = TranscriptChunker::new(150, 220, 3).chunk(&input.segments, Some(&de()));
    let map = builder.build_map(&input, &chunks[1], chunks.len(), 1_500);
    assert_golden(&render(&map), "prompts/summary-map-default.txt");
    assert!(
        map.messages[1]
            .content
            .contains("End of the previous part, for context only:")
    );
    assert!(map.messages[0].content.contains("with chunkIndex 1:"));

    let notes = [
        ChunkNotes {
            chunk_index: 0,
            topics: vec![NotesTopic {
                topic: "Onboarding".to_owned(),
                points: vec![
                    "Speaker 2 hat den Pull Request für das Onboarding gemerged.".to_owned(),
                ],
            }],
            decisions: Vec::new(),
            task_candidates: Vec::new(),
            speaker_cues: vec![speaker_name(
                "Speaker 2",
                Some("Nicolai"),
                0.5,
                "nicolai, willst du starten?",
            )],
        },
        ChunkNotes {
            chunk_index: 1,
            topics: vec![NotesTopic {
                topic: "Obsidian-Export".to_owned(),
                points: vec!["Die Firma Müller will den Export bis Freitag.".to_owned()],
            }],
            decisions: vec!["Der Export wird bis Freitag umgesetzt.".to_owned()],
            task_candidates: vec![task(
                "Export nach Obsidian umsetzen.",
                Some("Speaker 2"),
                TaskPriority::High,
                Some("2026-09-25"),
            )],
            speaker_cues: Vec::new(),
        },
    ];
    let reduce = builder.build_reduce(&input, &notes);
    assert_golden(&render(&reduce), "prompts/summary-reduce-default.txt");
    assert!(reduce.messages[1].content.contains("\"chunkIndex\" : 1"));
    assert_eq!(reduce.purpose, "summary-reduce");

    let repair = SummaryPromptBuilder::<Utc>::build_repair(
        &builder.build_single_shot(&input),
        &builder.draft_schema(),
        "{\"title\": ",
        "malformed JSON at root",
    );
    assert_golden(&render(&repair), "prompts/summary-repair.txt");
    let mut sized = map;
    sized.max_tokens = Some(1_500);
    let map_repair =
        SummaryPromptBuilder::<Utc>::build_repair(&sized, &builder.notes_schema(), "{", "x");
    assert_eq!(map_repair.purpose, "summary-map-repair");
    assert_eq!(map_repair.max_tokens, Some(1_500));
    assert_eq!(map_repair.response_format, sized.response_format);
    assert!(
        map_repair.messages[0]
            .content
            .contains("\"chunkIndex\": integer")
    );
}

#[tokio::test]
async fn map_bounded_keeps_order_and_propagates_the_first_error() {
    let doubled: Vec<i32> =
        steno_llm::support::map_bounded([3, 1, 2], 2, |v| async move { Ok::<_, ()>(v * 2) })
            .await
            .unwrap();
    assert_eq!(doubled, [6, 2, 4]);
    let empty: Vec<i32> =
        steno_llm::support::map_bounded(Vec::<i32>::new(), 2, |v| async move { Ok::<_, ()>(v) })
            .await
            .unwrap();
    assert_eq!(empty.len(), 0);
    let failed = steno_llm::support::map_bounded([1, 2, 3], 1, |v| async move {
        if v == 2 { Err("boom") } else { Ok(v) }
    })
    .await;
    assert_eq!(failed, Err("boom"));
}

// Map and reduce beyond the happy path

#[tokio::test]
async fn oversized_notes_throw_transcript_too_long_after_the_map_and_before_the_reduce() {
    let server = StubChatServer::start().await.unwrap();
    let draft = canned("summary-default-standup");
    // About 6 000 estimated tokens of notes per chunk: ten chunks cannot fit
    // an 8k context however the reduce prompt is trimmed.
    server.respond(Arc::new(move |request| {
        Some(if request.purpose.as_deref() == Some("summary-map") {
            scripts.json(&notes(0, 30, 600), Some(usage(10, 5)))
        } else {
            scripts.text(&draft)
        })
    }));
    let error = summarizer(&server, 8_000)
        .summarize(&call_input())
        .await
        .unwrap_err();
    let LlmError::TranscriptTooLong {
        estimated_tokens,
        budget,
    } = downcast::<LlmError>(&error)
    else {
        panic!("expected TranscriptTooLong, got {error}");
    };
    assert!(estimated_tokens > budget);
    let purposes = purposes(&server);
    assert!(
        purposes.iter().all(|p| p == "summary-map"),
        "no reduce call was made: {purposes:?}"
    );
    assert!(purposes.len() >= 6);
}

#[tokio::test]
async fn a_map_answer_is_repaired_once_with_the_notes_schema_and_the_chunk_index_is_overruled() {
    let server = StubChatServer::start().await.unwrap();
    let draft = canned("summary-default-standup");
    let map_usage = usage(500, 50);
    let repair_usage = usage(700, 60);
    let reduce_usage = usage(2_000, 400);
    server.respond(Arc::new(move |request| {
        Some(match request.purpose.as_deref() {
            Some("summary-map") => {
                // Part 3 answers prose the first time; every answer claims
                // chunk 0.
                if request
                    .chat
                    .as_ref()
                    .is_some_and(|c| c.messages[0].content.contains("part 3 of"))
                {
                    scripts.completion(
                        "Here are my notes, no JSON today.",
                        Some("stop"),
                        Some(map_usage),
                        "stub-model",
                    )
                } else {
                    scripts.json(&notes(0, 2, 60), Some(map_usage))
                }
            }
            Some("summary-map-repair") => scripts.json(&notes(0, 2, 60), Some(repair_usage)),
            _ => scripts.completion(&draft, Some("stop"), Some(reduce_usage), "stub-model"),
        })
    }));
    let summarizer = summarizer(&server, 8_000);
    let output = summarizer.summarize(&call_input()).await.unwrap();
    let purposes = purposes(&server);
    let map_count = purposes.iter().filter(|p| *p == "summary-map").count();
    let repairs = purposes
        .iter()
        .filter(|p| *p == "summary-map-repair")
        .count();
    assert_eq!(repairs, 1);
    assert_eq!(purposes.last().map(String::as_str), Some("summary-reduce"));
    assert_eq!(server.request_count(), map_count + 2);

    let repair = server
        .requests()
        .into_iter()
        .find(|r| r.purpose.as_deref() == Some("summary-map-repair"))
        .unwrap()
        .chat
        .unwrap();
    assert_eq!(
        repair.response_format.unwrap().json_schema.unwrap().name,
        "chunk_notes"
    );
    assert_eq!(
        repair.max_tokens,
        Some(
            summarizer
                .budget_for(&call_input(), None)
                .map_notes_output_tokens(map_count)
        )
    );
    assert!(
        repair.messages[0]
            .content
            .contains("\"chunkIndex\": integer")
    );
    assert!(
        repair.messages[1]
            .content
            .contains("Here are my notes, no JSON today.")
    );
    assert!(repair.messages[1].content.contains("Validation error: "));

    let reduce_user = server.requests().pop().unwrap().chat.unwrap().messages[1]
        .content
        .clone();
    for index in 0..map_count {
        assert!(
            reduce_user.contains(&format!("\"chunkIndex\" : {index}")),
            "chunk {index}"
        );
    }
    assert!(!reduce_user.contains(&format!("\"chunkIndex\" : {map_count}")));
    let maps = i64::try_from(map_count).unwrap();
    assert_eq!(
        output.usage,
        LlmUsage {
            prompt_tokens: 500 * maps + 700 + 2_000,
            completion_tokens: 50 * maps + 60 + 400,
            requests: maps + 2
        }
    );
}

/// The map ceiling is each chunk's share of the input budget, so a model
/// that fills its ceiling on every chunk still leaves notes that fit the
/// reduce call.
#[tokio::test]
async fn map_answers_that_fill_their_ceiling_still_fit_the_reduce() {
    let server = StubChatServer::start().await.unwrap();
    let draft = canned("summary-default-standup");
    // A model spending about 70 percent of its ceiling on notes: 20-word
    // points of 100 bytes, about 34 estimated tokens each.
    server.respond(Arc::new(move |request| {
        let ceiling = request.chat.as_ref().and_then(|c| c.max_tokens);
        Some(match (request.purpose.as_deref(), ceiling) {
            (Some("summary-map"), Some(ceiling)) => {
                let points = usize::try_from(ceiling * 7 / 10 / 34).unwrap_or(1).max(1);
                scripts.json(&notes(0, points, 100), Some(usage(10, 5)))
            }
            _ => scripts.text(&draft),
        })
    }));
    let summarizer = summarizer(&server, 8_000);
    let output = summarizer.summarize(&call_input()).await.unwrap();

    let purposes = purposes(&server);
    assert_eq!(purposes.last().map(String::as_str), Some("summary-reduce"));
    let map_count = purposes.iter().filter(|p| *p == "summary-map").count();
    assert!(map_count >= 6);
    let budget = summarizer.budget_for(&call_input(), None);
    let ceiling = budget.map_notes_output_tokens(map_count);
    assert!(
        ceiling * i64::try_from(map_count).unwrap() <= budget.input_budget(),
        "the pre-check and the ceiling agree"
    );
    let requests = server.requests();
    assert!(
        requests[..requests.len() - 1]
            .iter()
            .all(|r| r.chat.as_ref().and_then(|c| c.max_tokens) == Some(ceiling))
    );
    let reduce_user = &requests.last().unwrap().chat.as_ref().unwrap().messages[1].content;
    assert!(budget.fits(TokenBudget::estimate_tokens(
        reduce_user,
        Some(&LanguageTag::from("en"))
    )));
    assert_eq!(output.summary.sections[0].id, "executive-summary");
}

#[tokio::test]
async fn a_truncated_map_answer_fails_the_whole_pass_without_repair() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(Arc::new(|request| {
        Some(if request.purpose.as_deref() == Some("summary-map") {
            scripts.truncated("{\"chunkIndex\": 0, \"topics\": [")
        } else {
            scripts.text("{}")
        })
    }));
    let error = summarizer(&server, 8_000)
        .summarize(&call_input())
        .await
        .unwrap_err();
    assert_eq!(downcast::<LlmError>(&error), LlmError::Truncated);
    assert!(purposes(&server).iter().all(|p| p == "summary-map"));
    assert!(
        server.request_count() <= 2,
        "the first truncated chunk cancels the rest"
    );
}

#[tokio::test]
async fn the_map_path_is_chosen_exactly_when_the_transcript_exceeds_the_input_budget() {
    let server = StubChatServer::start().await.unwrap();
    server.respond(responder());
    let input = call_input();
    let tokens = TranscriptChunker::estimate_segments(&input.segments, Some(&de()));
    let overhead = TokenBudget::estimate_tokens(
        &SummaryPromptBuilder::new(input.template.clone(), Utc)
            .build_single_shot(&input)
            .messages[0]
            .content,
        Some(&LanguageTag::from("en")),
    ) + 64;
    let mut switched = None;
    let mut seen = 0;
    for context_tokens in (12_000..=40_000).step_by(4_000) {
        let summarizer = summarizer(&server, context_tokens);
        summarizer.summarize(&input).await.unwrap();
        let purposes: Vec<String> = purposes(&server)[seen..].to_vec();
        seen = server.request_count();
        let single = purposes == ["summary"];
        let budget = TokenBudget {
            context_tokens,
            reserved_output_tokens: summarizer.endpoint.summary_reserved_output_tokens(),
            prompt_overhead_tokens: overhead,
        };
        assert_eq!(
            single,
            budget.fits(tokens),
            "{context_tokens}: {purposes:?}"
        );
        if !single {
            assert_eq!(
                purposes.last().map(String::as_str),
                Some("summary-reduce"),
                "{context_tokens}"
            );
            assert!(
                purposes[..purposes.len() - 1]
                    .iter()
                    .all(|p| p == "summary-map"),
                "{context_tokens}"
            );
        }
        if single && switched.is_none() {
            switched = Some(context_tokens);
        }
    }
    let threshold = switched.unwrap();
    assert!(threshold > 12_000 && threshold <= 32_000, "{threshold}");
}
