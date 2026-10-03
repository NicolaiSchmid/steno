//! Pass 2: title, structured summary for the template, decisions, tasks and
//! speaker name suggestions.
//! Swift: `Sources/StenoLLM/Summary/`.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use steno_core::{
    BoundaryResult, LanguageModel, LanguageTag, LlmMessage, LlmRequest, LlmResponseFormat, LlmRole,
    LlmUsage, MeetingSummarizer, MeetingTask, Person, Speaker, SpeakerAssignment,
    SpeakerNameSuggestion, SummaryBullet, SummaryDocument, SummaryInput, SummaryOutput,
    SummarySection, SummaryTemplate, TaskPriority, async_trait, derived_uuid,
};
use uuid::Uuid;

use crate::budget::{BudgetPolicy, counted_usage};
use crate::concurrency::map_bounded;
use crate::labels::{SpeakerLabels, render_plain_lines};
use crate::language::OutputLanguage;
use crate::{
    JsonSchema, LlmEndpoint, LlmError, StructuredOutputDecoder, TokenBudget, TranscriptChunk,
    TranscriptChunker,
};

// The model's raw answers, before post-processing into `SummaryOutput`.
// Decoding into these types is the validation.

/// One task as the model wrote it. `due_date` is `YYYY-MM-DD` or null and
/// is validated by Steno, never trusted; `priority` decodes straight into
/// core's closed enum, so an unknown value is a decode failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftTask {
    pub text: String,
    pub assignee: Option<String>,
    pub priority: TaskPriority,
    pub due_date: Option<String>,
}

/// The model's guess who a speaker label is, with a quote as evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftSpeakerName {
    pub speaker_label: String,
    pub name: Option<String>,
    pub confidence: f64,
    pub evidence: String,
}

/// The single-shot or reduce answer.
/// Swift: `Sources/StenoLLM/Summary/AnalysisDraft.swift`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisDraft {
    pub title: String,
    pub sections: Vec<DraftSection>,
    pub decisions: Vec<String>,
    pub tasks: Vec<DraftTask>,
    pub speaker_names: Vec<DraftSpeakerName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftSection {
    pub id: String,
    pub heading: String,
    pub bullets: Vec<DraftBullet>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftBullet {
    pub lead: String,
    pub text: String,
}

/// The map answer for one chunk of a long transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkNotes {
    pub chunk_index: i64,
    pub topics: Vec<NotesTopic>,
    pub decisions: Vec<String>,
    pub task_candidates: Vec<DraftTask>,
    pub speaker_cues: Vec<DraftSpeakerName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotesTopic {
    pub topic: String,
    pub points: Vec<String>,
}

/// A resolved assignee: the name to show and the person, when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignee {
    pub name: String,
    pub person_id: Option<Uuid>,
}

// "The model returned X, what the pass makes of it": the summary pass's
// post-processing as pure functions over the draft, mirroring
// `CleanupDraft::problems` for pass 1. `LlmMeetingSummarizer` only talks to
// the model.

impl AnalysisDraft {
    /// The draft as [`SummaryOutput`]: sections in template order (unknown
    /// ids dropped, a section the model split merged, empty optional ones
    /// dropped, empty required ones kept with the template heading), a
    /// blank title replaced by the meeting's, decisions deduplicated, tasks
    /// with derived ids, strict `yyyy-MM-dd` due dates, assignees resolved
    /// against the participants, speaker labels mapped to ids, name
    /// suggestions below `minimum_confidence` dropped. The document's
    /// `language` is the meeting's, else English (what the renderer needs);
    /// the output's `language` is the meeting's tag as elected, `None`
    /// included, so the pipeline never stores English for a meeting the
    /// engine left untagged.
    /// Swift: `Sources/StenoLLM/Summary/AnalysisDraft+Output.swift`.
    #[must_use]
    pub fn summary_output(
        &self,
        input: &SummaryInput,
        usage: LlmUsage,
        minimum_confidence: f64,
    ) -> SummaryOutput {
        let labels = SpeakerLabels::new(&input.speakers);
        let title = match self.title.trim() {
            "" => input.meeting.title.clone(),
            trimmed => trimmed.to_owned(),
        };
        SummaryOutput {
            title,
            summary: SummaryDocument {
                template_id: input.template.id.clone(),
                language: Some(OutputLanguage::resolve(input.meeting.language.as_ref())),
                sections: Self::sections(self, &input.template),
            },
            decisions: Self::unique(
                self.decisions
                    .iter()
                    .map(|decision| decision.trim().to_owned())
                    .filter(|decision| !decision.is_empty()),
            ),
            tasks: self
                .tasks
                .iter()
                .enumerate()
                .filter_map(|(offset, task)| Self::make_task(task, offset, input, &labels))
                .collect(),
            speaker_names: Self::suggestions(
                &self.speaker_names,
                &labels,
                &input.speakers,
                minimum_confidence,
            ),
            language: input.meeting.language.clone(),
            usage,
        }
    }

