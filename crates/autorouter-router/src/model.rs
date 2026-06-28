//! Routing context, rules, and capability registry.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use autorouter_core::{ProviderKind, RequestContext, UniversalRequest};

/// Per-request routing context. Carries the parsed request and the
/// original source-provider hint.
#[derive(Debug, Clone)]
pub struct RoutingContext {
    pub request: UniversalRequest,
    pub source_provider: ProviderKind,
    pub request_meta: RequestContext,
    /// Optional tags the caller attached to the request. The smart
    /// router uses these to match rules.
    pub tags: Vec<String>,
}

impl RoutingContext {
    pub fn new(request: UniversalRequest, ctx: RequestContext) -> Self {
        let source_provider = ctx.source_provider;
        Self {
            request,
            source_provider,
            request_meta: ctx,
            tags: Vec::new(),
        }
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// M12: merge a list of default tags into the routing context
    /// without overwriting any tags the caller already set. Useful
    /// for the global `routing.default_tags` config.
    pub fn with_default_tags(mut self, defaults: &[String]) -> Self {
        for d in defaults {
            if !self.tags.iter().any(|t| t == d) {
                self.tags.push(d.clone());
            }
        }
        self
    }
}

/// Convenience matcher for capability-based rules.
pub struct CapabilityMatcher;

impl CapabilityMatcher {
    pub fn request_needs_vision(request: &UniversalRequest) -> bool {
        use autorouter_core::ContentPart;
        request.messages.iter().any(|m| {
            m.content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. }))
        })
    }
    pub fn request_needs_tools(request: &UniversalRequest) -> bool {
        !request.tools.is_empty()
            || request.messages.iter().any(|m| {
                m.content.iter().any(|p| {
                    matches!(
                        p,
                        autorouter_core::ContentPart::ToolCall { .. }
                            | autorouter_core::ContentPart::ToolCallRaw { .. }
                            | autorouter_core::ContentPart::ToolResult { .. }
                            | autorouter_core::ContentPart::ToolUse { .. }
                    )
                })
            })
    }
    pub fn request_needs_audio(request: &UniversalRequest) -> bool {
        use autorouter_core::ContentPart;
        request.messages.iter().any(|m| {
            m.content
                .iter()
                .any(|p| matches!(p, ContentPart::Audio { .. }))
        })
    }
    /// Approximate input token count from the message text. Used by
    /// the long-context rule.
    pub fn approx_input_tokens(request: &UniversalRequest) -> u32 {
        // ~4 chars per token is the standard rule of thumb.
        let chars: usize = request.messages.iter().map(|m| m.text().len()).sum();
        (chars / 4) as u32
    }
}

/// Static capability entry. The capability registry owns one of these
/// per (provider, model id) pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCapability {
    pub provider: ProviderKind,
    pub model: String,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_audio: bool,
    pub supports_streaming: bool,
    /// USD per million input tokens. `None` means the model is free.
    pub input_price_per_million: Option<f64>,
    /// USD per million output tokens.
    pub output_price_per_million: Option<f64>,
    /// Free-tier model (e.g. Gemini Flash). The router uses this flag
    /// when a rule says `kind = free`.
    pub is_free: bool,
}

