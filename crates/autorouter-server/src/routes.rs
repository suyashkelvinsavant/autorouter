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

use autorouter_core::{ProviderKind, RequestContext, RequestId, StreamEvent, UniversalRequest};
use autorouter_router::{RouteDecision, RoutingContext};
use autorouter_translate::streaming as sse_mod;
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
        // No custom provider's allowlist matches this model. Prefer the
        // configured `default_provider` (e.g. `tokenrouter`, which has NO
        // allowlist and accepts any model) over an arbitrary first-enabled
        // custom provider. Picking a custom provider that *has* an allowlist
        // (e.g. `openrouter_free`) would immediately reject the model with
        // "not in this provider's allowlist" and trigger a wasteful failover
        // chain — especially flaky on constrained hosts. Only fall back to
        // "first enabled" if the default_provider isn't a custom provider.
        let preferred = if !cfg.defaults.default_provider.is_empty()
            && cfg
                .providers
                .custom
                .get(&cfg.defaults.default_provider)
                .is_some_and(|e| e.enabled)
        {
            Some(cfg.defaults.default_provider.clone())
        } else {
            None
        };
        if let Some(name) = preferred {
            decision.custom_target = Some(name);
        } else if let Some(first_enabled) = cfg
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

/// When the router's default rule fires with an unconfigured built-in
/// provider slot, redirect to the first enabled custom provider that
/// accepts the model. No-op when the built-in slot IS configured.
///
/// Sentinel (`autorouter/autorouter`) falls through to the first custom
/// provider's first allowlist entry — the common first-run pattern.
fn maybe_route_unconfigured_built_in_to_custom(
    state: &AppState,
    decision: &mut RouteDecision,
    request: &autorouter_core::UniversalRequest,
) {
    let cfg = state.config.read();
    let slot_unconfigured = match decision.target_provider {
        ProviderKind::OpenAI => !cfg
            .providers
            .openai
            .as_ref()
            .is_some_and(|e| e.enabled && !e.base_url.is_empty()),
        ProviderKind::Anthropic => !cfg
            .providers
            .anthropic
            .as_ref()
            .is_some_and(|e| e.enabled && !e.base_url.is_empty()),
        ProviderKind::Gemini => !cfg
            .providers
            .gemini
            .as_ref()
            .is_some_and(|e| e.enabled && !e.base_url.is_empty()),
        ProviderKind::Custom => return,
    };
    if !slot_unconfigured {
        return;
    }

    let request_is_sentinel = autorouter_core::is_sentinel_model(&request.model);

    // Branch A — the operator typed a real model id in their tool
    // config (e.g. `nvidia/nemotron-3-ultra-550b-a55b:free`). Walk
    // the custom providers and pick the first one whose allowlist
    // accepts the model.
    if !request_is_sentinel && !request.model.is_empty() {
        for (name, entry) in &cfg.providers.custom {
            if !entry.enabled || entry.base_url.is_empty() {
                continue;
            }
            let model_allowed = entry.model_allowlist.is_empty()
                || entry
                    .model_allowlist
                    .iter()
                    .any(|m| crate::model_db::same_model(m, &request.model));
            if !model_allowed {
                continue;
            }
            rewrite_to_custom(decision, name, &request.model);
            tracing::info!(
                custom = %name,
                model = %request.model,
                "auto-routed unconfigured built-in target to configured custom provider (real model)"
            );
            return;
        }
    }

    // Branch B — the operator's tool sent the sentinel
    // `"autorouter/autorouter"` (the value the dashboard's OpenCode
    // snippet tells them to use). The sentinel cannot match any
    // custom allowlist, so we pick the first enabled custom provider
    // that has a non-empty allowlist and use its first entry as the
    // target model. This is the fix for "I registered OpenRouter +
    // one model, followed the dashboard recipe, and every request
    // showed up as openai/gpt-5".
    if request_is_sentinel {
        for (name, entry) in &cfg.providers.custom {
            if !entry.enabled || entry.base_url.is_empty() {
                continue;
            }
            // Prefer a provider that actually has models registered
            // so the wire body carries a real model id upstream.
            if let Some(first_model) = entry.model_allowlist.first() {
                rewrite_to_custom(decision, name, first_model);
                tracing::info!(
                    custom = %name,
                    model = %first_model,
                    "auto-routed sentinel request to first configured custom provider (sentinel branch)"
                );
                return;
            }
        }
        // Fall back: sentinel request, all custom providers have
        // empty allowlists. Don't route to custom — the sentinel
        // model string would be sent upstream and the provider
        // would reject it as an unknown model. Let MockUpstream
        // handle it with a sensible response instead.
        tracing::info!(
            "sentinel request with empty custom allowlists; leaving on built-in MockUpstream"
        );
    }
}

