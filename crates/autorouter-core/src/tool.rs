//! Tool / function calling types.

use serde::{Deserialize, Serialize};

/// Definition of a tool/function that the model may invoke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Provider-agnostic tool name.
    pub name: String,
    /// Human-readable description, surfaced to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the tool's parameters, serialised as a
    /// [`serde_json::Value`] to keep things provider-neutral. Providers
    /// that require a subset (Anthropic requires `type: "object"` at the
    /// root) get the value normalised during translation.
    pub parameters: serde_json::Value,
    /// Optional strict-mode flag (OpenAI Responses / Anthropic tool use).
    #[serde(default, skip_serializing_if = "is_false")]
    pub strict: bool,
}

/// Placeholder alias re-exported for compatibility with code that imports
/// `autorouter_core::Tool`. The real definition is [`ToolDefinition`].
#[deprecated(note = "use ToolDefinition instead")]
pub type Tool = ToolDefinition;

/// A tool call emitted by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// The result of running a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: ToolResultBody,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolResultBody {
    Text { text: String },
    Json { value: serde_json::Value },
}

fn is_false(b: &bool) -> bool {
    !*b
}
