#![deny(unused_crate_dependencies)]
//! autorouter-translate
//!
//! Provider-specific parsers and serializers. Phase 1 ships the
//! canonical OpenAI, Anthropic, and Gemini protocols plus a
//! pipeline facade.

pub mod anthropic;
pub mod error;
pub mod gemini;
pub mod openai_chat;
pub mod openai_responses;
pub mod pipeline;
pub mod reasoning_extractor;
pub mod streaming;
pub mod traits;

pub use anthropic::anthropic_tool_call_drop;
pub use gemini::gemini_cleanup_drop;
pub use openai_chat::openai_tool_call_drop;
pub use reasoning_extractor::{split_reasoning, streamer_drop, ReasoningSplit, ReasoningStreamer};

pub use anthropic::AnthropicAdapter;
pub use error::{TranslateError, TranslateResult};
pub use gemini::GeminiAdapter;
pub use openai_chat::OpenAiChatAdapter;
pub use openai_responses::OpenAiResponsesAdapter;
pub use pipeline::{decode_mock_response, Direction, OpenAiWireFormat, TranslationPipeline};
pub use streaming::{format_done_sentinel, format_sse_chunk};
pub use traits::{ProviderAdapter, UpstreamResponse, UpstreamStream};
