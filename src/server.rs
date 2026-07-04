//! Foundation axum router and `TraceLayer` wiring (Server boundary,
//! Requirements 1.1, 7.2).
//!
//! Scope: this module owns assembling the minimal axum `Router` every later
//! spec mounts its own routes onto — a single `GET /health` liveness route
//! (Requirement 1.1: proving the HTTP listener itself accepted the
//! connection and dispatched a request, not database or downstream-service
//! liveness, which feature specs that need that mount their own checks for)
//! — and attaching `tower_http::trace::TraceLayer` so every request and
//! response is logged with a request-scoped span carrying a correlation id
//! (Requirement 7.2).
//!
//! No path convention for a liveness route is fixed by requirements.md or
//! design.md, so `/health` (see [`HEALTH_PATH`]) is chosen as the
//! conventional default.
//!
//! The request span is opened via [`crate::telemetry::request_span`] (task
//! 3.1's canonical `request`/`request_id` span convention), so any
//! `sqlx::query` diagnostic event (Requirement 7.3) or `AppError` 5xx log
//! (task 6.1) emitted while handling a request nests inside it and inherits
//! `request_id` automatically through ordinary `tracing` span/event
//! inheritance — no separate correlation wiring is needed in those modules.
//! The `request_id` value itself is drawn from `AppState`'s
//! [`crate::runtime::RuntimeContext`] `IdGenerator` boundary (task 5.3)
//! rather than a fresh ad-hoc generator, consistent with this codebase's
//! non-determinism-behind-an-injection-boundary convention (Requirement
//! 5.2): a caller building `AppState` with a deterministic `RuntimeContext`
//! gets deterministic, reproducible `request_id` values too.
//!
//! Graceful shutdown (signal handling, in-flight drain, forced stop after a
//! grace period, pool release on exit — Requirements 1.3, 1.4, 1.5, task
//! 7.3) is implemented by [`serve_with_shutdown`] below, matching design.md's
//! Server Service Interface `serve_with_shutdown(state, cfg)`. [`serve`]
//! remains as a deliberately minimal bind-and-serve helper with no shutdown
//! wiring (kept from task 7.2, still used by this module's own router/
//! `TraceLayer` tests); production code and task 7.4's Bootstrap composition
//! root are expected to call [`serve_with_shutdown`] instead.

#[cfg(test)]
mod tests;

use std::fmt;
use std::future::Future;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::oneshot;
use tower_http::trace::TraceLayer;
use tracing::Span;

use crate::config::ServerConfig;
use crate::state::AppState;
use crate::telemetry;

/// Path of the minimal liveness route this task adds (Requirement 1.1).
pub const HEALTH_PATH: &str = "/health";

/// Response body shape for [`HEALTH_PATH`]: a minimal JSON status marker,
/// consistent with this codebase's convention of returning structured JSON
/// bodies (see `crate::error`'s `ErrorBody`) rather than a bare string.
#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, axum::Json(HealthBody { status: "ok" }))
}

/// Builds the foundation `Router<AppState>` (Requirement 1.1): the minimal
/// `GET /health` liveness route, plus the mount point later specs extend by
/// `.merge()`/`.nest()`-ing their own routes onto the returned value
/// *before* a caller finalizes it with [`build_router`]'s `.with_state()`
/// step (routes must share the `AppState` state type to merge cleanly).
///
/// Deliberately does not attach [`TraceLayer`] itself: middleware that needs
/// to close over a concrete `AppState` value (to draw `request_id`s from its
/// `RuntimeContext`, see module docs) can only be attached once a concrete
/// `AppState` is available, which is [`build_router`]'s job.
pub fn router() -> Router<AppState> {
    Router::new().route(HEALTH_PATH, get(health))
}

/// Builds the complete, ready-to-serve foundation router (Requirements 1.1,
/// 7.2): [`router`]'s routes, with [`tower_http::trace::TraceLayer`]
/// attached so every request/response is logged inside a
/// [`crate::telemetry::request_span`] carrying a `request_id` drawn from
/// `state`'s `RuntimeContext` (see module docs), and `state` applied so the
/// result is a plain `Router` ready for [`axum::serve`].
pub fn build_router(state: AppState) -> Router {
    let span_state = state.clone();

    router()
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(move |_request: &Request<Body>| {
                    let request_id = span_state.runtime().ids.next_id().as_i64().to_string();
                    telemetry::request_span(&request_id)
                })
                .on_request(|request: &Request<Body>, _span: &Span| {
                    tracing::info!(
                        method = %request.method(),
                        uri = %request.uri(),
                        "request received"
                    );
                })
                .on_response(
                    |response: &Response<Body>, latency: Duration, _span: &Span| {
                        tracing::info!(
                            status = %response.status(),
                            latency_ms = latency.as_millis() as u64,
                            "response sent"
                        );
                    },
                ),
        )
        .with_state(state)
}

/// Binds `state`'s router onto an already-bound `listener` and serves
/// requests indefinitely, with **no** graceful-shutdown handling (see
/// [`serve_with_shutdown`] for that). Exists so this task's router +
/// `TraceLayer` wiring is exercisable end-to-end over a real socket.
pub async fn serve(listener: TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(listener, build_router(state)).await
}

/// Failure binding the HTTP listener [`serve_with_shutdown`] serves on.
/// Unlike the request-serving loop itself (which, per [`axum::serve`]'s own
/// documentation, never surfaces an I/O error once bound — socket errors are
/// handled internally by a short retry sleep), binding the configured
/// address is the one step in this function that can fail outright (address
/// already in use, insufficient privilege for the configured port, etc.).
#[derive(Debug)]
pub enum ServeError {
    /// Binding `cfg.bind_addr` failed.
    Bind(std::io::Error),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServeError::Bind(e) => write!(f, "failed to bind HTTP listener: {e}"),
        }
    }
}