    /// Template order; unknown ids dropped; a section the model split in
    /// two is merged (bullets in order, the first non-blank heading); empty
    /// optional sections dropped, empty required ones kept; a missing or
    /// blank heading falls back to the template's.
    #[must_use]
    pub fn sections(draft: &AnalysisDraft, template: &SummaryTemplate) -> Vec<SummarySection> {
        template
            .sections
            .iter()
            .filter_map(|section| {
                let drafted: Vec<&DraftSection> = draft
                    .sections
                    .iter()
                    .filter(|drafted| drafted.id == section.id)
                    .collect();
                let bullets: Vec<SummaryBullet> = drafted
                    .iter()
                    .flat_map(|drafted| drafted.bullets.iter())
                    .filter_map(|bullet| {
                        let lead = bullet.lead.trim();
                        let lead = lead.strip_suffix(':').unwrap_or(lead);
                        let text = bullet.text.trim();
                        (!text.is_empty() || !lead.is_empty()).then(|| SummaryBullet {
                            lead: lead.to_owned(),
                            text: text.to_owned(),
                        })
                    })
                    .collect();
                if bullets.is_empty() && !section.required {
                    return None;
                }
                let heading = drafted
                    .iter()
                    .map(|drafted| drafted.heading.trim())
                    .find(|heading| !heading.is_empty())
                    .unwrap_or(&section.heading)
                    .to_owned();
                Some(SummarySection {
                    id: section.id.clone(),
                    heading,
                    bullets,
                })
            })
            .collect()
    }

    #[must_use]
    pub fn make_task(
        task: &DraftTask,
        index: usize,
        input: &SummaryInput,
        labels: &SpeakerLabels,
    ) -> Option<MeetingTask> {
        let text = task.text.trim();
        if text.is_empty() {
            return None;
        }
        let assignee = Self::resolve_assignee(task.assignee.as_deref(), input, labels);
        Some(MeetingTask {
            id: derived_uuid(input.meeting.id, &format!("task-{index}")),
            meeting_id: input.meeting.id,
            text: text.to_owned(),
            assignee_person_id: assignee.as_ref().and_then(|a| a.person_id),
            assignee_name: assignee.map(|a| a.name),
            priority: task.priority,
            due_date: Self::parse_due_date(task.due_date.as_deref()),
            done: false,
        })
    }

    /// A participant by full name, then by unique first name, then a
    /// speaker label (its confirmed or suggested person), then a known
    /// person; else the name as written with no person.
    #[must_use]
    pub fn resolve_assignee(
        raw: Option<&str>,
        input: &SummaryInput,
        labels: &SpeakerLabels,
    ) -> Option<Assignee> {
        let name = raw?.trim();
        if name.is_empty() {
            return None;
        }
        let lowered = name.to_lowercase();
        if let Some(participant) = input
            .participants
            .iter()
            .find(|participant| participant.display_name.to_lowercase() == lowered)
        {
            return Some(Assignee {
                name: participant.display_name.clone(),
                person_id: participant.person_id,
            });
        }
        let by_first_name: Vec<_> = input
            .participants
            .iter()
            .filter(|participant| {
                participant
                    .display_name
                    .to_lowercase()
                    .split(' ')
                    .next()
                    .is_some_and(|first| first == lowered)
            })
            .collect();
        if let [participant] = by_first_name.as_slice() {
            return Some(Assignee {
                name: participant.display_name.clone(),
                person_id: participant.person_id,
            });
        }
        if let Some(speaker_id) = labels.speaker_id(name)
            && let Some(speaker) = input.speakers.iter().find(|s| s.id == speaker_id)
        {
            if let Some(person_id) = speaker.person_id()
                && let Some(person) = input.known_people.iter().find(|p| p.id == person_id)
            {
                return Some(Assignee {
                    name: person.display_name.clone(),
                    person_id: Some(person.id),
                });
            }
            return Some(Assignee {
                name: speaker.cluster_label.clone(),
                person_id: None,
            });
        }
        if let Some(person) = input
            .known_people
            .iter()
            .find(|person| person.display_name.to_lowercase() == lowered)
        {
            return Some(Assignee {
                name: person.display_name.clone(),
                person_id: Some(person.id),
            });
        }
        Some(Assignee {
            name: name.to_owned(),
            person_id: None,
        })
    }

