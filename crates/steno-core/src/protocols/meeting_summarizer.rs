//! The summary pass.

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{SummaryInput, SummaryOutput, TitleInput, TitleOutput};

/// The summary pass (the LLM crate): title, structured summary, decisions,
/// tasks and speaker name suggestions for the selected template.
#[async_trait]
pub trait MeetingSummarizer: Send + Sync {
    async fn summarize(&self, input: &SummaryInput) -> BoundaryResult<SummaryOutput>;

    /// A new title from the stored summary alone, for "Regenerate title".
    /// The default refuses, for summarizers that only summarize.
    async fn retitle(&self, input: &TitleInput) -> BoundaryResult<TitleOutput> {
        let _ = input;
        Err("this summarizer cannot write a title on its own".into())
    }
}
