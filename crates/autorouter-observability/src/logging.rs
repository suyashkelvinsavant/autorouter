//! Structured logging initialisation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::error::{ObservabilityError, ObservabilityResult};

static INIT: OnceLock<()> = OnceLock::new();
static ALREADY_INIT: AtomicBool = AtomicBool::new(false);
static SINK: OnceLock<Arc<Mutex<Vec<LogSinkEntry>>>> = OnceLock::new();

/// A single in-process log entry. Captured by the optional log sink
/// so the desktop UI can show live traces.
#[derive(Debug, Clone)]
pub struct LogSinkEntry {
    pub level: String,
    pub target: String,
    pub message: String,
}

/// Configuration for [`init`].
#[derive(Debug, Clone)]
pub struct LoggingConfig {
    pub level: String,
    pub json: bool,
    pub file: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            json: false,
            file: None,
        }
    }
}

impl LoggingConfig {
    pub fn from_env() -> Self {
        let mut c = LoggingConfig::default();
        if let Ok(v) = std::env::var("AUTOROUTER_LOG_LEVEL") {
            c.level = v;
        }
        if let Ok(v) = std::env::var("AUTOROUTER_LOG_JSON") {
            c.json = matches!(v.as_str(), "1" | "true" | "TRUE" | "yes");
        }
        if let Ok(v) = std::env::var("AUTOROUTER_LOG_FILE") {
            c.file = Some(v);
        }
        c
    }
}

pub fn validate_filter(level: &str) -> ObservabilityResult<()> {
    EnvFilter::try_new(level).map_err(|e| ObservabilityError::Logging(e.to_string()))?;
    Ok(())
}

pub fn install_log_sink() -> Arc<Mutex<Vec<LogSinkEntry>>> {
    let buf = SINK
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone();
    buf
}

pub fn take_log_sink(n: usize) -> Vec<LogSinkEntry> {
    if let Some(buf) = SINK.get() {
        let mut guard = buf.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() <= n {
            return std::mem::take(&mut *guard);
        }
        let drop = guard.len() - n;
        guard.drain(0..drop).collect()
    } else {
        Vec::new()
    }
}

pub fn drain_log_sink() -> Vec<LogSinkEntry> {
    if let Some(buf) = SINK.get() {
        let mut guard = buf.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *guard)
    } else {
        Vec::new()
    }
}

pub fn with_log_sink<R>(f: impl FnOnce(&mut Vec<LogSinkEntry>) -> R) -> Option<R> {
    SINK.get().map(|buf| {
        let mut guard = buf.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    })
}

/// `tracing` layer that pushes events into the optional in-process
/// log sink. When no sink is installed the layer is a no-op.
struct LogSinkLayer;

impl<S> Layer<S> for LogSinkLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let Some(buf) = SINK.get() else { return };
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let level = event.metadata().level().to_string();
        let target = event.metadata().target().to_string();
        let message = visitor.message.unwrap_or_default();
        if let Ok(mut guard) = buf.lock() {
            guard.push(LogSinkEntry {
                level,
                target,
                message,
            });
            // M15: bound the in-process log sink so a stalled bridge
            // (or a very chatty service) cannot grow it without
            // limit. The first MAX entries are kept; older ones are
            // dropped to make room for the new one.
            const MAX: usize = 2_000;
            if guard.len() > MAX {
                let drop = guard.len() - MAX;
                guard.drain(0..drop);
            }
        }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{:?}", value).trim_matches('"').to_string());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }
}

pub fn init(config: LoggingConfig) -> ObservabilityResult<()> {
    // `OnceLock::set` returns `Ok(())` only on the FIRST successful
    // call; on subsequent calls it returns `Err(value)`. If we are
    // not the first caller, exit early without re-installing the
    // subscriber (which would silently double-log).
    if INIT.set(()).is_err() {
        ALREADY_INIT.store(true, Ordering::SeqCst);
        return Ok(());
    }
    ALREADY_INIT.store(true, Ordering::SeqCst);
    let filter = EnvFilter::try_new(&config.level)
        .map_err(|e| ObservabilityError::Logging(e.to_string()))?;
    let registry = tracing_subscriber::registry().with(filter);
    // H6: json flag selects the json() formatter below.
    if let Some(file) = &config.file {
        let path = std::path::PathBuf::from(file);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        if config.json {
            registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(true)
                        .json()
                        .with_writer(file),
                )
                .with(LogSinkLayer)
                .try_init()
                .map_err(|e| ObservabilityError::Logging(e.to_string()))?;
        } else {
            registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(true)
                        .with_writer(file),
                )
                .with(LogSinkLayer)
                .try_init()
                .map_err(|e| ObservabilityError::Logging(e.to_string()))?;
        }
    } else if config.json {
        registry
            .with(tracing_subscriber::fmt::layer().with_target(true).json())
            .with(LogSinkLayer)
            .try_init()
            .map_err(|e| ObservabilityError::Logging(e.to_string()))?;
    } else {
        registry
            .with(tracing_subscriber::fmt::layer().with_target(true))
            .with(LogSinkLayer)
            .try_init()
            .map_err(|e| ObservabilityError::Logging(e.to_string()))?;
    }

    Ok(())
}

pub fn is_initialised() -> bool {
    ALREADY_INIT.load(Ordering::SeqCst)
}
