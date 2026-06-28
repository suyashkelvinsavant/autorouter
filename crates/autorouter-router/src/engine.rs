//! Rule engine and combined smart router.

use autorouter_core::{ProviderKind, UniversalRequest};

use crate::decision::{RouteDecision, Router};
use crate::error::{RoutingError, RoutingResult};
use crate::health::HealthTracker;
use crate::model::{CapabilityRegistry, RoutingContext, RoutingRule};

/// The rule engine evaluates rules in priority order. The first rule
/// whose [`RoutingRule::matches`] returns true wins. If no rule
/// matches and no default is configured, the engine returns
/// [`RoutingError::NoRoute`].
pub struct RuleEngine {
    rules: Vec<RoutingRule>,
    default_target: Option<crate::model::RouteTarget>,
}

impl RuleEngine {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            default_target: None,
        }
    }

    pub fn with_default(target: crate::model::RouteTarget) -> Self {
        Self {
            rules: Vec::new(),
            default_target: Some(target),
        }
    }

    pub fn add_rule(&mut self, rule: RoutingRule) {
        self.rules.push(rule);
        // Keep the list sorted by priority ascending.
        self.rules.sort_by_key(|r| r.priority);
    }

    pub fn rules(&self) -> &[RoutingRule] {
        &self.rules
    }

    /// Evaluate the rules and return the first matching primary target.
    pub fn evaluate(&self, ctx: &RoutingContext) -> RoutingResult<RouteDecision> {
        self.evaluate_with_health(ctx, &|_, _| true)
    }

    /// Evaluate rules and walk the per-rule fallback chain. The
    /// is_healthy predicate decides whether a given target is
    /// usable; the first healthy target wins. If the primary
    /// target is unhealthy, the router tries every entry in
    /// targets in order. If nothing in the rule's chain is healthy
    /// the next rule is tried. The default target is consulted last.
    pub fn evaluate_with_health<F>(
        &self,
        ctx: &RoutingContext,
        is_healthy: &F,
    ) -> RoutingResult<RouteDecision>
    where
        F: Fn(ProviderKind, &str) -> bool,
    {
        for rule in &self.rules {
            if !rule.matches(ctx) {
                continue;
            }
            if is_healthy(rule.target.provider, &rule.target.model) {
                return Ok(RouteDecision {
                    target_provider: rule.target.provider,
                    target_model: rule.target.model.clone(),
                    reason: std::borrow::Cow::Owned(rule.name.clone()),
                    custom_target: None,
                });
            }
            for (idx, fb) in rule.targets.iter().enumerate() {
                if is_healthy(fb.provider, &fb.model) {
                    return Ok(RouteDecision {
                        target_provider: fb.provider,
                        target_model: fb.model.clone(),
                        reason: std::borrow::Cow::Owned(format!("{}#fallback[{}]", rule.name, idx)),
                        custom_target: None,
                    });
                }
            }
        }
        if let Some(default) = &self.default_target {
            if is_healthy(default.provider, &default.model) {
                return Ok(RouteDecision {
                    target_provider: default.provider,
                    target_model: default.model.clone(),
                    reason: std::borrow::Cow::Borrowed("default"),
                    custom_target: None,
                });
            }
        }
        Err(RoutingError::NoRoute(
            "no rule matched and no default is configured".into(),
        ))
    }

    /// Like [`evaluate_with_health`] but additionally consults runtime
    /// telemetry against the rule's `match_latency_below_ms`,
    /// `match_cost_below_per_million`, `match_quota_below_pct`, and
    /// `match_benchmark_above` clauses before accepting a target.
    ///
    /// The `telemetry` closure returns `(latency_ms, cost_per_million,
    /// quota_pct, benchmark_score)` for a `(provider, model)` pair.
    /// Any field that isn't tracked can be returned as `None`, in
    /// which case a rule that requires that field will NOT match
    /// (the safe direction — refusing to satisfy an unverifiable
    /// constraint).
    pub fn evaluate_with_telemetry<F, T>(
        &self,
        ctx: &RoutingContext,
        is_healthy: &F,
        telemetry: &T,
    ) -> RoutingResult<RouteDecision>
    where
        F: Fn(ProviderKind, &str) -> bool,
        T: Fn(ProviderKind, &str) -> (Option<u64>, Option<f64>, Option<f32>, Option<f32>),
    {
        let threshold_check = |rule: &RoutingRule, p: ProviderKind, m: &str| -> bool {
            if !rule.has_runtime_thresholds() {
                return true;
            }
            let (lat, cost, quota, bench) = telemetry(p, m);
            rule.passes_runtime_thresholds(lat, cost, quota, bench)
        };
        for rule in &self.rules {
            if !rule.matches(ctx) {
                continue;
            }
            if is_healthy(rule.target.provider, &rule.target.model)
                && threshold_check(rule, rule.target.provider, &rule.target.model)
            {
                return Ok(RouteDecision {
                    target_provider: rule.target.provider,
                    target_model: rule.target.model.clone(),
                    reason: std::borrow::Cow::Owned(rule.name.clone()),
                    custom_target: None,
                });
            }
            for (idx, fb) in rule.targets.iter().enumerate() {
                if is_healthy(fb.provider, &fb.model)
                    && threshold_check(rule, fb.provider, &fb.model)
                {
                    return Ok(RouteDecision {
                        target_provider: fb.provider,
                        target_model: fb.model.clone(),
                        reason: std::borrow::Cow::Owned(format!("{}#fallback[{}]", rule.name, idx)),
                        custom_target: None,
                    });
                }
            }
        }
        if let Some(default) = &self.default_target {
            if is_healthy(default.provider, &default.model)
                && threshold_check_for_default(ctx, telemetry, default)
            {
                return Ok(RouteDecision {
                    target_provider: default.provider,
                    target_model: default.model.clone(),
                    reason: std::borrow::Cow::Borrowed("default"),
                    custom_target: None,
                });
            }
        }
        Err(RoutingError::NoRoute(
            "no rule matched and no default is configured".into(),
        ))
    }
}