    /// `YYYY-MM-DD` at midnight UTC, else `None`; anything looser is
    /// dropped.
    #[must_use]
    pub fn parse_due_date(raw: Option<&str>) -> Option<DateTime<Utc>> {
        let text = raw?.trim();
        if text.chars().count() != 10 {
            return None;
        }
        let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
        if date.format("%Y-%m-%d").to_string() != text {
            return None;
        }
        Some(Utc.from_utc_datetime(&date.and_time(NaiveTime::MIN)))
    }

    /// Known labels only, a name present, confidence at or above `minimum`
    /// and clamped to 0...1, one suggestion per speaker (the strongest), in
    /// speaker order.
    #[must_use]
    pub fn suggestions(
        drafts: &[DraftSpeakerName],
        labels: &SpeakerLabels,
        speakers: &[Speaker],
        minimum: f64,
    ) -> Vec<SpeakerNameSuggestion> {
        let mut best: std::collections::HashMap<Uuid, SpeakerNameSuggestion> =
            std::collections::HashMap::new();
        for draft in drafts {
            let Some(speaker_id) = labels.speaker_id(&draft.speaker_label) else {
                continue;
            };
            let Some(name) = draft
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
            else {
                continue;
            };
            let confidence = draft.confidence.clamp(0.0, 1.0);
            if confidence < minimum {
                continue;
            }
            if best
                .get(&speaker_id)
                .is_some_and(|existing| existing.confidence >= confidence)
            {
                continue;
            }
            best.insert(
                speaker_id,
                SpeakerNameSuggestion {
                    speaker_id,
                    name: Some(name.to_owned()),
                    confidence,
                    evidence: draft.evidence.trim().to_owned(),
                },
            );
        }
        speakers
            .iter()
            .filter_map(|speaker| best.remove(&speaker.id))
            .collect()
    }

    fn unique(strings: impl Iterator<Item = String>) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        strings
            .filter(|text| seen.insert(text.to_lowercase()))
            .collect()
    }
}

/// Builds the analysis requests for one [`SummaryTemplate`]: the
/// single-shot request over the whole transcript, the map request per
/// chunk, the reduce request over the chunk notes, and the repair request
/// after a decode failure. English prompts; the model writes in the
/// meeting's language. Every string here is pinned by a golden in
/// `Tests/Fixtures/llm/prompts/`.
/// Swift: `Sources/StenoLLM/Summary/SummaryPromptBuilder.swift`.
#[derive(Debug, Clone)]
pub struct SummaryPromptBuilder<Tz: TimeZone> {
    pub template: SummaryTemplate,
    /// The zone the meeting date is written in; the user's in the app, UTC
    /// in goldens.
    pub zone: Tz,
}

impl<Tz: TimeZone> SummaryPromptBuilder<Tz> {
    /// Temperature of every analysis request.
    pub const TEMPERATURE: f64 = 0.2;

    #[must_use]
    pub fn new(template: SummaryTemplate, zone: Tz) -> Self {
        SummaryPromptBuilder { template, zone }
    }

    // Template

    /// The template as prompt text: name, context, one line per section
    /// with id, heading, whether it is required, and its instructions.
    #[must_use]
    pub fn template_blocks(&self) -> String {
        let mut lines = vec![
            format!("Template: {}", self.template.display_name),
            self.template.context.clone(),
            String::new(),
            "Sections, in this order, with exactly these ids:".to_owned(),
        ];
        for section in &self.template.sections {
            let requirement = if section.required {
                "required"
            } else {
                "optional, omit it when the meeting has nothing for it"
            };
            lines.push(format!(
                "- {} — \"{}\" ({requirement}): {}",
                section.id, section.heading, section.instructions
            ));
        }
        lines.join("\n")
    }

