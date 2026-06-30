//! Gateway supervisor: owns the running `axum::serve` task and the
//! `tokio::net::TcpListener` that backs it.
//!
//! ## Why this exists
//!
//! `GatewaySupervisor` treats the listener as replaceable state so
//! PATCH `/ui/settings` and `cmd_restart` can hot-rebind to a new
//! address or router without a process restart. The desktop binary
//! originally bound the gateway once at startup and could not move
//! the socket; changing `server.bind` silently had no effect until
//! restart.
//!
//! The supervisor is `Send + Sync` and thread-safe via `Arc`.
//! It does not own the `Router`; the caller passes a fresh `Router`
//! into `start_with_state` or `sync_router_state`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use parking_lot::Mutex;
use socket2::{Domain, Socket, Type};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Outcome of a [`GatewaySupervisor::sync_router_state`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebindOutcome {
    /// No work was required: the supervisor was already bound to
    /// the requested state, or it was idle and is now serving.
    AlreadyOnTarget,
    /// A new listener was created and the previous task was
    /// aborted. The supervisor is now serving with the new state.
    Rebound,
}

/// State that the supervisor tracks about the currently-running
/// router. When ANY of these fields change, the axum stack must
/// be rebuilt (and the supervisor task re-spawned) for the new
/// value to take effect. The list mirrors the fields that
/// `build_router_with_cors` reads from `ServerConfig` at build
/// time — any new field that influences the router stack MUST be
/// added here, or the PATCH /ui/settings handler will silently
/// ignore the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterBuildState {
    pub bind: String,
    pub enable_cors: bool,
    pub max_body_bytes: usize,
    pub request_timeout_seconds: u64,
    pub stream_idle_timeout_seconds: u64,
}

/// Owns the running gateway task. Cheap to clone via [`Arc`].
#[derive(Clone)]
pub struct GatewaySupervisor {
    inner: Arc<Mutex<Option<Slot>>>,
}

struct Slot {
    state: RouterBuildState,
    local_addr: SocketAddr,
    shutdown: Arc<Notify>,
    join: JoinHandle<()>,
}

impl Slot {
    fn signal_shutdown(&self) {
        self.shutdown.notify_waiters();
    }
}

impl GatewaySupervisor {
    /// Create an idle supervisor (no listener bound yet).
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Return the address the supervisor is currently bound to, or
    /// `None` if it is idle. The value reflects the live socket,
    /// not a config field.
    pub fn current_addr(&self) -> Option<SocketAddr> {
        self.inner.lock().as_ref().map(|s| s.local_addr)
    }

    /// String form of [`current_addr`](Self::current_addr), kept
    /// for compatibility with the rest of the dashboard which
    /// still uses `127.0.0.1:4073`-style strings.
    pub fn current_bind(&self) -> Option<String> {
        self.inner.lock().as_ref().map(|s| s.state.bind.clone())
    }

    /// Whether the supervisor is currently serving.
    pub fn is_running(&self) -> bool {
        self.inner.lock().is_some()
    }

    /// Return a snapshot of the current `RouterBuildState`. The
    /// PATCH /ui/settings handler compares this to the desired
    /// state and only triggers a rebind when they differ.
    pub fn current_state(&self) -> Option<RouterBuildState> {
        self.inner.lock().as_ref().map(|s| s.state.clone())
    }

    /// Stop the running task, if any, and leave the supervisor
    /// idle. Synchronous, so any in-flight request is hard-aborted
    /// via the `JoinHandle` drop. Prefer
    /// [`stop_graceful`](Self::stop_graceful) at process exit when
    /// the caller can `await` — that variant gives in-flight
    /// requests a 1-second drain window before aborting.
    pub fn stop(&self) {
        if let Some(slot) = self.inner.lock().take() {
            slot.signal_shutdown();
            slot.join.abort();
        }
    }

    /// Async counterpart to [`stop`](Self::stop) that lets the
    /// axum task drain in-flight requests for up to 1 second
    /// before aborting. Used by the headless binary so Ctrl-C
    /// doesn't drop the response on a request that was already
    /// mid-flight.
    pub async fn stop_graceful(&self) {
        let slot = match self.inner.lock().take() {
            Some(s) => s,
            None => return,
        };
        slot.signal_shutdown();
        let mut handle = slot.join;
        tokio::select! {
            _ = &mut handle => {}
            _ = tokio::time::sleep(std::time::Duration::from_millis(1000)) => {
                tracing::warn!("supervisor: gateway task did not drain within 1s of stop_graceful; aborting");
                handle.abort();
                let _ = handle.await;
            }
        }
    }

    /// Bind to `bind` and start serving `router`. CORS is left at
    /// `false` for the initial start; callers that need CORS
    /// enabled should follow up with
    /// [`sync_router_state`](Self::sync_router_state). This split
    /// keeps `start` from having to know the dashboard's CORS
    /// preference at boot.
    pub async fn start(self, router: Router, bind: &str) -> Result<SocketAddr, String> {
        self.start_with_state(
            router,
            RouterBuildState {
                bind: bind.to_string(),
                enable_cors: false,
                max_body_bytes: 16 * 1024 * 1024,
                request_timeout_seconds: 300,
                stream_idle_timeout_seconds: 600,
            },
        )
        .await
    }