impl ModelCapability {
    /// Score for capability routing. Higher is better.
    pub fn capability_score(&self, needs: &CapabilityNeeds) -> i32 {
        let mut score = 0;
        if needs.vision && self.supports_vision {
            score += 5;
        }
        if needs.audio && self.supports_audio {
            score += 5;
        }
        if needs.tools && self.supports_tools {
            score += 5;
        }
        if self.context_window >= needs.min_context {
            score += 2;
        }
        score
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultimodalNeeds {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub pdf: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityNeeds {
    pub vision: bool,
    pub audio: bool,
    pub tools: bool,
    pub min_context: u32,
}

/// M21: legacy/manual.md matcher shape. Field names match the
/// user-facing schema (e.g. `needs_tools`) so configs written
/// from `manual.md §8` examples parse without translation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyCapabilityNeeds {
    #[serde(default)]
    pub needs_tools: bool,
    #[serde(default)]
    pub needs_vision: bool,
    #[serde(default)]
    pub needs_audio: bool,
    #[serde(default)]
    pub approx_input_tokens_gt: Option<u32>,
}

impl From<LegacyCapabilityNeeds> for CapabilityNeeds {
    fn from(l: LegacyCapabilityNeeds) -> Self {
        Self {
            vision: l.needs_vision,
            audio: l.needs_audio,
            tools: l.needs_tools,
            min_context: l.approx_input_tokens_gt.unwrap_or(0),
        }
    }
}

impl CapabilityNeeds {
    pub fn from_request(request: &UniversalRequest) -> Self {
        Self {
            vision: CapabilityMatcher::request_needs_vision(request),
            audio: CapabilityMatcher::request_needs_audio(request),
            tools: CapabilityMatcher::request_needs_tools(request),
            min_context: CapabilityMatcher::approx_input_tokens(request),
        }
    }
}

/// In-memory capability registry.
#[derive(Debug, Default, Clone)]
pub struct CapabilityRegistry {
    entries: Vec<ModelCapability>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: Vec<ModelCapability>) -> Self {
        Self { entries }
    }

    pub fn register(&mut self, entry: ModelCapability) {
        // Replace any prior entry for the same (provider, model).
        self.entries
            .retain(|e| !(e.provider == entry.provider && e.model == entry.model));
        self.entries.push(entry);
    }

    pub fn get(&self, provider: ProviderKind, model: &str) -> Option<&ModelCapability> {
        self.entries
            .iter()
            .find(|e| e.provider == provider && e.model == model)
    }

    pub fn by_provider(&self, provider: ProviderKind) -> Vec<&ModelCapability> {
        self.entries
            .iter()
            .filter(|e| e.provider == provider)
            .collect()
    }

    /// All known models, across providers.
    pub fn all(&self) -> &[ModelCapability] {
        &self.entries
    }

    /// Best matches for the given capability needs. `prefer_free` is
    /// a tie-breaker.
    pub fn best_match(
        &self,
        needs: &CapabilityNeeds,
        prefer_free: bool,
    ) -> Option<&ModelCapability> {
        self.entries.iter().max_by_key(|entry| {
            let mut score = entry.capability_score(needs);
            if prefer_free && entry.is_free {
                score += 1;
            }
            if !entry.supports_streaming {
                // Streaming-capable models win over non-streaming ones.
                score -= 1;
            }
            score
        })
    }
}

/// Routing rule. The engine evaluates rules in priority order and the
/// first matching rule wins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingRule {
    pub name: String,
    /// Lower number = higher priority. The default rule has priority
    /// `100`; user-defined rules should be `0..99`.
    #[serde(default = "default_priority")]
    pub priority: i32,
    /// Match when the request tags include any of these values.
    #[serde(default)]
    pub match_tags_any: Vec<String>,
    /// Match when the request tags include all of these values.
    #[serde(default)]
    pub match_tags_all: Vec<String>,
    /// Match when the model name contains one of these substrings.
    #[serde(default)]
    pub match_model_contains: Vec<String>,
    /// Match when the request needs one of these capabilities.
    #[serde(default)]
    pub needs: CapabilityNeeds,
    /// M21: legacy manual.md schema (`when: { needs_tools: true }`).
    /// Folded into `needs` at evaluation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<LegacyCapabilityNeeds>,
    /// Match when the request is a "free-tier preferred" call.
    #[serde(default)]
    pub prefer_free: bool,
    #[serde(default)]
    pub match_latency_below_ms: Option<u64>,
    #[serde(default)]
    pub match_cost_below_per_million: Option<f64>,
    #[serde(default)]
    pub match_quota_below_pct: Option<f32>,
    #[serde(default)]
    pub match_benchmark_above: Option<f32>,
    #[serde(default)]
    pub max_context_tokens: Option<u32>,
    #[serde(default)]
    pub when_multimodal: MultimodalNeeds,
    #[serde(default)]
    pub targets: Vec<RouteTarget>,
    /// Target provider and model when the rule fires.
    pub target: RouteTarget,
    /// Human-readable rationale surfaced in logs.
    #[serde(default)]
    pub reason: String,
}

