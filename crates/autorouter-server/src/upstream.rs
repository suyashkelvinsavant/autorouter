//! Upstream provider client.
//!
//! Two implementations ship in this module:
//!
//!   * [`MockUpstream`] is an in-memory recorder used by tests and
//!     the offline `--mock` flag.
//!   * [`HttpUpstream`] is a real `reqwest`-backed client that calls
//!     the configured upstream provider over HTTPS, attaches the
//!     right auth header, and (for the OpenAI-compatible providers)
//!     streams the SSE response back to the gateway.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::StreamExt;
use serde_json::Value;
use tokio_util::bytes::Bytes;

use autorouter_config::{ApiFormat, ProviderEntry, SecretStore};
use autorouter_core::ProviderKind;
use autorouter_translate::{
    anthropic_tool_call_drop, decode_mock_response, gemini_cleanup_drop, openai_tool_call_drop,
    streamer_drop, AnthropicAdapter, GeminiAdapter, OpenAiChatAdapter, ProviderAdapter,
    TranslateError, TranslateResult, UpstreamResponse, UpstreamStream,
};

/// Convert an `ApiFormat` to the `ProviderKind` used by the adapter
/// and URL-builder. `Custom` is kept as a sentinel only when the
/// format is truly unknown (should not happen post-heuristic).
pub fn api_format_to_kind(fmt: ApiFormat) -> ProviderKind {
    match fmt {
        ApiFormat::OpenAI => ProviderKind::OpenAI,
        ApiFormat::Anthropic => ProviderKind::Anthropic,
        ApiFormat::Gemini => ProviderKind::Gemini,
    }
}

/// Strip an optional leading `Bearer ` token from a raw secret
/// value. Many shell wrappers and dotfile templates store keys with
/// `Bearer ` already prepended so they can be pasted into a curl
/// command without further editing. The gateway emits its own
/// `Authorization: Bearer <value>` header in `HttpUpstream`, so
/// keeping the prefix in the value would produce a doubled
/// `Bearer Bearer …` header that the upstream rejects. Trimming the
/// prefix (and surrounding whitespace) here means operators can
/// paste either form, including values that picked up a stray
/// newline from a multi-line shell heredoc.
fn strip_bearer_prefix(value: &str) -> &str {
    value.trim().strip_prefix("Bearer ").unwrap_or(value.trim())
}

/// Resolve a secret id into the concrete value to use as the upstream
/// auth header value. Supports two prefixes plus auto-detection:
///
///   * `env:NAME` — read `NAME` from the process environment.
///   * `keychain:ID` — look `ID` up in the supplied secret store.
///   * bare id — first try the secret store; if that fails
///     AND the id looks like an env-var name
///     (ALL_CAPS_SNAKE_CASE), fall back to the
///     process environment. This lets operators
///     write `api_key_secret_id = "OPENAI_API_KEY"`
///     without needing to type the `env:` prefix.
///
/// The returned value has any leading `Bearer ` prefix stripped (see
/// [`strip_bearer_prefix`]) so the gateway can safely wrap it in an
/// `Authorization: Bearer …` header without producing a doubled
/// prefix.
///
/// See [`autorouter_config::classify_api_key_reference`] for the
/// exact rules the UI uses to persist these values; the resolver
/// here applies the same classification so behaviour is symmetric.
pub fn resolve_secret(raw: Option<&str>, store: Option<&Arc<dyn SecretStore>>) -> Option<String> {
    let raw = raw?;
    let mut resolved: Option<String> = None;
    if let Some(name) = raw.strip_prefix("env:") {
        resolved = std::env::var(name).ok();
    } else {
        if let Some(id) = raw.strip_prefix("keychain:") {
            if let Some(store) = store {
                if let Ok(secret) = store.get(&id.to_string().into()) {
                    resolved = Some(secret.value);
                }
            }
            // Fall through to env-var fallback below.
        }
        if resolved.is_none() {
            if let Some(store) = store {
                if let Ok(secret) = store.get(&raw.to_string().into()) {
                    resolved = Some(secret.value);
                }
            }
        }
        // Auto-detect: a bare ALL_CAPS_SNAKE_CASE string that does not
        // exist in the secret store is almost certainly an env-var name.
        // Falling back to env here means an operator can write
        // `api_key_secret_id = "OPENAI_API_KEY"` (no `env:` prefix) and
        // it Just Works as long as the env var is set in the process
        // environment. If the string isn't an env-var name either, we
        // return None so the caller can surface a clear config error.
        if resolved.is_none() && autorouter_config::looks_like_env_var_name(raw) {
            resolved = std::env::var(raw).ok();
        }
    }
    resolved
        .as_deref()
        .map(strip_bearer_prefix)
        .map(String::from)
}

