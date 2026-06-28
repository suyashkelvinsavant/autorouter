//! HTTP route handlers.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::stream::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

use autorouter_core::{ProviderKind, RequestContext, RequestId, StreamEvent};
use autorouter_router::{RouteDecision, RoutingContext};
use autorouter_translate::{
    OpenAiResponsesAdapter, OpenAiWireFormat, UpstreamResponse, UpstreamStream,
};

use crate::error::{ServerError, ServerResult};
use crate::state::AppState;
use crate::upstream::UpstreamClient;

/// Convert a `ProviderKind` from a string. Used by the
/// `X-AutoRouter-Source` and `X-AutoRouter-Target` headers.
pub fn provider_kind_from_str(s: &str) -> Option<ProviderKind> {
    match s.to_ascii_lowercase().as_str() {
        "openai" | "openai-chat" | "openai_chat" => Some(ProviderKind::OpenAI),
        "anthropic" | "claude" => Some(ProviderKind::Anthropic),
        "gemini" | "google" => Some(ProviderKind::Gemini),
        "custom" => Some(ProviderKind::Custom),
        _ => None,
    }
}

fn extract_session_headers(headers: &HeaderMap) -> (Option<String>, Option<String>) {
    (
        headers
            .get("x-autorouter-session")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
        headers
            .get("x-autorouter-label")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
    )
}

/// Parse the `X-AutoRouter-Tag` header into the list of tags the
/// smart router will see. The header is a comma-separated list.
/// Whitespace around each tag is trimmed; empty entries are dropped;
/// the resulting `Vec<String>` is attached to `RequestContext` before
/// the routing decision is made.
fn extract_tag_header(headers: &HeaderMap) -> Vec<String> {
    let Some(raw) = headers.get("x-autorouter-tag") else {
        return Vec::new();
    };
    match raw.to_str() {
        Ok(s) => s
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect(),
        Err(_) => {
            tracing::warn!("X-AutoRouter-Tag header is not valid UTF-8; ignoring");
            Vec::new()
        }
    }
}

fn source_provider(headers: &HeaderMap) -> ProviderKind {
    let Some(raw) = headers.get("x-autorouter-source") else {
        return ProviderKind::OpenAI;
    };
    let s = match raw.to_str() {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!("X-AutoRouter-Source header is not valid UTF-8; falling back to OpenAI");
            return ProviderKind::OpenAI;
        }
    };
    provider_kind_from_str(s).unwrap_or_else(|| {
        tracing::warn!(value = %s, "unrecognised X-AutoRouter-Source value; falling back to OpenAI");
        ProviderKind::OpenAI
    })
}

fn target_override(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<(ProviderKind, Option<String>)> {
    let raw = headers.get("x-autorouter-target")?;
    let val = match raw.to_str() {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!("X-AutoRouter-Target header is not valid UTF-8; ignoring");
            return None;
        }
    };
    if let Some(kind) = provider_kind_from_str(val) {
        return Some((kind, None));
    }
    let cfg = state.config.read();
    if cfg.providers.custom.contains_key(val) {
        return Some((ProviderKind::Custom, Some(val.to_string())));
    }
    if !val.is_empty() {
        tracing::warn!(value = %val, "unrecognised X-AutoRouter-Target value; ignoring");
    }
    None
}

async fn read_json_body(body: axum::body::Bytes) -> ServerResult<Value> {
    if body.is_empty() {
        return Err(ServerError::BadRequest("empty body".into()));
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|e| ServerError::BadRequest(format!("invalid JSON: {e}")))?;
    Ok(value)
}

fn resolve_custom_target(state: &AppState, decision: &mut RouteDecision) {
    if decision.target_provider == ProviderKind::Custom && decision.custom_target.is_none() {
        let model = &decision.target_model;
        let cfg = state.config.read();
        for (name, entry) in &cfg.providers.custom {
            if entry.enabled && entry.model_allowlist.iter().any(|m| m == model) {
                decision.custom_target = Some(name.clone());
                return;
            }
        }
        if let Some(first_enabled) = cfg
            .providers
            .custom
            .iter()
            .find(|(_, entry)| entry.enabled)
            .map(|(name, _)| name.clone())
        {
            decision.custom_target = Some(first_enabled);
        }
    }
}

/// Run the configured smart router. The router sees the source
/// provider, request metadata, and the parsed universal request and
/// returns a target. The gateway overlays `X-AutoRouter-Target` on
/// top of the router's choice.
fn decide_route(
    state: &AppState,
    source: ProviderKind,
    request_ctx: &RequestContext,
    request: &autorouter_core::UniversalRequest,
) -> RouteDecision {
    // Propagate caller-supplied tags (X-AutoRouter-Tag header,
    // parsed by `extract_tag_header` in each handler and attached
    // to RequestContext) into the routing context. Without this step
    // the tags would silently disappear between RequestContext and
    // RoutingContext, and tag-based rules would never see per-request
    // tags.
    //
    // M12: merge routing.default_tags from the config into the
    // routing context. Per-request tags (set above) win; missing
    // tags are appended.
    let ctx = RoutingContext::new(request.clone(), request_ctx.clone())
        .with_tags(request_ctx.tags.clone())
        .with_default_tags(&state.config.read().routing.default_tags);
    // R11: snapshot the current router so the routing lock is held
    // only for the brief clone and not for the duration of the
    // upstream HTTP call. PATCH /ui/routing / /ui/settings can swap
    // the router in mid-flight; in-flight requests keep routing
    // with the rules that were live when they started.
    let mut decision = state
        .current_router()
        .decide(&ctx, request)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "router returned error, falling back to identity");
            RouteDecision {
                target_provider: source,
                target_model: request.model.clone(),
                reason: std::borrow::Cow::Borrowed("identity-fallback"),
                custom_target: None,
            }
        });
    resolve_custom_target(state, &mut decision);
    // Correlate router logs with the originating request so they
    // can be tied back to the source request id and the eventual
    // upstream response.
    tracing::info!(
        target_provider = %decision.target_provider,
        target_model = %decision.target_model,
        reason = %decision.reason,
        request_id = %request_ctx.request_id,
        source = %source,
        "routing decision"
    );
    decision
}

