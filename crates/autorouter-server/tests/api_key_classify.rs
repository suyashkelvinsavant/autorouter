//! Tests for the auto-detecting API-key classifier.
//!
//! These tests cover the two halves of the "smart detection"
//! behaviour:
//!
//!   1. `resolve_secret` (in `upstream.rs`) falls back to the process
//!      environment when the bare secret id looks like an env-var
//!      name. This makes `api_key_secret_id = "OPENROUTER_API_KEY"`
//!      work without the `env:` prefix.
//!
//!   2. `apply_settings_patch` (via `save_secret_for_provider`)
//!      classifies what the operator types into the
//!      `api_key_value` field on the Providers page and persists it
//!      either as an `env:` reference or as a `keychain:` secret.

use std::collections::BTreeMap;
use std::sync::Arc;

use autorouter_config::{
    classify_api_key_reference, looks_like_api_key, ApiKeyReference, InMemoryStore, SecretStore,
};
use autorouter_server::{
    resolve_secret,
    ui::{apply_settings_patch, ProviderPatch, ProvidersPatch, SettingsPatch},
};

// ---------------------------------------------------------------------
// Pure classifier tests (these also live in autorouter-config but we
// re-assert them here so the public API surface is exercised
// end-to-end).
// ---------------------------------------------------------------------

#[test]
fn classifier_recognises_env_prefix() {
    let r = classify_api_key_reference("env:OPENROUTER_API_KEY");
    assert_eq!(r, ApiKeyReference::EnvVar("OPENROUTER_API_KEY".into()));
    assert_eq!(r.canonical(), "env:OPENROUTER_API_KEY");
}

#[test]
fn classifier_recognises_keychain_prefix() {
    let r = classify_api_key_reference("keychain:openrouter_prod");
    assert_eq!(r, ApiKeyReference::SecretId("openrouter_prod".into()));
    assert_eq!(r.canonical(), "keychain:openrouter_prod");
}

#[test]
fn classifier_treats_matching_all_caps_as_env_var() {
    // SAFETY: process env mutation is contained to this test.
    unsafe {
        std::env::set_var("AUTOROUTER_DETECT_TEST_KEY", "value-from-env");
    }
    let r = classify_api_key_reference("AUTOROUTER_DETECT_TEST_KEY");
    unsafe {
        std::env::remove_var("AUTOROUTER_DETECT_TEST_KEY");
    }
    assert_eq!(
        r,
        ApiKeyReference::EnvVar("AUTOROUTER_DETECT_TEST_KEY".into())
    );
}

#[test]
fn classifier_treats_unmatched_all_caps_as_secret_id() {
    let r = classify_api_key_reference("DEFINITELY_NOT_SET_42");
    assert_eq!(r, ApiKeyReference::SecretId("DEFINITELY_NOT_SET_42".into()));
}

#[test]
fn classifier_treats_vendor_prefixed_string_as_secret_id() {
    for raw in [
        "sk-or-v1-abc123def456ghi789",
        "sk-abc123",
        "gsk_ABCdef123",
        "xai-abcdef",
    ] {
        let r = classify_api_key_reference(raw);
        assert_eq!(r, ApiKeyReference::SecretId(raw.into()), "raw={raw}");
        assert!(looks_like_api_key(raw));
    }
}

#[test]
fn classifier_strips_whitespace() {
    let r = classify_api_key_reference("  env:OPENAI_API_KEY  ");
    assert_eq!(r, ApiKeyReference::EnvVar("OPENAI_API_KEY".into()));
}

// ---------------------------------------------------------------------
// Resolver tests: env-fallback behaviour.
// ---------------------------------------------------------------------

#[test]
fn resolve_secret_explicit_env_prefix_reads_env() {
    unsafe {
        std::env::set_var("RESOLVE_TEST_ENV_KEY", "value-12345");
    }
    let got = resolve_secret(Some("env:RESOLVE_TEST_ENV_KEY"), None);
    unsafe {
        std::env::remove_var("RESOLVE_TEST_ENV_KEY");
    }
    assert_eq!(got.as_deref(), Some("value-12345"));
}

