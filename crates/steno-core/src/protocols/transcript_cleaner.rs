//! The cleanup pass.

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{CleanupInput, CleanupOutput};

/// The cleanup pass (the LLM crate): fixes Denglish, casing and names,
/// chunked, segment count and order preserved.
#[async_trait]
pub trait TranscriptCleaner: Send + Sync {
    async fn clean(&self, input: &CleanupInput) -> BoundaryResult<CleanupOutput>;
}