/// Apply the X-AutoRouter-Target override on top of the router's
/// decision. M5: if the operator disabled
/// `server.allow_target_override_when_unhealthy` and the target
/// provider's health score is below the configured floor, the
/// override is dropped and the router's original decision stands.
fn apply_target_override(
    state: &AppState,
    decision: &mut RouteDecision,
    target: Option<(ProviderKind, Option<String>)>,
) {
    let Some((t, custom_name)) = target else {
        return;
    };
    let cfg = state.config.read().server.clone();
    if !cfg.allow_target_override_when_unhealthy.unwrap_or(false) {
        // 0.5 is the legacy floor used by `HealthTracker::is_healthy`.
        if !state.health.is_healthy(t, 0.5) {
            tracing::warn!(
                target_provider = %t,
                "X-AutoRouter-Target override ignored: provider is below health floor"
            );
            return;
        }
    }
    decision.target_provider = t;
    decision.custom_target = custom_name;
    decision.reason = std::borrow::Cow::Borrowed("x-autorouter-target");
}

/// Resolve the final target provider/model and run the upstream call,
/// recording health and metrics along the way.
/// Record a per-request event to the storage layer when one is
/// attached. Failures to write are logged at warn level and do
/// not bubble up to the request.
///
/// `usage` is the parsed upstream `Usage` block; pass
/// `&Usage::default()` when the upstream did not report usage (the
/// stored row collapses to all-zeros for the token columns, which
/// the analytics page renders as "no usage recorded yet").
fn record_storage_event(
    state: &AppState,
    decision: &RouteDecision,
    model: &str,
    latency_ms: u64,
    status: u16,
    error: Option<&str>,
    usage: &autorouter_core::Usage,
) {
    let Some(storage) = state.storage.as_ref() else {
        return;
    };
    let mut event = autorouter_config::ProviderEvent::with_usage(
        decision.target_provider.to_string(),
        model,
        "request",
        latency_ms,
        usage,
    );
    event.status = status;
    event.error = error.map(|s| s.to_string());
    if let Err(e) = storage.record_provider_event(&event) {
        tracing::warn!(error = %e, "failed to record provider event");
    }
}

async fn call_upstream(
    state: &AppState,
    decision: &RouteDecision,
    upstream_body: &Value,
    request_ctx: &RequestContext,
) -> Result<UpstreamResponse, ServerError> {
    use autorouter_observability::{observe_upstream, record_failure, record_request};
    let upstream = state
        .upstream_for(decision.target_provider)
        .or_else(|| {
            decision
                .custom_target
                .as_deref()
                .and_then(|name| state.custom_upstream_for(name))
        })
        .ok_or_else(|| {
            ServerError::Internal(format!(
                "upstream for `{}` is not configured",
                decision.target_provider
            ))
        })?;
    let model = upstream_body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    record_request(
        &request_ctx.source_provider.to_string(),
        &decision.target_provider.to_string(),
        model,
    );
    let start = Instant::now();
    // Apply the request_timeout_seconds only to non-streaming requests.
    // Streaming requests have their own per-chunk idle timeout via
    // `stream_to_events(stream, source, idle_timeout)`; applying a
    // request-level deadline here would kill an actively-streaming SSE
    // session as soon as `request_timeout_seconds` elapsed even though
    // data is still flowing.
    let timeout_seconds = state.config.read().server.request_timeout_seconds.max(1);
    let send_fut = upstream.send(upstream_body);
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(timeout_seconds), send_fut).await;
    let result = match result {
        Ok(r) => r,
        Err(_) => Err(autorouter_translate::TranslateError::Upstream(format!(
            "request timeout after {timeout_seconds}s"
        ))),
    };
    let latency_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(resp) => {
            observe_upstream(
                &decision.target_provider.to_string(),
                model,
                if resp.status >= 400 { "error" } else { "ok" },
                start.elapsed().as_secs_f64(),
            );
            state.health.record_for_model(
                decision.target_provider,
                model,
                resp.status < 400,
                latency_ms,
            );
            // Record token metrics — the Prometheus
            // `record_tokens` gauge was previously registered but
            // never fed with data.
            autorouter_observability::record_tokens(
                &decision.target_provider.to_string(),
                model,
                "input",
                resp.response.usage.tokens.input.unwrap_or(0),
            );
            autorouter_observability::record_tokens(
                &decision.target_provider.to_string(),
                model,
                "output",
                resp.response.usage.tokens.output.unwrap_or(0),
            );
            record_storage_event(
                state,
                decision,
                model,
                latency_ms,
                resp.status,
                None,
                &resp.response.usage,
            );
            Ok(resp)
        }
        Err(e) => {
            observe_upstream(
                &decision.target_provider.to_string(),
                model,
                "error",
                start.elapsed().as_secs_f64(),
            );
            state
                .health
                .record_for_model(decision.target_provider, model, false, latency_ms);
            record_failure(
                &request_ctx.source_provider.to_string(),
                &decision.target_provider.to_string(),
                "upstream_error",
            );
            // Record rate-limit hits (HTTP 429) so the dashboard can
            // visualise upstream throttling separately from generic
            // failures. Check via the typed upstream_status method
            // rather than substring matching to avoid false positives
            // (e.g. a model name or request id containing "429").
            if e.upstream_status() == Some(429) {
                autorouter_observability::record_rate_limit_hit(
                    &decision.target_provider.to_string(),
                    model,
                );
            }
            record_storage_event(
                state,
                decision,
                model,
                latency_ms,
                502,
                Some(&e.to_string()),
                &autorouter_core::Usage::default(),
            );
            tracing::error!(error = %e, ?decision, "upstream call failed");
            Err(ServerError::Upstream(e.to_string()))
        }
    }
}

