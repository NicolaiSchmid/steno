//! Splits a transcript on segment boundaries into runs that fit one request.
//! Swift: `Sources/StenoLLM/Budget/TranscriptChunker.swift`.

use steno_core::{LanguageTag, TranscriptSegment};

use crate::TokenBudget;

/// A run of consecutive segments that fits one request, with the tail of
/// the previous chunk as read-only context.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptChunk {
    pub index: usize,
    pub segments: Vec<TranscriptSegment>,
    pub leading_context: Vec<TranscriptSegment>,
    /// Estimated tokens of `segments` as transcript lines, context excluded.
    pub estimated_tokens: i64,
}

/// Splits a transcript on segment boundaries. A chunk grows to at least
/// `target_tokens`, then closes at the next speaker change, or at the
/// segment that would push it past `max_tokens`. A single segment larger
/// than `max_tokens` gets a chunk of its own. Order is preserved;
/// concatenating every chunk's `segments` yields the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptChunker {
    pub target_tokens: i64,
    pub max_tokens: i64,
    pub context_segments: usize,
}

impl Default for TranscriptChunker {
    fn default() -> Self {
        TranscriptChunker::new(2_000, 3_000, 3)
    }
}

impl TranscriptChunker {
    /// Tokens for the `[n] Speaker 1: ` framing of one transcript line.
    pub const LINE_OVERHEAD_TOKENS: i64 = 6;

    #[must_use]
    pub fn new(target_tokens: i64, max_tokens: i64, context_segments: usize) -> Self {
        let target_tokens = target_tokens.max(1);
        TranscriptChunker {
            target_tokens,
            max_tokens: max_tokens.max(target_tokens),
            context_segments,
        }
    }

    /// A chunker whose chunks never exceed `budget`, keeping the defaults
    /// where the budget allows.
    #[must_use]
    pub fn with_budget(budget: i64, context_segments: usize) -> Self {
        TranscriptChunker::new(budget.min(2_000), budget.min(3_000), context_segments)
    }

    #[must_use]
    pub fn estimate_segment(segment: &TranscriptSegment, language: Option<&LanguageTag>) -> i64 {
        TokenBudget::estimate_tokens(&segment.text, language) + Self::LINE_OVERHEAD_TOKENS
    }

    #[must_use]
    pub fn estimate_segments(
        segments: &[TranscriptSegment],
        language: Option<&LanguageTag>,
    ) -> i64 {
        segments
            .iter()
            .map(|segment| Self::estimate_segment(segment, language))
            .sum()
    }

    #[must_use]
    pub fn chunk(
        &self,
        segments: &[TranscriptSegment],
        language: Option<&LanguageTag>,
    ) -> Vec<TranscriptChunk> {
        let mut chunks: Vec<TranscriptChunk> = Vec::new();
        let mut current: Vec<TranscriptSegment> = Vec::new();
        let mut current_tokens: i64 = 0;

        let close = |chunks: &mut Vec<TranscriptChunk>,
                     current: &mut Vec<TranscriptSegment>,
                     current_tokens: &mut i64| {
            if current.is_empty() {
                return;
            }
            let leading_context = chunks.last().map_or_else(Vec::new, |previous| {
                let skip = previous
                    .segments
                    .len()
                    .saturating_sub(self.context_segments);
                previous.segments[skip..].to_vec()
            });
            chunks.push(TranscriptChunk {
                index: chunks.len(),
                segments: std::mem::take(current),
                leading_context,
                estimated_tokens: *current_tokens,
            });
            *current_tokens = 0;
        };

        for segment in segments {
            let tokens = Self::estimate_segment(segment, language);
            if let Some(last) = current.last() {
                let exceeds_max = current_tokens + tokens > self.max_tokens;
                let turn_after_target =
                    current_tokens >= self.target_tokens && segment.speaker_id != last.speaker_id;
                if exceeds_max || turn_after_target {
                    close(&mut chunks, &mut current, &mut current_tokens);
                }
            }
            current.push(segment.clone());
            current_tokens += tokens;
        }
        close(&mut chunks, &mut current, &mut current_tokens);
        chunks
    }
}
