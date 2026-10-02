//! Token accounting for the LLM calls of a meeting.
//! Swift: `Sources/StenoCore/Model/LLM.swift`.

use std::ops::Add;

use serde::{Deserialize, Serialize};

/// Token accounting summed over every LLM call of a meeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub requests: i64,
}

impl LlmUsage {
    pub const ZERO: LlmUsage = LlmUsage {
        prompt_tokens: 0,
        completion_tokens: 0,
        requests: 0,
    };
}

impl Add for LlmUsage {
    type Output = LlmUsage;

    fn add(self, rhs: LlmUsage) -> LlmUsage {
        LlmUsage {
            prompt_tokens: self.prompt_tokens + rhs.prompt_tokens,
            completion_tokens: self.completion_tokens + rhs.completion_tokens,
            requests: self.requests + rhs.requests,
        }
    }
}