pub(crate) fn maybe_authorize(headers: &HeaderMap, state: &AppState) -> ServerResult<()> {
    let cfg = state.config.read();
    if !cfg.server.require_auth.unwrap_or(false) {
        return Ok(());
    }
    let expected = cfg
        .server
        .auth_token
        .as_deref()
        .ok_or_else(|| ServerError::Internal("auth required but no token configured".into()))?;
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    if provided.map(str::trim) == Some(expected.trim()) {
        Ok(())
    } else {
        Err(ServerError::Unauthorized(
            "invalid or missing bearer token".into(),
        ))
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct QueryParams {
    pub target: Option<String>,
    pub stream: Option<bool>,
}

fn wants_stream(body: &Value, query_stream: Option<bool>, default_stream: bool) -> bool {
    if matches!(query_stream, Some(true)) {
        return true;
    }
    match body.get("stream") {
        Some(v) => v.as_bool().unwrap_or(false),
        None => default_stream,
    }
}

/// Resolve the wire-format `ProviderKind` to use for serialising to a
/// target upstream and produce the upstream body.
///
/// For built-in providers (OpenAI / Anthropic / Gemini) the target is
/// already a built-in `ProviderKind`, so we can serialise directly.
///
/// For `ProviderKind::Custom` we look up the custom upstream's
/// `api_format` and translate to the matching built-in
/// `ProviderKind`. That built-in adapter produces the wire body the
/// custom provider actually expects (e.g. an OpenAI Chat
/// Completions body for an OpenAI-compatible custom provider). The
/// custom upstream then sends the body to its own `base_url` with
/// the custom auth headers.
fn serialise_for_target(
    state: &AppState,
    decision: &RouteDecision,
    request: &autorouter_core::UniversalRequest,
) -> ServerResult<Value> {
    use crate::upstream::api_format_to_kind;
    if decision.target_provider == ProviderKind::Custom {
        let custom_name = decision.custom_target.as_deref().ok_or_else(|| {
            ServerError::BadRequest(
                "custom target provider selected but no custom_target name is set on the decision"
                    .into(),
            )
        })?;
        let cfg = state.config.read();
        let entry = cfg.providers.custom.get(custom_name).ok_or_else(|| {
            ServerError::Internal(format!("custom provider `{custom_name}` is not configured"))
        })?;
        let effective_kind = api_format_to_kind(entry.api_format);
        tracing::debug!(
            custom_target = %custom_name,
            api_format = ?entry.api_format,
            effective_kind = %effective_kind,
            "serialising for custom target via built-in adapter"
        );
        return state
            .pipeline
            .serialise_request(effective_kind, request)
            .map_err(|e| ServerError::BadRequest(e.to_string()));
    }
    state
        .pipeline
        .serialise_request(decision.target_provider, request)
        .map_err(|e| ServerError::BadRequest(e.to_string()))
}

pub async fn openai_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(qp): axum::extract::Query<QueryParams>,
    body: axum::body::Bytes,
) -> ServerResult<Response> {
    maybe_authorize(&headers, &state)?;
    let body = read_json_body(body).await?;
    if wants_stream(
        &body,
        qp.stream,
        state
            .config
            .read()
            .defaults
            .stream_by_default
            .unwrap_or(false),
    ) {
        return Ok(openai_chat_stream_inner(state, headers, body)
            .await?
            .into_response());
    }
    let (session_hint, label) = extract_session_headers(&headers);
    let source = source_provider(&headers);
    let registry = state.sessions.clone();
    // Caller-supplied X-AutoRouter-Tag header reaches RequestContext
    // (and therefore the smart router). Without this, tag-based rules
    // only ever matched via the global routing.default_tags config.
    let tags = extract_tag_header(&headers);
    let mut request_ctx = RequestContext::new(source, source).with_tags(tags);
    if let Some((target_kind, _)) = target_override(&state, &headers) {
        request_ctx.target_provider = target_kind;
    }
    let session_id = registry
        .get_or_create(
            crate::session::session_id_from_header(session_hint.as_deref()),
            &source.to_string(),
            label,
        )
        .id;
    registry.record_request(&session_id, request_ctx.request_id.clone());

    let request = state
        .pipeline
        .parse_request(source, &body)
        .map_err(|e| ServerError::BadRequest(e.to_string()))?;
    let mut decision = decide_route(&state, source, &request_ctx, &request);
    apply_target_override(&state, &mut decision, target_override(&state, &headers));
    if decision.target_model.is_empty() {
        decision.target_model = request.model.clone();
    }
    let upstream_body = serialise_for_target(&state, &decision, &request)?;
    let upstream_resp = call_upstream(&state, &decision, &upstream_body, &request_ctx).await?;
    let wire = encode_openai_chat_response(&upstream_resp, &decision.target_model);
    Ok(Json(wire).into_response())
}

pub async fn openai_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(qp): axum::extract::Query<QueryParams>,
    body: axum::body::Bytes,
) -> ServerResult<Response> {
    maybe_authorize(&headers, &state)?;
    let body = read_json_body(body).await?;
    if wants_stream(
        &body,
        qp.stream,
        state
            .config
            .read()
            .defaults
            .stream_by_default
            .unwrap_or(false),
    ) {
        return Ok(openai_responses_stream_inner(state, headers, body)
            .await?
            .into_response());
    }
    let source = source_provider(&headers);
    let tags = extract_tag_header(&headers);
    let mut request_ctx = RequestContext::new(source, source).with_tags(tags);
    if let Some((target_kind, _)) = target_override(&state, &headers) {
        request_ctx.target_provider = target_kind;
    }
    let (session_hint, label) = extract_session_headers(&headers);
    let registry = state.sessions.clone();
    let session_id = registry
        .get_or_create(
            crate::session::session_id_from_header(session_hint.as_deref()),
            &source.to_string(),
            label,
        )
        .id;
    registry.record_request(&session_id, request_ctx.request_id.clone());
    let request = state
        .pipeline
        .parse_request_with_format(source, OpenAiWireFormat::Responses, &body)
        .map_err(|e| ServerError::BadRequest(e.to_string()))?;
    let mut decision = decide_route(&state, source, &request_ctx, &request);
    apply_target_override(&state, &mut decision, target_override(&state, &headers));
    if decision.target_model.is_empty() {
        decision.target_model = request.model.clone();
    }
    let upstream_body = serialise_for_target(&state, &decision, &request)?;
    let upstream_resp = call_upstream(&state, &decision, &upstream_body, &request_ctx).await?;
    let adapter = OpenAiResponsesAdapter::new();
    let wire = encode_responses_response(&upstream_resp, &adapter, &decision.target_model);
    Ok(Json(wire).into_response())
}

