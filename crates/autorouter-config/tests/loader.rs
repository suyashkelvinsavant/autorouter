//! Tests for the configuration loader.

use std::fs;

use autorouter_config::{AppConfig, ConfigLoader, DefaultsConfig, ServerConfig};
use tempfile::tempdir;

#[test]
fn defaults_chain() {
    let cfg = ConfigLoader::new().build().unwrap();
    assert_eq!(cfg.server.bind, "127.0.0.1:4073");
    assert_eq!(cfg.defaults.default_provider, "openai");
    assert_eq!(cfg.defaults.stream_by_default, Some(false));
}

#[test]
fn file_overrides_defaults() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
        [server]
        bind = "0.0.0.0:9999"

        [defaults]
        default_model = "claude-sonnet-4-5"
        default_provider = "anthropic"
        "#,
    )
    .unwrap();
    let cfg = ConfigLoader::new()
        .load_file(&path, autorouter_config::LayerSource::User)
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(cfg.server.bind, "0.0.0.0:9999");
    assert_eq!(cfg.defaults.default_model, "claude-sonnet-4-5");
    assert_eq!(cfg.defaults.default_provider, "anthropic");
    // Untouched fields retain defaults.
    assert_eq!(cfg.defaults.stream_by_default, Some(false));
}

#[test]
fn override_wins_over_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
        [server]
        bind = "0.0.0.0:9999"
        "#,
    )
    .unwrap();
    let override_cfg = AppConfig {
        server: ServerConfig {
            bind: "127.0.0.1:1111".into(),
            ..ServerConfig::default()
        },
        ..AppConfig::default()
    };
    let cfg = ConfigLoader::new()
        .load_file(&path, autorouter_config::LayerSource::User)
        .unwrap()
        .with_override(override_cfg)
        .build()
        .unwrap();
    assert_eq!(cfg.server.bind, "127.0.0.1:1111");
}

#[test]
fn missing_file_is_skipped() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("does-not-exist.toml");
    let cfg = ConfigLoader::new()
        .load_file(&path, autorouter_config::LayerSource::User)
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(cfg.server.bind, "127.0.0.1:4073");
}

#[test]
fn validates_log_level() {
    let mut bad = AppConfig::default();
    bad.logging.level = "verbose".into();
    let result = ConfigLoader::new().with_override(bad).build();
    assert!(result.is_err());
}

#[test]
fn validates_unknown_provider() {
    let bad = AppConfig {
        defaults: DefaultsConfig {
            default_provider: "mystery".into(),
            ..DefaultsConfig::default()
        },
        ..AppConfig::default()
    };
    let result = ConfigLoader::new().with_override(bad).build();
    assert!(result.is_err());
}

#[test]
fn layer_summary_lists_sources() {
    let loader = ConfigLoader::new().load_environment();
    let summary = loader.layer_summary();
    assert!(summary.contains(&autorouter_config::LayerSource::BuiltIn));
    assert!(summary.contains(&autorouter_config::LayerSource::Environment));
}

#[test]
fn h1_partial_server_override_preserves_unset_fields() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
[server]
bind = "0.0.0.0:9000"
"#,
    )
    .unwrap();
    let cfg = ConfigLoader::new()
        .load_file(&path, autorouter_config::LayerSource::User)
        .unwrap()
        .build()
        .unwrap();
    // bind comes from file, but max_body_bytes and other server fields
    // remain at the built-in default.
    assert_eq!(cfg.server.bind, "0.0.0.0:9000");
    assert_eq!(
        cfg.server.max_body_bytes,
        ServerConfig::default().max_body_bytes
    );
}

#[test]
fn h2_env_overrides_file_value() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r#"
[server]
bind = "0.0.0.0:1111"
"#,
    )
    .unwrap();
    // SAFETY: test-only env var, set+unset in the same test.
    unsafe {
        std::env::set_var("AUTOROUTER_BIND", "0.0.0.0:2222");
    }
    let cfg = ConfigLoader::new()
        .load_file(&path, autorouter_config::LayerSource::User)
        .unwrap()
        .load_environment()
        .build()
        .unwrap();
    unsafe {
        std::env::remove_var("AUTOROUTER_BIND");
    }
    assert_eq!(cfg.server.bind, "0.0.0.0:2222");
}

#[test]
fn h3_rules_dedupe_by_name() {
    let dir = tempdir().unwrap();
    let path1 = dir.path().join("first.toml");
    let path2 = dir.path().join("second.toml");
    fs::write(
        &path1,
        r#"
[[routing.rules]]
name = "shared"
priority = 1
[routing.rules.target]
provider = "OpenAI"
model = "gpt-5"
"#,
    )
    .unwrap();
    fs::write(
        &path2,
        r#"
[[routing.rules]]
name = "shared"
priority = 99
[routing.rules.target]
provider = "Gemini"
model = "gemini-2.5-pro"
"#,
    )
    .unwrap();
    let cfg = ConfigLoader::new()
        .load_file(&path1, autorouter_config::LayerSource::System)
        .unwrap()
        .load_file(&path2, autorouter_config::LayerSource::User)
        .unwrap()
        .build()
        .unwrap();
    // Only one rule named "shared" should remain, and the higher-priority
    // layer (User) should win.
    let shared: Vec<_> = cfg
        .routing
        .rules
        .iter()
        .filter(|r| r.get("name").and_then(|v| v.as_str()) == Some("shared"))
        .collect();
    assert_eq!(
        shared.len(),
        1,
        "duplicate rules named 'shared' should be merged"
    );
    assert_eq!(shared[0].get("priority").and_then(|v| v.as_i64()), Some(99));
}