    // Schemas

    #[must_use]
    pub fn task_schema() -> JsonSchema {
        let priorities: Vec<&str> = TaskPriority::ALL.iter().map(|p| p.as_str()).collect();
        JsonSchema::object(vec![
            (
                "text",
                JsonSchema::string().described("the commitment, as one sentence"),
            ),
            (
                "assignee",
                JsonSchema::string()
                    .described("known name or speaker label verbatim")
                    .nullable(),
            ),
            ("priority", JsonSchema::string_enum(&priorities)),
            (
                "dueDate",
                JsonSchema::string()
                    .described("YYYY-MM-DD, resolved against the meeting date")
                    .nullable(),
            ),
        ])
    }

    #[must_use]
    pub fn speaker_name_schema() -> JsonSchema {
        JsonSchema::object(vec![
            (
                "speakerLabel",
                JsonSchema::string().described("the label as it appears in the transcript"),
            ),
            (
                "name",
                JsonSchema::string()
                    .described("the person's name")
                    .nullable(),
            ),
            ("confidence", JsonSchema::number().described("0 to 1")),
            (
                "evidence",
                JsonSchema::string().described("one short quote from the transcript"),
            ),
        ])
    }

    /// The single-shot and reduce answer, section ids as an enum.
    #[must_use]
    pub fn draft_schema(&self) -> JsonSchema {
        let ids: Vec<&str> = self
            .template
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect();
        JsonSchema::object(vec![
            (
                "title",
                JsonSchema::string()
                    .described("under 80 characters, \"Topic: Subtopic\" when natural"),
            ),
            (
                "sections",
                JsonSchema::array(JsonSchema::object(vec![
                    ("id", JsonSchema::string_enum(&ids)),
                    (
                        "heading",
                        JsonSchema::string()
                            .described("the section heading in the output language"),
                    ),
                    (
                        "bullets",
                        JsonSchema::array(JsonSchema::object(vec![
                            ("lead", JsonSchema::string()),
                            ("text", JsonSchema::string()),
                        ])),
                    ),
                ])),
            ),
            ("decisions", JsonSchema::array(JsonSchema::string())),
            ("tasks", JsonSchema::array(Self::task_schema())),
            (
                "speakerNames",
                JsonSchema::array(Self::speaker_name_schema()),
            ),
        ])
    }

    /// The map answer for one chunk.
    #[must_use]
    pub fn notes_schema(&self) -> JsonSchema {
        JsonSchema::object(vec![
            ("chunkIndex", JsonSchema::integer()),
            (
                "topics",
                JsonSchema::array(JsonSchema::object(vec![
                    ("topic", JsonSchema::string()),
                    (
                        "points",
                        JsonSchema::array(
                            JsonSchema::string().described("one full sentence, names included"),
                        ),
                    ),
                ])),
            ),
            ("decisions", JsonSchema::array(JsonSchema::string())),
            ("taskCandidates", JsonSchema::array(Self::task_schema())),
            (
                "speakerCues",
                JsonSchema::array(Self::speaker_name_schema()),
            ),
        ])
    }

    // Requests

    /// One call over the whole cleaned transcript.
    #[must_use]
    pub fn build_single_shot(&self, input: &SummaryInput) -> LlmRequest {
        let labels = SpeakerLabels::new(&input.speakers);
        let system = [
            "You are Steno's meeting analyst. You read the transcript of one meeting and return one JSON object and nothing else: no prose before or after it.".to_owned(),
            String::new(),
            self.meeting_block(input, true),
            String::new(),
            self.template_blocks(),
            String::new(),
            Self::ANALYSIS_RULES.to_owned(),
            String::new(),
            "Return exactly this JSON shape:".to_owned(),
            self.draft_schema().prompt_text(),
        ]
        .join("\n");
        let user = format!(
            "Transcript:\n{}",
            render_plain_lines(&input.segments, &labels)
        );
        Self::request(
            system,
            user,
            &self.draft_schema(),
            "meeting_analysis",
            "summary",
            None,
        )
    }

