#![deny(unused_crate_dependencies)]
//! autorouter-observability
//!
//! Logging, metrics, and crash-recovery helpers for AutoRouter.

// Dev-dependency acknowledgements (used in tests/ benches/ only).
#[cfg(test)]
use autorouter_translate as _;
#[cfg(test)]
use criterion as _;
#[cfg(test)]
use serde_json as _;
#[cfg(test)]
use tempfile as _;

pub mod error;
pub mod logging;
pub mod metrics;
pub mod recovery;

pub use error::{ObservabilityError, ObservabilityResult};
pub use logging::{
    drain_log_sink, init as init_logging, install_log_sink, is_initialised, take_log_sink,
    validate_filter, with_log_sink, LogSinkEntry, LoggingConfig,
};
pub use metrics::{
    dec_session, inc_session, observe_overhead, observe_translation, observe_upstream,
    record_failure, record_rate_limit_hit, record_request, record_tokens, render as render_metrics,
};
pub use recovery::{prune_timestamped_backups, rotate_backup, validate_storage};