#[test]
fn resolve_secret_bare_all_caps_falls_back_to_env_when_store_misses() {
    unsafe {
        std::env::set_var("RESOLVE_TEST_BARE_KEY", "value-bare-67890");
    }
    // No store provided — the resolver must still find the value
    // because the bare id looks like an env var name.
    let got = resolve_secret(Some("RESOLVE_TEST_BARE_KEY"), None);
    unsafe {
        std::env::remove_var("RESOLVE_TEST_BARE_KEY");
    }
    assert_eq!(got.as_deref(), Some("value-bare-67890"));
}

#[test]
fn resolve_secret_bare_all_caps_prefers_store_over_env() {
    // When the secret store HAS the id, the resolver should use it
    // (the operator probably curated that id on purpose).
    unsafe {
        std::env::set_var("RESOLVE_TEST_PREFER_KEY", "value-from-env");
    }
    let store = InMemoryStore::new();
    store
        .put(autorouter_config::Secret::new(
            "RESOLVE_TEST_PREFER_KEY",
            "value-from-store",
        ))
        .unwrap();
    let store_arc: Arc<dyn SecretStore> = Arc::new(store);
    let got = resolve_secret(Some("RESOLVE_TEST_PREFER_KEY"), Some(&store_arc));
    unsafe {
        std::env::remove_var("RESOLVE_TEST_PREFER_KEY");
    }
    assert_eq!(got.as_deref(), Some("value-from-store"));
}

#[test]
fn resolve_secret_returns_none_for_non_env_named_bare_value() {
    // A literal key with no env match and no store match should
    // come back as None — the operator probably hasn't run through
    // the UI save flow yet.
    let got = resolve_secret(Some("sk-or-v1-abc123def456"), None);
    assert!(got.is_none());
}

#[test]
fn resolve_secret_returns_none_for_missing_env() {
    let got = resolve_secret(Some("env:DEFINITELY_NOT_SET_ABC_XYZ"), None);
    assert!(got.is_none());
}

// ---------------------------------------------------------------------
// UI save tests: apply_settings_patch + save_secret_for_provider.
// ---------------------------------------------------------------------

fn build_patch_with_custom_value(
    provider_id: &str,
    api_key_value: Option<&str>,
    api_key_secret_id: Option<&str>,
) -> SettingsPatch {
    let mut custom = BTreeMap::new();
    custom.insert(
        provider_id.to_string(),
        ProviderPatch {
            display_name: Some("Test".into()),
            base_url: Some("https://example.com/v1".into()),
            api_key_secret_id: api_key_secret_id.map(String::from),
            api_key_value: api_key_value.map(String::from),
            default_headers: None,
            enabled: Some(true),
            model_allowlist: None,
            delete: None,
            api_format: None,
        },
    );
    SettingsPatch {
        server: None,
        defaults: None,
        logging: None,
        providers: ProvidersPatch {
            openai: None,
            anthropic: None,
            gemini: None,
            custom,
        },
        persist: Some(false),
    }
}

fn extract_custom_entry<'a>(
    cfg: &'a autorouter_config::AppConfig,
    provider_id: &str,
) -> &'a autorouter_config::ProviderEntry {
    cfg.providers
        .custom
        .get(provider_id)
        .expect("custom provider entry should exist")
}

#[test]
fn ui_save_raw_key_without_explicit_id_persists_to_store() {
    let store = Arc::new(InMemoryStore::new());
    let store_dyn: Arc<dyn SecretStore> = store.clone();
    let mut cfg = autorouter_config::AppConfig::default();
    let patch =
        build_patch_with_custom_value("openrouter", Some("sk-or-v1-abc123def456ghi789"), None);
    apply_settings_patch(&mut cfg, patch, Some(&store_dyn)).unwrap();

    let entry = extract_custom_entry(&cfg, "openrouter");
    let id = entry
        .api_key_secret_id
        .as_deref()
        .expect("api_key_secret_id should be set");
    assert!(
        id.starts_with("keychain:"),
        "expected keychain: ref, got `{id}`"
    );
    let stored_id = id.strip_prefix("keychain:").unwrap();
    let stored = store.get(&stored_id.to_string().into()).unwrap();
    assert_eq!(stored.value, "sk-or-v1-abc123def456ghi789");
}

