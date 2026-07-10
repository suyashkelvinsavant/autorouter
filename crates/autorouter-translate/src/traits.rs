//! Provider adapter trait and supporting types.

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

use autorouter_core::{
    FinishReason, ModelDescriptor, ProviderKind, RequestContext, StreamChunk, UniversalRequest,
    UniversalResponse,
};

use crate::error::TranslateResult;

/// Wire-level envelope returned from a non-streaming upstream call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpstreamResponse {
    /// Parsed universal body.
    pub response: UniversalResponse,
    /// Original HTTP status code returned by the provider.
    pub status: u16,
    /// Raw provider payload, retained for debugging and feature
    /// negotiations in later phases.
    pub raw: serde_json::Value,
}

/// Wire-level envelope returned from a streaming upstream call. The
/// stream is the universal representation; the HTTP framing happens
/// in `autorouter-server`.
pub type UpstreamStream = BoxStream<'static, TranslateResult<StreamChunk>>;

/// A provider adapter encapsulates everything AutoRouter needs to talk
/// to a single upstream AI provider.
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Identifier of the provider this adapter serves.
    fn kind(&self) -> ProviderKind;

    /// Human-readable name used in logs and the desktop UI.
    fn display_name(&self) -> &'static str;

    /// Adapter-specific model descriptors for the capability registry.
    fn models(&self) -> Vec<ModelDescriptor>;

    /// Validate that a universal request can be expressed in the
    /// provider's wire format. Returns [`CoreError::UnsupportedCapability`]
    /// if a required feature is missing.
    fn validate(&self, request: &UniversalRequest) -> TranslateResult<()>;

    /// Serialise a universal request into the provider's wire format.
    fn encode_request(&self, request: &UniversalRequest) -> TranslateResult<serde_json::Value>;

    /// Deserialise a non-streaming provider response.
    fn decode_response(
        &self,
        request: &UniversalRequest,
        body: &serde_json::Value,
        status: u16,
    ) -> TranslateResult<UpstreamResponse>;

    /// Deserialise a single streaming chunk from the provider.
    fn decode_stream_chunk(
        &self,
        request: &UniversalRequest,
        chunk: &str,
    ) -> TranslateResult<Vec<StreamChunk>>;

    /// Encode a single streaming chunk for the consumer. The default
    /// implementation assumes the chunk is already in the provider's
    /// on-wire format (used for pass-through); adapters that need to
    /// reshape chunks override this.
    fn encode_stream_chunk(&self, chunk: &StreamChunk) -> TranslateResult<Option<String>> {
        // Default: no re-encoding; the server uses the universal
        // chunk directly.
        let _ = chunk;
        Ok(None)
    }

    /// Map a provider-specific finish reason to the universal enum.
    fn map_finish_reason(&self, raw: &str) -> FinishReason {
        match raw {
            "stop" | "end_turn" | "STOP" => FinishReason::Stop,
            "length" | "max_tokens" | "MAX_TOKENS" => FinishReason::Length,
            "tool_calls" | "tool_use" => FinishReason::ToolCalls,
            "content_filter" | "SAFETY" => FinishReason::ContentFilter,
            _ => FinishReason::Other,
        }
    }
}

/// Routing layer helpers - the server uses these to dispatch by provider
/// without taking a generic on the adapter.
pub trait ProviderKindExt {
    /// The default OpenAI Chat Completions path.
    fn openai() -> Self;
}

impl ProviderKindExt for ProviderKind {
    fn openai() -> Self {
        ProviderKind::OpenAI
    }
}

/// Convenience trait for adapters that need access to the request
/// context (for tracing). The default impl ignores it.
pub trait RequestContextAware {
    fn with_context(&self, _ctx: &RequestContext) {}
}
