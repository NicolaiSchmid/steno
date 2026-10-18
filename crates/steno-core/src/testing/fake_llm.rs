//! A language model, a cleaner and a summarizer that need no service.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use thiserror::Error;

use super::{CallLog, FakeFailure, lock};
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
/// records every request.
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
        lock(&self.responses).len()
    }
}

#[async_trait]
impl LanguageModel for FakeLanguageModel {
    async fn complete(&self, request: &LlmRequest) -> BoundaryResult<LlmResponse> {
        self.requests.record(request.clone());
        lock(&self.responses)
            .pop_front()
            .ok_or_else(|| Box::new(Exhausted) as BoxError)
    }
}

/// A `TranscriptCleaner` that returns the segments untouched with a fixed usage.
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

/// A `MeetingSummarizer` that builds a deterministic [`SummaryOutput`] from the
/// template or returns a canned one.
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