#[test]
fn ui_save_explicit_env_prefix_creates_env_reference_without_persisting_value() {
    let store = Arc::new(InMemoryStore::new());
    let store_dyn: Arc<dyn SecretStore> = store.clone();
    let mut cfg = autorouter_config::AppConfig::default();
    let patch = build_patch_with_custom_value(
        "openrouter",
        Some("this-is-not-the-real-key-just-a-placeholder"),
        Some("env:OPENROUTER_API_KEY"),
    );
    apply_settings_patch(&mut cfg, patch, Some(&store_dyn)).unwrap();

    let entry = extract_custom_entry(&cfg, "openrouter");
    assert_eq!(
        entry.api_key_secret_id.as_deref(),
        Some("env:OPENROUTER_API_KEY")
    );
    // The placeholder value must NOT have been written to the
    // secret store — env refs are value-less.
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn ui_save_curated_id_with_separate_value_persists_under_that_id() {
    // Operator types a curated short env-var-name into
    // `api_key_secret_id` AND the literal key into the separate
    // `api_key_value` field. The curated id should be honoured
    // and the value stored under it (not auto-generated).
    unsafe {
        std::env::remove_var("AUTOROUTER_CURATED_ID_TEST_KEY");
    }
    let store = Arc::new(InMemoryStore::new());
    let store_dyn: Arc<dyn SecretStore> = store.clone();
    let mut cfg = autorouter_config::AppConfig::default();
    let patch = build_patch_with_custom_value(
        "openrouter",
        Some("sk-or-v1-real-secret-value"),
        Some("AUTOROUTER_CURATED_ID_TEST_KEY"),
    );
    apply_settings_patch(&mut cfg, patch, Some(&store_dyn)).unwrap();

    let entry = extract_custom_entry(&cfg, "openrouter");
    assert_eq!(
        entry.api_key_secret_id.as_deref(),
        Some("keychain:AUTOROUTER_CURATED_ID_TEST_KEY")
    );
    let stored = store
        .get(&"AUTOROUTER_CURATED_ID_TEST_KEY".to_string().into())
        .unwrap();
    assert_eq!(stored.value, "sk-or-v1-real-secret-value");
}

#[test]
fn ui_save_value_that_matches_env_var_name_uses_env_reference() {
    // Operator typed `OPENROUTER_API_KEY` into the value field
    // (probably a UX slip). The classifier should treat it as an
    // env reference.
    unsafe {
        std::env::set_var("OPENROUTER_API_KEY", "value-from-env");
    }
    let store = Arc::new(InMemoryStore::new());
    let store_dyn: Arc<dyn SecretStore> = store.clone();
    let mut cfg = autorouter_config::AppConfig::default();
    let patch = build_patch_with_custom_value("openrouter", Some("OPENROUTER_API_KEY"), None);
    apply_settings_patch(&mut cfg, patch, Some(&store_dyn)).unwrap();

    let entry = extract_custom_entry(&cfg, "openrouter");
    assert_eq!(
        entry.api_key_secret_id.as_deref(),
        Some("env:OPENROUTER_API_KEY"),
        "value field that matches an env-var name should be saved as an env reference"
    );
    assert!(store.list().unwrap().is_empty());
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
    }
}

#[test]
fn ui_save_empty_value_does_nothing() {
    let store = Arc::new(InMemoryStore::new());
    let store_dyn: Arc<dyn SecretStore> = store.clone();
    let mut cfg = autorouter_config::AppConfig::default();
    let patch = build_patch_with_custom_value("openrouter", Some("   "), None);
    apply_settings_patch(&mut cfg, patch, Some(&store_dyn)).unwrap();

    // The entry IS created from the other patch fields
    // (base_url, enabled), but the secret-save path is gated on
    // a non-empty `api_key_value` so no secret is written and
    // `api_key_secret_id` stays None.
    let entry = cfg
        .providers
        .custom
        .get("openrouter")
        .expect("entry created");
    assert!(entry.api_key_secret_id.is_none());
    assert!(store.list().unwrap().is_empty());
}

// ---------------------------------------------------------------------
// End-to-end: TOML load + resolve_secret should agree.
// ---------------------------------------------------------------------

