//! Bridge between the global tracing log sink and the in-memory
//! `UiState::log_lines` buffer.
//!
//! The desktop and headless binaries install the log sink in
//! `init_logging` and then call [`LogBridge::start`] (headless)
//! or [`LogBridge::start_on_tauri`] (desktop) to spawn a
//! background task that periodically drains the sink into the
//! UI buffer.

use std::sync::Arc;
use std::time::Duration;

use autorouter_observability::drain_log_sink;
use parking_lot::RwLock;

use crate::ui::LogLine;

/// Handle to a running bridge task.
///
/// The bridge task itself is bound to whichever async runtime
/// spawned it. We use `futures::future::abortable` to wrap the
/// task so we get a clone-able `AbortHandle` that lets us cancel
/// from anywhere without naming the concrete `JoinHandle` type
/// of the underlying runtime.
pub struct LogBridge {
    /// Optional abort handle. Stored so `stop_and_join` can
    /// short-circuit a long `tokio::time::sleep`. The actual
    /// future is owned by the runtime.
    abort: Option<futures::future::AbortHandle>,
    /// Cooperative shutdown flag. The loop polls this on every
    /// iteration; setting it is the primary way to stop the
    /// bridge.
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl LogBridge {
    /// Spawn the bridge on a `tokio` runtime. Use this from the
    /// headless binary (which is wrapped in `#[tokio::main]`).
    pub fn start_on_tokio(
        handle: &tokio::runtime::Handle,
        log_lines: Arc<RwLock<Vec<LogLine>>>,
        interval: Duration,
    ) -> Self {
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = shutdown.clone();
        let task = run_bridge_loop(log_lines, interval, s);
        let (spawned, abort) = futures::future::abortable(task);
        handle.spawn(spawned);
        Self {
            abort: Some(abort),
            shutdown,
        }
    }

    /// Backwards-compatible alias for the headless binarys
    /// `#[tokio::main]`. Panics from a sync context -- those
    /// callers must use [`Self::start_on_tauri`].
    pub fn start(log_lines: Arc<RwLock<Vec<LogLine>>>, interval: Duration) -> Self {
        let handle = tokio::runtime::Handle::current();
        Self::start_on_tokio(&handle, log_lines, interval)
    }

    /// Spawn the bridge on a caller-supplied async runtime.
    /// `spawner` is invoked with the boxed future and is
    /// responsible for putting it on whatever async runtime
    /// the caller has. The desktop binary passes
    /// `|fut| { tauri::async_runtime::spawn(fut); }` so the task
    /// is bound to Tauri's global async runtime.
    ///
    /// This is the fix for the desktop-binary crash that
    /// occurred when `LogBridge::start` called `tokio::spawn`
    /// directly from a sync context. The Tauri setup hook is
    /// `Fn` (not `async fn`); the current thread is not in a
    /// tokio runtime, so `tokio::spawn` panicked with "there is
    /// no reactor running, must be called from the context of a
    /// Tokio 1.x runtime". Routing through Tauri's
    /// `async_runtime::spawn` puts the task on the Tauri global
    /// runtime, which IS in scope at that point.
    pub fn start_on_tauri<F>(
        spawner: F,
        log_lines: Arc<RwLock<Vec<LogLine>>>,
        interval: Duration,
    ) -> Self
    where
        F: FnOnce(futures::future::BoxFuture<'static, ()>),
    {
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = shutdown.clone();
        let task = run_bridge_loop(log_lines, interval, s);
        let (spawned, abort) = futures::future::abortable(task);
        // `abortable` returns a future that resolves to
        // `Result<(), Aborted>`; the spawner takes a
        // `BoxFuture<'static, ()>` so we swallow the Aborted
        // marker. (Cancellation is handled by the AbortHandle
        // we keep, not by inspecting the future's output.)
        spawner(Box::pin(async move {
            let _ = spawned.await;
        }));
        Self {
            abort: Some(abort),
            shutdown,
        }
    }

    /// Signal the background task to stop.
    pub fn stop(&self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Stop the bridge. The cooperative shutdown flag is set
    /// first (this unparks the loop on its next sleep tick),
    /// then the abort handle is fired as a fast-path so a
    /// task stuck in a long sleep wakes up immediately. The
    /// task is detached; the runtime reclaims it on its own
    /// once the loop exits.
    pub async fn stop_and_join(mut self) {
        self.stop();
        if let Some(a) = self.abort.take() {
            a.abort();
        }
    }
}

/// The actual loop body. Shared by all spawn paths.
async fn run_bridge_loop(
    log_lines: Arc<RwLock<Vec<LogLine>>>,
    interval: Duration,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
) {
    while !shutdown.load(std::sync::atomic::Ordering::SeqCst) {
        // The `parking_lot` write guard is `!Send` and must NOT
        // be held across `.await`. We collect any new entries
        // into a local Vec inside a synchronous block, then drop
        // the guard, then push them into the shared Vec
        // (acquiring the lock again briefly). The result is a
        // future that holds no `!Send` values across awaits.
        let to_push: Vec<LogLine> = {
            let drained = drain_log_sink();
            drained
                .into_iter()
                .map(|e| LogLine {
                    ts: chrono::Utc::now(),
                    level: e.level,
                    target: e.target,
                    message: e.message,
                })
                .collect()
        };
        if !to_push.is_empty() {
            let mut g = log_lines.write();
            for line in to_push {
                g.push(line);
            }
            if g.len() > 2_000 {
                let drop = g.len() - 2_000;
                g.drain(0..drop);
            }
        }
        tokio::time::sleep(interval).await;
    }
}