pub async fn anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(qp): axum::extract::Query<QueryParams>,
    body: axum::body::Bytes,
) -> ServerResult<Response> {
    maybe_authorize(&headers, &state)?;
    let body = read_json_body(body).await?;
    if wants_stream(
        &body,
        qp.stream,
        state
            .config
            .read()
            .defaults
            .stream_by_default
            .unwrap_or(false),
    ) {
        return Ok(anthropic_messages_stream_inner(state, headers, body)
            .await?
            .into_response());
    }
    let source = source_provider(&headers);
    let tags = extract_tag_header(&headers);
    let mut request_ctx = RequestContext::new(source, source).with_tags(tags);
    if let Some((target_kind, _)) = target_override(&state, &headers) {
        request_ctx.target_provider = target_kind;
    }
    let (session_hint, label) = extract_session_headers(&headers);
    let registry = state.sessions.clone();
    let session_id = registry
        .get_or_create(
            crate::session::session_id_from_header(session_hint.as_deref()),
            &source.to_string(),
            label,
        )
        .id;
    registry.record_request(&session_id, request_ctx.request_id.clone());
    let request = state
        .pipeline
        .parse_request(source, &body)
        .map_err(|e| ServerError::BadRequest(e.to_string()))?;
    let mut decision = decide_route(&state, source, &request_ctx, &request);
    apply_target_override(&state, &mut decision, target_override(&state, &headers));
    if decision.target_model.is_empty() {
        decision.target_model = request.model.clone();
    }
    let upstream_body = serialise_for_target(&state, &decision, &request)?;
    let upstream_resp = call_upstream(&state, &decision, &upstream_body, &request_ctx).await?;
    let wire = encode_anthropic_response(&upstream_resp, &decision.target_model);
    Ok(Json(wire).into_response())
}

/// Strip the trailing method suffix from a Gemini URL path. The
/// wildcard route `/v1beta/models/*path` arrives here with the
/// `models/` already stripped by axum, so we accept both shapes.
/// The model id is everything up to the FIRST `:`, but only when
/// the suffix is a known Gemini method name. Without the
/// allowlist, a model like `nvidia/nemotron-3-nano-30b-a3b:free`
/// would be truncated to `nvidia/nemotron-3-nano-30b-a3b` and
/// rejected by the upstream allowlist.
fn extract_gemini_model_id(path: &str) -> Option<String> {
    let after = path.strip_prefix("models/").unwrap_or(path);
    // The known Gemini RPC method suffixes. Anything else after the
    // first `:` is part of the model id (e.g. OpenRouter
    // ":free" or Bedrock "-latest" tags).
    const METHOD_SUFFIXES: &[&str] = &[
        ":generateContent",
        ":streamGenerateContent",
        ":countTokens",
        ":embedContent",
        ":batchEmbedContents",
        ":list",
        ":get",
    ];
    for suffix in METHOD_SUFFIXES {
        if let Some(id) = after.strip_suffix(suffix) {
            return (!id.is_empty()).then(|| id.to_string());
        }
    }
    if after.is_empty() {
        None
    } else {
        Some(after.to_string())
    }
}

pub async fn gemini_generate_content(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(qp): axum::extract::Query<QueryParams>,
    axum::extract::Path(path): axum::extract::Path<String>,
    body: axum::body::Bytes,
) -> ServerResult<Response> {
    maybe_authorize(&headers, &state)?;
    let body = read_json_body(body).await?;
    if wants_stream(
        &body,
        qp.stream,
        state
            .config
            .read()
            .defaults
            .stream_by_default
            .unwrap_or(false),
    ) {
        return Ok(
            gemini_generate_content_stream_inner(state, headers, body, path)
                .await?
                .into_response(),
        );
    }
    let source = source_provider(&headers);
    let tags = extract_tag_header(&headers);
    let mut request_ctx = RequestContext::new(source, source).with_tags(tags);
    if let Some((target_kind, _)) = target_override(&state, &headers) {
        request_ctx.target_provider = target_kind;
    }
    let (session_hint, label) = extract_session_headers(&headers);
    let registry = state.sessions.clone();
    let session_id = registry
        .get_or_create(
            crate::session::session_id_from_header(session_hint.as_deref()),
            &source.to_string(),
            label,
        )
        .id;
    registry.record_request(&session_id, request_ctx.request_id.clone());
    let mut request = state
        .pipeline
        .parse_request(source, &body)
        .map_err(|e| ServerError::BadRequest(e.to_string()))?;
    // The Gemini protocol carries the model id in the URL path
    // (e.g. `models/gemini-2.5-pro:generateContent`) rather than in
    // the request body. If the body omitted `model`, lift the path
    // id into the universal request so the smart router, the model
    // allowlist, and the upstream body all see it. If the body did
    // set `model`, prefer the body's value (the caller was explicit).
    if request.model.is_empty() {
        if let Some(id) = extract_gemini_model_id(&path) {
            request.model = id;
        }
    }
    let mut decision = decide_route(&state, source, &request_ctx, &request);
    apply_target_override(&state, &mut decision, target_override(&state, &headers));
    if decision.target_model.is_empty() {
        decision.target_model = request.model.clone();
    }
    let upstream_body = serialise_for_target(&state, &decision, &request)?;
    let upstream_resp = call_upstream(&state, &decision, &upstream_body, &request_ctx).await?;
    let wire = encode_gemini_response(&upstream_resp);
    Ok(Json(wire).into_response())
}

pub async fn health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ServerResult<Json<Value>> {
    // Gateway metadata endpoints must respect `require_auth`.
    // Without this check, anyone with loopback/network access can
    // enumerate sessions, models, and provider health even when the
    // operator has enabled bearer auth on the rest of the gateway.
    maybe_authorize(&headers, &state)?;
    let mut providers = serde_json::Map::new();
    for kind in [
        ProviderKind::OpenAI,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
    ] {
        let snap = state.health.snapshot(kind);
        providers.insert(
            kind.to_string(),
            json!({
                "samples": snap.samples,
                "success_rate": snap.success_rate,
                "avg_latency_ms": snap.avg_latency_ms,
                "score": snap.score,
            }),
        );
    }
    Ok(Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "providers": providers,
        "sessions": state.sessions.list().len(),
    })))
}

pub async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ServerResult<Json<Value>> {
    maybe_authorize(&headers, &state)?;
    let sessions: Vec<Value> = state
        .sessions
        .list()
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id,
                "label": s.label,
                "source_provider": s.source_provider,
                "created_at": s.created_at,
                "last_request_at": s.updated_at,
                "last_request_id": s.last_request_id,
                "request_count": s.request_count,
            })
        })
        .collect();
    Ok(Json(json!({ "sessions": sessions })))
}

