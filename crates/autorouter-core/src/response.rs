//! Universal response type.

use serde::{Deserialize, Serialize};

use crate::message::ContentPart;
use crate::tool::ToolCall;
use crate::usage::Usage;

/// Reason the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Safety,
    Other,
}

/// The provider-neutral response body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UniversalResponse {
    pub id: String,
    pub model: String,
    pub message: crate::message::Message,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl UniversalResponse {
    pub fn text(&self) -> String {
        self.message
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
