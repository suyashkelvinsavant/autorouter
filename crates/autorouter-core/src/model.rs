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

/// The sentinel model id. A client that has no opinion about which
/// upstream model to use sends this and lets the routing engine
/// decide at runtime. The gateway substitutes the resolved target
/// model before the request reaches any upstream, so the editor can
/// switch models without touching its own config.
pub const SENTINEL_MODEL: &str = "autorouter";

/// True when `model` is the sentinel id. Case-insensitive and
/// tolerates surrounding whitespace. Accepts both the bare form
/// (`autorouter`, which is what editors send in the request body) and
/// the `provider/model` form (`autorouter/autorouter`, which is what
/// a `model` field in an editor config expands to if it is not
/// split into a provider prefix).
pub fn is_sentinel_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower == SENTINEL_MODEL || lower == "autorouter/autorouter"
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

#[cfg(test)]
mod tests {
    use super::is_sentinel_model;

    #[test]
    fn bare_sentinel_matches_case_and_whitespace_insensitive() {
        assert!(is_sentinel_model("autorouter"));
        assert!(is_sentinel_model("AutoRouter"));
        assert!(is_sentinel_model("  AUTOROUTER  "));
    }

    #[test]
    fn provider_slash_form_matches() {
        // opencode's `model` field is `autorouter/autorouter`; some
        // clients forward the whole string instead of splitting it.
        assert!(is_sentinel_model("autorouter/autorouter"));
        assert!(is_sentinel_model("AutoRouter/AutoRouter"));
    }

    #[test]
    fn real_model_ids_do_not_match() {
        assert!(!is_sentinel_model("gpt-5"));
        assert!(!is_sentinel_model("claude-sonnet-4-5"));
        assert!(!is_sentinel_model("nvidia/nemotron-3-ultra-550b-a55b:free"));
        assert!(!is_sentinel_model(""));
        // A model whose last path segment happens to be "autorouter"
        // but is namespaced under a real provider is NOT the sentinel.
        assert!(!is_sentinel_model("openai/autorouter"));
    }
}