/// Default targets don't carry their own `RoutingRule`, so the
/// telemetry check has nothing to validate against. Defaulting to
/// `true` preserves the previous behaviour — the default is the
/// last-resort fallback and not subject to runtime gating. This
/// function is pulled out so the closure on
/// `evaluate_with_telemetry` stays focused on rule-bound targets.
fn threshold_check_for_default<T>(
    _ctx: &RoutingContext,
    _telemetry: &T,
    _target: &crate::model::RouteTarget,
) -> bool
where
    T: Fn(ProviderKind, &str) -> (Option<u64>, Option<f64>, Option<f32>, Option<f32>),
{
    true
}

impl Default for RuleEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// The full smart router. Combines the rule engine, the capability
/// registry, the health tracker, and a manual override map. It
/// implements the [`Router`] trait.
pub struct SmartRouter {
    rules: RuleEngine,
    capabilities: CapabilityRegistry,
    health: HealthTracker,
    min_health: f64,
}

impl SmartRouter {
    pub fn new(rules: RuleEngine, capabilities: CapabilityRegistry, health: HealthTracker) -> Self {
        Self {
            rules,
            capabilities,
            health,
            min_health: 0.0,
        }
    }

    pub fn with_min_health(mut self, min: f64) -> Self {
        self.min_health = min;
        self
    }

    pub fn capabilities(&self) -> &CapabilityRegistry {
        &self.capabilities
    }

    pub fn health(&self) -> &HealthTracker {
        &self.health
    }

    pub fn rules(&self) -> &RuleEngine {
        &self.rules
    }
}

impl Router for SmartRouter {
    fn decide(
        &self,
        ctx: &RoutingContext,
        request: &UniversalRequest,
    ) -> RoutingResult<RouteDecision> {
        // Walk the rule's primary target then its fallback chain,
        // using the per-model health tracker to skip unhealthy
        // upstreams. `is_model_healthy` checks just the
        // (provider, model) bucket rather than the whole provider.
        //
        // The rule engine additionally consults runtime telemetry —
        // latency from the health tracker, cost from the capability
        // registry. Quota/benchmark are currently untracked and
        // resolve to `None` (which safely fails rules that require
        // them).
        let telemetry =
            |p: ProviderKind, m: &str| -> (Option<u64>, Option<f64>, Option<f32>, Option<f32>) {
                let snap = self.health.snapshot_for_model(p, m);
                let latency_ms = if snap.avg_latency_ms.is_finite() && snap.avg_latency_ms > 0.0 {
                    Some(snap.avg_latency_ms as u64)
                } else {
                    None
                };
                let cost_per_million = self
                    .capabilities
                    .get(p, m)
                    .and_then(|cap| cap.input_price_per_million);
                // Quota / benchmark are not tracked yet. Returning
                // `None` here is the safe direction: a rule with
                // `match_quota_below_pct` or `match_benchmark_above`
                // will refuse to match until telemetry for those
                // signals is wired in.
                (latency_ms, cost_per_million, None, None)
            };
        let result = self.rules.evaluate_with_telemetry(
            ctx,
            &|p, m| self.health.is_model_healthy(p, m, self.min_health),
            &telemetry,
        );
        let mut decision = match result {
            Ok(d) => d,
            Err(_) => {
                // No rule matched AND the default target is unhealthy.
                // Fall through to the best capability match as a last resort.
                let needs = crate::model::CapabilityNeeds::from_request(request);
                if let Some(fallback) = self.capabilities.best_match(&needs, false) {
                    if self.health.is_model_healthy(
                        fallback.provider,
                        &fallback.model,
                        self.min_health,
                    ) {
                        return Ok(RouteDecision {
                            target_provider: fallback.provider,
                            target_model: fallback.model.clone(),
                            reason: std::borrow::Cow::Borrowed("health-fallback"),
                            custom_target: None,
                        });
                    }
                }
                return Err(crate::error::RoutingError::NoRoute(
                    "no rule matched, no default available, no healthy capability fallback".into(),
                ));
            }
        };
        // If the rule did not pick a model, fill in from the registry.
        if decision.target_model.is_empty() {
            if let Some(entry) = self
                .capabilities
                .get(decision.target_provider, &request.model)
            {
                decision.target_model = entry.model.clone();
            } else {
                decision.target_model = request.model.clone();
            }
        }
        Ok(decision)
    }
}