pub async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ServerResult<Json<Value>> {
    maybe_authorize(&headers, &state)?;
    let mut models: Vec<Value> = Vec::new();
    for adapter in state.pipeline_adapters() {
        for m in adapter.models() {
            models.push(json!({
                "id": m.id,
                "display_name": m.display_name,
                "family": m.family,
                "provider": m.provider,
                "context_window": m.context_window,
                "max_output_tokens": m.max_output_tokens,
                "supports_tools": m.supports_tools,
                "supports_vision": m.supports_vision,
                "supports_audio": m.supports_audio,
                "supports_streaming": m.supports_streaming,
            }));
        }
    }
    let cfg = state.config.read().clone();
    for entry in cfg.providers.custom.values() {
        for model_id in &entry.model_allowlist {
            models.push(json!({
                "id": model_id,
                "display_name": model_id,
                "family": autorouter_core::ModelFamily::Chat,
                "provider": autorouter_core::ProviderKind::Custom,
                "context_window": 131072,
                "max_output_tokens": 4096,
                "supports_tools": true,
                "supports_vision": true,
                "supports_audio": false,
                "supports_streaming": true,
            }));
        }
    }
    Ok(Json(json!({ "models": models })))
}

fn encode_openai_chat_response(upstream: &UpstreamResponse, fallback_model: &str) -> Value {
    let model = if upstream.response.model.is_empty() {
        fallback_model.to_string()
    } else {
        upstream.response.model.clone()
    };
    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for part in &upstream.response.message.content {
        match part {
            autorouter_core::ContentPart::Text { text } => content.push_str(text),
            autorouter_core::ContentPart::Reasoning { text } => {
                // Collect reasoning text into a sibling
                // `reasoning_content` field on the assistant message
                // (OpenAI o-series wire shape). Only emitted when
                // non-empty to avoid adding a noisy field on plain
                // completions.
                reasoning_content.push_str(text);
            }
            autorouter_core::ContentPart::ToolCall {
                id,
                name,
                arguments,
            } => {
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(arguments).unwrap_or_default(),
                    }
                }));
            }
            _ => {}
        }
    }
    let finish_reason = match upstream.response.finish_reason {
        autorouter_core::FinishReason::Stop => "stop",
        autorouter_core::FinishReason::Length => "length",
        autorouter_core::FinishReason::ToolCalls => "tool_calls",
        autorouter_core::FinishReason::ContentFilter => "content_filter",
        _ => "stop",
    };
    let mut message = json!({ "role": "assistant", "content": content });
    if !reasoning_content.is_empty() {
        message["reasoning_content"] = json!(reasoning_content);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    json!({
        "id": upstream.response.id,
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": upstream.response.usage.tokens.input.unwrap_or(0),
            "completion_tokens": upstream.response.usage.tokens.output.unwrap_or(0),
            "total_tokens": upstream.response.usage.total_tokens(),
        }
    })
}

fn encode_responses_response(
    upstream: &UpstreamResponse,
    _adapter: &OpenAiResponsesAdapter,
    fallback_model: &str,
) -> Value {
    let model = if upstream.response.model.is_empty() {
        fallback_model.to_string()
    } else {
        upstream.response.model.clone()
    };
    let mut output: Vec<Value> = Vec::new();
    for part in &upstream.response.message.content {
        match part {
            autorouter_core::ContentPart::Text { text } => {
                output.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": text }],
                }));
            }
            autorouter_core::ContentPart::Reasoning { text } => {
                // OpenAI Responses wire format accepts reasoning
                // items as siblings of message items in `output[]`.
                // The `summary` array shape is what the API returns
                // for completed reasoning items; we emit a single
                // `summary_text` fragment with the captured thinking
                // text.
                output.push(json!({
                    "type": "reasoning",
                    "summary": [{
                        "type": "summary_text",
                        "text": text,
                    }],
                }));
            }
            autorouter_core::ContentPart::ToolCall {
                id,
                name,
                arguments,
            } => {
                output.push(json!({
                    "type": "function_call",
                    "id": id,
                    "name": name,
                    "arguments": serde_json::to_string(arguments).unwrap_or_default(),
                }));
            }
            _ => {}
        }
    }
    let status = match upstream.response.finish_reason {
        autorouter_core::FinishReason::Stop => "completed",
        autorouter_core::FinishReason::Length => "incomplete",
        _ => "completed",
    };
    json!({
        "id": upstream.response.id,
        "object": "response",
        "model": model,
        "status": status,
        "output": output,
        "usage": {
            "input_tokens": upstream.response.usage.tokens.input.unwrap_or(0),
            "output_tokens": upstream.response.usage.tokens.output.unwrap_or(0),
        }
    })
}

fn encode_anthropic_response(upstream: &UpstreamResponse, fallback_model: &str) -> Value {
    let model = if upstream.response.model.is_empty() {
        fallback_model.to_string()
    } else {
        upstream.response.model.clone()
    };
    let mut content: Vec<Value> = Vec::new();
    for part in &upstream.response.message.content {
        match part {
            autorouter_core::ContentPart::Text { text } => {
                content.push(json!({ "type": "text", "text": text }));
            }
            autorouter_core::ContentPart::Reasoning { text } => {
                // Anthropic extended-thinking wire format. The
                // `signature` field is required by the Anthropic
                // API for client emissions but we don't have one
                // from the upstream decode path — omitting it is
                // tolerated by consumers that don't validate
                // signatures on outbound responses. We also don't
                // emit a paired `redacted_thinking` block because
                // the decoded text was visible (not redacted).
                content.push(json!({
                    "type": "thinking",
                    "thinking": text,
                }));
            }
            autorouter_core::ContentPart::ToolCall {
                id,
                name,
                arguments,
            } => {
                content.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": arguments,
                }));
            }
            _ => {}
        }
    }
    let stop_reason = match upstream.response.finish_reason {
        autorouter_core::FinishReason::Stop => "end_turn",
        autorouter_core::FinishReason::Length => "max_tokens",
        autorouter_core::FinishReason::ToolCalls => "tool_use",
        autorouter_core::FinishReason::Safety => "refusal",
        _ => "end_turn",
    };
    json!({
        "id": upstream.response.id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "usage": {
            "input_tokens": upstream.response.usage.tokens.input.unwrap_or(0),
            "output_tokens": upstream.response.usage.tokens.output.unwrap_or(0),
        }
    })
}