/// Calls the upstream provider and returns the parsed response. The
/// implementation owns HTTP details, retries, and timeouts.
#[async_trait]
pub trait UpstreamClient: Send + Sync {
    /// Provider kind this client serves. Used to validate the call.
    fn kind(&self) -> ProviderKind;

    /// Send a non-streaming request to the provider.
    async fn send(&self, body: &Value) -> TranslateResult<UpstreamResponse>;

    /// Open a streaming request. The returned stream yields universal
    /// [`StreamChunk`](autorouter_core::StreamChunk) values.
    async fn send_streaming(&self, body: &Value) -> TranslateResult<UpstreamStream>;
}

// ---------------------------------------------------------------------
// MockUpstream
// ---------------------------------------------------------------------

/// In-memory upstream that records calls and returns a configured
/// response. Used by tests and by the `--mock` flag.
///
/// The default response shape is the OpenAI Chat Completions wire
/// format and now includes a realistic `usage` block plus a small
/// simulated latency so the analytics pipeline has non-zero tokens
/// and timing to aggregate. Real upstreams still populate these
/// from their own `usage` field, so the mock's behaviour does not
/// affect the wire protocol in production.
pub struct MockUpstream {
    kind: ProviderKind,
    response: parking_lot::Mutex<Option<Value>>,
    recorded: parking_lot::Mutex<Vec<Value>>,
}

impl MockUpstream {
    pub fn new(kind: ProviderKind) -> Self {
        Self {
            kind,
            response: parking_lot::Mutex::new(None),
            recorded: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// Configure the JSON body returned for the next non-streaming
    /// call. Subsequent calls reuse the same value. When unset, a
    /// realistic default body (including `usage`) is returned so
    /// the analytics dashboard renders meaningful numbers even
    /// without a real upstream.
    pub fn set_response(&self, body: Value) {
        *self.response.lock() = Some(body);
    }

    /// Snapshot of the bodies that have been sent through this mock.
    pub fn recorded(&self) -> Vec<Value> {
        self.recorded.lock().clone()
    }

    /// Estimate the prompt token count from a request body. Counts
    /// the characters in every message's string content and divides
    /// by ~4 (the common heuristic for English tokens). Anything
    /// richer than that is wasted precision — the dashboard is
    /// rendering a Sparkline, not running the actual BPE.
    fn estimate_prompt_tokens(body: &Value) -> u64 {
        let mut chars: usize = 0;
        if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
            for m in messages {
                if let Some(content) = m.get("content") {
                    if let Some(s) = content.as_str() {
                        chars += s.len();
                    } else if let Some(arr) = content.as_array() {
                        for part in arr {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                chars += text.len();
                            }
                        }
                    }
                }
            }
        }
        if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
            chars += instructions.len();
        }
        if let Some(contents) = body.get("contents").and_then(|v| v.as_array()) {
            for c in contents {
                if let Some(parts) = c.get("parts").and_then(|v| v.as_array()) {
                    for p in parts {
                        if let Some(text) = p.get("text").and_then(|v| v.as_str()) {
                            chars += text.len();
                        }
                    }
                }
            }
        }
        if let Some(sys) = body.get("system") {
            if let Some(s) = sys.as_str() {
                chars += s.len();
            }
        }
        // Add ~16 tokens of framing overhead (role markers,
        // separators) so even a single-token user message has a
        // non-zero count.
        (chars / 4) as u64 + 16
    }

    /// Build the default mock response body for a given request.
    /// Returns an OpenAI Chat Completions-shaped JSON body with a
    /// `usage` block sized proportionally to the request so the
    /// analytics page renders real numbers from the mock.
    fn default_mock_response(body: &Value) -> Value {
        let input_tokens = Self::estimate_prompt_tokens(body);
        // Output tokens: deterministic-ish from the request hash so
        // the same payload gives the same number on repeat runs.
        // Using a simple LCG over the body bytes keeps tests
        // stable without dragging in `rand`.
        let seed: u64 = body
            .to_string()
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(131).wrapping_add(b as u64));
        let output_tokens = 50 + (seed % 150); // 50..200
        let total_tokens = input_tokens + output_tokens;
        serde_json::json!({
            "id": format!("mock-{}", seed),
            "model": body.get("model").cloned().unwrap_or(Value::Null),
            "choices": [{
                "message": { "role": "assistant", "content": "mock" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": input_tokens,
                "completion_tokens": output_tokens,
                "total_tokens": total_tokens,
            }
        })
    }
}

