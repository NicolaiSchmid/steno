//! One completion against the configured model service.
//! Swift: `Sources/StenoCore/Protocols/LanguageModel.swift`.

use async_trait::async_trait;

use super::BoundaryResult;
use crate::{LlmRequest, LlmResponse};

/// One completion against the configured model service (an
/// OpenAI-compatible endpoint or the Codex backend). The only code path
/// besides [`Destination`](super::Destination) that may send bytes off the
/// device, and it sends text only.
#[async_trait]
pub trait LanguageModel: Send + Sync {
    async fn complete(&self, request: &LlmRequest) -> BoundaryResult<LlmResponse>;
}