fn encode_gemini_response(upstream: &UpstreamResponse) -> Value {
    let mut parts: Vec<Value> = Vec::new();
    for part in &upstream.response.message.content {
        match part {
            autorouter_core::ContentPart::Text { text } => {
                parts.push(json!({ "text": text }));
            }
            autorouter_core::ContentPart::Reasoning { text } => {
                // Gemini "thought" part shape: text + `thought: true`.
                // Consumers that don't render thinking can still
                // surface the model output; clients that understand
                // `thought` will render it as a separate chain of
                // thought.
                parts.push(json!({
                    "text": text,
                    "thought": true,
                }));
            }
            autorouter_core::ContentPart::ToolCall {
                id,
                name,
                arguments,
            } => {
                parts.push(json!({
                    "functionCall": { "name": name, "args": arguments, "id": id }
                }));
            }
            _ => {}
        }
    }
    let finish_reason = match upstream.response.finish_reason {
        autorouter_core::FinishReason::Length => "MAX_TOKENS",
        autorouter_core::FinishReason::Safety => "SAFETY",
        _ => "STOP",
    };
    json!({
        "candidates": [{
            "content": { "role": "model", "parts": parts },
            "finishReason": finish_reason,
        }],
        "modelVersion": upstream.response.model,
        "usageMetadata": {
            "promptTokenCount": upstream.response.usage.tokens.input.unwrap_or(0),
            "candidatesTokenCount": upstream.response.usage.tokens.output.unwrap_or(0),
        }
    })
}

// ----- streaming variants -----

/// Shared boilerplate for the four protocol streaming handlers
/// (`openai_chat_stream_inner`, `openai_responses_stream_inner`,
/// `anthropic_messages_stream_inner`,
/// `gemini_generate_content_stream_inner`). Each one called
/// `prepare_stream_decision[_with_format]` with a different
/// source-format combo, then ran the same session-creation,
/// `record_request`, `send_streaming` error-handling, and
/// "tracked stream" wrapper. Extract that into a single helper so
/// the per-protocol functions are a thin shim that just supplies
/// the protocol-specific plan.
async fn stream_inner_from_plan(
    state: AppState,
    headers: HeaderMap,
    decision: StreamPlan,
) -> ServerResult<Sse<impl Stream<Item = Result<Event, axum::Error>>>> {
    use autorouter_observability::{
        observe_upstream, record_failure, record_request, record_tokens,
    };
    let (session_hint, label) = extract_session_headers(&headers);
    let registry = state.sessions.clone();
    let session_id = registry
        .get_or_create(
            crate::session::session_id_from_header(session_hint.as_deref()),
            &decision.source.to_string(),
            label,
        )
        .id;
    registry.record_request(&session_id, decision.request_id.clone());
    let model = &decision.target_model;
    record_request(
        &decision.source.to_string(),
        &decision.decision.target_provider.to_string(),
        model,
    );
    let start = std::time::Instant::now();
    let stream = match decision
        .upstream
        .send_streaming(&decision.upstream_body)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            observe_upstream(
                &decision.decision.target_provider.to_string(),
                model,
                "error",
                start.elapsed().as_secs_f64(),
            );
            record_failure(
                &decision.source.to_string(),
                &decision.decision.target_provider.to_string(),
                "upstream_error",
            );
            if e.upstream_status() == Some(429) {
                autorouter_observability::record_rate_limit_hit(
                    &decision.decision.target_provider.to_string(),
                    model,
                );
            }
            state.health.record_for_model(
                decision.decision.target_provider,
                model,
                false,
                start.elapsed().as_millis() as u64,
            );
            record_storage_event(
                &state,
                &decision.decision,
                model,
                start.elapsed().as_millis() as u64,
                502,
                Some(&e.to_string()),
                &autorouter_core::Usage::default(),
            );
            return Err(ServerError::Upstream(e.to_string()));
        }
    };
    let health = state.health.clone();
    let storage = state.storage.clone();
    let target_provider = decision.decision.target_provider;
    let target_model = decision.target_model.clone();
    let started = start;
    let tracked = async_stream::stream! {
        let mut first = true;
        let mut last_usage: Option<autorouter_core::Usage> = None;
        let mut s = std::pin::pin!(stream);
        while let Some(item) = s.next().await {
            if first {
                first = false;
                let latency_ms = started.elapsed().as_millis() as u64;
                observe_upstream(&target_provider.to_string(), &target_model, "ok", started.elapsed().as_secs_f64());
                health.record_for_model(target_provider, &target_model, true, latency_ms);
            }
            if let Ok(ref chunk) = item {
                for event in &chunk.events {
                    if let autorouter_core::StreamEvent::Finish { usage: Some(u), .. } = event {
                        last_usage = Some(u.clone());
                    }
                }
            }
            yield item;
        }
        // Record provider event with actual usage from Finish (not zeros)
        let usage = last_usage.unwrap_or_default();
        // Record token metrics — the Prometheus `record_tokens`
        // gauge was registered but never fed with data.
        record_tokens(
            &target_provider.to_string(),
            &target_model,
            "input",
            usage.tokens.input.unwrap_or(0),
        );
        record_tokens(
            &target_provider.to_string(),
            &target_model,
            "output",
            usage.tokens.output.unwrap_or(0),
        );
        if let Some(ref storage) = storage {
            let mut event = autorouter_config::ProviderEvent::with_usage(
                target_provider.to_string(),
                &target_model,
                "request",
                started.elapsed().as_millis() as u64,
                &usage,
            );
            event.status = 200;
            let _ = storage.record_provider_event(&event);
        }
    };
    Ok(Sse::new(stream_to_events(
        Box::pin(tracked),
        decision.source,
        decision.idle_timeout,
    )))
}

pub async fn openai_chat_stream_inner(
    state: AppState,
    headers: HeaderMap,
    body: Value,
) -> ServerResult<Sse<impl Stream<Item = Result<Event, axum::Error>>>> {
    let decision = prepare_stream_decision(&state, &headers, body, None).await?;
    stream_inner_from_plan(state, headers, decision).await
}

pub async fn openai_responses_stream_inner(
    state: AppState,
    headers: HeaderMap,
    body: Value,
) -> ServerResult<Sse<impl Stream<Item = Result<Event, axum::Error>>>> {
    let decision = prepare_stream_decision_with_format(
        &state,
        &headers,
        OpenAiWireFormat::Responses,
        body,
        None,
    )
    .await?;
    stream_inner_from_plan(state, headers, decision).await
}

