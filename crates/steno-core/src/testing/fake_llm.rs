//! A language model, a cleaner and a summarizer that need no service.
//! Swift: `Sources/StenoCore/Testing/FakeLLM.swift`.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use thiserror::Error;

use super::{CallLog, FakeFailure};
use crate::{
    BoundaryResult, BoxError, CleanupInput, CleanupOutput, LanguageModel, LlmRequest, LlmResponse,
    LlmUsage, MeetingSummarizer, MeetingTask, SpeakerNameSuggestion, SummaryBullet,
    SummaryDocument, SummaryInput, SummaryOutput, SummarySection, TaskPriority, TranscriptCleaner,
    derived_uuid,
};

/// The queue of canned responses ran dry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("the fake language model has no response left")]
pub struct Exhausted;

/// A `LanguageModel` that answers from a queue of canned responses and
/// records every request. Fails with [`Exhausted`] when the queue runs dry.
#[derive(Debug, Default)]
pub struct FakeLanguageModel {
    responses: Mutex<VecDeque<LlmResponse>>,
    pub requests: CallLog<LlmRequest>,
}

impl FakeLanguageModel {
    #[must_use]
    pub fn new(responses: impl IntoIterator<Item = LlmResponse>) -> Self {
        FakeLanguageModel {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: CallLog::new(),
        }
    }

    /// Responses not yet handed out.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[async_trait]
impl LanguageModel for FakeLanguageModel {
    async fn complete(&self, request: &LlmRequest) -> BoundaryResult<LlmResponse> {
        self.requests.record(request.clone());
        self.responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .ok_or_else(|| Box::new(Exhausted) as BoxError)
    }
}

/// A `TranscriptCleaner` that returns the segments untouched with a fixed
/// usage, so pipeline tests can assert on usage sums.
#[derive(Debug)]
pub struct PassthroughCleaner {
    pub usage: LlmUsage,
    pub failure: Option<String>,
    /// Segment counts of every `clean` call.
    pub cleanups: CallLog<usize>,
}

impl Default for PassthroughCleaner {
    fn default() -> Self {
        PassthroughCleaner {
            usage: LlmUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                requests: 1,
            },
            failure: None,
            cleanups: CallLog::new(),
        }
    }
}

#[async_trait]
impl TranscriptCleaner for PassthroughCleaner {
    async fn clean(&self, input: &CleanupInput) -> BoundaryResult<CleanupOutput> {
        self.cleanups.record(input.segments.len());
        FakeFailure::check(self.failure.as_ref())?;
        Ok(CleanupOutput {
            segments: input.segments.clone(),
            failed_chunks: Vec::new(),
            usage: self.usage,
        })
    }
}

/// A `MeetingSummarizer` that builds a deterministic [`SummaryOutput`] from
/// the template (one bullet per section quoting the first segment) or
/// returns a canned one; records inputs and can fail.
#[derive(Debug)]
pub struct FakeSummarizer {
    pub canned: Option<SummaryOutput>,
    pub usage: LlmUsage,
    pub failure: Option<String>,
    /// Template ids of every `summarize` call.
    pub summaries: CallLog<String>,
}

impl Default for FakeSummarizer {
    fn default() -> Self {
        FakeSummarizer {
            canned: None,
            usage: LlmUsage {
                prompt_tokens: 200,
                completion_tokens: 100,
                requests: 1,
            },
            failure: None,
            summaries: CallLog::new(),
        }
    }
}