/// Apply the rewrite shared by both branches of
/// [`maybe_route_unconfigured_built_in_to_custom`].
fn rewrite_to_custom(decision: &mut RouteDecision, name: &str, model: &str) {
    decision.target_provider = ProviderKind::Custom;
    decision.custom_target = Some(name.to_string());
    decision.target_model = model.to_string();
    decision.reason = std::borrow::Cow::Borrowed("auto-route-to-custom");
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
    // If the default rule picked a built-in slot that the operator
    // has not configured (so the call would land on a MockUpstream
    // and be recorded as `openai/gpt-5` even though the user
    // configured a real custom provider for the model), redirect
    // the decision to that custom provider before we serialise and
    // call upstream.
    maybe_route_unconfigured_built_in_to_custom(state, &mut decision, request);
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
        provider_label(decision),
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

/// Resolve the human-meaningful provider label for a routing
/// decision. For built-in slots we return the kind's lowercase
/// name (`openai`, `anthropic`, `gemini`); for custom slots we
/// return the operator-chosen `custom_target` id (`openrouter`,
/// `groq`, ...). Falling back to the generic `custom` string only
/// when the decision somehow lost its `custom_target` (which should
/// not happen post-`resolve_custom_target` but is guarded for
/// safety).
///
/// This label is what lands in the `provider_events` table, the
/// Prometheus counters, the dashboard's Requests page, and the
/// Analytics roll-ups. Returning the real provider id (instead of
/// the generic `custom`) is what stops the dashboard from showing
/// phantom `openai/gpt-5` rows when the user only configured a
/// custom provider like OpenRouter.
fn provider_label(decision: &RouteDecision) -> String {
    if decision.target_provider == ProviderKind::Custom {
        return decision
            .custom_target
            .clone()
            .unwrap_or_else(|| ProviderKind::Custom.to_string());
    }
    decision.target_provider.to_string()
}

fn find_other_providers_for_model(
    state: &AppState,
    current_provider: ProviderKind,
    current_custom_target: Option<&str>,
    model_id: &str,
) -> Vec<(ProviderKind, Option<String>)> {
    let config = state.config.read();
    let mut providers = Vec::new();

    let model_allowed = |list: &[String]| -> bool {
        list.is_empty()
            || list
                .iter()
                .any(|a| crate::model_db::same_model(a, model_id))
    };

    if current_provider != ProviderKind::OpenAI
        && config
            .providers
            .openai
            .as_ref()
            .map(|p| p.enabled)
            .unwrap_or(false)
    {
        if let Some(list) = config.providers.openai.as_ref().map(|p| &p.model_allowlist) {
            if model_allowed(list) {
                providers.push((ProviderKind::OpenAI, None));
            }
        }
    }
    if current_provider != ProviderKind::Anthropic
        && config
            .providers
            .anthropic
            .as_ref()
            .map(|p| p.enabled)
            .unwrap_or(false)
    {
        if let Some(list) = config
            .providers
            .anthropic
            .as_ref()
            .map(|p| &p.model_allowlist)
        {
            if model_allowed(list) {
                providers.push((ProviderKind::Anthropic, None));
            }
        }
    }
    if current_provider != ProviderKind::Gemini
        && config
            .providers
            .gemini
            .as_ref()
            .map(|p| p.enabled)
            .unwrap_or(false)
    {
        if let Some(list) = config.providers.gemini.as_ref().map(|p| &p.model_allowlist) {
            if model_allowed(list) {
                providers.push((ProviderKind::Gemini, None));
            }
        }
    }

    for (name, entry) in &config.providers.custom {
        if entry.enabled
            && current_custom_target != Some(name.as_str())
            && model_allowed(&entry.model_allowlist)
        {
            providers.push((ProviderKind::Custom, Some(name.clone())));
        }
    }

    providers
}

/// Classify an upstream attempt result as retryable.
///
/// Only **hard failures** trigger failover:
///   * upstream HTTP `5xx` (server error, overload, gateway down)
///   * network-level errors (connection refused, DNS failure, TLS error)
///   * request timeouts
///
/// `4xx` responses are **not** retried. A 400/401/403/429 from one
/// provider would fail identically on another for the same payload
/// (or, for auth errors, indicates a per-provider config bug that
/// retrying only masks). Returning the original error to the client
/// is both faster and more honest.
fn is_retryable_failure(result: &Result<UpstreamResponse, ServerError>) -> bool {
    match result {
        Err(ServerError::UpstreamStatus(status, _)) => *status >= 500,
        Err(ServerError::Upstream(_)) => true, // network / timeout (no status)
        Err(ServerError::Internal(_)) => false, // upstream-not-configured — config bug
        Err(_) => false,                       // BadRequest etc. — caller error
        Ok(resp) => resp.status >= 500,
    }
}

/// Stream-specific retryability. `send_streaming` returns
/// `TranslateError` (not `ServerError`), and the status is extracted
/// from the error message via `upstream_status()`. Only 5xx and
/// network-level failures (status = `None`) are retryable — a 4xx
/// indicates a payload/auth problem that will fail identically on
/// another provider.
fn is_retryable_stream_error(e: &autorouter_translate::TranslateError) -> bool {
    match e.upstream_status() {
        Some(status) => status >= 500,
        None => true, // network error / connection refused / timeout
    }
}

/// Run the configured smart router. The router sees the source
/// provider, request metadata, and the parsed universal request and
/// returns a target. The gateway overlays `X-AutoRouter-Target` on
/// top of the router's choice.
///
/// On **hard upstream failure** (5xx / network error / timeout) the
/// gateway attempts recovery in two stages before returning the
/// original error to the client:
///
///   1. **Provider failover** — retry the same model on other
///      enabled, allowlisted providers.
///   2. **Similar-model failover** — consult the model DB for a
///      comparably-intelligent, comparably-priced model on an
///      enabled provider.
///
/// `4xx` responses are returned immediately — they indicate a
/// payload or auth problem that retrying cannot fix.
///
/// When the caller sends `X-AutoRouter-Target`, the override
/// short-circuits all failover: the operator explicitly chose this
/// provider, so the gateway respects that even on failure.
async fn call_upstream_with_failover(
    state: &AppState,
    source: ProviderKind,
    request_ctx: &RequestContext,
    request: &UniversalRequest,
    headers: &HeaderMap,
    pin: Option<ProviderKind>,
) -> Result<(UpstreamResponse, RouteDecision), ServerError> {
    // Compute the target override ONCE. It's a pure function of
    // (headers, config) so re-computing it below was wasted work
    // that also re-acquired the config read lock.
    let target_override_opt = target_override(state, headers);
    let has_override = target_override_opt.is_some();

    let mut decision = decide_route(state, source, request_ctx, request);
    apply_target_override(state, &mut decision, target_override_opt);
    finalise_target_model(state, &mut decision, request)?;
    // Pin the target to a protocol-native provider when the caller is
    // speaking that protocol over its dedicated route (e.g. Gemini over
    // `/v1beta/models/*`, Anthropic over `/v1/messages`). This prevents
    // the smart router from matching the model id against a custom
    // provider's catalog and rerouting a Gemini-shaped request to an
    // OpenAI-compatible endpoint (which would 404/503 on the model).
    // An explicit X-AutoRouter-Target header still wins over the pin.
    if let Some(p) = pin {
        if !has_override {
            decision.target_provider = p;
            decision.custom_target = None;
            decision.reason = std::borrow::Cow::Borrowed("protocol-native-pin");
        }
    }

    // --- Primary attempt ------------------------------------------------
    let upstream_body = serialise_for_target(state, &decision, request)?;
    let first_attempt = call_upstream(state, &decision, &upstream_body, request_ctx).await;

    if let Ok(ref resp) = first_attempt {
        if resp.status < 400 {
            return Ok((resp.clone(), decision));
        }
    }

    // If the operator pinned a target via X-AutoRouter-Target, respect
    // it — no failover. They asked for THIS provider specifically.
    if has_override {
        tracing::info!(
            "Target override header present; bypassing failover (operator pinned target)"
        );
        return first_attempt.map(|resp| (resp, decision));
    }

    // Only retry on hard failures (5xx, network, timeout). A 4xx is a
    // client/auth/content-filter error — retrying it against another
    // provider wastes tokens and masks the real problem.
    if !is_retryable_failure(&first_attempt) {
        return first_attempt.map(|resp| (resp, decision));
    }

    let err_msg = match &first_attempt {
        Err(e) => e.to_string(),
        Ok(resp) => format!("HTTP status {}", resp.status),
    };
    tracing::warn!(
        model = %decision.target_model,
        provider = %decision.target_provider,
        error = %err_msg,
        "Primary upstream failed with a retryable error; attempting provider failover"
    );

    // --- Failover stage 1: other providers for the SAME model -----------
    let other_providers = find_other_providers_for_model(
        state,
        decision.target_provider,
        decision.custom_target.as_deref(),
        &decision.target_model,
    );
    for (prov, custom_target) in other_providers {
        let mut retry_decision = decision.clone();
        retry_decision.target_provider = prov;
        retry_decision.custom_target = custom_target;

        let Ok(upstream_body) = serialise_for_target(state, &retry_decision, request) else {
            continue;
        };
        let attempt = call_upstream(state, &retry_decision, &upstream_body, request_ctx).await;
        if let Ok(ref resp) = attempt {
            if resp.status < 400 {
                tracing::info!(
                    model = %retry_decision.target_model,
                    provider = %retry_decision.target_provider,
                    "Provider failover succeeded"
                );
                return Ok((resp.clone(), retry_decision));
            }
        }
        tracing::warn!(
            model = %retry_decision.target_model,
            provider = %retry_decision.target_provider,
            error = %match &attempt { Err(e) => e.to_string(), Ok(r) => format!("HTTP {}", r.status) },
            "Provider failover attempt did not succeed"
        );
    }

    // --- Failover stage 2: similar intelligent model --------------------
    if let Some((similar_model, similar_prov)) =
        crate::model_db::find_similar_model_and_provider(state, &decision.target_model)
    {
        let mut retry_decision = decision.clone();
        retry_decision.target_model = similar_model.clone();
        retry_decision.target_provider = similar_prov;

        // Resolve the custom_target for the similar model using
        // canonical id comparison so suffix/case drift doesn't
        // silently kill the failover.
        if similar_prov == ProviderKind::Custom {
            let config = state.config.read();
            if let Some((name, _)) = config.providers.custom.iter().find(|(_, entry)| {
                entry.enabled
                    && (entry.model_allowlist.is_empty()
                        || entry
                            .model_allowlist
                            .iter()
                            .any(|a| crate::model_db::same_model(a, &retry_decision.target_model)))
            }) {
                retry_decision.custom_target = Some(name.clone());
            }
        } else {
            retry_decision.custom_target = None;
        }

        tracing::info!(
            original_model = %decision.target_model,
            retry_model = %retry_decision.target_model,
            retry_provider = %retry_decision.target_provider,
            "Attempting failover to similar intelligent model"
        );

        let Ok(upstream_body) = serialise_for_target(state, &retry_decision, request) else {
            return first_attempt.map(|resp| (resp, decision));
        };
        let attempt = call_upstream(state, &retry_decision, &upstream_body, request_ctx).await;
        if let Ok(ref resp) = attempt {
            if resp.status < 400 {
                tracing::info!(
                    model = %retry_decision.target_model,
                    provider = %retry_decision.target_provider,
                    "Similar-model failover succeeded"
                );
                return Ok((resp.clone(), retry_decision));
            }
        }
        tracing::warn!(
            model = %retry_decision.target_model,
            provider = %retry_decision.target_provider,
            error = %match &attempt { Err(e) => e.to_string(), Ok(r) => format!("HTTP {}", r.status) },
            "Similar-model failover attempt did not succeed"
        );
    }

    // --- Failover stage 3: balance-exhausted safety net ---------------
    // When the primary was the custom (TokenRouter) provider and it
    // failed, retry against OpenRouter free models. OpenRouter is
    // OpenAI-compatible, so the chat/responses wire format is identical
    // to TokenRouter and the caller (e.g. Codex) sees no API change.
    if decision.target_provider == ProviderKind::Custom && !has_override {
        if let Some(result) = try_balance_fallback(state, request, request_ctx, &decision).await {
            return Ok(result);
        }
    }

    // All failover exhausted — return the original error/response.
    first_attempt.map(|resp| (resp, decision))
}

/// Stage 3 safety net: when the primary was the custom TokenRouter
/// provider and it failed (e.g. balance exhausted / model unavailable),
/// retry against OpenRouter's free models. OpenRouter is OpenAI-compatible,
/// so the chat/responses wire format is identical to TokenRouter and the
/// caller (e.g. Codex) sees no change in API shape. Only triggered when no
/// operator override pinned the target.
fn balance_exhausted_fallbacks() -> &'static [(&'static str, &'static str)] {
    &[
        ("openrouter_free", "openai/gpt-oss-20b:free"),
        ("openrouter_free", "qwen/qwen3-coder:free"),
        ("openrouter_free", "meta-llama/llama-3.3-70b-instruct:free"),
        ("openrouter_free", "deepseek/deepseek-chat-v3.1:free"),
    ]
}