pub async fn anthropic_messages_stream_inner(
    state: AppState,
    headers: HeaderMap,
    body: Value,
) -> ServerResult<Sse<impl Stream<Item = Result<Event, axum::Error>>>> {
    let decision = prepare_stream_decision(&state, &headers, body, None).await?;
    stream_inner_from_plan(state, headers, decision).await
}

pub async fn gemini_generate_content_stream_inner(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    path: String,
) -> ServerResult<Sse<impl Stream<Item = Result<Event, axum::Error>>>> {
    let decision = prepare_stream_decision(&state, &headers, body, Some(path.as_str())).await?;
    stream_inner_from_plan(state, headers, decision).await
}

struct StreamPlan {
    source: ProviderKind,
    target_model: String,
    request_id: RequestId,
    upstream_body: Value,
    upstream: std::sync::Arc<dyn UpstreamClient>,
    idle_timeout: std::time::Duration,
    decision: RouteDecision,
}

async fn prepare_stream_decision(
    state: &AppState,
    headers: &HeaderMap,
    body: Value,
    gemini_path: Option<&str>,
) -> ServerResult<StreamPlan> {
    prepare_stream_decision_with_format(
        state,
        headers,
        OpenAiWireFormat::default(),
        body,
        gemini_path,
    )
    .await
}

async fn prepare_stream_decision_with_format(
    state: &AppState,
    headers: &HeaderMap,
    openai_format: OpenAiWireFormat,
    body: Value,
    gemini_path: Option<&str>,
) -> ServerResult<StreamPlan> {
    maybe_authorize(headers, state)?;
    let source = source_provider(headers);
    let tags = extract_tag_header(headers);
    let mut request_ctx = RequestContext::new(source, source).with_tags(tags);
    if let Some((target_kind, _)) = target_override(state, headers) {
        request_ctx.target_provider = target_kind;
    }
    let mut request = state
        .pipeline
        .parse_request_with_format(source, openai_format, &body)
        .map_err(|e| ServerError::BadRequest(e.to_string()))?;
    // Gemini source: the model id lives in the URL path, not the
    // body. Lift it onto the universal request when the body did
    // not carry one.
    if source == ProviderKind::Gemini && request.model.is_empty() {
        if let Some(p) = gemini_path {
            if let Some(id) = extract_gemini_model_id(p) {
                request.model = id;
            }
        }
    }
    let mut decision = decide_route(state, source, &request_ctx, &request);
    apply_target_override(state, &mut decision, target_override(state, headers));
    if decision.target_model.is_empty() {
        decision.target_model = request.model.clone();
    }
    let upstream_body = serialise_for_target(state, &decision, &request)?;
    let upstream = state
        .upstream_for(decision.target_provider)
        .or_else(|| {
            decision
                .custom_target
                .as_deref()
                .and_then(|name| state.custom_upstream_for(name))
        })
        .ok_or_else(|| ServerError::Internal("upstream not configured".into()))?;
    let idle_timeout = std::time::Duration::from_secs(
        state
            .config
            .read()
            .server
            .stream_idle_timeout_seconds
            .max(1),
    );
    Ok(StreamPlan {
        source,
        target_model: decision.target_model.clone(),
        request_id: request_ctx.request_id,
        upstream_body,
        upstream,
        idle_timeout,
        decision,
    })
}

fn stream_to_events(
    stream: UpstreamStream,
    source: ProviderKind,
    idle_timeout: std::time::Duration,
) -> impl Stream<Item = Result<Event, axum::Error>> {
    async_stream::stream! {
        let mut s = stream;
        let mut errored = false;
        loop {
            let next = tokio::time::timeout(idle_timeout, s.next()).await;
            match next {
                Ok(Some(Ok(chunk))) => {
                    for event in &chunk.events {
                        yield Ok(event_to_sse(event, source));
                    }
                    if matches!(chunk.events.last(), Some(StreamEvent::Finish { .. })) {
                        break;
                    }
                }
                Ok(Some(Err(e))) => {
                    let payload = json!({ "error": { "message": e.to_string() } }).to_string();
                    yield Ok(Event::default().event("error").data(payload));
                    errored = true;
                    break;
                }
                Ok(None) => break,
                Err(_elapsed) => {
                    let payload = json!({ "error": { "message": "stream idle timeout" } }).to_string();
                    yield Ok(Event::default().event("error").data(payload));
                    errored = true;
                    break;
                }
            }
        }
        if !errored {
            match source {
                ProviderKind::OpenAI => {
                    yield Ok(Event::default().data("[DONE]"));
                }
                ProviderKind::Anthropic => {
                    yield Ok(Event::default()
                        .event("message_stop")
                        .data(json!({"type":"message_stop"}).to_string()));
                }
                ProviderKind::Gemini => {
                    // Gemini streams end implicitly; no terminal sentinel.
                }
                ProviderKind::Custom => {
                    yield Ok(Event::default().data("[DONE]"));
                }
            }
        }
    }
}

/// M1: route the per-event SSE shape to the streaming helpers in
/// `autorouter-translate` so the wire format is owned by one place.
/// The adapter is still the single source of truth via its
/// `encode_stream_chunk` override (AGENTS rule #3); this function is
/// used when a single `StreamEvent` is emitted (e.g. by the Finish
/// event the gateway synthesises after the upstream closes).
fn event_to_sse(event: &StreamEvent, source: ProviderKind) -> Event {
    use autorouter_translate::streaming as s;
    let sse = match source {
        ProviderKind::OpenAI | ProviderKind::Custom => s::encode_openai_sse(event),
        ProviderKind::Anthropic => s::encode_anthropic_sse(event),
        ProviderKind::Gemini => s::encode_gemini_sse(event),
    };
    // An adapter may emit a multi-line SSE frame:
    //   "event: content_block_delta\ndata: {\"type\":...}\n\n"
    // (Anthropic), or a single-line one:
    //   "data: {\"object\":...}\n\n"
    // (OpenAI). We must split those into the axum `Event` `event`
    // name and `data` payload separately. The previous
    // implementation only checked the first line for `event:` and
    // then passed the *entire* multi-line string as the data
    // payload, producing output like
    //   data: event: content_block_delta
    //   data: data: {"type":...}
    // which is invalid SSE and breaks Claude Code / any strict
    // consumer.
    let mut event_name: Option<String> = None;
    let mut data_payload: Option<String> = None;
    for line in sse.lines() {
        if let Some(name) = line.strip_prefix("event: ") {
            let name = name.trim();
            if !name.is_empty() {
                event_name = Some(name.to_string());
            }
        } else if let Some(payload) = line.strip_prefix("data: ") {
            data_payload = Some(payload.to_string());
        } else if let Some(payload) = line.strip_prefix("data:") {
            // No leading space — accept it anyway so adapters that
            // emit `data:foo` (no space) don't get dropped.
            let payload = payload.trim();
            if !payload.is_empty() {
                data_payload = Some(payload.to_string());
            }
        }
    }
    let mut ev = Event::default();
    if let Some(name) = event_name {
        ev = ev.event(name);
    }
    match data_payload {
        Some(p) => ev.data(p),
        // No data line at all: emit the event with an empty data
        // payload so the consumer at least sees the event name.
        None => ev.data(""),
    }
}

