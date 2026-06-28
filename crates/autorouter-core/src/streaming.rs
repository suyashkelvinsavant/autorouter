//! Streaming event types.

use serde::{Deserialize, Serialize};

use crate::response::FinishReason;
use crate::tool::ToolCall;
use crate::usage::Usage;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamEvent {
    Start {
        id: String,
        model: String,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallStart {
        call: ToolCall,
    },
    ToolCallDelta {
        id: String,
        arguments_fragment: String,
    },
    ToolCallEnd {
        id: String,
    },
    Finish {
        reason: FinishReason,
        usage: Option<Usage>,
    },
    /// Mid-stream usage update (e.g. when an OpenAI chunk carries a partial usage field).
    UsageDelta {
        usage: Usage,
    },
    Error {
        message: String,
        code: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChunk {
    pub events: Vec<StreamEvent>,
    /// Content-block or choice index within the stream. Used by
    /// Anthropic content_block_* events (where each distinct
    /// text / tool_use / thinking block occupies a sequential index)
    /// and by OpenAI choices[]. Proxy-aware encoders read this field
    /// instead of hardcoding 0 everywhere.
    #[serde(default)]
    pub index: u32,
}

impl StreamChunk {
    pub fn new(events: Vec<StreamEvent>) -> Self {
        Self { events, index: 0 }
    }
    pub fn with_index(events: Vec<StreamEvent>, index: u32) -> Self {
        Self { events, index }
    }
    pub fn empty() -> Self {
        Self {
            events: Vec::new(),
            index: 0,
        }
    }
    /// First event in this chunk. Used by SSE encoders as the
    /// "leading" event for delta coalescing. Returns `None` for an
    /// empty chunk.
    pub fn delta(&self) -> Option<&StreamEvent> {
        self.events.first()
    }
    pub fn index(&self) -> u32 {
        self.index
    }
}

impl From<StreamEvent> for StreamChunk {
    fn from(event: StreamEvent) -> Self {
        Self {
            events: vec![event],
            index: 0,
        }
    }
}
