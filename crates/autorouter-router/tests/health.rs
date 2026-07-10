//! Tests for the health tracker.

use autorouter_core::ProviderKind;
use autorouter_router::HealthTracker;

#[test]
fn empty_tracker_is_healthy() {
    let tracker = HealthTracker::new();
    let snap = tracker.snapshot(ProviderKind::OpenAI);
    assert_eq!(snap.samples, 0);
    assert_eq!(snap.success_rate, 1.0);
    assert!(tracker.is_healthy(ProviderKind::OpenAI, 0.5));
}

#[test]
fn record_changes_score() {
    let tracker = HealthTracker::new();
    for _ in 0..5 {
        tracker.record(ProviderKind::OpenAI, true, 100);
    }
    for _ in 0..3 {
        tracker.record(ProviderKind::OpenAI, false, 100);
    }
    let snap = tracker.snapshot(ProviderKind::OpenAI);
    assert_eq!(snap.samples, 8);
    assert!((snap.success_rate - 5.0 / 8.0).abs() < 0.001);
}

#[test]
fn rank_orders_by_score() {
    let tracker = HealthTracker::new();
    for _ in 0..10 {
        tracker.record(ProviderKind::OpenAI, true, 100);
        tracker.record(ProviderKind::Anthropic, true, 200);
        tracker.record(ProviderKind::Gemini, true, 500);
    }
    let ranked = tracker.ranked();
    assert!(!ranked.is_empty());
    // Latency ceiling of 5000ms gives a small advantage to OpenAI.
    assert!(ranked[0].avg_latency_ms <= ranked[1].avg_latency_ms);
}

#[test]
fn window_size_is_respected() {
    use autorouter_router::HealthConfig;
    use std::time::Duration;
    let tracker = HealthTracker::with_config(HealthConfig {
        window_size: 3,
        window_duration: Duration::from_secs(60),
        ..HealthConfig::default()
    });
    for i in 0..10 {
        tracker.record(ProviderKind::OpenAI, i % 2 == 0, 100);
    }
    let snap = tracker.snapshot(ProviderKind::OpenAI);
    assert_eq!(snap.samples, 3);
}

#[test]
fn per_model_buckets_isolate_health() {
    // Failures on one OpenAI model must NOT poison sibling models on
    // the same provider.
    let tracker = HealthTracker::new();
    for _ in 0..20 {
        tracker.record_for_model(ProviderKind::OpenAI, "gpt-5", false, 5_000);
    }
    for _ in 0..20 {
        tracker.record_for_model(ProviderKind::OpenAI, "gpt-4o", true, 200);
    }
    let bad = tracker.snapshot_for_model(ProviderKind::OpenAI, "gpt-5");
    let good = tracker.snapshot_for_model(ProviderKind::OpenAI, "gpt-4o");
    assert_eq!(bad.samples, 20);
    assert!(bad.success_rate < 0.01, "gpt-5 should be all-failure");
    assert_eq!(good.samples, 20);
    assert!(
        (good.success_rate - 1.0).abs() < 0.001,
        "gpt-4o should be all-success"
    );

    assert!(!tracker.is_model_healthy(ProviderKind::OpenAI, "gpt-5", 0.6));
    assert!(tracker.is_model_healthy(ProviderKind::OpenAI, "gpt-4o", 0.6));
}

#[test]
fn per_model_aggregate_is_sample_weighted() {
    // Provider aggregate must be sample-weighted so one bad model
    // cannot dominate a small bucket.
    let tracker = HealthTracker::with_config(autorouter_router::HealthConfig {
        window_size: 256,
        ..autorouter_router::HealthConfig::default()
    });
    // 99 healthy samples for model A, 1 failure for model B.
    for _ in 0..99 {
        tracker.record_for_model(ProviderKind::OpenAI, "gpt-4o", true, 100);
    }
    tracker.record_for_model(ProviderKind::OpenAI, "gpt-5", false, 5_000);
    let agg = tracker.snapshot(ProviderKind::OpenAI);
    assert_eq!(agg.samples, 100);
    // 99/100 = 0.99 success
    assert!((agg.success_rate - 0.99).abs() < 0.001);
    // Both models appear in the per-model view.
    assert_eq!(agg.models.len(), 2);
}

#[test]
fn empty_model_bucket_is_legacy_back_compat() {
    // `record()` without a model id still works — it lands in the
    // (p, "") bucket and surfaces through the aggregate snapshot.
    let tracker = HealthTracker::new();
    for _ in 0..5 {
        tracker.record(ProviderKind::OpenAI, true, 100);
    }
    let agg = tracker.snapshot(ProviderKind::OpenAI);
    assert_eq!(agg.samples, 5);
    assert!((agg.success_rate - 1.0).abs() < 0.001);
}

#[test]
fn unknown_model_is_healthy_by_default() {
    // A (provider, model) bucket with zero samples must NOT block
    // routing — score is 1.0 until evidence arrives.
    let tracker = HealthTracker::new();
    assert!(tracker.is_model_healthy(ProviderKind::OpenAI, "never-seen", 0.6));
    let snap = tracker.snapshot_for_model(ProviderKind::OpenAI, "never-seen");
    assert_eq!(snap.samples, 0);
    assert!((snap.score - 1.0).abs() < 0.001);
}

#[test]
fn expired_samples_do_not_blacklist_forever() {
    // Failures whose timestamps fall outside the window must not keep
    // the model unhealthy once the wall clock advances past the window.
    use autorouter_router::{HealthConfig, HealthSample};
    use std::time::Duration;

    let window = Duration::from_secs(60);
    let tracker = HealthTracker::with_config(HealthConfig {
        window_size: 64,
        window_duration: window,
        ..HealthConfig::default()
    });

    // Inject failures from 5 minutes ago (well outside a 60s window).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let aged = now.saturating_sub(5 * 60 * 1000);
    for _ in 0..20 {
        tracker.record_sample_for_model(
            ProviderKind::OpenAI,
            "gpt-5",
            HealthSample::at(false, 5_000, aged),
        );
    }

    // Read-path filtering: live sample count is 0, success_rate is 1.0.
    let snap = tracker.snapshot_for_model(ProviderKind::OpenAI, "gpt-5");
    assert_eq!(snap.samples, 0, "aged samples must not count");
    assert!(
        (snap.success_rate - 1.0).abs() < 0.001,
        "expired failure window must treat model as healthy"
    );
    assert!(
        tracker.is_model_healthy(ProviderKind::OpenAI, "gpt-5", 0.6),
        "model must recover after window expiry without new traffic"
    );
}