    /// Notes for one chunk of a long transcript. `notes_tokens` is the
    /// answer's ceiling (`max_tokens`) and sizes the length rule in the
    /// prompt, so the notes of every chunk fit the reduce call together.
    #[must_use]
    pub fn build_map(
        &self,
        input: &SummaryInput,
        chunk: &TranscriptChunk,
        total: usize,
        notes_tokens: i64,
    ) -> LlmRequest {
        let labels = SpeakerLabels::new(&input.speakers);
        let part = chunk.index + 1;
        let system = [
            format!("You are Steno's meeting analyst. You read part {part} of {total} of one meeting's transcript and return structured notes as one JSON object and nothing else."),
            String::new(),
            self.meeting_block(input, false),
            String::new(),
            format!("Template: {}", self.template.display_name),
            self.template.context.clone(),
            String::new(),
            Self::notes_rules(Self::max_notes_points(notes_tokens)),
            String::new(),
            format!("Return exactly this JSON shape, with chunkIndex {}:", chunk.index),
            self.notes_schema().prompt_text(),
        ]
        .join("\n");
        let mut user = format!("Part {part} of {total}.");
        if !chunk.leading_context.is_empty() {
            user.push_str("\n\nEnd of the previous part, for context only:\n");
            user.push_str(&render_plain_lines(&chunk.leading_context, &labels));
        }
        user.push_str("\n\nTranscript:\n");
        user.push_str(&render_plain_lines(&chunk.segments, &labels));
        Self::request(
            system,
            user,
            &self.notes_schema(),
            "chunk_notes",
            "summary-map",
            Some(notes_tokens),
        )
    }

    /// The map prompt's length rule from the answer ceiling
    /// ([`BudgetPolicy::TOKENS_PER_NOTES_POINT`]), never below the minimum.
    #[must_use]
    pub fn max_notes_points(notes_tokens: i64) -> i64 {
        BudgetPolicy::MAP_NOTES_MINIMUM_POINTS
            .max(notes_tokens / BudgetPolicy::TOKENS_PER_NOTES_POINT)
    }

    /// One call merging every chunk's notes into the final analysis.
    #[must_use]
    pub fn build_reduce(&self, input: &SummaryInput, notes: &[ChunkNotes]) -> LlmRequest {
        let system = [
            "You are Steno's meeting analyst. You receive structured notes taken from the consecutive parts of one meeting's transcript, in order. Merge them into the final analysis and return one JSON object and nothing else.".to_owned(),
            String::new(),
            self.meeting_block(input, true),
            String::new(),
            self.template_blocks(),
            String::new(),
            Self::ANALYSIS_RULES.to_owned(),
            String::new(),
            "Notes rules: the notes are your only source; merge duplicate decisions and tasks, keep every distinct one, and prefer later notes when a plan changed. Speaker cues with the same label agree or the higher confidence wins.".to_owned(),
            String::new(),
            "Return exactly this JSON shape:".to_owned(),
            self.draft_schema().prompt_text(),
        ]
        .join("\n");
        let json = serde_json::to_value(notes).map_or_else(
            |_| "[]".to_owned(),
            |value| crate::wire::swift_pretty(&value),
        );
        let user = format!("Notes from {} parts, in order (JSON):\n{json}", notes.len());
        Self::request(
            system,
            user,
            &self.draft_schema(),
            "meeting_analysis",
            "summary-reduce",
            None,
        )
    }

    /// The one repair round after `request`'s answer failed to decode: the
    /// invalid output, the error, the shape; same response format, token
    /// ceiling and purpose (suffixed `-repair`) as the request it repairs.
    #[must_use]
    pub fn build_repair(
        request: &LlmRequest,
        schema: &JsonSchema,
        invalid: &str,
        error: &str,
    ) -> LlmRequest {
        let system = [
            "You fix a JSON answer that failed validation. Return only the corrected JSON object, nothing else. Keep the content; change only what the error requires.",
            "",
            "The JSON must have exactly this shape:",
            &schema.prompt_text(),
        ]
        .join("\n");
        let user = format!("Validation error: {error}\n\nInvalid answer:\n{invalid}");
        LlmRequest {
            messages: Self::messages(system, user),
            response_format: request.response_format.clone(),
            temperature: Some(Self::TEMPERATURE),
            max_tokens: request.max_tokens,
            purpose: format!("{}-repair", request.purpose),
        }
    }

    // Blocks

