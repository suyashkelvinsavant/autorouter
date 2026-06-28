//! Model and provider descriptors.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The upstream provider family a request is destined for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// OpenAI Chat Completions or Responses API.
    OpenAI,
    /// Anthropic Messages API.
    Anthropic,
    /// Google Gemini `generateContent` / `streamGenerateContent`.
    Gemini,
    /// A generic HTTP provider that follows none of the canonical schemas.
    #[default]
    Custom,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ProviderKind::OpenAI => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Custom => "custom",
        };
        f.write_str(s)
    }
}

/// Coarse classification of a model, used by the routing engine to make
/// fast capability decisions without consulting a model registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelFamily {
    /// Frontier chat/instruct model (e.g. GPT-5, Claude Sonnet, Gemini Pro).
    Chat,
    /// Small/cheap chat model (e.g. GPT-4o-mini, Claude Haiku, Gemini Flash).
    Mini,
    /// Reasoning-tuned model (o-series, Claude with extended thinking).
    Reasoning,
    /// Embedding model. AutoRouter passes these through unchanged.
    Embedding,
    /// Image generation model. Not part of the Phase 1 translation scope.
    Image,
    /// Audio or speech model.
    Audio,
    /// Unknown / not yet classified.
    #[default]
    Unknown,
}

/// Static description of a model known to AutoRouter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub display_name: String,
    pub family: ModelFamily,
    pub provider: ProviderKind,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_audio: bool,
    pub supports_streaming: bool,
}