#[async_trait]
impl UpstreamClient for MockUpstream {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    async fn send(&self, body: &Value) -> TranslateResult<UpstreamResponse> {
        self.recorded.lock().push(body.clone());
        // Simulate realistic upstream latency (50..300ms). The exact
        // value is deterministic per request body so test snapshots
        // stay stable; the analytics page is happy with any
        // non-zero number.
        let seed: u64 = body
            .to_string()
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(131).wrapping_add(b as u64));
        let delay_ms = 50 + (seed >> 8) % 250;
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        let response = self
            .response
            .lock()
            .clone()
            .unwrap_or_else(|| Self::default_mock_response(body));
        let status = 200;
        Ok(UpstreamResponse {
            response: decode_mock_response(&self.kind, &response)?,
            status,
            raw: response,
        })
    }
    async fn send_streaming(&self, body: &Value) -> TranslateResult<UpstreamStream> {
        self.recorded.lock().push(body.clone());
        let s = futures::stream::empty();
        Ok(Box::pin(s))
    }
}

// ---------------------------------------------------------------------
// HttpUpstream
// ---------------------------------------------------------------------

/// Configuration for [`HttpUpstream`]. Built from a [`ProviderEntry`]
/// and the resolved secret (if any).
#[derive(Debug, Clone)]
pub struct HttpUpstreamConfig {
    pub kind: ProviderKind,
    pub base_url: String,
    pub auth_header: String,
    pub auth_value: Option<String>,
    pub default_headers: BTreeMap<String, String>,
    pub model_allowlist: Vec<String>,
    pub timeout: Duration,
}

impl HttpUpstreamConfig {
    /// Build a config for a provider entry. `auth_value` is the
    /// resolved bearer / api-key string (or `None`).
    pub fn from_entry(
        entry: &ProviderEntry,
        kind: ProviderKind,
        auth_value: Option<String>,
        timeout: Duration,
    ) -> Self {
        let auth_header = match kind {
            ProviderKind::Anthropic => "x-api-key",
            ProviderKind::Gemini => "x-goog-api-key",
            _ => "Authorization",
        };
        Self {
            kind,
            base_url: entry.base_url.clone(),
            auth_header: auth_header.to_string(),
            auth_value,
            default_headers: entry.default_headers.clone(),
            model_allowlist: entry.model_allowlist.clone(),
            timeout,
        }
    }
}

/// Real HTTP upstream backed by `reqwest`. Constructed by
/// [`HttpUpstream::new`].
pub struct HttpUpstream {
    cfg: HttpUpstreamConfig,
    client: reqwest::Client,
}