    /// Output language, meeting date, participants and speaker labels.
    /// `headings_follow` appends [`Self::headings_rule`] to the language
    /// line: the single-shot and reduce prompts list the template sections
    /// below it, the map prompt has no headings to translate.
    #[must_use]
    pub fn meeting_block(&self, input: &SummaryInput, headings_follow: bool) -> String {
        let language = OutputLanguage::resolve(input.meeting.language.as_ref());
        let language_name = OutputLanguage::prompt_name(&language);
        let mut language_line = format!(
            "Output language: {language_name}. Write the title, every heading, bullet, decision and task in {language_name}; keep product names, code and terms the speakers used in another language as spoken."
        );
        if headings_follow {
            language_line.push(' ');
            language_line.push_str(&Self::headings_rule(&language_name));
        }
        let mut lines = vec![
            language_line,
            format!(
                "Meeting date: {}. Resolve relative dates such as \"next Friday\" against it and write dates as YYYY-MM-DD.",
                self.format_date(input.meeting.started_at)
            ),
            "Participants:".to_owned(),
        ];
        for participant in &input.participants {
            let me = if participant.role == steno_core::ParticipantRole::Me {
                " (the user, \"me\")"
            } else {
                ""
            };
            lines.push(format!("- {}{me}", participant.display_name));
        }
        if input.participants.is_empty() {
            lines.push("- unknown".to_owned());
        }
        lines.push("Speaker labels in the transcript:".to_owned());
        for speaker in &input.speakers {
            lines.push(format!(
                "- {}{}",
                speaker.cluster_label,
                Self::known_name(speaker, &input.known_people)
            ));
        }
        lines.join("\n")
    }

    /// Used only where the template's section headings follow in the
    /// prompt.
    #[must_use]
    pub fn headings_rule(language_name: &str) -> String {
        format!("Translate the section headings given below into {language_name}.")
    }

    #[must_use]
    pub fn known_name(speaker: &Speaker, people: &[Person]) -> String {
        let Some(person) = speaker
            .person_id()
            .and_then(|person_id| people.iter().find(|person| person.id == person_id))
        else {
            return " (unknown; suggest a name only with evidence from the transcript)".to_owned();
        };
        match speaker.assignment {
            SpeakerAssignment::Confirmed { .. } => format!(" = {}", person.display_name),
            SpeakerAssignment::Suggested { .. } => {
                format!(" = probably {} (unconfirmed)", person.display_name)
            }
            SpeakerAssignment::Unknown => String::new(),
        }
    }