async fn try_balance_fallback(
    state: &AppState,
    request: &UniversalRequest,
    request_ctx: &RequestContext,
    decision: &RouteDecision,
) -> Option<(UpstreamResponse, RouteDecision)> {
    let mut fallback_decision = decision.clone();
    for (custom_name, model) in balance_exhausted_fallbacks() {
        fallback_decision.target_provider = ProviderKind::Custom;
        fallback_decision.custom_target = Some((*custom_name).to_string());
        fallback_decision.target_model = (*model).to_string();
        fallback_decision.reason = std::borrow::Cow::Borrowed("balance-exhausted-openrouter-free");
        let Ok(upstream_body) = serialise_for_target(state, &fallback_decision, request) else {
            continue;
        };
        let attempt = call_upstream(state, &fallback_decision, &upstream_body, request_ctx).await;
        if let Ok(ref resp) = attempt {
            if resp.status < 400 {
                tracing::info!(
                    model = %model,
                    provider = %custom_name,
                    "Balance-exhausted failover to OpenRouter free model succeeded"
                );
                return Some((resp.clone(), fallback_decision));
            }
        }
        tracing::warn!(
            model = %model,
            provider = %custom_name,
            error = %match &attempt { Err(e) => e.to_string(), Ok(r) => format!("HTTP {}", r.status) },
            "Balance-exhausted failover attempt did not succeed"
        );
    }
    None
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
        &provider_label(decision),
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
                &provider_label(decision),
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
                &provider_label(decision),
                model,
                "input",
                resp.response.usage.tokens.input.unwrap_or(0),
            );
            autorouter_observability::record_tokens(
                &provider_label(decision),
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
                &provider_label(decision),
                model,
                "error",
                start.elapsed().as_secs_f64(),
            );
            state
                .health
                .record_for_model(decision.target_provider, model, false, latency_ms);
            record_failure(
                &request_ctx.source_provider.to_string(),
                &provider_label(decision),
                "upstream_error",
            );
            // Record rate-limit hits (HTTP 429) so the dashboard can
            // visualise upstream throttling separately from generic
            // failures. Check via the typed upstream_status method
            // rather than substring matching to avoid false positives
            // (e.g. a model name or request id containing "429").
            if e.upstream_status() == Some(429) {
                autorouter_observability::record_rate_limit_hit(&provider_label(decision), model);
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
            Err(match e.upstream_status() {
                Some(status) => ServerError::UpstreamStatus(status, e.to_string()),
                None => ServerError::Upstream(e.to_string()),
            })
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
    // Use constant-time comparison to prevent timing-based token discovery.
    if ct_str_eq(provided.map(str::trim), Some(expected.trim())) {
        Ok(())
    } else {
        Err(ServerError::Unauthorized(
            "invalid or missing bearer token".into(),
        ))
    }
}