    /// Like [`start`](Self::start) but with an explicit
    /// `RouterBuildState`. New code should prefer this so the
    /// supervisor's view of the current state stays in sync with
    /// the actual axum stack.
    pub async fn start_with_state(
        self,
        router: Router,
        state: RouterBuildState,
    ) -> Result<SocketAddr, String> {
        let _addr: SocketAddr = state
            .bind
            .parse()
            .map_err(|e| format!("invalid bind address {:?}: {e}", state.bind))?;

        // Bind new socket FIRST (SO_REUSEADDR lets us share the port),
        // then tear down the old one. This avoids a window where no
        // listener is active.
        let slot = self.spawn(router, &state).await?;
        let addr = slot.local_addr;

        // Now safe to abort the old task — new socket is already live.
        if let Some(old) = self.inner.lock().take() {
            old.signal_shutdown();
            old.join.abort();
        }

        *self.inner.lock() = Some(slot);
        Ok(addr)
    }

    /// If the supervisor's tracked `RouterBuildState` differs from
    /// `new_state`, stop the current task and rebuild the axum
    /// stack with the new state. Otherwise return
    /// [`RebindOutcome::AlreadyOnTarget`] without disturbing the
    /// running task.
    ///
    /// This is the new entry point for PATCH /ui/settings
    /// toggles (e.g. `enable_cors`) that need to take effect on
    /// the next request without a process restart. The legacy
    /// `rebind_if_needed` only compared the bind string, so
    /// toggling CORS via the Settings page silently did nothing
    /// until the next process restart.
    pub async fn sync_router_state<F, Fut>(
        &self,
        new_state: RouterBuildState,
        build_router: F,
    ) -> Result<RebindOutcome, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Router>,
    {
        let _addr: SocketAddr = new_state
            .bind
            .parse()
            .map_err(|e| format!("invalid bind address {:?}: {e}", new_state.bind))?;
        let prev_slot: Option<Slot> = {
            let mut guard = self.inner.lock();
            match guard.as_ref() {
                Some(slot) if slot.state == new_state => {
                    return Ok(RebindOutcome::AlreadyOnTarget);
                }
                _ => guard.take(),
            }
        };

        let router = build_router().await;

        // Bind new socket FIRST (SO_REUSEADDR lets us share the port),
        // then drain the old one. If the new bind fails, the old
        // listener stays intact.
        let slot = match self.spawn(router, &new_state).await {
            Ok(s) => s,
            Err(e) => {
                // New bind failed — keep old running (if any).
                if let Some(s) = prev_slot {
                    *self.inner.lock() = Some(s);
                }
                return Err(e);
            }
        };

        // New socket is live. Now drain the old one gracefully.
        if let Some(slot) = prev_slot {
            slot.signal_shutdown();
            let mut handle = slot.join;
            tokio::select! {
                _ = &mut handle => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(1000)) => {
                    tracing::warn!(
                        "supervisor: previous gateway task did not drain within 1s; aborting"
                    );
                    handle.abort();
                    let _ = handle.await;
                }
            }
        }

        *self.inner.lock() = Some(slot);
        Ok(RebindOutcome::Rebound)
    }

    /// Convenience wrapper retained for callers that only care
    /// about the bind string. New code should prefer
    /// [`sync_router_state`](Self::sync_router_state) so that
    /// router-affecting toggles (e.g. `enable_cors`) are also
    /// honoured without a process restart.
    pub async fn rebind_if_needed<F, Fut>(
        self,
        current_bind: &str,
        build_router: F,
    ) -> Result<RebindOutcome, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Router>,
    {
        // Look up the current `enable_cors` (if we know it) so a
        // hidden toggle doesn't get clobbered. Without this the
        // old call site would pass a default `false` and
        // accidentally turn CORS off on every "Restart" click.
        let slot_state = self.inner.lock().as_ref().map(|s| s.state.clone());
        let new_state = match slot_state {
            Some(s) => RouterBuildState {
                bind: current_bind.to_string(),
                ..s
            },
            None => RouterBuildState {
                bind: current_bind.to_string(),
                enable_cors: false,
                max_body_bytes: 16 * 1024 * 1024,
                request_timeout_seconds: 300,
                stream_idle_timeout_seconds: 600,
            },
        };
        self.sync_router_state(new_state, build_router).await
    }

    async fn spawn(&self, router: Router, state: &RouterBuildState) -> Result<Slot, String> {
        let listener = bind_with_reuse(&state.bind)
            .await
            .map_err(|e| format!("binding {}: {e}", state.bind))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| format!("local_addr: {e}"))?;
        let shutdown = Arc::new(Notify::new());
        let shutdown_for_task = shutdown.clone();
        let join = tokio::spawn(async move {
            let signal = async move {
                shutdown_for_task.notified().await;
            };
            if let Err(e) = axum::serve(listener, router)
                .with_graceful_shutdown(signal)
                .await
            {
                tracing::error!(error = %e, "gateway exited with error");
            }
        });
        Ok(Slot {
            state: state.clone(),
            local_addr,
            shutdown,
            join,
        })
    }
}

impl Default for GatewaySupervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Bind a `TcpListener` on `bind` with `SO_REUSEADDR` set.
///
/// The supervisor tears down the previous `axum::serve` task on
/// every `sync_router_state` call so the new build (with the
/// toggled `enable_cors`, etc.) can take over. Without
/// `SO_REUSEADDR`, the OS treats the freshly-closed socket as
/// "still in use" until the TIME_WAIT expires — most visible on
/// Windows where the rebind silently fails with WSAEADDRINUSE
/// (os error 10048). `SO_REUSEADDR` lets the rebind succeed on
/// the same port, which is exactly what gap #4 requires when the
/// operator flips CORS without changing the bind string.
async fn bind_with_reuse(bind: &str) -> std::io::Result<TcpListener> {
    let addr: SocketAddr = bind
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::STREAM, None)?;
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    let std_listener: std::net::TcpListener = socket.into();
    TcpListener::from_std(std_listener)
}
