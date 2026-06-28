//! Tests for the metrics registry.

use autorouter_observability::{
    dec_session, inc_session, observe_overhead, observe_translation, observe_upstream,
    record_failure, record_request, render_metrics,
};

#[test]
fn records_and_renders() {
    record_request("openai", "openai", "gpt-5");
    record_request("openai", "anthropic", "claude-sonnet-4-5");
    record_failure("openai", "openai", "validation");
    observe_translation("in", "openai", 0.0012);
    observe_translation("out", "openai", 0.0008);
    observe_upstream("openai", "gpt-5", "ok", 0.45);
    observe_upstream("openai", "gpt-5", "error", 0.51);
    observe_overhead("openai", "openai", 0.003);
    inc_session("openai");
    inc_session("openai");
    dec_session("openai");
    let body = render_metrics().unwrap();
    assert!(body.contains("autorouter_requests_total"));
    assert!(body.contains("autorouter_failures_total"));
    assert!(body.contains("autorouter_translation_seconds"));
    assert!(body.contains("autorouter_upstream_seconds"));
    assert!(body.contains("autorouter_translation_overhead_seconds"));
    assert!(body.contains("autorouter_active_sessions"));
}