impl std::error::Error for ServeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServeError::Bind(e) => Some(e),
        }
    }
}

/// Waits for an OS shutdown signal, grouping both signals Requirement 1.3
/// names under "割り込みおよび終了シグナル" (interrupt and terminate signals):
/// SIGINT (`Ctrl-C`, via [`tokio::signal::ctrl_c`]) and SIGTERM (via
/// [`tokio::signal::unix::SignalKind::terminate`]). Unix-only
/// (`tokio::signal::unix`), consistent with this project's Linux-only
/// deployment target.
async fn os_shutdown_signal() {
    let mut sigterm = signal(SignalKind::terminate())
        .expect("installing a SIGTERM handler must succeed on a supported Unix target");
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    tokio::select! {
        result = &mut ctrl_c => {
            match result {
                Ok(()) => tracing::info!("received SIGINT; beginning graceful shutdown"),
                Err(e) => tracing::error!(
                    error = %e,
                    "failed to listen for SIGINT; proceeding with shutdown anyway"
                ),
            }
        }
        _ = sigterm.recv() => {
            tracing::info!("received SIGTERM; beginning graceful shutdown");
        }
    }
}

/// Binds `cfg.bind_addr`, then serves `state`'s foundation router
/// ([`build_router`]) until an OS shutdown signal is received (Requirement
/// 1.3): new connections stop being accepted and in-flight requests are
/// drained, up to `cfg.shutdown_grace` before remaining work is forced aside
/// (Requirement 1.4), after which `state`'s database connection pool is
/// released before this function returns (Requirement 1.5).
///
/// # Pool-release placement
/// design.md's Server Service Interface names only `serve_with_shutdown`;
/// the "起動シーケンスと安全停止" flow diagram draws "close pool and exit" /
/// "force stop remaining and exit" as steps in the overall lifecycle without
/// pinning which component performs them. This function closes
/// `state.pool()` itself — inside `serve_with_shutdown`, not deferred to
/// task 7.4's Bootstrap composition root — because Requirement 1.5 ties pool
/// release to the moment "graceful shutdown が完了したとき" (graceful shutdown
/// *completes*), and this function is what directly observes that moment
/// (it already holds `state`, and is the only component that knows whether
/// the drain finished naturally or was forced). Bootstrap (7.4) does not
/// need to close the pool again after calling this.
///
/// # Force-stop semantics
/// Axum's `with_graceful_shutdown` has no built-in grace-period timeout: once
/// triggered, it waits for in-flight requests indefinitely. This function
/// races the drain against a timer that starts the instant the shutdown
/// signal actually fires (not from when serving started); if the timer
/// elapses first, it aborts the task driving the accept-and-drain loop and
/// proceeds immediately to pool release instead of continuing to wait.
/// Axum's public API does not expose a hook to sever an already-accepted TCP
/// connection out from under an in-flight handler, so "force stop" here
/// means this function stops waiting and moves on — it does not guarantee
/// every in-flight socket is severed at the OS level. In production this is
/// sufficient: releasing the pool immediately fails any still-running
/// handler's subsequent database access, and process exit (this function's
/// caller) tears down any sockets still open.
pub async fn serve_with_shutdown(state: AppState, cfg: &ServerConfig) -> Result<(), ServeError> {
    let listener = TcpListener::bind(cfg.bind_addr)
        .await
        .map_err(ServeError::Bind)?;
    let app = build_router(state.clone());
    drive_shutdown(listener, app, state, cfg.shutdown_grace, os_shutdown_signal()).await
}

/// The listener-bind-independent, signal-source-independent core behind
/// [`serve_with_shutdown`]: serves `app` over `listener` until `signal`
/// resolves, drains in-flight requests up to `grace`, force-stops if `grace`
/// is exceeded, then closes `state`'s pool. Factored out from
/// `serve_with_shutdown` so tests can drive this exact drain/grace/
/// force-stop/pool-release logic with a test-only router (e.g. one carrying
/// an artificially slow route) and an injectable shutdown trigger, instead
/// of needing a production listener bound from `ServerConfig` and real OS
/// signals sent to the whole test process — see `tests.rs`.
async fn drive_shutdown(
    listener: TcpListener,
    app: Router,
    state: AppState,
    grace: Duration,
    signal: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    // Marks the instant `signal` actually resolves, so the grace period
    // (Requirement 1.4) starts counting from that point, not from when
    // serving began.
    let (fired_tx, fired_rx) = oneshot::channel::<()>();
    let signal_with_marker = async move {
        signal.await;
        let _ = fired_tx.send(());
    };

    let mut drain = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(signal_with_marker)
            .await
    });
    let abort_handle = drain.abort_handle();

    tokio::select! {
        result = &mut drain => {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(io_err)) => {
                    tracing::error!(error = %io_err, "http server exited with an I/O error");
                }
                Err(join_err) if join_err.is_cancelled() => {}
                Err(join_err) => {
                    tracing::error!(error = %join_err, "http server task panicked");
                }
            }
        }
        _ = async {
            let _ = fired_rx.await;
            tokio::time::sleep(grace).await;
        } => {
            tracing::warn!(
                grace_secs = grace.as_secs_f64(),
                "graceful shutdown grace period exceeded; forcing remaining work aside"
            );
            abort_handle.abort();
        }
    }

    state.pool().close().await;
    Ok(())
}
