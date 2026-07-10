//! Health tracking for upstream providers.
//!
//! Phase 4 ships an in-memory tracker with sliding windows.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use autorouter_core::ProviderKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSample {
    pub success: bool,
    pub latency_ms: u64,
    /// Unix epoch millis. Stored as `u128` because `Instant` is not
    /// `Serialize`.
    pub timestamp_ms: u128,
}

impl HealthSample {
    pub fn new(success: bool, latency_ms: u64) -> Self {
        Self {
            success,
            latency_ms,
            timestamp_ms: epoch_ms(),
        }
    }

    /// Build a sample with an explicit timestamp. Used by tests (and
    /// recovery tooling) to inject aged observations without waiting
    /// for the wall clock.
    pub fn at(success: bool, latency_ms: u64, timestamp_ms: u128) -> Self {
        Self {
            success,
            latency_ms,
            timestamp_ms,
        }
    }
}

fn epoch_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[derive(Debug, Default, Clone)]
struct HealthWindow {
    samples: VecDeque<HealthSample>,
    max: usize,
    window: Duration,
}

impl HealthWindow {
    fn new(max: usize, window: Duration) -> Self {
        Self {
            samples: VecDeque::with_capacity(max),
            max,
            window,
        }
    }
    fn record(&mut self, sample: HealthSample) {
        // Samples can be restored or injected out of order, so eviction must
        // inspect the whole deque rather than assume insertion order tracks
        // time. Pruning against the current clock also prevents a delayed
        // stale sample from consuming a slot in the bounded window.
        let now_ms = epoch_ms();
        self.evict_expired(now_ms);
        if !Self::is_live(&sample, now_ms, self.window.as_millis()) {
            return;
        }
        if self.samples.len() == self.max {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    /// Drop samples older than the window relative to `now_ms`.
    fn evict_expired(&mut self, now_ms: u128) {
        let window_ms = self.window.as_millis();
        self.samples
            .retain(|sample| Self::is_live(sample, now_ms, window_ms));
    }

    /// `true` when the sample is still inside the sliding window.
    fn is_live(sample: &HealthSample, now_ms: u128, window_ms: u128) -> bool {
        now_ms.saturating_sub(sample.timestamp_ms) <= window_ms
    }

    fn success_rate(&self) -> f64 {
        self.success_rate_at(epoch_ms())
    }

    /// Compute success rate considering only samples whose age is
    /// within the window relative to `now_ms`. Read-path filtering
    /// means health scores decay naturally even when no new traffic
    /// arrives, so a model that failed earlier can recover without
    /// waiting for a write that would have pruned the queue.
    fn success_rate_at(&self, now_ms: u128) -> f64 {
        let window_ms = self.window.as_millis();
        let mut total = 0usize;
        let mut ok = 0usize;
        for s in &self.samples {
            if Self::is_live(s, now_ms, window_ms) {
                total += 1;
                if s.success {
                    ok += 1;
                }
            }
        }
        if total == 0 {
            // All samples expired (or never recorded): treat as healthy.
            1.0
        } else {
            ok as f64 / total as f64
        }
    }

    fn avg_latency_ms(&self) -> f64 {
        self.avg_latency_ms_at(epoch_ms())
    }

    fn avg_latency_ms_at(&self, now_ms: u128) -> f64 {
        let window_ms = self.window.as_millis();
        let mut total = 0usize;
        let mut sum = 0u64;
        for s in &self.samples {
            if Self::is_live(s, now_ms, window_ms) {
                total += 1;
                sum = sum.saturating_add(s.latency_ms);
            }
        }
        if total == 0 {
            0.0
        } else {
            sum as f64 / total as f64
        }
    }

    fn samples(&self) -> usize {
        // Report only live samples so the UI / router see the same
        // evidence the score is computed from.
        let now_ms = epoch_ms();
        let window_ms = self.window.as_millis();
        self.samples
            .iter()
            .filter(|s| Self::is_live(s, now_ms, window_ms))
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub provider: ProviderKind,
    pub samples: usize,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub score: f64,
    /// Per-model breakdown. Always non-empty when the provider
    /// has any samples — at minimum a synthetic `(empty)` bucket
    /// holds samples that were recorded without a model id.
    /// Callers can see which specific model is unhealthy, instead of
    /// treating the whole provider kind as one bucket and routing
    /// every healthy sibling model away from a single bad one.
    #[serde(default)]
    pub models: Vec<ModelHealthSnapshot>,
}

/// Per-model health breakdown. `model == ""` is the bucket for
/// samples recorded without a model id (callers using the legacy
/// `record(provider, success, latency_ms)` API land here).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelHealthSnapshot {
    pub model: String,
    pub samples: usize,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub score: f64,
}

#[derive(Debug, Default, Clone)]
pub struct HealthTracker {
    inner: Arc<RwLock<HashMapInner>>,
}

/// Key into the per-model bucket. `model == ""` is the legacy
/// fallback for traffic that didn't carry a model id.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ProviderModelKey {
    pub provider: ProviderKind,
    pub model: String,
}

#[derive(Debug, Default)]
struct HashMapInner {
    /// Per-(provider, model) windows. The provider-level snapshot
    /// is computed by aggregating all models under a provider.
    /// Per-model buckets isolate failures so one bad model does not
    /// penalise the entire provider kind.
    models: std::collections::HashMap<ProviderModelKey, HealthWindow>,
    config: HealthConfig,
}

#[derive(Debug, Clone)]
pub struct HealthConfig {
    pub window_size: usize,
    pub window_duration: Duration,
    pub latency_weight: f64,
    pub latency_ceiling_ms: f64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            window_size: 64,
            window_duration: Duration::from_secs(60 * 5),
            latency_weight: 0.5,
            latency_ceiling_ms: 5_000.0,
        }
    }
}