    pub const ANALYSIS_RULES: &'static str = "Rules:
- Each bullet is a short lead phrase plus one to three sentences of text. The lead names the topic (or the person, where the template says so); the text carries the substance.
- Name people by their known name where one is listed above, otherwise by their speaker label verbatim (for example \"Speaker 2\"), so Steno can replace it with the right name later. Never guess a name inside a bullet.
- Decisions are things the participants agreed on, not proposals or ideas. One sentence each.
- Tasks are explicit commitments with an owner (\"I will send the offer by Friday\"). Priority is \"high\" only when urgency was said, \"low\" only when it was called optional, otherwise \"normal\". dueDate is an absolute date or null.
- speakerNames: suggest a name for a speaker label only with evidence from the transcript (addressed by name, a self-introduction) or by elimination against the participant list; confidence between 0 and 1; evidence is one short quote. Leave name null when there is no evidence.
- The title is under 80 characters, \"Topic: Subtopic\" when that reads naturally, in the output language.
- Only what was said. Never invent facts, names, numbers or dates.";

    #[must_use]
    pub fn notes_rules(max_points: i64) -> String {
        format!(
            "Rules:
- Group what was said into topics in the order they came up. Each point is one full sentence in the output language that names who said or asked for what, using known names where listed above and the speaker label verbatim otherwise.
- Keep the notes short: at most {max_points} points in total across all topics, one sentence each, so that the notes of every part fit one final call together. Prefer the points that carry a decision, a commitment, a number, a date or a name.
- decisions are things agreed in this part, not proposals. taskCandidates are explicit commitments with an owner as said in this part; dueDate is an absolute date or null.
- speakerCues: evidence in this part for who a speaker label is (addressed by name, a self-introduction), with a short quote; name null when there is none.
- Only what was said in this part. Never invent anything."
        )
    }

    /// `2026-09-24 (Thursday)` in the builder's zone.
    #[must_use]
    pub fn format_date(&self, date: DateTime<Utc>) -> String {
        date.with_timezone(&self.zone)
            .naive_local()
            .format("%Y-%m-%d (%A)")
            .to_string()
    }

    fn request(
        system: String,
        user: String,
        schema: &JsonSchema,
        name: &str,
        purpose: &str,
        max_tokens: Option<i64>,
    ) -> LlmRequest {
        LlmRequest {
            messages: Self::messages(system, user),
            response_format: LlmResponseFormat::JsonSchema {
                name: name.to_owned(),
                schema: schema.json_value(),
                strict: true,
            },
            temperature: Some(Self::TEMPERATURE),
            max_tokens,
            purpose: purpose.to_owned(),
        }
    }

    fn messages(system: String, user: String) -> Vec<LlmMessage> {
        vec![
            LlmMessage {
                role: LlmRole::System,
                content: system,
            },
            LlmMessage {
                role: LlmRole::User,
                content: user,
            },
        ]
    }
}

/// Pass 2: title, structured summary for the template, decisions, tasks and
/// speaker name suggestions. One call when the transcript fits the input
/// budget, else map (notes per chunk) and reduce (one call). A decode
/// failure goes once through the repair request, then fails the call.
/// Post-processing is [`AnalysisDraft::summary_output`]. No Markdown is
/// produced here; the core renders the [`SummaryDocument`].
/// Swift: `Sources/StenoLLM/Summary/LLMMeetingSummarizer.swift`.
pub struct LlmMeetingSummarizer<Tz: TimeZone> {
    pub model: Arc<dyn LanguageModel>,
    pub endpoint: LlmEndpoint,
    /// The zone the meeting date is written in for the model.
    pub zone: Tz,
    /// Suggestions below this confidence are dropped.
    pub minimum_confidence: f64,
}

impl<Tz: TimeZone + Clone + Send + Sync> LlmMeetingSummarizer<Tz>
where
    Tz::Offset: Send + Sync,
{
    pub const DEFAULT_MINIMUM_CONFIDENCE: f64 = 0.3;

    #[must_use]
    pub fn new(model: Arc<dyn LanguageModel>, endpoint: LlmEndpoint, zone: Tz) -> Self {
        LlmMeetingSummarizer {
            model,
            endpoint,
            zone,
            minimum_confidence: Self::DEFAULT_MINIMUM_CONFIDENCE,
        }
    }

    #[must_use]
    pub fn with_minimum_confidence(mut self, minimum_confidence: f64) -> Self {
        self.minimum_confidence = minimum_confidence;
        self
    }

    fn builder(&self, input: &SummaryInput) -> SummaryPromptBuilder<Tz> {
        SummaryPromptBuilder::new(input.template.clone(), self.zone.clone())
    }

    /// The pass over `input` with this crate's own types; the
    /// [`MeetingSummarizer`] impl calls it.
    pub async fn summarize_input(&self, input: &SummaryInput) -> BoundaryResult<SummaryOutput> {
        let builder = self.builder(input);
        let mut single_shot = builder.build_single_shot(input);
        let budget = self.budget_for(input, Some(&single_shot.messages[0].content));
        let transcript_tokens =
            TranscriptChunker::estimate_segments(&input.segments, input.meeting.language.as_ref());

        let (draft, usage) = if budget.fits(transcript_tokens) {
            single_shot.max_tokens = Some(self.endpoint.summary_reserved_output_tokens());
            self.complete::<AnalysisDraft>(&single_shot, &builder.draft_schema())
                .await?
        } else {
            self.map_reduce(input, &builder, &budget, transcript_tokens)
                .await?
        };
        Ok(draft.summary_output(input, usage, self.minimum_confidence))
    }

    /// The budget both paths work within: the context less the reserved
    /// answer and the single-shot system prompt (the largest of the three)
    /// plus message framing. `system_prompt` defaults to building it.
    #[must_use]
    pub fn budget_for(&self, input: &SummaryInput, system_prompt: Option<&str>) -> TokenBudget {
        let system = system_prompt.map_or_else(
            || {
                self.builder(input).build_single_shot(input).messages[0]
                    .content
                    .clone()
            },
            str::to_owned,
        );
        let english = LanguageTag::from("en");
        TokenBudget {
            context_tokens: self.endpoint.context_tokens,
            reserved_output_tokens: self.endpoint.summary_reserved_output_tokens(),
            prompt_overhead_tokens: TokenBudget::estimate_tokens(&system, Some(&english))
                + BudgetPolicy::PROMPT_FRAMING_TOKENS,
        }
    }

    /// Notes per chunk (`max_concurrent_requests` at a time), then one
    /// reduce call over every chunk's notes. Two levels only. Each map call
    /// may spend the chunk's share of the input budget on its notes
    /// ([`TokenBudget::map_notes_output_tokens`]), which is also what the
    /// up-front check reserves, so `TranscriptTooLong` is returned before
    /// the first call when the chunks are too many for that share; the
    /// post-map check only catches a model that ignored its ceiling and the
    /// prompt's length rule.
    async fn map_reduce(
        &self,
        input: &SummaryInput,
        builder: &SummaryPromptBuilder<Tz>,
        budget: &TokenBudget,
        transcript_tokens: i64,
    ) -> BoundaryResult<(AnalysisDraft, LlmUsage)> {
        let chunks = TranscriptChunker::with_budget(budget.input_budget(), 3)
            .chunk(&input.segments, input.meeting.language.as_ref());
        let notes_tokens = budget.map_notes_output_tokens(chunks.len());
        let chunk_count = i64::try_from(chunks.len()).unwrap_or(i64::MAX);
        if !budget.fits(chunk_count.saturating_mul(notes_tokens)) {
            return Err(LlmError::TranscriptTooLong {
                estimated_tokens: transcript_tokens,
                budget: budget.input_budget(),
            }
            .into());
        }
        let notes_schema = builder.notes_schema();
        let mapped = map_bounded(
            0..chunks.len(),
            self.endpoint.max_concurrent_requests,
            |position| {
                let chunk = &chunks[position];
                let request = builder.build_map(input, chunk, chunks.len(), notes_tokens);
                let notes_schema = &notes_schema;
                async move {
                    let (mut notes, usage) =
                        self.complete::<ChunkNotes>(&request, notes_schema).await?;
                    notes.chunk_index = i64::try_from(chunk.index).unwrap_or(i64::MAX);
                    Ok::<_, steno_core::BoxError>((notes, usage))
                }
            },
        )
        .await?;
        let (notes, usages): (Vec<ChunkNotes>, Vec<LlmUsage>) = mapped.into_iter().unzip();
        let mut reduce = builder.build_reduce(input, &notes);
        reduce.max_tokens = Some(self.endpoint.summary_reserved_output_tokens());
        let english = LanguageTag::from("en");
        let notes_estimate =
            TokenBudget::estimate_tokens(&reduce.messages[1].content, Some(&english));
        if !budget.fits(notes_estimate) {
            return Err(LlmError::TranscriptTooLong {
                estimated_tokens: notes_estimate,
                budget: budget.input_budget(),
            }
            .into());
        }
        let (draft, reduce_usage) = self
            .complete::<AnalysisDraft>(&reduce, &builder.draft_schema())
            .await?;
        Ok((
            draft,
            usages
                .into_iter()
                .fold(reduce_usage, |total, usage| total + usage),
        ))
    }

    /// One request, decoded into `T`; an undecodable answer gets exactly
    /// one repair round. Usage sums both calls.
    async fn complete<T: DeserializeOwned>(
        &self,
        request: &LlmRequest,
        schema: &JsonSchema,
    ) -> BoundaryResult<(T, LlmUsage)> {
        let first = self.model.complete(request).await?;
        match StructuredOutputDecoder::decode::<T>(&first) {
            Ok(value) => Ok((value, counted_usage(&first))),
            Err(LlmError::InvalidJson(detail)) => {
                let repair =
                    SummaryPromptBuilder::<Tz>::build_repair(request, schema, &first.text, &detail);
                let second = self.model.complete(&repair).await?;
                let value = StructuredOutputDecoder::decode::<T>(&second)?;
                Ok((value, counted_usage(&first) + counted_usage(&second)))
            }
            Err(error) => Err(error.into()),
        }
    }
}

#[async_trait]
impl<Tz> MeetingSummarizer for LlmMeetingSummarizer<Tz>
where
    Tz: TimeZone + Clone + Send + Sync + 'static,
    Tz::Offset: Send + Sync,
{
    async fn summarize(&self, input: &SummaryInput) -> BoundaryResult<SummaryOutput> {
        self.summarize_input(input).await
    }
}
