//! The summary pass.
//! Swift: `MeetingSummarizer` in `Sources/StenoCore/Protocols/PipelineBoundaries.swift`.

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{SummaryInput, SummaryOutput};

/// The summary pass (the LLM crate): title, structured summary, decisions,
/// tasks and speaker name suggestions for the selected template.
#[async_trait]
pub trait MeetingSummarizer: Send + Sync {
    async fn summarize(&self, input: &SummaryInput) -> BoundaryResult<SummaryOutput>;
}