impl FakeSummarizer {
    /// The deterministic output for `input`.
    #[must_use]
    pub fn output(input: &SummaryInput, usage: LlmUsage) -> SummaryOutput {
        let first_text = input
            .segments
            .first()
            .map_or("Nothing was said.", |segment| segment.text.as_str());
        let first_label = input
            .speakers
            .first()
            .map_or("Speaker 1", |speaker| speaker.cluster_label.as_str());
        let sections = input
            .template
            .sections
            .iter()
            .map(|section| SummarySection {
                id: section.id.clone(),
                heading: section.heading.clone(),
                bullets: vec![SummaryBullet {
                    lead: first_label.to_owned(),
                    text: first_text.to_owned(),
                }],
            })
            .collect();
        SummaryOutput {
            title: format!("Summary of {}", input.meeting.title),
            summary: SummaryDocument {
                template_id: input.template.id.clone(),
                language: input.meeting.language.clone(),
                sections,
            },
            decisions: vec![format!("Decision from {first_label}.")],
            tasks: vec![MeetingTask {
                id: derived_uuid(input.meeting.id, "fake-task-0"),
                meeting_id: input.meeting.id,
                text: format!("Follow up on {}.", input.template.display_name),
                assignee_person_id: None,
                assignee_name: Some(first_label.to_owned()),
                priority: TaskPriority::Normal,
                due_date: None,
                done: false,
            }],
            speaker_names: input
                .speakers
                .iter()
                .map(|speaker| SpeakerNameSuggestion {
                    speaker_id: speaker.id,
                    name: None,
                    confidence: 0.0,
                    evidence: "The fake summarizer never guesses names.".to_owned(),
                })
                .collect(),
            language: input.meeting.language.clone(),
            usage,
        }
    }
}

#[async_trait]
impl MeetingSummarizer for FakeSummarizer {
    async fn summarize(&self, input: &SummaryInput) -> BoundaryResult<SummaryOutput> {
        self.summaries.record(input.template.id.clone());
        FakeFailure::check(self.failure.as_ref())?;
        if let Some(canned) = &self.canned {
            return Ok(canned.clone());
        }
        Ok(Self::output(input, self.usage))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_data;
    use crate::{LlmFinishReason, LlmMessage, LlmResponseFormat, LlmRole, SummaryTemplate};

    fn request(content: &str) -> LlmRequest {
        LlmRequest {
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: content.to_owned(),
            }],
            response_format: LlmResponseFormat::Text,
            temperature: None,
            max_tokens: None,
            purpose: "test".to_owned(),
        }
    }

    #[tokio::test]
    async fn the_model_answers_in_order_and_then_runs_dry() {
        let model = FakeLanguageModel::new([LlmResponse {
            text: "one".to_owned(),
            finish_reason: LlmFinishReason::Stop,
            usage: None,
            model: None,
        }]);
        assert_eq!(model.complete(&request("a")).await.unwrap().text, "one");
        let error = model.complete(&request("b")).await.unwrap_err();
        assert!(error.is::<Exhausted>());
        assert_eq!(model.requests.count(), 2);
        assert_eq!(model.remaining(), 0);
    }

    #[tokio::test]
    async fn the_cleaner_passes_segments_through() {
        let cleaner = PassthroughCleaner::default();
        let input = CleanupInput {
            segments: Vec::new(),
            language: None,
            participants: Vec::new(),
            speakers: Vec::new(),
            known_people: Vec::new(),
        };
        let output = cleaner.clean(&input).await.unwrap();
        assert_eq!(output.segments, Vec::new());
        assert_eq!(output.usage.prompt_tokens, 100);
        assert_eq!(cleaner.cleanups.entries(), vec![0]);
    }

    #[tokio::test]
    async fn the_summarizer_fills_every_template_section() {
        let summarizer = FakeSummarizer::default();
        let template = SummaryTemplate::bundled_with_id("default").unwrap().clone();
        let input = SummaryInput {
            meeting: sample_data::meeting(),
            segments: Vec::new(),
            speakers: Vec::new(),
            participants: Vec::new(),
            known_people: Vec::new(),
            template: template.clone(),
        };
        let output = summarizer.summarize(&input).await.unwrap();
        assert_eq!(output.title, "Summary of Call 2026-09-29 15:49");
        assert_eq!(output.summary.sections.len(), template.sections.len());
        assert_eq!(
            output.summary.sections[0].bullets[0].text,
            "Nothing was said."
        );
        assert_eq!(output.tasks[0].text, "Follow up on Default.");
        assert_eq!(output.language, Some("de".into()));
        assert_eq!(summarizer.summaries.entries(), vec!["default".to_owned()]);
        // Codable in Swift: the output round-trips through JSON.
        let text = crate::json::to_column_string(&output).unwrap();
        assert_eq!(
            serde_json::from_str::<SummaryOutput>(&text).unwrap(),
            output
        );
    }
}
