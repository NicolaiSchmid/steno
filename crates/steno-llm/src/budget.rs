//! Tokenizer-free budgeting and the numbers a small context window makes
//! someone tune.
//! Swift: `Sources/StenoLLM/Budget/BudgetPolicy.swift` and `TokenBudget.swift`.

use steno_core::{LanguageTag, LlmResponse, LlmUsage};

use crate::LlmEndpoint;

/// The numbers a small context window makes someone tune, in one place and
/// with units in every name. [`LlmEndpoint`] derives its budgets from them
/// so the app can show "cleanup chunks of N tokens, summary answers of M"
/// next to the context setting.
pub struct BudgetPolicy;

impl BudgetPolicy {
    /// Cleanup: half the context for the chunk, the other half for its
    /// echo, less this reserve for the system prompt and framing.
    pub const CLEANUP_CONTEXT_RESERVE_TOKENS: i64 = 512;
    /// The smallest chunk budget worth sending.
    pub const CLEANUP_CHUNK_FLOOR_TOKENS: i64 = 256;

    /// The cleanup answer repeats the chunk as JSON: about this much
    /// framing per segment (`{"index": n, "text": ""}` and separators)...
    pub const CLEANUP_FRAMING_TOKENS_PER_SEGMENT: i64 = 12;
    /// ...plus this much for the envelope, then a third of headroom, never
    /// under the floor.
    pub const CLEANUP_ANSWER_FIXED_TOKENS: i64 = 64;
    pub const CLEANUP_ANSWER_HEADROOM_NUMERATOR: i64 = 4;
    pub const CLEANUP_ANSWER_HEADROOM_DENOMINATOR: i64 = 3;
    pub const CLEANUP_ANSWER_FLOOR_TOKENS: i64 = 256;

    /// Summary: the answer gets at most this fraction of the context (a
    /// quarter), capped by the endpoint's `max_output_tokens`, at least the
    /// floor.
    pub const SUMMARY_OUTPUT_CONTEXT_DIVISOR: i64 = 4;
    pub const SUMMARY_OUTPUT_FLOOR_TOKENS: i64 = 256;

    /// Map: one chunk's notes may take its share of the input budget, at
    /// most the ceiling, at least the floor (below it the chunk cannot be
    /// carried and the pass refuses before the first call).
    pub const MAP_NOTES_CEILING_TOKENS: i64 = 1_500;
    pub const MAP_NOTES_FLOOR_TOKENS: i64 = 256;
    /// One notes point, a sentence plus its JSON framing, costs about this
    /// many tokens; the map prompt's length rule follows from the ceiling.
    pub const TOKENS_PER_NOTES_POINT: i64 = 60;
    pub const MAP_NOTES_MINIMUM_POINTS: i64 = 3;

    /// Message framing beyond the system prompt text (role markers, the
    /// user message's header line), counted against the input budget.
    pub const PROMPT_FRAMING_TOKENS: i64 = 64;
}

impl LlmEndpoint {
    /// Tokens one cleanup chunk may hold: half the context less the reserve.
    #[must_use]
    pub fn cleanup_chunk_budget_tokens(&self) -> i64 {
        BudgetPolicy::CLEANUP_CHUNK_FLOOR_TOKENS
            .max(self.context_tokens / 2 - BudgetPolicy::CLEANUP_CONTEXT_RESERVE_TOKENS)
    }

    /// Tokens kept for a summary answer: the endpoint's ceiling, at most a
    /// quarter of the context.
    #[must_use]
    pub fn summary_reserved_output_tokens(&self) -> i64 {
        BudgetPolicy::SUMMARY_OUTPUT_FLOOR_TOKENS.max(
            self.max_output_tokens
                .min(self.context_tokens / BudgetPolicy::SUMMARY_OUTPUT_CONTEXT_DIVISOR),
        )
    }
}

/// Tokenizer-free budgeting. English prose runs about 4 bytes per token on
/// current tokenizers, German 2.8 to 3.3; Steno divides UTF-8 bytes by 3.6
/// for English and by 3.0 for everything else (German, mixed, unknown), a
/// deliberate 10 to 30 percent overestimate. Real counts come back in the
/// server's `usage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBudget {
    pub context_tokens: i64,
    /// Tokens kept free for the model's answer.
    pub reserved_output_tokens: i64,
    /// Tokens the system prompt and message framing take.
    pub prompt_overhead_tokens: i64,
}

impl TokenBudget {
    /// What is left for the transcript or notes; never negative.
    #[must_use]
    pub fn input_budget(&self) -> i64 {
        (self.context_tokens - self.reserved_output_tokens - self.prompt_overhead_tokens).max(0)
    }

    #[must_use]
    pub fn fits(&self, tokens: i64) -> bool {
        tokens <= self.input_budget()
    }

    /// Tokens one map call may spend on its chunk's notes: the chunk's share
    /// of the input budget, so the notes of every chunk fit the reduce
    /// prompt together and the up-front check and the post-map check agree;
    /// within [`BudgetPolicy`]'s ceiling and floor (below the floor a chunk
    /// cannot be carried, and the caller refuses before the first call).
    #[must_use]
    pub fn map_notes_output_tokens(&self, chunk_count: usize) -> i64 {
        let count = i64::try_from(chunk_count).unwrap_or(i64::MAX).max(1);
        BudgetPolicy::MAP_NOTES_CEILING_TOKENS
            .min(BudgetPolicy::MAP_NOTES_FLOOR_TOKENS.max(self.input_budget() / count))
    }

    #[must_use]
    pub fn bytes_per_token(language: Option<&LanguageTag>) -> f64 {
        if language.is_some_and(|tag| primary_subtag(tag) == "en") {
            3.6
        } else {
            3.0
        }
    }

    /// `ceil(bytes / bytes_per_token)`; 0 for empty text.
    #[must_use]
    pub fn estimate_tokens(text: &str, language: Option<&LanguageTag>) -> i64 {
        let bytes = text.len();
        if bytes == 0 {
            return 0;
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        {
            (bytes as f64 / Self::bytes_per_token(language)).ceil() as i64
        }
    }
}

/// The language subtag alone, lowercased: `de` for `de-CH`.
#[must_use]
pub fn primary_subtag(tag: &LanguageTag) -> String {
    tag.as_str()
        .split('-')
        .next()
        .unwrap_or(tag.as_str())
        .to_lowercase()
}

/// What one call cost; a body without `usage` still counts as a request,
/// so per-meeting totals stay honest about the number of calls.
#[must_use]
pub fn counted_usage(response: &LlmResponse) -> LlmUsage {
    response.usage.unwrap_or(LlmUsage {
        prompt_tokens: 0,
        completion_tokens: 0,
        requests: 1,
    })
}
