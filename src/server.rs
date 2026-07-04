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
//! grace period, pool release on exit) is explicitly out of scope here —
//! that is task 7.3's job, per design.md's "Server" component
//! responsibilities ("土台ルータ...を組み立て、TraceLayer を装着（7.2）。シグナル
//! 受信で受付停止 ... （7.3 以降）"). [`serve`] below is a deliberately minimal
//! bind-and-serve helper with **no** shutdown wiring, kept just large enough
//! to make this task's router + `TraceLayer` behavior testable end-to-end
//! over a real socket; task 7.3 is expected to replace or extend it with
//! real graceful-shutdown handling (design.md's
//! `serve_with_shutdown(state, cfg)` Service Interface), and task 7.4's
//! Bootstrap composition root calls into whatever `serve`-shaped function
//! 7.3 lands, not necessarily this one.

#[cfg(test)]
mod tests;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use serde::Serialize;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::Span;

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
/// module docs — that is task 7.3's job). Exists so this task's router +
/// `TraceLayer` wiring is exercisable end-to-end over a real socket.
pub async fn serve(listener: TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(listener, build_router(state)).await
}