/// Constant-time string equality. Returns `true` iff `a == b`.
///
/// Content comparison runs in time proportional to `max(len(a),
/// len(b))` so a timing side-channel cannot reveal a shared prefix of
/// the expected token. Length equality is checked separately so a
/// zero-padded shorter string cannot spuriously match a longer one
/// that ends in NUL bytes.
pub(crate) fn ct_str_eq(a: Option<&str>, b: Option<&str>) -> bool {
    use subtle::ConstantTimeEq;
    match (a, b) {
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
        (Some(a), Some(b)) => {
            let max_len = a.len().max(b.len());
            let mut a_bytes = vec![0u8; max_len];
            let mut b_bytes = vec![0u8; max_len];
            a_bytes[..a.len()].copy_from_slice(a.as_bytes());
            b_bytes[..b.len()].copy_from_slice(b.as_bytes());
            let content_eq: bool = a_bytes.ct_eq(&b_bytes).into();
            content_eq && a.len() == b.len()
        }
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
    // The routing decision owns the target model. When the caller
    // sent the sentinel model id ("autorouter") the upstream body
    // must carry the router-resolved model, not the sentinel — a
    // real provider rejects "autorouter" as an unknown model.
    // `finalise_target_model` (called by every handler before this)
    // guarantees decision.target_model is a concrete model id in
    // that case, so we project it onto the request before encoding.
    let owned;
    let request: &autorouter_core::UniversalRequest =
        if autorouter_core::is_sentinel_model(&request.model)
            && !decision.target_model.is_empty()
            && !autorouter_core::is_sentinel_model(&decision.target_model)
        {
            owned = {
                let mut r = request.clone();
                r.model = decision.target_model.clone();
                r
            };
            &owned
        } else {
            request
        };
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
    tracing::debug!(
        target = %decision.target_provider,
        target_model = %decision.target_model,
        request_model = %request.model,
        tools = request.tools.len(),
        tool_choice = ?request.tool_choice,
        system_msg_count = request.messages.iter().filter(|m| m.role == autorouter_core::MessageRole::System).count(),
        total_msg_count = request.messages.len(),
        "serialise_for_target: forwarding to upstream"
    );
    state
        .pipeline
        .serialise_request(decision.target_provider, request)
        .map_err(|e| ServerError::BadRequest(e.to_string()))
}

/// Finalise the decision's target model after routing. The behavior
/// differs based on the router type:
///
/// **SmartRouter (production with rules):**
/// - If the router returned an empty or sentinel model, it means no
///   routes matched and the router couldn't decide. Resolve to the
///   configured `defaults.default_model` if available; otherwise,
///   this is an error condition (no route to target).
///
/// **IdentityRouter (dev/headless mode):**
/// - Always returns an empty model (pass-through). Fall back to the
///   request's model for normal traffic, then resolve the sentinel to
///   the configured default.
fn finalise_target_model(
    state: &AppState,
    decision: &mut RouteDecision,
    request: &autorouter_core::UniversalRequest,
) -> ServerResult<()> {
    let is_smart = state.router.read().is_smart();

    // If the router didn't pick a model, fall back to the request's model.
    // This is expected for IdentityRouter (pass-through) but indicates
    // a routing gap for SmartRouter.
    if decision.target_model.is_empty() {
        decision.target_model = request.model.clone();
    }

    // Resolve the sentinel model to the configured default.
    // For SmartRouter, this happens after routing; for IdentityRouter,
    // this is the primary resolution mechanism.
    if autorouter_core::is_sentinel_model(&decision.target_model) {
        let default_model = state.config.read().defaults.default_model.clone();
        if !default_model.is_empty() && !autorouter_core::is_sentinel_model(&default_model) {
            decision.target_model = default_model;
        } else if is_smart {
            // SmartRouter with the sentinel and no configured default is
            // an error — the operator explicitly chose "autorouter" but
            // provided no target for it to resolve to.
            return Err(ServerError::BadRequest(
                "Sentinel model 'autorouter' was requested but no default model is configured. \
                 Either configure a default model in settings, or specify a concrete model ID."
                    .into(),
            ));
        }
        // For IdentityRouter (dev mode), silently continue with the
        // sentinel — tests often use this mode with no configured providers.
    }
    Ok(())
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
    let (upstream_resp, final_decision) =
        call_upstream_with_failover(&state, source, &request_ctx, &request, &headers, None).await?;
    let wire = encode_openai_chat_response(&upstream_resp, &final_decision.target_model);
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
    let (upstream_resp, final_decision) =
        call_upstream_with_failover(&state, source, &request_ctx, &request, &headers, None).await?;
    let adapter = OpenAiResponsesAdapter::new();
    let wire = encode_responses_response(&upstream_resp, &adapter, &final_decision.target_model);
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
        return Ok(anthropic_messages_stream_inner(
            state,
            headers,
            body,
            Some(ProviderKind::Anthropic),
        )
        .await?
        .into_response());
    }
    // The route itself defines the wire protocol — parse as Anthropic
    // regardless of any X-AutoRouter-Source header (which only affects routing).
    let source = ProviderKind::Anthropic;
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
    let (upstream_resp, final_decision) = call_upstream_with_failover(
        &state,
        source,
        &request_ctx,
        &request,
        &headers,
        Some(ProviderKind::Anthropic),
    )
    .await?;
    let wire = encode_anthropic_response(&upstream_resp, &final_decision.target_model);
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
        return Ok(gemini_generate_content_stream_inner(
            state,
            headers,
            body,
            path,
            Some(ProviderKind::Gemini),
        )
        .await?
        .into_response());
    }
    // The route itself defines the wire protocol — parse as Gemini
    // regardless of any X-AutoRouter-Source header (which only affects routing).
    let source = ProviderKind::Gemini;
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
    let (upstream_resp, _final_decision) = call_upstream_with_failover(
        &state,
        source,
        &request_ctx,
        &request,
        &headers,
        Some(ProviderKind::Gemini),
    )
    .await?;
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

    // Add the "autorouter" sentinel models so editors can discover them
    models.push(json!({
        "id": "autorouter",
        "object": "model",
        "created": 1686935002,
        "owned_by": "autorouter",
        "display_name": "AutoRouter Sentinel",
        "family": autorouter_core::ModelFamily::Chat,
        "provider": autorouter_core::ProviderKind::Custom,
        "context_window": 2000000,
        "max_output_tokens": 8192,
        "supports_tools": true,
        "supports_vision": true,
        "supports_audio": true,
        "supports_streaming": true,
    }));
    models.push(json!({
        "id": "autorouter/autorouter",
        "object": "model",
        "created": 1686935002,
        "owned_by": "autorouter",
        "display_name": "AutoRouter Sentinel (Slash)",
        "family": autorouter_core::ModelFamily::Chat,
        "provider": autorouter_core::ProviderKind::Custom,
        "context_window": 2000000,
        "max_output_tokens": 8192,
        "supports_tools": true,
        "supports_vision": true,
        "supports_audio": true,
        "supports_streaming": true,
    }));

    for adapter in state.pipeline_adapters() {
        for m in adapter.models() {
            models.push(json!({
                "id": m.id,
                "object": "model",
                "created": 1686935002,
                "owned_by": m.provider.to_string(),
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
                "object": "model",
                "created": 1686935002,
                "owned_by": "custom",
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
    Ok(Json(json!({
        "object": "list",
        "data": models,
        "models": models
    })))
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
                        "arguments": match serde_json::to_string(arguments) {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::warn!(error = %e, "failed to serialize tool call arguments to JSON string");
                                String::new()
                            }
                        },
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
                    "call_id": id,
                    "name": name,
                    "arguments": match serde_json::to_string(arguments) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to serialize tool call arguments to JSON string");
                            String::new()
                        }
                    },
                    "status": "completed",
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
    conversation_key: String,
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
        &provider_label(&decision.decision),
        model,
    );
    let start = std::time::Instant::now();
    let primary_provider = decision.decision.target_provider;
    let primary_model = decision.target_model.clone();

    // Establish the upstream stream. On **retryable pre-first-byte
    // failure** (5xx / network error), try provider + similar-model
    // failover — exactly like the non-streaming path. Once bytes are
    // flowing, no retry is possible (it would corrupt the SSE output),
    // so this failover is limited to the connection-establishment
    // phase.
    let mut effective_decision = decision.decision.clone();
    let mut effective_model = decision.target_model.clone();

    let stream = match decision
        .upstream
        .send_streaming(&decision.upstream_body)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            // Record against the same provider label that the
            // non-streaming path uses (i.e. `openrouter` for
            // custom providers, not the generic `custom` bucket).
            let primary_label = provider_label(&decision.decision);
            observe_upstream(
                &primary_label,
                &primary_model,
                "error",
                start.elapsed().as_secs_f64(),
            );
            record_failure(
                &decision.source.to_string(),
                &primary_label,
                "upstream_error",
            );
            if e.upstream_status() == Some(429) {
                autorouter_observability::record_rate_limit_hit(&primary_label, &primary_model);
            }
            state.health.record_for_model(
                primary_provider,
                &primary_model,
                false,
                start.elapsed().as_millis() as u64,
            );
            record_storage_event(
                &state,
                &effective_decision,
                &primary_model,
                start.elapsed().as_millis() as u64,
                502,
                Some(&e.to_string()),
                &autorouter_core::Usage::default(),
            );

            // --- Pre-first-byte failover (mirrors non-streaming path)
            let can_failover = !decision.has_override && is_retryable_stream_error(&e);
            if !can_failover {
                return Err(ServerError::Upstream(e.to_string()));
            }

            tracing::warn!(
                model = %primary_model,
                provider = %primary_provider,
                error = %e,
                "Streaming upstream failed pre-first-byte; attempting failover"
            );

            let mut established: Option<autorouter_translate::UpstreamStream> = None;

            // Stage 1: other providers for the same model
            let other_providers = find_other_providers_for_model(
                &state,
                primary_provider,
                effective_decision.custom_target.as_deref(),
                &primary_model,
            );
            for (prov, custom_target) in other_providers {
                let mut retry = effective_decision.clone();
                retry.target_provider = prov;
                retry.custom_target = custom_target.clone();
                let Ok(upstream_body) = serialise_for_target(&state, &retry, &decision.request)
                else {
                    continue;
                };
                let upstream = state.upstream_for(prov).or_else(|| {
                    custom_target
                        .as_deref()
                        .and_then(|n| state.custom_upstream_for(n))
                });
                let Some(upstream) = upstream else { continue };
                match upstream.send_streaming(&upstream_body).await {
                    Ok(s) => {
                        tracing::info!(
                            model = %retry.target_model,
                            provider = %retry.target_provider,
                            "Streaming provider failover succeeded"
                        );
                        effective_decision = retry;
                        effective_model = primary_model.clone();
                        established = Some(s);
                        break;
                    }
                    Err(retry_err) => {
                        tracing::warn!(
                            model = %retry.target_model,
                            provider = %retry.target_provider,
                            error = %retry_err,
                            "Streaming provider failover attempt failed"
                        );
                    }
                }
            }

            // Stage 2: similar intelligent model
            if established.is_none() {
                if let Some((similar_model, similar_prov)) =
                    crate::model_db::find_similar_model_and_provider(&state, &primary_model)
                {
                    let mut retry = effective_decision.clone();
                    retry.target_model = similar_model.clone();
                    retry.target_provider = similar_prov;
                    if similar_prov == ProviderKind::Custom {
                        let config = state.config.read();
                        if let Some((name, _)) =
                            config.providers.custom.iter().find(|(_, entry)| {
                                entry.enabled
                                    && (entry.model_allowlist.is_empty()
                                        || entry.model_allowlist.iter().any(|a| {
                                            crate::model_db::same_model(a, &retry.target_model)
                                        }))
                            })
                        {
                            retry.custom_target = Some(name.clone());
                        }
                    } else {
                        retry.custom_target = None;
                    }
                    let Ok(upstream_body) = serialise_for_target(&state, &retry, &decision.request)
                    else {
                        return Err(ServerError::Upstream(e.to_string()));
                    };
                    let upstream = state.upstream_for(similar_prov).or_else(|| {
                        retry
                            .custom_target
                            .as_deref()
                            .and_then(|n| state.custom_upstream_for(n))
                    });
                    if let Some(upstream) = upstream {
                        match upstream.send_streaming(&upstream_body).await {
                            Ok(s) => {
                                tracing::info!(
                                    original_model = %primary_model,
                                    retry_model = %retry.target_model,
                                    provider = %retry.target_provider,
                                    "Streaming similar-model failover succeeded"
                                );
                                effective_decision = retry.clone();
                                effective_model = retry.target_model.clone();
                                established = Some(s);
                            }
                            Err(retry_err) => {
                                tracing::warn!(
                                    model = %retry.target_model,
                                    provider = %retry.target_provider,
                                    error = %retry_err,
                                    "Streaming similar-model failover failed"
                                );
                            }
                        }
                    }
                }
            }

            // Stage 3: balance-exhausted safety net. When the primary was
            // the custom (TokenRouter) provider and it failed, retry against
            // OpenRouter free models. OpenRouter is OpenAI-compatible, so the
            // wire format is identical to TokenRouter (chat/responses) and the
            // caller is unaffected. Only runs when the operator did not pin an
            // explicit target.
            if established.is_none()
                && !decision.has_override
                && primary_provider == ProviderKind::Custom
            {
                for (fb_target, fb_model) in balance_exhausted_fallbacks() {
                    let mut retry = effective_decision.clone();
                    retry.target_provider = ProviderKind::Custom;
                    retry.custom_target = Some(fb_target.to_string());
                    retry.target_model = fb_model.to_string();
                    retry.reason = std::borrow::Cow::Borrowed("balance-exhausted-openrouter-free");
                    let Ok(upstream_body) = serialise_for_target(&state, &retry, &decision.request)
                    else {
                        continue;
                    };
                    let Some(upstream) = state.custom_upstream_for(fb_target) else {
                        continue;
                    };
                    match upstream.send_streaming(&upstream_body).await {
                        Ok(s) => {
                            tracing::info!(
                                original_model = %primary_model,
                                retry_model = %retry.target_model,
                                provider = %retry.target_provider,
                                custom_target = %fb_target,
                                "Balance-exhausted streaming failover succeeded"
                            );
                            effective_decision = retry.clone();
                            effective_model = retry.target_model.clone();
                            established = Some(s);
                            break;
                        }
                        Err(retry_err) => {
                            tracing::warn!(
                                model = %retry.target_model,
                                provider = %retry.target_provider,
                                custom_target = %fb_target,
                                error = %retry_err,
                                "Balance-exhausted streaming failover attempt did not succeed"
                            );
                        }
                    }
                }
            }

            match established {
                Some(s) => s,
                None => return Err(ServerError::Upstream(e.to_string())),
            }
        }
    };
    let health = state.health.clone();
    let storage = state.storage.clone();
    let target_provider = effective_decision.target_provider;
    // Capture the human-meaningful provider label (`openrouter`,
    // `groq`, ...) instead of the generic `custom` bucket so the
    // Prometheus counters, token gauges, and storage events all
    // surface the operator-chosen provider name.
    let target_provider_label = provider_label(&effective_decision);
    let target_model = effective_model.clone();
    let started = start;
    let tracked = async_stream::stream! {
        let mut first = true;
        let mut last_usage: Option<autorouter_core::Usage> = None;
        let mut s = std::pin::pin!(stream);
        while let Some(item) = s.next().await {
            if first {
                first = false;
                let latency_ms = started.elapsed().as_millis() as u64;
                observe_upstream(&target_provider_label, &target_model, "ok", started.elapsed().as_secs_f64());
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
            &target_provider_label,
            &target_model,
            "input",
            usage.tokens.input.unwrap_or(0),
        );
        record_tokens(
            &target_provider_label,
            &target_model,
            "output",
            usage.tokens.output.unwrap_or(0),
        );
        if let Some(ref storage) = storage {
            let mut event = autorouter_config::ProviderEvent::with_usage(
                target_provider_label.clone(),
                &target_model,
                "request",
                started.elapsed().as_millis() as u64,
                &usage,
            );
            event.status = 200;
            let _ = storage.record_provider_event(&event);
        }
    };
    // Derive a stable per-run key from the FIRST user message. Codex drives an
    // agentic loop as a sequence of separate /v1/responses requests and sends
    // NO stable anchor (no previous_response_id / conversation.id / session
    // header), so every tool turn looks like a brand-new run. Deriving the key
    // from the initial user text lets the per-run loop guard (max_tool_rounds)
    // correlate all turns of one agentic run and catch runaway loops.
    let run_key = {
        use autorouter_core::MessageRole;
        let first_user = decision
            .request
            .messages
            .iter()
            .find(|m| m.role == MessageRole::User)
            .map(|m| m.text())
            .unwrap_or_default();
        if first_user.is_empty() {
            // No user text (e.g. pure system turn) — fall back to the request id
            // so the counter at least persists for this single request.
            decision.request_id.to_string()
        } else {
            // Hash to keep the key compact and stable.
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            first_user.hash(&mut h);
            format!("run-{:x}", h.finish())
        }
    };
    Ok(Sse::new(stream_to_events(
        Box::pin(tracked),
        StreamEventContext {
            source: decision.source,
            openai_format: decision.openai_format,
            idle_timeout: decision.idle_timeout,
            request_id: decision.request_id.to_string(),
            target_model: decision.target_model.clone(),
            conversation_key,
            run_key,
        },
    )))
}