#[test]
fn resolver_handles_user_config_with_bare_env_var_name() {
    // Simulates the situation in the user's config.toml before
    // this fix: api_key_secret_id = "OPENROUTER_API_KEY" (bare,
    // no env: prefix). The gateway used to return None here; now
    // it should find the value via env fallback.
    unsafe {
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-fake-value-for-test");
    }
    let got = resolve_secret(Some("OPENROUTER_API_KEY"), None);
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
    }
    assert_eq!(got.as_deref(), Some("sk-or-v1-fake-value-for-test"));
}

#[test]
fn resolver_handles_user_config_with_raw_key_inline() {
    // The user's *current* (broken) config: api_key_secret_id
    // contains the raw key. The resolver can't help here (no env
    // var by that name and no store entry), but it should at least
    // return None cleanly rather than panicking.
    let got = resolve_secret(Some("sk-or-v1-abc123"), None);
    assert!(got.is_none());
}

// ---------------------------------------------------------------------
// Single-field shape regression tests.
// The Providers UI posts only `api_key_secret_id` (no separate
// `api_key_value`). The Cases below pin down the canonicalization
// behaviour so a literal key pasted into the field is stored under
// an auto-generated id and never becomes the secret-store id
// itself.
// ---------------------------------------------------------------------

#[test]
fn ui_save_single_field_literal_key_persists_under_generated_id() {
    // Single-field shape: the operator pastes a literal OpenRouter
    // key into the only textbox the Providers UI surfaces. The
    // canonical form must NOT keep the literal key as the
    // secret-store id; it must auto-generate
    // `openrouter_api_key` (because the test provider_id is
    // `openrouter` — a custom id, not a first-class slot — and
    // first-class slots now refuse custom-shape PATCHes, see
    // providers_endpoint.rs) and rewrite the entry to
    // `keychain:openrouter_api_key`.
    let store = Arc::new(InMemoryStore::new());
    let store_dyn: Arc<dyn SecretStore> = store.clone();
    let mut cfg = autorouter_config::AppConfig::default();
    let patch =
        build_patch_with_custom_value("openrouter", None, Some("sk-or-v1-abc123def456ghi789"));
    apply_settings_patch(&mut cfg, patch, Some(&store_dyn)).unwrap();

    let entry = extract_custom_entry(&cfg, "openrouter");
    let id = entry
        .api_key_secret_id
        .as_deref()
        .expect("api_key_secret_id should be set");
    assert!(
        id.starts_with("keychain:"),
        "expected keychain: ref, got `{id}`"
    );
    let suffix = id.strip_prefix("keychain:").unwrap();
    assert_eq!(
        suffix, "openrouter_api_key",
        "expected auto-generated id, got `{suffix}`"
    );
    let stored = store.get(&"openrouter_api_key".to_string().into()).unwrap();
    assert_eq!(stored.value, "sk-or-v1-abc123def456ghi789");
    // The literal key must NOT be present anywhere as a
    // secret-store id — that was the original bug.
    assert!(store
        .get(&"sk-or-v1-abc123def456ghi789".to_string().into())
        .is_err());
}

#[test]
fn ui_save_single_field_with_curated_id_honours_it() {
    // Single-field shape: the operator types a curated short
    // env-var-name into the textbox AND the literal key into the
    // separate value field (via the harness's `api_key_value`).
    // The curated id should be honoured and the value stored
    // under it (not auto-generated).
    unsafe {
        std::env::remove_var("MY_OPENROUTER_KEY");
    }
    let store = Arc::new(InMemoryStore::new());
    let store_dyn: Arc<dyn SecretStore> = store.clone();
    let mut cfg = autorouter_config::AppConfig::default();
    let patch = build_patch_with_custom_value(
        "openrouter",
        Some("sk-or-v1-actual-secret-value"),
        Some("MY_OPENROUTER_KEY"),
    );
    apply_settings_patch(&mut cfg, patch, Some(&store_dyn)).unwrap();

    let entry = extract_custom_entry(&cfg, "openrouter");
    assert_eq!(
        entry.api_key_secret_id.as_deref(),
        Some("keychain:MY_OPENROUTER_KEY")
    );
    let stored = store.get(&"MY_OPENROUTER_KEY".to_string().into()).unwrap();
    assert_eq!(stored.value, "sk-or-v1-actual-secret-value");
}
