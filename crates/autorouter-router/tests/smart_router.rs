//! End-to-end tests for the smart router.

use autorouter_core::{Message, ProviderKind, RequestContext, UniversalRequest};
use autorouter_router::{
    CapabilityRegistry, HealthTracker, ModelCapability, RouteTarget, Router, RoutingContext,
    RoutingRule, RuleEngine, SmartRouter,
};

fn empty() -> UniversalRequest {
    UniversalRequest {
        model: String::new(),
        system: None,
        messages: Vec::new(),
        tool_choice: None,
        metadata: serde_json::Value::Null,
        tools: Vec::new(),
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        stop: Vec::new(),
        stream: false,
        extra: serde_json::Value::Null,
        user: None,
        prior_usage: Default::default(),
        ..Default::default()
    }
}

fn fixture() -> SmartRouter {
    let mut rules = RuleEngine::with_default(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    rules.add_rule(RoutingRule::for_tags(
        vec!["reasoning".into()],
        RouteTarget {
            provider: ProviderKind::OpenAI,
            model: "o3".into(),
            headers: Default::default(),
        },
    ));
    rules.add_rule(RoutingRule::for_tags(
        vec!["long-context".into()],
        RouteTarget {
            provider: ProviderKind::Gemini,
            model: "gemini-2.5-pro".into(),
            headers: Default::default(),
        },
    ));
    let mut reg = CapabilityRegistry::new();
    reg.register(ModelCapability {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        context_window: 400_000,
        max_output_tokens: 16_384,
        supports_tools: true,
        supports_vision: true,
        supports_audio: false,
        supports_streaming: true,
        input_price_per_million: Some(5.0),
        output_price_per_million: Some(15.0),
        is_free: false,
    });
    reg.register(ModelCapability {
        provider: ProviderKind::Gemini,
        model: "gemini-2.5-pro".into(),
        context_window: 1_000_000,
        max_output_tokens: 64_000,
        supports_tools: true,
        supports_vision: true,
        supports_audio: true,
        supports_streaming: true,
        input_price_per_million: Some(2.0),
        output_price_per_million: Some(8.0),
        is_free: false,
    });
    SmartRouter::new(rules, reg, HealthTracker::new()).with_min_health(0.6)
}

#[test]
fn routes_by_tag() {
    let router = fixture();
    let ctx = RoutingContext::new(
        UniversalRequest {
            model: "gpt-5".into(),
            messages: vec![Message::user("think hard")],
            ..empty()
        },
        RequestContext::new(ProviderKind::Anthropic, ProviderKind::Anthropic),
    )
    .with_tags(vec!["reasoning".into()]);
    let decision = router.decide(&ctx, &ctx.request).unwrap();
    assert_eq!(decision.target_model, "o3");
}

#[test]
fn long_context_routes_to_gemini() {
    let router = fixture();
    let ctx = RoutingContext::new(
        UniversalRequest {
            model: "gpt-5".into(),
            messages: vec![Message::user("hi")],
            ..empty()
        },
        RequestContext::new(ProviderKind::OpenAI, ProviderKind::OpenAI),
    )
    .with_tags(vec!["long-context".into()]);
    let decision = router.decide(&ctx, &ctx.request).unwrap();
    assert_eq!(decision.target_provider, ProviderKind::Gemini);
    assert_eq!(decision.target_model, "gemini-2.5-pro");
}

#[test]
fn health_fallback_picks_best_capability() {
    let router = fixture();
    // Mark the OpenAI `gpt-5` bucket as consistently failing with
    // Per-model routing — only the targeted bucket must look bad.
    for _ in 0..30 {
        router
            .health()
            .record_for_model(ProviderKind::OpenAI, "gpt-5", false, 4_500);
    }
    // Mark Gemini Pro as healthy.
    for _ in 0..10 {
        router
            .health()
            .record_for_model(ProviderKind::Gemini, "gemini-2.5-pro", true, 800);
    }
    let ctx = RoutingContext::new(
        UniversalRequest {
            model: "gpt-5".into(),
            messages: vec![Message::user("hi")],
            ..empty()
        },
        RequestContext::new(ProviderKind::OpenAI, ProviderKind::OpenAI),
    );
    let decision = router.decide(&ctx, &ctx.request).unwrap();
    assert_eq!(decision.target_provider, ProviderKind::Gemini);
    assert_eq!(decision.reason, "health-fallback");
}

#[test]
fn m6_fallback_chain() {
    // M6: a rule with a multi-target fallback chain must walk
    // through it when the primary target is unhealthy.
    let mut rules = RuleEngine::new();
    rules.add_rule(RoutingRule {
        name: "try-groq-then-together".to_string(),
        priority: 10,
        match_tags_any: vec!["fast".to_string()],
        match_tags_all: vec![],
        match_model_contains: vec![],
        needs: autorouter_router::CapabilityNeeds::default(),
        when: None,
        prefer_free: false,
        match_latency_below_ms: None,
        match_cost_below_per_million: None,
        match_quota_below_pct: None,
        match_benchmark_above: None,
        max_context_tokens: None,
        when_multimodal: Default::default(),
        targets: vec![
            RouteTarget {
                provider: ProviderKind::OpenAI,
                model: "gpt-primary".to_string(),
                headers: Default::default(),
            },
            RouteTarget {
                provider: ProviderKind::Anthropic,
                model: "claude-fallback".to_string(),
                headers: Default::default(),
            },
        ],
        target: RouteTarget {
            provider: ProviderKind::Gemini,
            model: "gemini-fallback".to_string(),
            headers: Default::default(),
        },
        reason: "fallback chain".to_string(),
    });
    let health = HealthTracker::new();
    let cap = CapabilityRegistry::default();
    let router = SmartRouter::new(rules, cap, health).with_min_health(0.5);

    // Mark Gemini AND OpenAI (their specific rule model ids) as
    // unhealthy by recording failures. The remaining fallback
    // (Anthropic) is the only healthy target and must win.
    // Per-model health — hit the actual model ids the router consults.
    let health = router.health();
    for _ in 0..10 {
        health.record_for_model(ProviderKind::Gemini, "gemini-fallback", false, 1_000);
        health.record_for_model(ProviderKind::OpenAI, "gpt-primary", false, 1_000);
    }
    let req = empty();
    let ctx = RoutingContext::new(
        req.clone(),
        autorouter_core::RequestContext::new(ProviderKind::OpenAI, ProviderKind::OpenAI),
    )
    .with_tags(vec!["fast".to_string()]);
    let decision = router.decide(&ctx, &req).expect("decision");
    // The primary target is the Gemini one, but the first entry of
    // `targets` is OpenAI. With a zero-health tracker, OpenAI is
    // unhealthy and we should walk to the Anthropic fallback.
    assert_eq!(decision.target_provider, ProviderKind::Anthropic);
    assert_eq!(decision.target_model, "claude-fallback");
}
