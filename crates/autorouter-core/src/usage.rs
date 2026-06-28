//! Token usage accounting.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenBreakdown {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
}

impl TokenBreakdown {
    pub fn total(&self) -> u64 {
        self.input.unwrap_or(0)
            + self.output.unwrap_or(0)
            + self.cache_read.unwrap_or(0)
            + self.cache_write.unwrap_or(0)
    }

    /// Total tokens including the reasoning subset (already counted
    /// in `output`). Useful for diagnostics where you want the full
    /// picture, but not for billing.
    pub fn total_with_reasoning(&self) -> u64 {
        self.total() + self.reasoning.unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub tokens: TokenBreakdown,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micro_cents: Option<u64>,
}

impl Usage {
    pub fn total_tokens(&self) -> u64 {
        self.tokens.total()
    }
}
