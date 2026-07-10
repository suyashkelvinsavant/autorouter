//! Tests for the rule engine and capability registry.

use autorouter_core::{Message, ProviderKind, RequestContext, UniversalRequest};
use autorouter_router::{
    CapabilityNeeds, CapabilityRegistry, HealthTracker, ModelCapability, RouteTarget,
    RoutingContext, RoutingRule, RuleEngine,
};

fn empty_request(model: &str) -> UniversalRequest {
    UniversalRequest {
        model: model.to_string(),
        messages: vec![Message::user("hi")],
        ..empty()
    }
}

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

fn ctx_for(request: UniversalRequest, source: ProviderKind, tags: Vec<String>) -> RoutingContext {
    let req_ctx = RequestContext::new(source, source);
    RoutingContext::new(request, req_ctx).with_tags(tags)
}

#[test]
fn rule_for_tags_matches_when_tag_present() {
    let mut engine = RuleEngine::with_default(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    engine.add_rule(RoutingRule::for_tags(
        vec!["code".into()],
        RouteTarget {
            provider: ProviderKind::Anthropic,
            model: "claude-sonnet-4-5".into(),
            headers: Default::default(),
        },
    ));
    let decision = engine
        .evaluate(&ctx_for(
            empty_request("gpt-5"),
            ProviderKind::OpenAI,
            vec!["code".into()],
        ))
        .unwrap();
    assert_eq!(decision.target_provider, ProviderKind::Anthropic);
    assert_eq!(decision.target_model, "claude-sonnet-4-5");
}

#[test]
fn rule_priority_picks_highest() {
    let mut engine = RuleEngine::with_default(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    let mut low = RoutingRule::for_tags(
        vec!["any".into()],
        RouteTarget {
            provider: ProviderKind::Anthropic,
            model: "claude-sonnet-4-5".into(),
            headers: Default::default(),
        },
    );
    low.priority = 90;
    let mut high = RoutingRule::for_tags(
        vec!["any".into()],
        RouteTarget {
            provider: ProviderKind::Gemini,
            model: "gemini-2.5-pro".into(),
            headers: Default::default(),
        },
    );
    high.priority = 1;
    engine.add_rule(low);
    engine.add_rule(high);
    let decision = engine
        .evaluate(&ctx_for(
            empty_request("gpt-5"),
            ProviderKind::OpenAI,
            vec!["any".into()],
        ))
        .unwrap();
    assert_eq!(decision.target_provider, ProviderKind::Gemini);
}

#[test]
fn free_rule_routes_to_free_model() {
    let mut engine = RuleEngine::with_default(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    engine.add_rule(RoutingRule::for_free(RouteTarget {
        provider: ProviderKind::Gemini,
        model: "gemini-2.5-flash".into(),
        headers: Default::default(),
    }));
    // The free rule matches any request and beats the default.
    let decision = engine
        .evaluate(&ctx_for(
            empty_request("gpt-5"),
            ProviderKind::OpenAI,
            vec!["prefer-free".into()],
        ))
        .unwrap();
    assert_eq!(decision.target_provider, ProviderKind::Gemini);
    assert_eq!(decision.target_model, "gemini-2.5-flash");
}

#[test]
fn capability_rule_matches_vision_request() {
    let mut engine = RuleEngine::with_default(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    engine.add_rule(RoutingRule::for_capability(
        CapabilityNeeds {
            vision: true,
            ..Default::default()
        },
        RouteTarget {
            provider: ProviderKind::Gemini,
            model: "gemini-2.5-pro".into(),
            headers: Default::default(),
        },
    ));
    let mut request = empty_request("gpt-5");
    request.messages.push(Message {
        role: autorouter_core::MessageRole::User,
        content: vec![autorouter_core::ContentPart::Image {
            source: autorouter_core::ImageSource::Url {
                url: "https://example.com/a.png".into(),
            },
            detail: None,
        }],
        name: None,
    });
    let decision = engine
        .evaluate(&ctx_for(request, ProviderKind::OpenAI, Vec::new()))
        .unwrap();
    assert_eq!(decision.target_provider, ProviderKind::Gemini);
}

#[test]
fn no_rule_no_default_returns_error() {
    let engine = RuleEngine::new();
    let result = engine.evaluate(&ctx_for(
        empty_request("gpt-5"),
        ProviderKind::OpenAI,
        Vec::new(),
    ));
    assert!(result.is_err());
}

#[test]
fn default_rule_fires_when_no_other_matches() {
    let engine = RuleEngine::with_default(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    let decision = engine
        .evaluate(&ctx_for(
            empty_request("gpt-5"),
            ProviderKind::OpenAI,
            Vec::new(),
        ))
        .unwrap();
    assert_eq!(decision.target_provider, ProviderKind::OpenAI);
    assert_eq!(decision.reason, "default");
}

#[test]
fn capability_registry_best_match() {
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
    reg.register(ModelCapability {
        provider: ProviderKind::Gemini,
        model: "gemini-2.5-flash".into(),
        context_window: 1_000_000,
        max_output_tokens: 64_000,
        supports_tools: true,
        supports_vision: true,
        supports_audio: true,
        supports_streaming: true,
        input_price_per_million: Some(0.0),
        output_price_per_million: Some(0.0),
        is_free: true,
    });
    let needs = CapabilityNeeds {
        audio: true,
        tools: true,
        ..Default::default()
    };
    let best = reg.best_match(&needs, true).unwrap();
    assert_eq!(best.model, "gemini-2.5-flash");
    let best = reg.best_match(&needs, false).unwrap();
    assert!(best.provider == ProviderKind::Gemini);
    // The health tracker compiles for downstream use; ensure the
    // import does not get dead-code eliminated.
    let _ = HealthTracker::new();
}

// --- B7 routing strategies ---

#[test]
fn b7_max_context_rejects_long_request() {
    let mut engine = RuleEngine::new();
    let target = RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    };
    let mut rule = RoutingRule::default_rule(target.clone());
    rule.name = "small-ctx".into();
    rule.priority = 10;
    rule.max_context_tokens = Some(4);
    engine.add_rule(rule);
    let big = "a".repeat(64);
    let mut req = empty_request("gpt-5");
    req.messages.clear();
    req.messages.push(Message::user(&big));
    let decision = engine.evaluate(&ctx_for(req, ProviderKind::OpenAI, Vec::new()));
    assert!(
        decision.is_err(),
        "rule with max_context_tokens=4 should not match a ~16-token request"
    );
}

#[test]
fn b7_multimodal_image_matches_vision_request() {
    let mut engine = RuleEngine::new();
    let target = RouteTarget {
        provider: ProviderKind::Gemini,
        model: "gemini-2.5-pro".into(),
        headers: Default::default(),
    };
    let mut rule = RoutingRule::default_rule(target);
    rule.name = "vision-only".into();
    rule.priority = 10;
    rule.when_multimodal = autorouter_router::MultimodalNeeds {
        image: true,
        ..Default::default()
    };
    engine.add_rule(rule);
    let mut req = empty_request("gpt-5");
    req.messages.push(Message {
        role: autorouter_core::MessageRole::User,
        content: vec![autorouter_core::ContentPart::Image {
            source: autorouter_core::ImageSource::Url {
                url: "https://x".into(),
            },
            detail: None,
        }],
        name: None,
    });
    let decision = engine
        .evaluate(&ctx_for(req, ProviderKind::OpenAI, Vec::new()))
        .unwrap();
    assert_eq!(decision.target_provider, ProviderKind::Gemini);
}

#[test]
fn b7_fallback_chain_walks_when_primary_unhealthy() {
    let mut engine = RuleEngine::new();
    let primary = RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    };
    let fb1 = RouteTarget {
        provider: ProviderKind::Anthropic,
        model: "claude-sonnet-4-5".into(),
        headers: Default::default(),
    };
    let fb2 = RouteTarget {
        provider: ProviderKind::Gemini,
        model: "gemini-2.5-pro".into(),
        headers: Default::default(),
    };
    let mut rule = RoutingRule::default_rule(primary);
    rule.targets = vec![fb1, fb2];
    engine.add_rule(rule);
    let decision = engine
        .evaluate_with_health(
            &ctx_for(empty_request("gpt-5"), ProviderKind::OpenAI, Vec::new()),
            &|p, _| p != ProviderKind::OpenAI,
        )
        .unwrap();
    assert_eq!(decision.target_provider, ProviderKind::Anthropic);
    assert!(decision.reason.contains("fallback"));
    let decision = engine
        .evaluate_with_health(
            &ctx_for(empty_request("gpt-5"), ProviderKind::OpenAI, Vec::new()),
            &|p, _| p == ProviderKind::Gemini,
        )
        .unwrap();
    assert_eq!(decision.target_provider, ProviderKind::Gemini);
}

#[test]
fn b7_runtime_thresholds_latency() {
    let mut rule = RoutingRule::default_rule(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    rule.match_latency_below_ms = Some(100);
    assert!(!rule.passes_runtime_thresholds(Some(200), None, None, None));
    assert!(rule.passes_runtime_thresholds(Some(50), None, None, None));
    assert!(!rule.passes_runtime_thresholds(None, None, None, None));
}

#[test]
fn b7_runtime_thresholds_cost() {
    let mut rule = RoutingRule::default_rule(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    rule.match_cost_below_per_million = Some(5.0);
    assert!(rule.passes_runtime_thresholds(None, Some(3.0), None, None));
    assert!(!rule.passes_runtime_thresholds(None, Some(10.0), None, None));
}

#[test]
fn b7_runtime_thresholds_quota() {
    let mut rule = RoutingRule::default_rule(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    rule.match_quota_below_pct = Some(80.0);
    assert!(rule.passes_runtime_thresholds(None, None, Some(50.0), None));
    assert!(!rule.passes_runtime_thresholds(None, None, Some(95.0), None));
}

#[test]
fn b7_runtime_thresholds_benchmark() {
    let mut rule = RoutingRule::default_rule(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    rule.match_benchmark_above = Some(0.8);
    assert!(rule.passes_runtime_thresholds(None, None, None, Some(0.9)));
    assert!(!rule.passes_runtime_thresholds(None, None, None, Some(0.5)));
}

#[test]
fn m21_legacy_when_clause_matches_request() {
    // M21: the manual.md `when: { needs_tools: true }` schema must
    // be honored as well as the explicit `needs: { tools: true }`.
    use autorouter_core::{ContentPart, Message, MessageRole, ToolDefinition, UniversalRequest};
    use autorouter_core::{ProviderKind, RequestContext};
    use autorouter_router::{RoutingContext, RoutingRule};
    let rule: RoutingRule = serde_json::from_value(serde_json::json!({
        "name": "haiku-tools",
        "priority": 10,
        "when": { "needs_tools": true },
        "target": { "provider": "anthropic", "model": "claude-haiku-4-5" }
    }))
    .expect("rule with when clause must parse");
    let request = UniversalRequest {
        model: "gpt-5".into(),
        messages: vec![Message {
            role: MessageRole::User,
            content: vec![ContentPart::Text { text: "hi".into() }],
            name: None,
        }],
        tools: vec![ToolDefinition {
            name: "lookup".into(),
            description: Some("x".into()),
            parameters: serde_json::json!({}),
            strict: false,
        }],
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        stop: Vec::new(),
        stream: false,
        extra: serde_json::Value::Null,
        user: None,
        prior_usage: Default::default(),
        metadata: Default::default(),
        system: None,
        tool_choice: None,
        ..Default::default()
    };
    let ctx = RoutingContext::new(
        request,
        RequestContext::new(ProviderKind::OpenAI, ProviderKind::OpenAI),
    );
    assert!(
        rule.matches(&ctx),
        "when-clause should fold needs_tools into effective needs"
    );
}

#[test]
fn gap3_runtime_thresholds_block_rule_in_engine() {
    // A rule carrying match_latency_below_ms = 100 must not match
    // when the live latency is 500ms. Verify the engine calls
    // passes_runtime_thresholds.
    let mut engine = RuleEngine::with_default(RouteTarget {
        provider: ProviderKind::Gemini,
        model: "gemini-2.5-pro".into(),
        headers: Default::default(),
    });
    let target = RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    };
    let mut rule = RoutingRule::default_rule(target);
    rule.name = "fast-only".into();
    rule.priority = 10;
    rule.match_latency_below_ms = Some(100);
    engine.add_rule(rule);
    let telemetry =
        |_p: ProviderKind, _m: &str| -> (Option<u64>, Option<f64>, Option<f32>, Option<f32>) {
            // Live latency for gpt-5 is 500ms — the rule says
            // \"match when latency is below 100ms\", so the rule
            // must NOT match.
            (Some(500), None, None, None)
        };
    let result = engine.evaluate_with_telemetry(
        &ctx_for(empty_request("gpt-5"), ProviderKind::OpenAI, Vec::new()),
        &|_, _| true,
        &telemetry,
    );
    let decision = result.expect("default must rescue the request");
    assert_eq!(
        decision.target_provider,
        ProviderKind::Gemini,
        "rule should be skipped because latency exceeds the threshold"
    );
    assert_eq!(decision.reason, "default");
}

#[test]
fn gap3_runtime_thresholds_admit_rule_when_within_budget() {
    // Positive case: when live latency is 50ms and the rule allows
    // under 100ms, the rule must match.
    let mut engine = RuleEngine::new();
    let target = RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    };
    let mut rule = RoutingRule::default_rule(target);
    rule.name = "fast-only".into();
    rule.priority = 10;
    rule.match_latency_below_ms = Some(100);
    engine.add_rule(rule);
    let telemetry =
        |_p: ProviderKind, _m: &str| -> (Option<u64>, Option<f64>, Option<f32>, Option<f32>) {
            (Some(50), None, None, None)
        };
    let result = engine.evaluate_with_telemetry(
        &ctx_for(empty_request("gpt-5"), ProviderKind::OpenAI, Vec::new()),
        &|_, _| true,
        &telemetry,
    );
    let decision = result.unwrap();
    assert_eq!(decision.target_provider, ProviderKind::OpenAI);
    assert_eq!(decision.target_model, "gpt-5");
}

#[test]
fn gap3_cost_threshold_blocks_expensive_model() {
    // A rule with match_cost_below_per_million = 2.0 must not match
    // when the target model costs /M — verify passes_runtime_thresholds.
    use autorouter_router::CapabilityRegistry;
    use autorouter_router::ModelCapability;
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
    let cost = reg
        .get(ProviderKind::OpenAI, "gpt-5")
        .unwrap()
        .input_price_per_million;
    assert_eq!(cost, Some(5.0));
    // Now verify the threshold gate:
    let mut rule = RoutingRule::default_rule(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    rule.match_cost_below_per_million = Some(2.0);
    let passes = rule.passes_runtime_thresholds(None, cost, None, None);
    assert!(!passes, "/M must fail a /M ceiling");
}

#[test]
fn gap3_untracked_quota_safely_blocks_rule() {
    // The engine has no quota telemetry. Returning None must make
    // match_quota_below_pct rules fail (the safe direction).
    let mut rule = RoutingRule::default_rule(RouteTarget {
        provider: ProviderKind::OpenAI,
        model: "gpt-5".into(),
        headers: Default::default(),
    });
    rule.match_quota_below_pct = Some(80.0);
    let passes = rule.passes_runtime_thresholds(None, None, None, None);
    assert!(!passes, "untracked quota must not be assumed satisfied");
}
