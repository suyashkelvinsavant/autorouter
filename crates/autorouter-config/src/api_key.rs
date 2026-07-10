//! Heuristics for classifying the strings users type into the
//! `api_key_secret_id` / `api_key_value` fields on the Providers page.
//!
//! The goal is for an operator to be able to paste *either* of the
//! following and have AutoRouter do the right thing without having
//! to know about the `env:` / `keychain:` prefixes:
//!
//! ```text
//! OPENAI_API_KEY          # env var name  -> read from process env
//! env:OPENAI_API_KEY      # explicit env  -> read from process env
//! sk-or-v1-abc123...      # literal key   -> save to secret store
//! keychain:openai         # explicit store -> read from secret store
//! ```
//!
//! Detection is intentionally conservative: when a string *could* be
//! either an env-var name or a secret-store id, we ask the secret
//! store first (so explicit, curated ids like `openai_prod` still
//! work) and only fall back to the process environment when the
//! string *looks* like an env-var name AND matches one.

use std::borrow::Cow;

/// How a string in the `api_key_secret_id` / `api_key_value` field
/// should be interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeyReference {
    /// `env:NAME` or a bare ALL_CAPS_SNAKE_CASE name. The wrapped
    /// `Cow<str>` is the env var name (without the `env:` prefix).
    EnvVar(Cow<'static, str>),
    /// `keychain:ID` or any other string. The wrapped `Cow<str>` is the
    /// secret-store id (without the `keychain:` prefix).
    SecretId(Cow<'static, str>),
}

impl ApiKeyReference {
    /// Render the reference as the canonical string we persist in
    /// `ProviderEntry.api_key_secret_id`. This is the form the
    /// resolver recognises on the next startup.
    pub fn canonical(&self) -> String {
        match self {
            ApiKeyReference::EnvVar(name) => format!("env:{name}"),
            ApiKeyReference::SecretId(id) => format!("keychain:{id}"),
        }
    }

    /// True if the reference points at an environment variable.
    pub fn is_env_var(&self) -> bool {
        matches!(self, ApiKeyReference::EnvVar(_))
    }
}

/// Heuristic: does this string look like a Unix environment variable
/// name? Conservative — requires ALL_CAPS_SNAKE_CASE so we don't
/// misclassify a literal key like `sk-or-v1-abc` (which has dashes
/// and lowercase letters).
///
/// Rules:
/// * non-empty
/// * first char is ASCII letter
/// * remaining chars are ASCII uppercase, digit, or underscore
pub fn looks_like_env_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Heuristic: does this string look like a literal API key? We do
/// not try to be exhaustive — we just want to catch the common
/// vendor prefixes so the UI can auto-store the value instead of
/// treating it as an env-var name.
pub fn looks_like_api_key(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let vendor_prefixes = [
        "sk-",        // OpenAI, OpenRouter, Mistral, LangSmith
        "sk_",        // some providers use underscores
        "gsk_",       // Groq
        "xai-",       // xAI
        "xoxb-",      // Slack bot tokens (probably wrong context but cheap)
        "ghp_",       // GitHub PATs
        "anthropic-", // some Anthropic key formats
    ];
    if vendor_prefixes.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    // Generic: long strings with mixed case + digits + dashes that
    // *don't* look like env-var names are almost certainly literal
    // keys (the resolve_secret path would otherwise fail to find
    // them in either env or the secret store).
    s.len() >= 24 && !looks_like_env_var_name(s)
}

/// Classify a single user-entered string. Returns the canonical
/// [`ApiKeyReference`] we should persist and the *original* raw
/// value (so the UI can echo it back to the user without losing
/// their typing).
///
/// Detection order:
/// 1. `env:NAME` or `keychain:ID` prefix → honour it verbatim.
/// 2. Bare ALL_CAPS_SNAKE_CASE that exists in the process env → env var.
/// 3. Bare ALL_CAPS_SNAKE_CASE that does NOT exist in env → store as
///    a secret-store id (the user may be planning to populate it
///    later, or it may be a custom store id).
/// 4. Anything else → secret-store id.
pub fn classify_api_key_reference(raw: &str) -> ApiKeyReference {
    let trimmed = raw.trim();
    if let Some(name) = trimmed.strip_prefix("env:") {
        return ApiKeyReference::EnvVar(Cow::Owned(name.to_string()));
    }
    if let Some(id) = trimmed.strip_prefix("keychain:") {
        return ApiKeyReference::SecretId(Cow::Owned(id.to_string()));
    }
    if looks_like_env_var_name(trimmed) && std::env::var_os(trimmed).is_some() {
        return ApiKeyReference::EnvVar(Cow::Owned(trimmed.to_string()));
    }
    ApiKeyReference::SecretId(Cow::Owned(trimmed.to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_prefix_is_classified_as_env() {
        let r = classify_api_key_reference("env:OPENAI_API_KEY");
        assert_eq!(r, ApiKeyReference::EnvVar("OPENAI_API_KEY".into()));
        assert!(r.is_env_var());
        assert_eq!(r.canonical(), "env:OPENAI_API_KEY");
    }

    #[test]
    fn keychain_prefix_is_classified_as_secret() {
        let r = classify_api_key_reference("keychain:openai_prod");
        assert_eq!(r, ApiKeyReference::SecretId("openai_prod".into()));
        assert!(!r.is_env_var());
        assert_eq!(r.canonical(), "keychain:openai_prod");
    }

    #[test]
    fn bare_all_caps_with_matching_env_var_is_env() {
        // SAFETY: only mutates the process env for the duration of this
        // test; the helper holds no locks.
        unsafe {
            std::env::set_var("AUTOROUTER_TEST_API_KEY_DETECT", "hello");
        }
        let r = classify_api_key_reference("AUTOROUTER_TEST_API_KEY_DETECT");
        unsafe {
            std::env::remove_var("AUTOROUTER_TEST_API_KEY_DETECT");
        }
        assert_eq!(
            r,
            ApiKeyReference::EnvVar("AUTOROUTER_TEST_API_KEY_DETECT".into())
        );
    }

    #[test]
    fn bare_all_caps_without_env_var_falls_back_to_secret_id() {
        // No env var set — should NOT be classified as an env var.
        let r = classify_api_key_reference("DEFINITELY_NOT_SET_XYZ_42");
        assert_eq!(
            r,
            ApiKeyReference::SecretId("DEFINITELY_NOT_SET_XYZ_42".into())
        );
    }

    #[test]
    fn literal_key_is_secret_id() {
        let r = classify_api_key_reference("sk-or-v1-abc123def456ghi789");
        assert_eq!(
            r,
            ApiKeyReference::SecretId("sk-or-v1-abc123def456ghi789".into())
        );
    }

    #[test]
    fn groq_style_key_is_secret_id() {
        let r = classify_api_key_reference("gsk_ABCdef123");
        assert_eq!(r, ApiKeyReference::SecretId("gsk_ABCdef123".into()));
    }

    #[test]
    fn mixed_case_string_is_secret_id() {
        // Even if it happens to be in env, mixed case means it's a
        // literal value (env-var convention is ALL_CAPS).
        let r = classify_api_key_reference("MixedCaseToken");
        assert_eq!(r, ApiKeyReference::SecretId("MixedCaseToken".into()));
    }

    #[test]
    fn looks_like_env_var_name_rules() {
        assert!(looks_like_env_var_name("OPENAI_API_KEY"));
        assert!(looks_like_env_var_name("A"));
        assert!(looks_like_env_var_name("FOO_BAR_42"));
        assert!(!looks_like_env_var_name(""));
        assert!(!looks_like_env_var_name("1ABC")); // must start with letter
        assert!(!looks_like_env_var_name("FOO-BAR")); // no dashes
        assert!(!looks_like_env_var_name("foo_bar")); // must be all upper
        assert!(!looks_like_env_var_name("FOO BAR")); // no spaces
    }

    #[test]
    fn looks_like_api_key_rules() {
        assert!(looks_like_api_key("sk-or-v1-abc"));
        assert!(looks_like_api_key("sk-abc"));
        assert!(looks_like_api_key("gsk_abc"));
        assert!(looks_like_api_key("xai-abc"));
        assert!(looks_like_api_key(
            "any-long-string-with-mixed-case-and-dashes-1234567890"
        ));
        assert!(!looks_like_api_key(""));
        assert!(!looks_like_api_key("OPENAI_API_KEY"));
        assert!(!looks_like_api_key("short"));
    }
}
