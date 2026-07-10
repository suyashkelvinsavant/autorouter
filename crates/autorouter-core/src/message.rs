//! Message and content part types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Audio {
        source: ImageSource,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolCallRaw {
        id: String,
        name: String,
        arguments_raw: String,
    },
    ToolResult {
        tool_call_id: String,
        content: ToolResultPayload,
        #[serde(default)]
        is_error: bool,
    },
    /// PDF / document input. Carries the raw bytes (base64) plus media type.
    Document {
        source: ImageSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
    /// Provider-specific tool_use block (e.g. Anthropic content_block tool_use).
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Reasoning / thinking content emitted by reasoning-tuned models
    /// (Anthropic extended thinking, OpenAI o-series, DeepSeek R1,
    /// Qwen QwQ, Gemini thoughts, etc.). Preserved through the
    /// pipeline so the target wire format can re-emit it on the
    /// response side. Serde wire form: `{ "type": "reasoning", "text": "..." }`.
    Reasoning {
        text: String,
    },
    Unknown {
        provider: String,
        raw: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    Url { url: String },
    Base64 { media_type: String, data: String },
    FileId { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultPayload {
    Text { text: String },
    Json { value: serde_json::Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    #[serde(default)]
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn user<S: Into<String>>(text: S) -> Self {
        Self {
            role: MessageRole::User,
            content: vec![ContentPart::Text { text: text.into() }],
            name: None,
        }
    }
    pub fn system<S: Into<String>>(text: S) -> Self {
        Self {
            role: MessageRole::System,
            content: vec![ContentPart::Text { text: text.into() }],
            name: None,
        }
    }
    pub fn assistant<S: Into<String>>(text: S) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text { text: text.into() }],
            name: None,
        }
    }
    pub fn text(&self) -> String {
        let mut out = String::new();
        for part in &self.content {
            if let ContentPart::Text { text } = part {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }
}