impl HttpUpstream {
    pub fn new(cfg: HttpUpstreamConfig) -> TranslateResult<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| TranslateError::http(format!("build client: {e}")))?;
        Ok(Self { cfg, client })
    }

    pub fn config(&self) -> &HttpUpstreamConfig {
        &self.cfg
    }

    fn check_allowlist(&self, body: &Value) -> TranslateResult<()> {
        if self.cfg.model_allowlist.is_empty() {
            return Ok(());
        }
        let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("");
        if self.cfg.model_allowlist.iter().any(|m| m == model) {
            Ok(())
        } else {
            Err(TranslateError::upstream(format!(
                "model `{model}` is not in this provider's allowlist"
            )))
        }
    }

    fn build_url(&self, body: &Value, streaming: bool) -> TranslateResult<String> {
        let base = self.cfg.base_url.trim_end_matches('/');
        match self.cfg.kind {
            ProviderKind::OpenAI => Ok(format!("{base}/chat/completions")),
            ProviderKind::Anthropic => Ok(format!("{base}/v1/messages")),
            ProviderKind::Gemini => {
                let model = body.get("model").and_then(|v| v.as_str()).ok_or_else(|| {
                    TranslateError::invalid_payload("gemini", "missing model field")
                })?;
                let suffix = if streaming {
                    "streamGenerateContent"
                } else {
                    "generateContent"
                };
                let mut url = format!("{base}/v1beta/models/{model}:{suffix}");
                if streaming {
                    url.push_str("?alt=sse");
                }
                Ok(url)
            }
            ProviderKind::Custom => Ok(base.to_string()),
        }
    }

    fn build_request(
        &self,
        body: &Value,
        streaming: bool,
    ) -> TranslateResult<reqwest::RequestBuilder> {
        let url = self.build_url(body, streaming)?;
        let mut req = self.client.post(&url);
        for (k, v) in &self.cfg.default_headers {
            req = req.header(k, v);
        }
        if let Some(value) = &self.cfg.auth_value {
            let header_value: String = match self.cfg.kind {
                ProviderKind::Anthropic | ProviderKind::Gemini => value.clone(),
                _ => format!("Bearer {value}"),
            };
            req = req.header(&self.cfg.auth_header, header_value);
        }
        if self.cfg.kind == ProviderKind::Anthropic
            && !self.cfg.default_headers.contains_key("anthropic-version")
        {
            req = req.header("anthropic-version", "2023-06-01");
        }
        req = req.json(body);
        Ok(req)
    }

    /// Decode a wire-format response body to a universal response
    /// using the matching adapter.
    fn decode_wire(&self, body: &Value) -> TranslateResult<autorouter_translate::UpstreamResponse> {
        let response = decode_mock_response(&self.cfg.kind, body)?;
        Ok(autorouter_translate::UpstreamResponse {
            response,
            status: 200,
            raw: body.clone(),
        })
    }
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Strip leading and trailing ASCII whitespace bytes from a
/// response body. Some upstreams (e.g. OpenRouter) prepend stray
/// newlines before the JSON payload; this is the smallest fix that
/// keeps us robust without changing the wire contract.
fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|p| p + 1)
        .unwrap_or(0);
    if start >= end {
        &[]
    } else {
        &bytes[start..end]
    }
}

#[async_trait]
impl UpstreamClient for HttpUpstream {
    fn kind(&self) -> ProviderKind {
        self.cfg.kind
    }

