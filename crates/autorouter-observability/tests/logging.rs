//! Tests for the logging init helper.

use autorouter_observability::{init_logging, validate_filter, LoggingConfig};

#[test]
fn init_is_idempotent() {
    init_logging(LoggingConfig::default()).unwrap();
    init_logging(LoggingConfig::default()).unwrap();
}

#[test]
fn custom_level_is_accepted() {
    init_logging(LoggingConfig {
        level: "debug".into(),
        json: true,
        file: None,
    })
    .unwrap();
    tracing::debug!("observability test message");
}

#[test]
fn unparseable_directive_fails_validation() {
    assert!(validate_filter("=invalid syntax").is_err());
}

#[test]
fn valid_filter_passes_validation() {
    assert!(validate_filter("info").is_ok());
    assert!(validate_filter("autorouter_core=trace,info").is_ok());
}