// ---- in-module tests ---------------------------------------------------
//
// Round-trip the four response encoders against a UniversalResponse
// that carries a `ContentPart::Reasoning`. Asserts the wire format
// emitted in each case so a regression in any of the four encoders
// (or in the decoder-side `ContentPart::Reasoning` production) is
// caught locally without spinning up an upstream.
//
// Kept inside this file (not in `tests/`) because the encoder
// helpers are intentionally private — making them `pub` purely to
// satisfy a test would widen the API surface for no caller benefit.

#[cfg(test)]
mod reasoning_round_trip_tests {
    use super::*;
    use autorouter_core::{ContentPart, FinishReason, Message, MessageRole, UniversalResponse};
    use autorouter_translate::OpenAiResponsesAdapter;

    /// Build a fake UpstreamResponse with a single Reasoning part
    /// followed by a Text part. Used as input to every encoder test.
    fn upstream_with_reasoning() -> UpstreamResponse {
        let response = UniversalResponse {
            id: "resp_x".to_string(),
            model: "test-model".to_string(),
            message: Message {
                role: MessageRole::Assistant,
                content: vec![
                    ContentPart::Reasoning {
                        text: "thinking out loud".to_string(),
                    },
                    ContentPart::Text {
                        text: "the answer".to_string(),
                    },
                ],
                name: None,
            },
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: Default::default(),
            created_at: None,
        };
        UpstreamResponse {
            response,
            status: 200,
            raw: serde_json::json!({}),
        }
    }

    #[test]
    fn openai_chat_response_emits_reasoning_content() {
        let wire = encode_openai_chat_response(&upstream_with_reasoning(), "test-model");
        let message = &wire["choices"][0]["message"];
        assert_eq!(message["role"], "assistant");
        assert_eq!(message["content"], "the answer");
        assert_eq!(
            message["reasoning_content"], "thinking out loud",
            "OpenAI Chat Completions must surface reasoning on the message object"
        );
    }

    #[test]
    fn openai_chat_response_omits_reasoning_when_empty() {
        // A plain (non-reasoning) completion must not add a noisy
        // empty `reasoning_content` field.
        let mut u = upstream_with_reasoning();
        u.response.message.content = vec![ContentPart::Text {
            text: "just an answer".to_string(),
        }];
        let wire = encode_openai_chat_response(&u, "test-model");
        assert!(
            wire["choices"][0]["message"]
                .get("reasoning_content")
                .is_none(),
            "reasoning_content must not be emitted when there is no reasoning: {wire:?}"
        );
    }

    #[test]
    fn openai_responses_response_emits_reasoning_item() {
        let adapter = OpenAiResponsesAdapter::new();
        let wire = encode_responses_response(&upstream_with_reasoning(), &adapter, "test-model");
        let output = wire["output"].as_array().expect("output array");
        // First item: the reasoning. Second: the message.
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["type"], "summary_text");
        assert_eq!(output[0]["summary"][0]["text"], "thinking out loud");
        assert_eq!(output[1]["type"], "message");
        assert_eq!(output[1]["content"][0]["type"], "output_text");
        assert_eq!(output[1]["content"][0]["text"], "the answer");
    }

    #[test]
    fn anthropic_response_emits_thinking_block() {
        let wire = encode_anthropic_response(&upstream_with_reasoning(), "test-model");
        let content = wire["content"].as_array().expect("content array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "thinking out loud");
        // Signature is intentionally absent — we don't have one
        // from the upstream decode path, and the Anthropic API
        // tolerates its absence on client-emitted thinking blocks.
        assert!(
            content[0].get("signature").is_none(),
            "signature must not be invented; got {content:?}"
        );
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "the answer");
    }

    #[test]
    fn gemini_response_emits_thought_part() {
        let wire = encode_gemini_response(&upstream_with_reasoning());
        let parts = wire["candidates"][0]["content"]["parts"]
            .as_array()
            .expect("parts array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "thinking out loud");
        assert_eq!(parts[0]["thought"], true);
        assert_eq!(parts[1]["text"], "the answer");
        assert!(
            parts[1].get("thought").is_none(),
            "non-reasoning parts must NOT carry thought=true"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::extract_gemini_model_id;
    #[test]
    fn gemini_strips_generate_content_suffix() {
        assert_eq!(
            extract_gemini_model_id("gemini-2.5-pro:generateContent"),
            Some("gemini-2.5-pro".to_string())
        );
    }
    #[test]
    fn gemini_strips_stream_generate_content_suffix() {
        assert_eq!(
            extract_gemini_model_id("gemini-2.5-pro:streamGenerateContent"),
            Some("gemini-2.5-pro".to_string())
        );
    }
    #[test]
    fn gemini_preserves_colons_in_model_id() {
        assert_eq!(
            extract_gemini_model_id("nvidia/nemotron-3-nano-30b-a3b:free:generateContent"),
            Some("nvidia/nemotron-3-nano-30b-a3b:free".to_string())
        );
    }
    #[test]
    fn gemini_handles_models_prefix() {
        assert_eq!(
            extract_gemini_model_id("models/gemini-2.5-pro:generateContent"),
            Some("gemini-2.5-pro".to_string())
        );
    }
    #[test]
    fn gemini_returns_none_for_empty() {
        assert_eq!(extract_gemini_model_id(""), None);
    }
    #[test]
    fn gemini_passes_through_bare_id() {
        assert_eq!(
            extract_gemini_model_id("some/model"),
            Some("some/model".to_string())
        );
    }
}