    async fn send(&self, body: &Value) -> TranslateResult<UpstreamResponse> {
        self.check_allowlist(body)?;
        let req = self.build_request(body, false)?.timeout(self.cfg.timeout);
        let response = req
            .send()
            .await
            .map_err(|e| TranslateError::upstream(format!("upstream send: {e}")))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| TranslateError::upstream(format!("upstream read: {e}")))?;
        if !is_success(status) {
            let snippet: String = String::from_utf8_lossy(&bytes).chars().take(512).collect();
            return Err(TranslateError::upstream(format!(
                "upstream returned {status}: {snippet}"
            )));
        }
        // Some upstreams (notably OpenRouter) prefix the JSON body
        // with stray whitespace/newlines. Trim leading + trailing
        // ASCII whitespace before handing to serde_json so we are
        // robust to that without changing the wire-format contract
        // we publish.
        let trimmed = trim_ascii_whitespace(&bytes);
        let raw: Value = serde_json::from_slice(trimmed)
            .map_err(|e| TranslateError::upstream(format!("upstream json: {e}")))?;
        let mut resp = self.decode_wire(&raw)?;
        resp.status = status;
        Ok(resp)
    }

    async fn send_streaming(&self, body: &Value) -> TranslateResult<UpstreamStream> {
        self.check_allowlist(body)?;
        let req = self.build_request(body, true)?;
        let response = req
            .send()
            .await
            .map_err(|e| TranslateError::upstream(format!("upstream send: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|e| format!("<read error: {e}>"));
            return Err(TranslateError::upstream(format!(
                "upstream returned {status}: {}",
                body.chars().take(512).collect::<String>()
            )));
        }
        let kind = self.cfg.kind;
        let byte_stream = response.bytes_stream();
        let decoded = async_stream::stream! {
            // Create a per-stream identity token so the adapters
            // can key on a stable (non-aliasing) `stream_id` across
            // multiple SSE frames. Using the request's built-in
            // stream_id (assigned from a global atomic counter)
            // avoids the aliasing bug where two streams allocated
            // on the same stack address would share state.
            let stream_request = autorouter_core::UniversalRequest::default();
            let stream_request_ptr = stream_request.stream_id as usize;
            let mut s = Box::pin(byte_stream);
            let mut buffer = String::new();
            while let Some(chunk_res) = s.next().await {
                match chunk_res {
                    Ok(bytes) => {
                        if let Some(decoded) = decode_bytes(kind, &bytes, &mut buffer, &stream_request) {
                            yield decoded;
                        }
                    }
                    Err(e) => {
                        streamer_drop(stream_request_ptr);
                        openai_tool_call_drop(stream_request_ptr);
                        anthropic_tool_call_drop(stream_request_ptr);
                        gemini_cleanup_drop(stream_request_ptr);
                        yield Err(TranslateError::upstream(format!("upstream read: {e}")));
                        return;
                    }
                }
            }
            if !buffer.is_empty() {
                if let Some(decoded) = decode_text(kind, &buffer, &stream_request) {
                    yield decoded;
                }
            }
            streamer_drop(stream_request_ptr);
            openai_tool_call_drop(stream_request_ptr);
            anthropic_tool_call_drop(stream_request_ptr);
            gemini_cleanup_drop(stream_request_ptr);
        };
        let stream: UpstreamStream = Box::pin(decoded);
        Ok(stream)
    }
}

/// Append the UTF-8 view of `bytes` to `buffer`, then pull out and
/// decode any complete SSE frames (delimited by `\n\n`).
fn decode_bytes(
    kind: ProviderKind,
    bytes: &Bytes,
    buffer: &mut String,
    request: &autorouter_core::UniversalRequest,
) -> Option<TranslateResult<autorouter_core::StreamChunk>> {
    buffer.push_str(&String::from_utf8_lossy(bytes));
    if let Some(pos) = buffer.find("\n\n") {
        let frame: String = buffer.drain(..pos + 2).collect();
        decode_text(kind, &frame, request)
    } else {
        None
    }
}

/// Decode a single SSE frame (or raw JSON for Gemini) into universal
/// stream chunks. `None` means the frame had no decodeable events
/// (e.g. an OpenAI `[DONE]` sentinel that the adapter already
/// converted to a finish event).
fn decode_text(
    kind: ProviderKind,
    frame: &str,
    request: &autorouter_core::UniversalRequest,
) -> Option<TranslateResult<autorouter_core::StreamChunk>> {
    let mut merged: Vec<autorouter_core::StreamEvent> = Vec::new();
    let result = match kind {
        ProviderKind::OpenAI => OpenAiChatAdapter::new().decode_stream_chunk(request, frame),
        ProviderKind::Anthropic => AnthropicAdapter::new().decode_stream_chunk(request, frame),
        ProviderKind::Gemini => GeminiAdapter::new().decode_stream_chunk(request, frame),
        ProviderKind::Custom => Ok(Vec::new()),
    };
    match result {
        Ok(chunks) => {
            for c in chunks {
                merged.extend(c.events);
            }
            if merged.is_empty() {
                None
            } else {
                Some(Ok(autorouter_core::StreamChunk {
                    events: merged,
                    index: 0,
                }))
            }
        }
        Err(e) => Some(Err(e)),
    }
}

/// Helper: wraps an `UpstreamClient` in an `Arc` for the gateway builder.
pub type SharedUpstream = Arc<dyn UpstreamClient>;

/// A pair of maps: the three built-in providers and the custom ones.
///
/// Derives [`Clone`] because the rebuild path stores a fresh
/// `UpstreamSet` under `parking_lot::RwLock<UpstreamSet>` and we want
/// cheap snapshot accessors (`AppState::snapshot_upstreams`). The
/// inner maps clone in `O(n)` over providers, which is fine for the
/// handful of providers we ever register.
#[derive(Default, Clone)]
pub struct UpstreamSet {
    pub built_in: std::collections::HashMap<ProviderKind, SharedUpstream>,
    pub custom: BTreeMap<String, SharedUpstream>,
}

/// Build the upstream map from a config + secret store. Each enabled
/// provider with a configured base URL gets a real `HttpUpstream`;
/// providers without a base URL fall back to `MockUpstream` so the
/// gateway remains usable in offline mode.
pub fn build_upstreams(
    config: &autorouter_config::AppConfig,
    secret_store: Option<Arc<dyn SecretStore>>,
) -> UpstreamSet {
    use std::collections::HashMap;
    let mut built_in: HashMap<ProviderKind, SharedUpstream> = HashMap::new();
    let mut custom: BTreeMap<String, SharedUpstream> = BTreeMap::new();
    let timeout = Duration::from_secs(config.server.request_timeout_seconds.max(1));
    let entries: [(ProviderKind, Option<&ProviderEntry>); 3] = [
        (ProviderKind::OpenAI, config.providers.openai.as_ref()),
        (ProviderKind::Anthropic, config.providers.anthropic.as_ref()),
        (ProviderKind::Gemini, config.providers.gemini.as_ref()),
    ];
    for (kind, maybe_entry) in entries {
        let upstream: SharedUpstream = match maybe_entry {
            Some(entry) if entry.enabled && !entry.base_url.is_empty() => {
                let secret =
                    resolve_secret(entry.api_key_secret_id.as_deref(), secret_store.as_ref());
                let cfg = HttpUpstreamConfig::from_entry(entry, kind, secret, timeout);
                match HttpUpstream::new(cfg) {
                    Ok(client) => Arc::new(client),
                    Err(e) => {
                        tracing::warn!(error = %e, ?kind, "failed to build HttpUpstream; using mock");
                        Arc::new(MockUpstream::new(kind))
                    }
                }
            }
            _ => {
                // H14: warn so users notice when a provider has no
                // configured base_url (e.g. first run) so they can fix it.
                tracing::warn!(?kind, "provider has no base_url or is disabled; using MockUpstream (no real API calls)");
                Arc::new(MockUpstream::new(kind))
            }
        };
        built_in.insert(kind, upstream);
    }
    for (name, entry) in &config.providers.custom {
        let upstream: SharedUpstream = if entry.enabled && !entry.base_url.is_empty() {
            let secret = resolve_secret(entry.api_key_secret_id.as_deref(), secret_store.as_ref());
            // Use the api_format field (auto-detected from base_url at
            // save time) so the right adapter and URL path are used.
            let kind = api_format_to_kind(entry.api_format);
            let cfg = HttpUpstreamConfig::from_entry(entry, kind, secret, timeout);
            match HttpUpstream::new(cfg) {
                Ok(client) => Arc::new(client),
                Err(e) => {
                    tracing::error!(error = %e, custom = %name, "failed to build custom HttpUpstream; FALLING BACK TO MOCK upstream");
                    Arc::new(MockUpstream::new(ProviderKind::Custom))
                }
            }
        } else {
            Arc::new(MockUpstream::new(ProviderKind::Custom))
        };
        custom.insert(name.clone(), upstream);
    }
    UpstreamSet { built_in, custom }
}

/// Convenience wrapper around [`build_upstreams`] that the
/// `PATCH /ui/settings` handler calls after applying a patch.
///
/// Why a separate name:
///   * The handler reads "rebuild upstreams from the new config"
///     more clearly than "build upstreams from a config".
///   * The helper is the single entry point the rebuild path uses,
///     so future changes (e.g. tracking which providers changed,
///     emitting per-provider metrics) only need to touch one
///     function. Keeping the call surface tiny avoids forcing every
///     caller to be aware of the rebuild contract.
///
/// Behaviour:
///   * Re-resolves every `api_key_secret_id` against the supplied
///     secret store + process environment. Operators who paste a
///     new `env:NAME` reference (or flip a provider from disabled
///     to enabled, or change `base_url`) see the new value on the
///     next request.
///   * Returns the new `UpstreamSet`. The caller is responsible
///     for handing it to `AppState::replace_upstreams` so the swap
///     is atomic.
pub fn rebuild_upstreams(
    config: &autorouter_config::AppConfig,
    secret_store: Option<Arc<dyn SecretStore>>,
) -> UpstreamSet {
    tracing::info!("rebuilding upstream set from updated config");
    build_upstreams(config, secret_store)
}

// ---------------------------------------------------------------------
// Unit tests for the helpers that don't need a running server.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_ascii_whitespace_strips_leading_and_trailing() {
        let input: &[u8] = b"\n\n\n  {\"hello\":\"world\"}  \n\n";
        let trimmed = trim_ascii_whitespace(input);
        assert_eq!(trimmed, br#"{"hello":"world"}"#);
        // The trimmed slice must be valid JSON.
        let parsed: serde_json::Value = serde_json::from_slice(trimmed).unwrap();
        assert_eq!(parsed["hello"], "world");
    }

    #[test]
    fn trim_ascii_whitespace_handles_pure_whitespace() {
        let input: &[u8] = b"   \n\t  ";
        assert_eq!(trim_ascii_whitespace(input), b"");
    }

    #[test]
    fn trim_ascii_whitespace_preserves_inner_whitespace() {
        // Whitespace *inside* the JSON value must NOT be stripped
        // (that would corrupt strings containing spaces).
        let input: &[u8] = br#"  {"key": "value with spaces"}  "#;
        let trimmed = trim_ascii_whitespace(input);
        let parsed: serde_json::Value = serde_json::from_slice(trimmed).unwrap();
        assert_eq!(parsed["key"], "value with spaces");
    }

    #[test]
    fn resolve_secret_prefers_env_over_store_when_no_store_provided() {
        unsafe {
            std::env::set_var("RESOLVE_SECRET_ENV_ONLY_TEST", "from-env");
        }
        let got = resolve_secret(Some("RESOLVE_SECRET_ENV_ONLY_TEST"), None);
        unsafe {
            std::env::remove_var("RESOLVE_SECRET_ENV_ONLY_TEST");
        }
        assert_eq!(got.as_deref(), Some("from-env"));
    }

    #[test]
    fn resolve_secret_returns_none_for_missing_everything() {
        let got = resolve_secret(Some("env:NEVER_SET_THIS_PLEASE_42"), None);
        assert!(got.is_none());
        let got2 = resolve_secret(Some("sk-or-v1-abc"), None);
        assert!(got2.is_none());
        let got3: Option<String> = resolve_secret(None, None);
        assert!(got3.is_none());
    }

    #[test]
    fn resolve_secret_strips_bearer_prefix_from_env_value() {
        // Operators routinely store keys with `Bearer ` already
        // prepended (e.g. from a curl one-liner). The gateway must
        // strip the prefix so the Authorization header is not
        // emitted as `Bearer Bearer …`.
        unsafe {
            std::env::set_var("RESOLVE_SECRET_BEARER_TEST", "Bearer sk-or-v1-test");
        }
        let got = resolve_secret(Some("env:RESOLVE_SECRET_BEARER_TEST"), None);
        unsafe {
            std::env::remove_var("RESOLVE_SECRET_BEARER_TEST");
        }
        assert_eq!(got.as_deref(), Some("sk-or-v1-test"));
    }

    #[test]
    fn resolve_secret_strips_bearer_prefix_with_leading_whitespace() {
        unsafe {
            std::env::set_var("RESOLVE_SECRET_BEARER_WS_TEST", "  Bearer sk-or-v1-test  ");
        }
        let got = resolve_secret(Some("env:RESOLVE_SECRET_BEARER_WS_TEST"), None);
        unsafe {
            std::env::remove_var("RESOLVE_SECRET_BEARER_WS_TEST");
        }
        assert_eq!(got.as_deref(), Some("sk-or-v1-test"));
    }

    #[test]
    fn resolve_secret_passes_through_keys_without_bearer_prefix() {
        unsafe {
            std::env::set_var("RESOLVE_SECRET_RAW_TEST", "sk-or-v1-raw");
        }
        let got = resolve_secret(Some("env:RESOLVE_SECRET_RAW_TEST"), None);
        unsafe {
            std::env::remove_var("RESOLVE_SECRET_RAW_TEST");
        }
        assert_eq!(got.as_deref(), Some("sk-or-v1-raw"));
    }
}
