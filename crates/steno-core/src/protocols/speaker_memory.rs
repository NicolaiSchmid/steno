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
    /// above the runner-up; `None` otherwise. Swift's default `margin` is
    /// [`DEFAULT_MATCH_MARGIN`].
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
        if best.similarity < threshold {
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
