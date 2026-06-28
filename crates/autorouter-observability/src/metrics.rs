//! Prometheus metrics. Single static registry, exposed through
//! [`registry`] and the [`record`] helper.

use std::sync::OnceLock;

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, Registry, TextEncoder,
};

use crate::error::{ObservabilityError, ObservabilityResult};

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static REQUESTS: OnceLock<IntCounterVec> = OnceLock::new();
static FAILURES: OnceLock<IntCounterVec> = OnceLock::new();
static TRANSLATION_LATENCY: OnceLock<HistogramVec> = OnceLock::new();
static UPSTREAM_LATENCY: OnceLock<HistogramVec> = OnceLock::new();
static TRANSLATION_OVERHEAD: OnceLock<HistogramVec> = OnceLock::new();
static ACTIVE_SESSIONS: OnceLock<IntGaugeVec> = OnceLock::new();
static TOKENS_TOTAL: OnceLock<IntCounterVec> = OnceLock::new();
static RATE_LIMIT_HITS: OnceLock<IntCounterVec> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::new)
}

fn requests() -> &'static IntCounterVec {
    REQUESTS.get_or_init(|| {
        let v = IntCounterVec::new(
            Opts::new("autorouter_requests_total", "Total requests processed"),
            &["source_provider", "target_provider", "model"],
        )
        .expect("counter");
        registry().register(Box::new(v.clone())).expect("register");
        v
    })
}

fn failures() -> &'static IntCounterVec {
    FAILURES.get_or_init(|| {
        let v = IntCounterVec::new(
            Opts::new("autorouter_failures_total", "Total failed requests"),
            &["source_provider", "target_provider", "reason"],
        )
        .expect("counter");
        registry().register(Box::new(v.clone())).expect("register");
        v
    })
}

fn translation_latency() -> &'static HistogramVec {
    TRANSLATION_LATENCY.get_or_init(|| {
        let v = HistogramVec::new(
            HistogramOpts::new(
                "autorouter_translation_seconds",
                "Time spent translating a single request or response",
            )
            .buckets(vec![
                0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0,
            ]),
            &["direction", "provider"],
        )
        .expect("histogram");
        registry().register(Box::new(v.clone())).expect("register");
        v
    })
}

fn upstream_latency() -> &'static HistogramVec {
    UPSTREAM_LATENCY.get_or_init(|| {
        let v = HistogramVec::new(
            HistogramOpts::new(
                "autorouter_upstream_seconds",
                "Time spent waiting for an upstream provider",
            )
            .buckets(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
            &["provider", "model", "outcome"],
        )
        .expect("histogram");
        registry().register(Box::new(v.clone())).expect("register");
        v
    })
}

fn translation_overhead() -> &'static HistogramVec {
    TRANSLATION_OVERHEAD.get_or_init(|| {
        let v = HistogramVec::new(
            HistogramOpts::new(
                "autorouter_translation_overhead_seconds",
                "Translation overhead per request, excluding upstream latency",
            )
            .buckets(vec![
                0.0001, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1,
            ]),
            &["source_provider", "target_provider"],
        )
        .expect("histogram");
        registry().register(Box::new(v.clone())).expect("register");
        v
    })
}

fn tokens_total() -> &'static IntCounterVec {
    TOKENS_TOTAL.get_or_init(|| {
        let v = IntCounterVec::new(
            Opts::new("autorouter_tokens_total", "Total tokens processed"),
            &["provider", "model", "kind"],
        )
        .expect("counter");
        registry().register(Box::new(v.clone())).expect("register");
        v
    })
}

fn rate_limit_hits() -> &'static IntCounterVec {
    RATE_LIMIT_HITS.get_or_init(|| {
        let v = IntCounterVec::new(
            Opts::new(
                "autorouter_rate_limit_hits_total",
                "Total upstream 429 rate-limit responses",
            ),
            &["provider", "model"],
        )
        .expect("counter");
        registry().register(Box::new(v.clone())).expect("register");
        v
    })
}

fn active_sessions() -> &'static IntGaugeVec {
    ACTIVE_SESSIONS.get_or_init(|| {
        let v = IntGaugeVec::new(
            Opts::new("autorouter_active_sessions", "Active sessions by source"),
            &["source_provider"],
        )
        .expect("gauge");
        registry().register(Box::new(v.clone())).expect("register");
        v
    })
}

/// Record a single request that successfully entered the pipeline.
pub fn record_request(source: &str, target: &str, model: &str) {
    requests().with_label_values(&[source, target, model]).inc();
}

/// Record a failure with a short reason label (e.g. `validation`,
/// `upstream_5xx`, `timeout`).
pub fn record_failure(source: &str, target: &str, reason: &str) {
    failures()
        .with_label_values(&[source, target, reason])
        .inc();
}

/// Observe a translation duration. `direction` is `in` or `out`.
pub fn observe_translation(direction: &str, provider: &str, seconds: f64) {
    translation_latency()
        .with_label_values(&[direction, provider])
        .observe(seconds);
}

/// Observe an upstream call duration. `outcome` is `ok` or `error`.
pub fn observe_upstream(provider: &str, model: &str, outcome: &str, seconds: f64) {
    upstream_latency()
        .with_label_values(&[provider, model, outcome])
        .observe(seconds);
}

/// Observe the translation overhead: total request time minus the
/// upstream portion.
pub fn observe_overhead(source: &str, target: &str, seconds: f64) {
    translation_overhead()
        .with_label_values(&[source, target])
        .observe(seconds);
}

/// Record token usage. `kind` is `input` or `output`.
pub fn record_tokens(provider: &str, model: &str, kind: &str, count: u64) {
    tokens_total()
        .with_label_values(&[provider, model, kind])
        .inc_by(count);
}

/// Record a rate-limit hit (HTTP 429 from an upstream).
pub fn record_rate_limit_hit(provider: &str, model: &str) {
    rate_limit_hits()
        .with_label_values(&[provider, model])
        .inc();
}

/// Increment the active-session gauge.
pub fn inc_session(source: &str) {
    active_sessions().with_label_values(&[source]).inc();
}

/// Decrement the active-session gauge.
pub fn dec_session(source: &str) {
    active_sessions().with_label_values(&[source]).dec();
}

/// Render the current registry as the Prometheus text exposition
/// format. The returned string is suitable to serve on
/// `GET /metrics`.
pub fn render() -> ObservabilityResult<String> {
    let mut buf = Vec::new();
    let encoder = TextEncoder::new();
    let metrics = registry().gather();
    encoder
        .encode(&metrics, &mut buf)
        .map_err(|e| ObservabilityError::Metrics(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| ObservabilityError::Metrics(e.to_string()))
}
