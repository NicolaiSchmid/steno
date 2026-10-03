//! Known voices across meetings.
//! Swift: `Sources/StenoCore/Protocols/SpeakerMemory.swift`.

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{Embedding, SpeakerMatch};

/// The default `margin` of [`SpeakerMemory::match_voice`]: the best
/// candidate must lead the runner-up by this much.
pub const DEFAULT_MATCH_MARGIN: f32 = 0.05;

/// Known voices across meetings: pure math over the store's persons. A
/// person's voice (`Person::embedding` and `sample_count`) is written by
/// the store on confirm and merge; this trait only reads it.
#[async_trait]
pub trait SpeakerMemory: Send + Sync {
    /// Candidates ranked by cosine similarity, best first; feeds
    /// `match_voice` and the speaker picker.
    async fn candidates(
        &self,
        embedding: &Embedding,
        limit: usize,
    ) -> BoundaryResult<Vec<SpeakerMatch>>;

    /// The best candidate at or above `threshold` that is at least `margin`
    /// above the runner-up; `None` otherwise. Exactly Swift's arithmetic: a
    /// NaN best never matches (Swift's `>=` rejects it, Rust's `<` would
    /// not, hence the explicit check), while a non-finite runner-up does
    /// not block the match, because `best - NaN < margin` is false in both
    /// languages. A NaN threshold matches nobody, as Swift's `>=` does.
    /// Swift's default `margin` is [`DEFAULT_MATCH_MARGIN`].
    async fn match_voice(
        &self,
        embedding: &Embedding,
        threshold: f32,
        margin: f32,
    ) -> BoundaryResult<Option<SpeakerMatch>> {
        let mut ranked = self.candidates(embedding, 2).await?;
        let Some(best) = ranked.first() else {
            return Ok(None);
        };
        if best.similarity.is_nan() || threshold.is_nan() || best.similarity < threshold {
            return Ok(None);
        }
        if let Some(runner_up) = ranked.get(1)
            && best.similarity - runner_up.similarity < margin
        {
            return Ok(None);
        }
        Ok(Some(ranked.swap_remove(0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_data;

    /// A memory that answers with a fixed ranking, so the default
    /// `match_voice` is tested on its own.
    struct Fixed(Vec<SpeakerMatch>);

    #[async_trait]
    impl SpeakerMemory for Fixed {
        async fn candidates(
            &self,
            _embedding: &Embedding,
            limit: usize,
        ) -> BoundaryResult<Vec<SpeakerMatch>> {
            Ok(self.0.iter().take(limit).cloned().collect())
        }
    }

    fn ranked(similarities: &[f32]) -> Fixed {
        Fixed(
            similarities
                .iter()
                .enumerate()
                .map(|(axis, similarity)| SpeakerMatch {
                    person: sample_data::person(axis, "Someone"),
                    similarity: *similarity,
                })
                .collect(),
        )
    }

    async fn matched(memory: &Fixed, threshold: f32, margin: f32) -> Option<f32> {
        memory
            .match_voice(&sample_data::embedding(0), threshold, margin)
            .await
            .unwrap()
            .map(|found| found.similarity)
    }

    #[tokio::test]
    async fn a_similarity_exactly_at_the_threshold_matches() {
        let memory = ranked(&[0.8, 0.5]);
        assert_eq!(matched(&memory, 0.8, DEFAULT_MATCH_MARGIN).await, Some(0.8));
        assert_eq!(matched(&memory, 0.800_01, DEFAULT_MATCH_MARGIN).await, None);
    }

    #[tokio::test]
    async fn a_lead_exactly_equal_to_the_margin_matches() {
        let memory = ranked(&[0.75, 0.5]);
        assert_eq!(matched(&memory, 0.0, 0.25).await, Some(0.75));
        assert_eq!(matched(&memory, 0.0, 0.250_01).await, None);
        assert_eq!(matched(&ranked(&[0.75]), 0.0, 1.0).await, Some(0.75));
        assert_eq!(matched(&ranked(&[]), 0.0, 0.0).await, None);
    }

    #[tokio::test]
    async fn a_nan_best_never_matches() {
        assert_eq!(matched(&ranked(&[f32::NAN]), 0.0, 0.0).await, None);
        assert_eq!(matched(&ranked(&[f32::NAN, 0.9]), 0.0, 0.0).await, None);
    }

    #[tokio::test]
    async fn a_nan_threshold_matches_nobody() {
        assert_eq!(matched(&ranked(&[0.9]), f32::NAN, 0.0).await, None);
        assert_eq!(matched(&ranked(&[0.9, 0.1]), f32::NAN, 0.0).await, None);
        assert_eq!(
            matched(&ranked(&[f32::INFINITY]), f32::NAN, 0.0).await,
            None
        );
    }

    #[tokio::test]
    async fn non_finite_values_elsewhere_decide_as_in_swift() {
        // `inf >= threshold` holds in Swift, so an infinite best matches.
        assert_eq!(
            matched(&ranked(&[f32::INFINITY]), 0.0, 0.0).await,
            Some(f32::INFINITY)
        );
        assert_eq!(matched(&ranked(&[f32::NEG_INFINITY]), 0.0, 0.0).await, None);
        // `best - NaN < margin` is false in both languages: no block.
        assert_eq!(
            matched(&ranked(&[0.9, f32::NAN]), 0.0, DEFAULT_MATCH_MARGIN).await,
            Some(0.9)
        );
    }
}
