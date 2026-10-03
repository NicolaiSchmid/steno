//! Pass 1: the transcript in chunks through the model, fixing speech-to-text
//! mistakes while keeping count, order and wording.
//! Swift: `Sources/StenoLLM/Cleanup/`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use steno_core::{
    BoundaryResult, CleanupInput, CleanupOutput, LanguageModel, LanguageTag, LlmMessage,
    LlmRequest, LlmResponseFormat, LlmRole, LlmUsage, TranscriptCleaner, async_trait,
};

use crate::budget::{BudgetPolicy, counted_usage};
use crate::concurrency::map_bounded;
use crate::labels::{SpeakerLabels, render_lines};
use crate::language::OutputLanguage;
use crate::{
    JsonSchema, LlmEndpoint, LlmError, StructuredOutputDecoder, TokenBudget, TranscriptChunk,
    TranscriptChunker,
};

/// The model's answer to one cleanup chunk: the same segments by index.
/// Swift: `Sources/StenoLLM/Cleanup/CleanupDraft.swift`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupDraft {
    pub segments: Vec<CleanupDraftSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupDraftSegment {
    pub index: i64,
    pub text: String,
}

impl CleanupDraft {
    /// Word counts may drift by at most this ratio, or by one word so that
    /// "Git Hub" can become "GitHub".
    pub const WORD_RATIO: (f64, f64) = (0.7, 1.3);

    /// The draft that echoes `chunk` unchanged.
    #[must_use]
    pub fn echo(chunk: &TranscriptChunk) -> Self {
        CleanupDraft {
            segments: chunk
                .segments
                .iter()
                .enumerate()
                .map(|(index, segment)| CleanupDraftSegment {
                    index: i64::try_from(index).unwrap_or(i64::MAX),
                    text: segment.text.clone(),
                })
                .collect(),
        }
    }

    /// Why the draft does not fit its chunk, in prompt-ready sentences;
    /// empty when it does. Same count, indices `0..n` each once, no emptied
    /// segment, word count within [`Self::WORD_RATIO`] of the input.
    #[must_use]
    pub fn problems(&self, chunk: &TranscriptChunk) -> Vec<String> {
        let expected = chunk.segments.len();
        let expected_count = i64::try_from(expected).unwrap_or(i64::MAX);
        let mut problems = Vec::new();
        if self.segments.len() != expected {
            problems.push(format!(
                "Expected {expected} segments, got {}.",
                self.segments.len()
            ));
        }
        let mut indices: Vec<i64> = self.segments.iter().map(|segment| segment.index).collect();
        indices.sort_unstable();
        if indices != (0..expected_count).collect::<Vec<_>>() {
            problems.push(format!(
                "Indices must be 0 to {}, each exactly once; got {}.",
                expected_count - 1,
                indices
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !problems.is_empty() {
            return problems;
        }
        for segment in &self.segments {
            let Ok(position) = usize::try_from(segment.index) else {
                continue;
            };
            let original_words = Self::word_count(&chunk.segments[position].text);
            let cleaned_words = Self::word_count(&segment.text);
            if original_words == 0 {
                continue;
            }
            if cleaned_words == 0 {
                problems.push(format!("Segment {} came back empty.", segment.index));
            } else if cleaned_words.abs_diff(original_words) > 1 {
                #[allow(clippy::cast_precision_loss)]
                let ratio = cleaned_words as f64 / original_words as f64;
                if !(Self::WORD_RATIO.0..=Self::WORD_RATIO.1).contains(&ratio) {
                    problems.push(format!(
                        "Segment {} changed from {original_words} to {cleaned_words} words; keep the wording, only fix spelling, casing and punctuation.",
                        segment.index
                    ));
                }
            }
        }
        problems
    }

    /// The cleaned texts in segment order, trimmed; meaningful once
    /// [`problems`](Self::problems) is empty.
    #[must_use]
    pub fn ordered_texts(&self) -> Vec<String> {
        let mut segments: Vec<&CleanupDraftSegment> = self.segments.iter().collect();
        segments.sort_by_key(|segment| segment.index);
        segments
            .into_iter()
            .map(|segment| segment.text.trim().to_owned())
            .collect()
    }

    #[must_use]
    pub fn word_count(text: &str) -> usize {
        text.split_whitespace().count()
    }
}

/// Names the cleanup pass must spell exactly: participants (calendar
/// attendees included) first, then known people, each once
/// (case-insensitively), in order of appearance. No product glossary in v1.
#[must_use]
pub fn glossary(input: &CleanupInput) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    input
        .participants
        .iter()
        .map(|participant| participant.display_name.as_str())
        .chain(
            input
                .known_people
                .iter()
                .map(|person| person.display_name.as_str()),
        )
        .filter_map(|name| {
            let trimmed = name.trim();
            (!trimmed.is_empty() && seen.insert(trimmed.to_lowercase())).then(|| trimmed.to_owned())
        })
        .collect()
}

/// Builds the pass 1 request for one chunk: fix speech-to-text mistakes,
/// keep count, order and wording. Temperature 0. Pinned by the goldens in
/// `Tests/Fixtures/llm/prompts/cleanup-*.txt`.
/// Swift: `Sources/StenoLLM/Cleanup/CleanupPromptBuilder.swift`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupPromptBuilder {
    /// The endpoint's ceiling for one answer.
    pub max_output_tokens: i64,
}

impl Default for CleanupPromptBuilder {
    fn default() -> Self {
        CleanupPromptBuilder {
            max_output_tokens: 4_096,
        }
    }
}

impl CleanupPromptBuilder {
    #[must_use]
    pub fn new(max_output_tokens: i64) -> Self {
        CleanupPromptBuilder { max_output_tokens }
    }