fn default_priority() -> i32 {
    50
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteTarget {
    pub provider: ProviderKind,
    pub model: String,
    /// Optional override headers, e.g. to use a different key.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl RoutingRule {
    /// Build the catch-all default rule used when no other rule
    /// matches.
    pub fn default_rule(target: RouteTarget) -> Self {
        Self {
            name: "default".into(),
            priority: 100,
            match_tags_any: Vec::new(),
            match_tags_all: Vec::new(),
            match_model_contains: Vec::new(),
            needs: CapabilityNeeds::default(),
            prefer_free: false,
            match_latency_below_ms: None,
            match_cost_below_per_million: None,
            match_quota_below_pct: None,
            match_benchmark_above: None,
            max_context_tokens: None,
            when_multimodal: MultimodalNeeds::default(),
            targets: Vec::new(),
            target,
            when: None,
            reason: "default fallback".into(),
        }
    }

    /// Convenience: build a rule that routes `tags` to the given target.
    pub fn for_tags(tags: Vec<String>, target: RouteTarget) -> Self {
        Self {
            name: format!("tags:{}", tags.join("|")),
            priority: 10,
            match_tags_any: tags,
            when: None,
            ..Self::default_rule(target)
        }
    }

    /// Convenience: build a capability-based rule.
    pub fn for_capability(needs: CapabilityNeeds, target: RouteTarget) -> Self {
        let name = format!(
            "needs:vision={},audio={},tools={},min_ctx={}",
            needs.vision, needs.audio, needs.tools, needs.min_context
        );
        Self {
            name,
            priority: 20,
            needs,
            when: None,
            ..Self::default_rule(target)
        }
    }

    /// Convenience: build a free-tier rule.
    pub fn for_free(target: RouteTarget) -> Self {
        Self {
            name: "free".into(),
            priority: 5,
            prefer_free: true,
            when: None,
            ..Self::default_rule(target)
        }
    }

    pub fn matches(&self, ctx: &RoutingContext) -> bool {
        // All non-empty match conditions must hold.
        if !self.match_tags_any.is_empty() {
            let hit = self
                .match_tags_any
                .iter()
                .any(|t| ctx.tags.iter().any(|c| c == t));
            if !hit {
                return false;
            }
        }
        if !self.match_tags_all.is_empty() {
            let ok = self
                .match_tags_all
                .iter()
                .all(|t| ctx.tags.iter().any(|c| c == t));
            if !ok {
                return false;
            }
        }
        if !self.match_model_contains.is_empty() {
            let model = &ctx.request.model;
            let ok = self.match_model_contains.iter().any(|m| model.contains(m));
            if !ok {
                return false;
            }
        }
        let needs = CapabilityNeeds::from_request(&ctx.request);
        // M21: the legacy `when: { needs_tools, ... }` matcher is a
        // MATCHER, not a hint. The earlier implementation folded
        // the legacy fields into `needs` and then compared against
        // `self.needs` (the modern field), which made the legacy
        // matcher a no-op — every rule with a legacy `when` fired
        // on every request. Apply the legacy match here so configs
        // authored from `manual.md §8` behave as documented.
        if let Some(when) = &self.when {
            if when.needs_tools && !needs.tools {
                return false;
            }
            if when.needs_vision && !needs.vision {
                return false;
            }
            if when.needs_audio && !needs.audio {
                return false;
            }
            if let Some(gt) = when.approx_input_tokens_gt {
                if needs.min_context <= gt {
                    return false;
                }
            }
        }
        if self.needs.vision && !needs.vision {
            return false;
        }
        if self.needs.audio && !needs.audio {
            return false;
        }
        if self.needs.tools && !needs.tools {
            return false;
        }
        if self.needs.min_context > 0 && needs.min_context < self.needs.min_context {
            return false;
        }
        // B7 strategy #1: max_context_tokens — request must fit in the model.
        if let Some(max_ctx) = self.max_context_tokens {
            if needs.min_context > max_ctx {
                return false;
            }
        }
        // B7 strategy #2: prefer_free — match when caller asks for free models.
        if self.prefer_free && !ctx.tags.iter().any(|t| t == "prefer-free") {
            return false;
        }
        // B7 strategy #3: when_multimodal — request must have the required media types.
        let mm = &self.when_multimodal;
        if mm.image && !needs.vision {
            return false;
        }
        if mm.audio && !needs.audio {
            return false;
        }
        if mm.pdf
            && !ctx.request.messages.iter().any(|m| {
                m.content
                    .iter()
                    .any(|p| matches!(p, autorouter_core::ContentPart::Document { .. }))
            })
        {
            return false;
        }
        // B7 strategies #4-#6 (latency, cost, quota, benchmark) require
        // runtime telemetry; the matches() check accepts the rule and
        // the SmartRouter::decide() applies the threshold during the
        // fallback-chain walk.
        true
    }

    /// True if the rule carries any runtime-threshold match clause
    /// (latency / cost / quota / benchmark). The router applies these
    /// against live telemetry before accepting the rule.
    pub fn has_runtime_thresholds(&self) -> bool {
        self.match_latency_below_ms.is_some()
            || self.match_cost_below_per_million.is_some()
            || self.match_quota_below_pct.is_some()
            || self.match_benchmark_above.is_some()
    }

    /// True if a given telemetry snapshot satisfies this rule's runtime
    /// thresholds.
    pub fn passes_runtime_thresholds(
        &self,
        latency_ms: Option<u64>,
        cost_per_million: Option<f64>,
        quota_pct: Option<f32>,
        benchmark_score: Option<f32>,
    ) -> bool {
        if let Some(threshold) = self.match_latency_below_ms {
            match latency_ms {
                Some(actual) if actual <= threshold => {}
                Some(_) => return false,
                None => return false,
            }
        }
        if let Some(threshold) = self.match_cost_below_per_million {
            match cost_per_million {
                Some(actual) if actual <= threshold => {}
                Some(_) => return false,
                None => return false,
            }
        }
        if let Some(threshold) = self.match_quota_below_pct {
            match quota_pct {
                Some(actual) if actual < threshold => {}
                Some(_) => return false,
                None => return false,
            }
        }
        if let Some(threshold) = self.match_benchmark_above {
            match benchmark_score {
                Some(actual) if actual > threshold => {}
                Some(_) => return false,
                None => return false,
            }
        }
        true
    }
}