pub async fn openai_chat_stream_inner(
    state: AppState,
    headers: HeaderMap,
    body: Value,
) -> ServerResult<Sse<impl Stream<Item = Result<Event, axum::Error>>>> {
    let decision = prepare_stream_decision(&state, &headers, body, None, None).await?;
    stream_inner_from_plan(state, headers, decision, String::new()).await
}

pub async fn openai_responses_stream_inner(
    state: AppState,
    headers: HeaderMap,
    body: Value,
) -> ServerResult<Sse<impl Stream<Item = Result<Event, axum::Error>>>> {
    // NOTE: The previous code called `clear_loop_for_tool_results` here to
    // reset the loop-guard counters when a tool result arrived. This was
    // added to avoid suppressing transient-error retries (e.g. DNS EAI_AGAIN).
    // However, it had a critical flaw: it reset the counter on EVERY tool
    // result — including no-op results like "Operation cancelled" (directory
    // already exists) — which meant the 8-repeat threshold could never
    // accumulate, and the identical-repeat guard was effectively disabled
    // for any command that completes. The 8-repeat / 120s window is already
    // lenient enough for transient retries (8 DNS failures in 2 minutes =
    // real problem worth stopping). So we no longer clear counters here.

    // Extract a stable conversation key (previous_response_id, falling back to
    // conversation.id) so the cross-turn loop guard can correlate turns.
    let conversation_key = body
        .get("previous_response_id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.get("conversation")
                .and_then(|c| c.get("id"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();
    let decision = prepare_stream_decision_with_format(
        &state,
        &headers,
        OpenAiWireFormat::Responses,
        body,
        None,
        None,
    )
    .await?;
    stream_inner_from_plan(state, headers, decision, conversation_key).await
}

pub async fn anthropic_messages_stream_inner(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    pin: Option<ProviderKind>,
) -> ServerResult<Sse<impl Stream<Item = Result<Event, axum::Error>>>> {
    let decision = prepare_stream_decision(&state, &headers, body, None, pin).await?;
    stream_inner_from_plan(state, headers, decision, String::new()).await
}

pub async fn gemini_generate_content_stream_inner(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    path: String,
    pin: Option<ProviderKind>,
) -> ServerResult<Sse<impl Stream<Item = Result<Event, axum::Error>>>> {
    let decision =
        prepare_stream_decision(&state, &headers, body, Some(path.as_str()), pin).await?;
    stream_inner_from_plan(state, headers, decision, String::new()).await
}

struct StreamPlan {
    source: ProviderKind,
    /// The wire format the consumer expects on the SSE output.
    /// When `OpenAiWireFormat::Responses`, SSE events use the
    /// Responses API shape (`response.output_text.delta` etc.)
    /// instead of the Chat Completions shape.
    openai_format: OpenAiWireFormat,
    target_model: String,
    request_id: RequestId,
    upstream_body: Value,
    upstream: std::sync::Arc<dyn UpstreamClient>,
    idle_timeout: std::time::Duration,
    decision: RouteDecision,
    /// The parsed universal request, retained so the streaming path
    /// can re-serialise for failover targets when `send_streaming`
    /// fails before the first byte reaches the client.
    request: UniversalRequest,
    /// True when the operator sent `X-AutoRouter-Target`. When set,
    /// pre-stream failover is skipped — the operator pinned the
    /// provider and wants the error, not a silent retry.
    has_override: bool,
}

async fn prepare_stream_decision(
    state: &AppState,
    headers: &HeaderMap,
    body: Value,
    gemini_path: Option<&str>,
    pin: Option<ProviderKind>,
) -> ServerResult<StreamPlan> {
    prepare_stream_decision_with_format(
        state,
        headers,
        OpenAiWireFormat::default(),
        body,
        gemini_path,
        pin,
    )
    .await
}

async fn prepare_stream_decision_with_format(
    state: &AppState,
    headers: &HeaderMap,
    openai_format: OpenAiWireFormat,
    body: Value,
    gemini_path: Option<&str>,
    pin: Option<ProviderKind>,
) -> ServerResult<StreamPlan> {
    maybe_authorize(headers, state)?;
    // The route itself defines the wire protocol — do NOT derive the
    // source from a (possibly absent) X-AutoRouter-Source header when a
    // protocol-native pin is supplied by the handler.
    let source = pin.unwrap_or_else(|| source_provider(headers));
    let tags = extract_tag_header(headers);
    let mut request_ctx = RequestContext::new(source, source).with_tags(tags);
    if let Some((target_kind, _)) = target_override(state, headers) {
        request_ctx.target_provider = target_kind;
    }
    let mut request = state
        .pipeline
        .parse_request_with_format(source, openai_format, &body)
        .map_err(|e| ServerError::BadRequest(e.to_string()))?;
    // This handler is only ever invoked when the caller (or
    // `stream_by_default`) wants a streaming response. The decoded
    // universal request defaults `stream` to false when the field is
    // absent, which would make `serialise_for_target` emit a
    // non-streaming upstream body — the upstream then returns a single
    // JSON object that the SSE parser yields nothing for (just `[DONE]`).
    // Force `stream = true` so the upstream actually streams.
    if state
        .config
        .read()
        .defaults
        .stream_by_default
        .unwrap_or(false)
    {
        request.stream = true;
    }
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
    let has_override = target_override(state, headers).is_some();
    let mut decision = decide_route(state, source, &request_ctx, &request);
    apply_target_override(state, &mut decision, target_override(state, headers));
    finalise_target_model(state, &mut decision, &request)?;
    // Pin the target to the protocol-native provider when the caller is
    // speaking that protocol over its dedicated route (e.g. Gemini over
    // `/v1beta/models/*`, Anthropic over `/v1/messages`). An explicit
    // X-AutoRouter-Target header still wins over the pin.
    if let Some(p) = pin {
        if !has_override {
            decision.target_provider = p;
            decision.custom_target = None;
            decision.reason = std::borrow::Cow::Borrowed("protocol-native-pin");
        }
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
        openai_format,
        target_model: decision.target_model.clone(),
        request_id: request_ctx.request_id,
        upstream_body,
        upstream,
        idle_timeout,
        decision,
        request,
        has_override,
    })
}

struct StreamEventContext {
    source: ProviderKind,
    openai_format: OpenAiWireFormat,
    idle_timeout: std::time::Duration,
    request_id: String,
    target_model: String,
    conversation_key: String,
    run_key: String,
}

fn stream_to_events(
    stream: UpstreamStream,
    context: StreamEventContext,
) -> impl Stream<Item = Result<Event, axum::Error>> {
    async_stream::stream! {
        let StreamEventContext {
            source,
            openai_format,
            idle_timeout,
            request_id,
            target_model,
            conversation_key,
            run_key,
        } = context;
        let mut s = stream;
        let mut errored = false;
        let mut responses_state = autorouter_translate::streaming::ResponsesSseState::new(
            request_id.clone(),
            target_model.clone(),
            conversation_key,
        );
        let mut first_chunk = true;
        loop {
            let next = tokio::time::timeout(idle_timeout, s.next()).await;
            match next {
                Ok(Some(Ok(chunk))) => {
                    // For Responses API format, synthesize a Start event if the
                    // upstream adapter didn't emit one (e.g. Chat Completions).
                    if first_chunk && openai_format == OpenAiWireFormat::Responses {
                        first_chunk = false;
                        let has_start = chunk.events.iter().any(|e| matches!(e, StreamEvent::Start { .. }));
                        if !has_start {
                            let synthetic = StreamEvent::Start {
                                id: uuid::Uuid::new_v4().to_string(),
                                model: "unknown".to_string(),
                            };
                            let evs = event_to_sse_multi(&synthetic, source, openai_format, Some(&mut responses_state), &run_key);
                            for ev in evs {
                                yield Ok(ev);
                            }
                        }
                    }
                    for event in &chunk.events {
                        // Log every event from upstream for debugging
                        let event_name = match event {
                            StreamEvent::Start { .. } => "Start",
                            StreamEvent::TextDelta { .. } => "TextDelta",
                            StreamEvent::ToolCallStart { .. } => "ToolCallStart",
                            StreamEvent::ToolCallDelta { .. } => "ToolCallDelta",
                            StreamEvent::ToolCallEnd { .. } => "ToolCallEnd",
                            StreamEvent::Finish { .. } => "Finish",
                            StreamEvent::ReasoningDelta { .. } => "ReasoningDelta",
                            StreamEvent::Error { .. } => "Error",
                            _ => "Other",
                        };
                        tracing::info!(event = event_name, "⬇ UPSTREAM EVENT");
                        let st = &mut responses_state;
                        let evs = event_to_sse_multi(event, source, openai_format, Some(st), &run_key);
                        for ev in evs {
                            yield Ok(ev);
                        }
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
fn event_to_sse_multi(
    event: &StreamEvent,
    source: ProviderKind,
    openai_format: OpenAiWireFormat,
    responses_state: Option<&mut sse_mod::ResponsesSseState>,
    run_key: &str,
) -> Vec<Event> {
    use autorouter_translate::streaming as s;
    let frames = match source {
        ProviderKind::OpenAI | ProviderKind::Custom => {
            if openai_format == OpenAiWireFormat::Responses {
                let state =
                    responses_state.expect("ResponsesSseState required for Responses format");
                s::encode_openai_responses_sse(event, state, run_key)
            } else {
                vec![s::encode_openai_sse(event)]
            }
        }
        ProviderKind::Anthropic => vec![s::encode_anthropic_sse(event)],
        ProviderKind::Gemini => vec![s::encode_gemini_sse(event)],
    };
    // Each frame is a complete SSE message (event: + data: + blank line).
    // Convert each to an axum Event.
    let mut events = Vec::new();
    for frame in frames {
        let mut event_name: Option<String> = None;
        let mut data_payload: Option<String> = None;
        for line in frame.lines() {
            if let Some(name) = line.strip_prefix("event: ") {
                let name = name.trim();
                if !name.is_empty() {
                    event_name = Some(name.to_string());
                }
            } else if let Some(payload) = line.strip_prefix("data: ") {
                data_payload = Some(payload.to_string());
            } else if let Some(payload) = line.strip_prefix("data:") {
                data_payload = Some(payload.to_string());
            }
        }
        if let Some(data) = data_payload {
            let mut ev = Event::default().data(data);
            if let Some(name) = event_name {
                ev = ev.event(name);
            }
            events.push(ev);
        }
    }
    events
}
// An adapter may emit a multi-line SSE frame:

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