impl HealthTracker {
    pub fn new() -> Self {
        Self::with_config(HealthConfig::default())
    }

    pub fn with_config(config: HealthConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMapInner {
                models: std::collections::HashMap::new(),
                config,
            })),
        }
    }

    /// Record a sample for `provider` without attributing it to a
    /// specific model. The sample lands in the legacy
    /// `(provider, "")` bucket. Prefer
    /// [`record_for_model`](Self::record_for_model) at call sites
    /// that know the model id so the per-model breakdown stays
    /// useful.
    pub fn record(&self, provider: ProviderKind, success: bool, latency_ms: u64) {
        self.record_for_model(provider, "", success, latency_ms);
    }

    /// Record a sample for the specific `(provider, model)`
    /// combination. The provider-level snapshot aggregates across
    /// all model buckets; a single bad model no longer penalises
    /// healthy siblings on the same provider.
    pub fn record_for_model(
        &self,
        provider: ProviderKind,
        model: &str,
        success: bool,
        latency_ms: u64,
    ) {
        self.record_sample_for_model(provider, model, HealthSample::new(success, latency_ms));
    }

    /// Record a fully-specified sample (including timestamp). Prefer
    /// [`record_for_model`] at normal call sites; this entry point is
    /// for tests and any path that needs to inject aged observations.
    pub fn record_sample_for_model(
        &self,
        provider: ProviderKind,
        model: &str,
        sample: HealthSample,
    ) {
        let key = ProviderModelKey {
            provider,
            model: model.to_string(),
        };
        let mut guard = self.inner.write();
        let window_size = guard.config.window_size;
        let window_duration = guard.config.window_duration;
        let window = guard
            .models
            .entry(key)
            .or_insert_with(|| HealthWindow::new(window_size, window_duration));
        window.record(sample);
    }

    /// Snapshot for a single `(provider, model)` bucket. Returns
    /// a neutral `score = 1.0` snapshot when no samples exist, so
    /// the smart router never penalises a model that has not been
    /// exercised yet.
    pub fn snapshot_for_model(&self, provider: ProviderKind, model: &str) -> HealthSnapshot {
        let guard = self.inner.read();
        let key = ProviderModelKey {
            provider,
            model: model.to_string(),
        };
        let (samples, success_rate, avg_latency) = match guard.models.get(&key) {
            Some(window) => (
                window.samples(),
                window.success_rate(),
                window.avg_latency_ms(),
            ),
            None => (0, 1.0, 0.0),
        };
        let score = compute_score(
            guard.config.latency_weight,
            guard.config.latency_ceiling_ms,
            success_rate,
            avg_latency,
        );
        HealthSnapshot {
            provider,
            samples,
            success_rate,
            avg_latency_ms: avg_latency,
            score,
            models: vec![ModelHealthSnapshot {
                model: model.to_string(),
                samples,
                success_rate,
                avg_latency_ms: avg_latency,
                score,
            }],
        }
    }

    /// Provider-level snapshot that aggregates across all known
    /// models for `provider`. The aggregate is the weighted
    /// average across each model's own `(latency_weight * latency
    /// + (1 - latency_weight) * success_rate)` score, weighted by
    ///   the number of samples — so a noisy model with one or two
    ///   samples cannot dominate the aggregate.
    pub fn snapshot(&self, provider: ProviderKind) -> HealthSnapshot {
        let guard = self.inner.read();
        let mut total_samples: usize = 0;
        let mut total_successes: usize = 0;
        let mut weighted_latency_num: f64 = 0.0;
        let mut model_snapshots: Vec<ModelHealthSnapshot> = Vec::new();
        for (key, window) in guard.models.iter() {
            if key.provider != provider {
                continue;
            }
            let samples = window.samples();
            let sr = window.success_rate();
            let latency = window.avg_latency_ms();
            let score = compute_score(
                guard.config.latency_weight,
                guard.config.latency_ceiling_ms,
                sr,
                latency,
            );
            model_snapshots.push(ModelHealthSnapshot {
                model: key.model.clone(),
                samples,
                success_rate: sr,
                avg_latency_ms: latency,
                score,
            });
            total_samples += samples;
            total_successes += (sr * samples as f64).round() as usize;
            weighted_latency_num += latency * samples as f64;
        }
        if model_snapshots.is_empty() {
            return HealthSnapshot {
                provider,
                samples: 0,
                success_rate: 1.0,
                avg_latency_ms: 0.0,
                score: 1.0,
                models: Vec::new(),
            };
        }
        let success_rate = if total_samples > 0 {
            total_successes as f64 / total_samples as f64
        } else {
            1.0
        };
        let avg_latency = if total_samples > 0 {
            weighted_latency_num / total_samples as f64
        } else {
            0.0
        };
        // Aggregate score is the sample-weighted average of
        // per-model scores. This is the bug fix: a single
        // misbehaving model on `provider` no longer poisons every
        // sibling model on the same provider — the score reflects
        // only the models that were actually exercised, in
        // proportion to their traffic.
        let score_num: f64 = model_snapshots
            .iter()
            .map(|m| m.score * m.samples as f64)
            .sum();
        let score_den: usize = model_snapshots.iter().map(|m| m.samples).sum();
        let score = if score_den > 0 {
            score_num / score_den as f64
        } else {
            1.0
        };
        // Stable order for serialisation: by samples desc, then
        // by model name. Makes dashboard rendering deterministic.
        model_snapshots.sort_by(|a, b| {
            b.samples
                .cmp(&a.samples)
                .then_with(|| a.model.cmp(&b.model))
        });
        HealthSnapshot {
            provider,
            samples: total_samples,
            success_rate,
            avg_latency_ms: avg_latency,
            score,
            models: model_snapshots,
        }
    }

    pub fn is_healthy(&self, provider: ProviderKind, min_health: f64) -> bool {
        self.snapshot(provider).score >= min_health
    }

    /// True when the specific `(provider, model)` bucket has
    /// enough samples to make a decision AND its score meets
    /// `min_health`. A model with zero samples is treated as
    /// healthy (we have no signal that it's broken). This is the
    /// preferred API for the smart router's per-model guard.
    pub fn is_model_healthy(&self, provider: ProviderKind, model: &str, min_health: f64) -> bool {
        let snap = self.snapshot_for_model(provider, model);
        snap.samples == 0 || snap.score >= min_health
    }

    /// Log the current per-provider health snapshot at INFO level.
    /// Called after restoring runtime settings from storage so the
    /// operator can see the starting health state in the logs.
    pub fn print_samples(&self) {
        use ProviderKind::*;
        for p in [OpenAI, Anthropic, Gemini] {
            let snap = self.snapshot(p);
            tracing::info!(
                provider = %p,
                samples = snap.samples,
                success_rate = snap.success_rate,
                avg_latency_ms = snap.avg_latency_ms,
                score = snap.score,
                "health samples"
            );
        }
    }

    /// Providers with the best aggregate score first.
    pub fn ranked(&self) -> Vec<HealthSnapshot> {
        let guard = self.inner.read();
        let providers: Vec<ProviderKind> = guard.models.keys().map(|k| k.provider).collect();
        drop(guard);
        // ProviderKind isn't Ord, so dedup with HashSet and sort
        // snapshots by score afterwards.
        let mut snapshots: Vec<HealthSnapshot> = providers
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .map(|p| self.snapshot(p))
            .collect();
        snapshots.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        snapshots
    }
}

/// Compute the weighted score from success_rate and avg_latency.
fn compute_score(
    latency_weight: f64,
    latency_ceiling_ms: f64,
    success_rate: f64,
    avg_latency: f64,
) -> f64 {
    let latency_term = if latency_ceiling_ms > 0.0 {
        (1.0 - (avg_latency / latency_ceiling_ms).min(1.0)).max(0.0)
    } else {
        0.0
    };
    let success_term = success_rate;
    (1.0 - latency_weight) * success_term + latency_weight * latency_term
}
