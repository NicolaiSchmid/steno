//! One completion against the configured model service.

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{LlmRequest, LlmResponse};

/// One completion against the configured model service. Besides
/// [`Destination`](super::Destination) the only path that may send bytes off
/// the device, and it sends text only.
#[async_trait]
pub trait LanguageModel: Send + Sync {
    async fn complete(&self, request: &LlmRequest) -> BoundaryResult<LlmResponse>;
}