    #[must_use]
    pub fn output_schema() -> JsonSchema {
        JsonSchema::object(vec![(
            "segments",
            JsonSchema::array(JsonSchema::object(vec![
                (
                    "index",
                    JsonSchema::integer().described("the segment's number as given"),
                ),
                ("text", JsonSchema::string().described("the corrected text")),
            ])),
        )])
    }

    /// `glossary` is [`glossary`]: the names to spell exactly.
    #[must_use]
    pub fn build(
        &self,
        chunk: &TranscriptChunk,
        language: Option<&LanguageTag>,
        glossary: &[String],
        labels: &SpeakerLabels,
    ) -> LlmRequest {
        let count = chunk.segments.len();
        let language_name = OutputLanguage::prompt_name(&OutputLanguage::resolve(language));
        let mut system = vec![
            "You are Steno's transcript editor. You receive numbered segments of a speech-to-text transcript and return the same segments, corrected, as one JSON object and nothing else.".to_owned(),
            String::new(),
            format!("Meeting language: {language_name}. Speakers may mix {language_name} and English; keep every code-switch exactly as spoken and never translate."),
        ];
        if !glossary.is_empty() {
            system.push(format!(
                "Names to spell exactly like this: {}.",
                glossary.join(", ")
            ));
        }
        system.extend([
            String::new(),
            "Rules:".to_owned(),
            format!("- Return exactly {count} segments with the indices 0 to {}, each once, in order. Never merge, split, drop, add, shorten, expand or summarise a segment.", count.saturating_sub(1)),
            "- Fix speech-to-text mistakes only: misheard anglicisms and product names (\"git hub\" to \"GitHub\", \"kuber netes\" to \"Kubernetes\"), the names listed above, German noun capitalisation, sentence-initial capitals, punctuation.".to_owned(),
            "- Keep the wording, the word order and the speaker's register. Keep fillers unless they are transcription noise.".to_owned(),
            "- When a segment needs no change, return its text unchanged.".to_owned(),
            "- Lines marked as context are read-only and are not part of the answer.".to_owned(),
            String::new(),
            "Return exactly this JSON shape:".to_owned(),
            Self::output_schema().prompt_text(),
        ]);
        let mut user: Vec<String> = Vec::new();
        if !chunk.leading_context.is_empty() {
            user.push("Context (read-only, do not return):".to_owned());
            user.push(
                chunk
                    .leading_context
                    .iter()
                    .map(|segment| {
                        format!(
                            "[context] {}: {}",
                            labels.label(segment.speaker_id),
                            segment.text
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            user.push(String::new());
        }
        user.push(format!("Segments to correct ({count}):"));
        user.push(render_lines(&chunk.segments, labels));
        LlmRequest {
            messages: vec![
                LlmMessage {
                    role: LlmRole::System,
                    content: system.join("\n"),
                },
                LlmMessage {
                    role: LlmRole::User,
                    content: user.join("\n"),
                },
            ],
            response_format: LlmResponseFormat::JsonSchema {
                name: "transcript_cleanup".to_owned(),
                schema: Self::output_schema().json_value(),
                strict: true,
            },
            temperature: Some(0.0),
            max_tokens: Some(self.output_tokens(chunk, language)),
            purpose: "cleanup".to_owned(),
        }
    }

    /// The same request with the rejected answer and the reason appended,
    /// for the one retry.
    #[must_use]
    pub fn build_retry(request: &LlmRequest, previous_answer: &str, error: &str) -> LlmRequest {
        let mut retry = request.clone();
        retry.messages.push(LlmMessage {
            role: LlmRole::Assistant,
            content: previous_answer.to_owned(),
        });
        retry.messages.push(LlmMessage {
            role: LlmRole::User,
            content: format!(
                "That answer was rejected: {error} Return the JSON again with every segment, the same indices and the wording kept."
            ),
        });
        "cleanup-retry".clone_into(&mut retry.purpose);
        retry
    }

    /// The answer repeats the chunk as JSON: its text plus the framing per
    /// segment and the envelope, with headroom ([`BudgetPolicy`]), capped at
    /// the endpoint's ceiling.
    #[must_use]
    pub fn output_tokens(&self, chunk: &TranscriptChunk, language: Option<&LanguageTag>) -> i64 {
        let text = TokenBudget::estimate_tokens(
            &chunk
                .segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            language,
        );
        let framing = i64::try_from(chunk.segments.len()).unwrap_or(i64::MAX)
            * BudgetPolicy::CLEANUP_FRAMING_TOKENS_PER_SEGMENT;
        let estimate = (text + framing + BudgetPolicy::CLEANUP_ANSWER_FIXED_TOKENS)
            * BudgetPolicy::CLEANUP_ANSWER_HEADROOM_NUMERATOR
            / BudgetPolicy::CLEANUP_ANSWER_HEADROOM_DENOMINATOR;
        estimate
            .max(BudgetPolicy::CLEANUP_ANSWER_FLOOR_TOKENS)
            .min(self.max_output_tokens)
    }
}

/// Pass 1: the transcript in chunks through the model, at most
/// `endpoint.max_concurrent_requests` at a time. A chunk whose answer is
/// refused, fails validation (count, indices, emptied or reworded segments)
/// or does not decode is retried once with the reason appended, then kept
/// as raw text and listed in `failed_chunks`. Network and HTTP failures
/// propagate: the pipeline keeps the raw transcript and marks the stage
/// failed. `raw_text` is never touched; ids, order and count come back as
/// they went in. Swift: `Sources/StenoLLM/Cleanup/LLMTranscriptCleaner.swift`.
pub struct LlmTranscriptCleaner {
    pub model: Arc<dyn LanguageModel>,
    pub endpoint: LlmEndpoint,
    pub chunker: TranscriptChunker,
}

impl LlmTranscriptCleaner {
    /// A cleaner whose chunker follows the endpoint's cleanup budget.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use steno_llm::{LlmEndpoint, LlmTranscriptCleaner, OpenAiCompatibleClient};
    /// use url::Url;
    ///
    /// let endpoint = LlmEndpoint::new(Url::parse("http://127.0.0.1:1234/v1")?, "qwen3");
    /// let model = Arc::new(OpenAiCompatibleClient::new(endpoint.clone(), None));
    /// let cleaner = LlmTranscriptCleaner::new(model, endpoint);
    /// assert_eq!(cleaner.endpoint.max_concurrent_requests, 2);
    /// # Ok::<(), url::ParseError>(())
    /// ```
    #[must_use]
    pub fn new(model: Arc<dyn LanguageModel>, endpoint: LlmEndpoint) -> Self {
        let chunker = TranscriptChunker::with_budget(endpoint.cleanup_chunk_budget_tokens(), 3);
        LlmTranscriptCleaner {
            model,
            endpoint,
            chunker,
        }
    }

    #[must_use]
    pub fn with_chunker(mut self, chunker: TranscriptChunker) -> Self {
        self.chunker = chunker;
        self
    }

    /// The pass over `input` with this crate's own types; the
    /// [`TranscriptCleaner`] impl calls it.
    pub async fn clean_input(&self, input: &CleanupInput) -> BoundaryResult<CleanupOutput> {
        let chunks = self.chunker.chunk(&input.segments, input.language.as_ref());
        let glossary = glossary(input);
        let labels = SpeakerLabels::new(&input.speakers);
        let builder = CleanupPromptBuilder::new(self.endpoint.max_output_tokens);

        let results = map_bounded(
            0..chunks.len(),
            self.endpoint.max_concurrent_requests,
            |position| {
                let chunk = &chunks[position];
                let request = builder.build(chunk, input.language.as_ref(), &glossary, &labels);
                self.clean_chunk(request, chunk)
            },
        )
        .await?;

        let mut segments = input.segments.clone();
        let mut offset = 0;
        let mut failed = Vec::new();
        let mut usage = LlmUsage::ZERO;
        for (chunk, (texts, chunk_usage)) in chunks.iter().zip(results) {
            usage = usage + chunk_usage;
            match texts {
                Some(texts) => {
                    for (position, text) in texts.into_iter().enumerate() {
                        segments[offset + position].text = text;
                    }
                }
                None => failed.push(chunk.index),
            }
            offset += chunk.segments.len();
        }
        Ok(CleanupOutput {
            segments,
            failed_chunks: failed,
            usage,
        })
    }

    /// One chunk: request, validate, one retry with the reason, else
    /// `None`.
    async fn clean_chunk(
        &self,
        request: LlmRequest,
        chunk: &TranscriptChunk,
    ) -> BoundaryResult<(Option<Vec<String>>, LlmUsage)> {
        let mut request = request;
        let mut usage = LlmUsage::ZERO;
        for attempt in 0..2 {
            let response = match self.model.complete(&request).await {
                Ok(response) => response,
                Err(error) => {
                    let Some(problem) = error
                        .downcast_ref::<LlmError>()
                        .filter(|error| error.is_answer_problem())
                    else {
                        return Err(error);
                    };
                    // A refusal arrives as an error from the client, with no
                    // body to validate; it costs a request all the same.
                    usage = usage
                        + LlmUsage {
                            prompt_tokens: 0,
                            completion_tokens: 0,
                            requests: 1,
                        };
                    if attempt > 0 {
                        break;
                    }
                    request =
                        CleanupPromptBuilder::build_retry(&request, "", &format!("{problem}."));
                    continue;
                }
            };
            usage = usage + counted_usage(&response);
            let rejection = match StructuredOutputDecoder::decode::<CleanupDraft>(&response) {
                Ok(draft) => {
                    let problems = draft.problems(chunk);
                    if problems.is_empty() {
                        return Ok((Some(draft.ordered_texts()), usage));
                    }
                    problems.join(" ")
                }
                Err(error) if error.is_answer_problem() => format!("{error}."),
                Err(error) => return Err(error.into()),
            };
            if attempt > 0 {
                break;
            }
            request = CleanupPromptBuilder::build_retry(&request, &response.text, &rejection);
        }
        Ok((None, usage))
    }
}

#[async_trait]
impl TranscriptCleaner for LlmTranscriptCleaner {
    async fn clean(&self, input: &CleanupInput) -> BoundaryResult<CleanupOutput> {
        self.clean_input(input).await
    }
}
