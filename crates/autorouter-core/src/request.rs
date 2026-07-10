//! Universal request type.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::ids::{RequestId, SessionId};
use crate::message::Message;
use crate::model::ProviderKind;
use crate::tool::ToolDefinition;
use crate::usage::Usage;

/// Global counter for unique stream IDs. Incremented on every
/// `UniversalRequest::default()` to guarantee per-stream identity
/// without relying on pointer addresses.
static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);

/// Per-request metadata that follows the request through translation,
/// routing, and observability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestContext {
    pub request_id: RequestId,
    pub session_id: SessionId,
    #[serde(default)]
    pub tags: Vec<String>,
    pub source_provider: ProviderKind,
    pub target_provider: ProviderKind,
}

impl RequestContext {
    /// Construct a minimal context for a request originating from
    /// `source` and destined for `target`.
    pub fn new(source: ProviderKind, target: ProviderKind) -> Self {
        Self {
            request_id: RequestId::new(),
            session_id: SessionId::new(),
            tags: Vec::new(),
            source_provider: source,
            target_provider: target,
        }
    }

    /// Attach caller-supplied tags (from the `X-AutoRouter-Tag`
    /// header). Tags reach the smart router so tag-based rules
    /// (`match_tags_all` / `match_tags_any`) can match on
    /// per-request tags in addition to `routing.default_tags`.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
}

/// The provider-neutral request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UniversalRequest {
    /// Unique per-instance stream identifier, used in place of
    /// pointer addresses (`*const _ as usize`) for per-stream state
    /// maps. Auto-assigned from a global counter on construction.
    #[serde(skip)]
    pub stream_id: u64,
    pub model: String,
    /// Optional system prompt as a separate field (e.g. OpenAI Responses instructions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    /// Optional tool choice override (e.g. "any" / "none" / specific tool name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    /// Request metadata (user id, trace id, etc.) round-tripped losslessly.
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "is_null_value")]
    pub extra: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_usage")]
    pub prior_usage: Usage,
}

fn is_null_value(v: &serde_json::Value) -> bool {
    v.is_null()
}

impl Default for UniversalRequest {
    fn default() -> Self {
        Self {
            stream_id: NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed),
            model: String::new(),
            system: None,
            messages: Vec::new(),
            tool_choice: None,
            metadata: serde_json::Value::Null,
            tools: Vec::new(),
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            stop: Vec::new(),
            stream: false,
            extra: serde_json::Value::Null,
            user: None,
            prior_usage: Usage::default(),
        }
    }
}

fn is_default_usage(u: &Usage) -> bool {
    u.tokens.input.is_none()
        && u.tokens.output.is_none()
        && u.tokens.cache_read.is_none()
        && u.tokens.cache_write.is_none()
        && u.tokens.reasoning.is_none()
        && u.cost_micro_cents.is_none()
}
