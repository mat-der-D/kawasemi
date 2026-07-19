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
use axum::extract::{DefaultBodyLimit, FromRef};
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::oneshot;
use tower_http::trace::TraceLayer;
use tracing::Span;

use crate::api::ratelimit::{RateLimitPolicy, rate_limit_layer};
use crate::config::ServerConfig;
use crate::federation::{
    ApGetState, ConcreteBlockPolicy, ConcreteReceivedActivityStore, ConcreteVerifier, InboxState,
    NodeInfoState, OutboxState, WebfingerState, actor_get, actor_inbox, nodeinfo_discovery,
    nodeinfo_document, object_get, outbox_get, shared_inbox, webfinger,
};
use crate::media::{self, LocalFsStore, MediaEndpointsState};
use crate::oauth::apps_endpoint::{self, AppsEndpointState};
use crate::oauth::authorize_endpoint::{self, AuthorizeEndpointState};
use crate::oauth::middleware::AuthState;
use crate::oauth::token_endpoint::{self, TokenEndpointState};
use crate::state::AppState;
use crate::telemetry;

/// `POST /api/v1/apps` / `GET /api/v1/apps/verify_credentials` path
/// (design.md's API Contract table, api-foundation task 5.1).
const APPS_PATH: &str = "/api/v1/apps";
const APPS_VERIFY_CREDENTIALS_PATH: &str = "/api/v1/apps/verify_credentials";
/// `GET`/`POST /oauth/authorize` path (task 5.2).
const AUTHORIZE_PATH: &str = "/oauth/authorize";
/// `POST /oauth/token` / `POST /oauth/revoke` paths (task 5.3).
const TOKEN_PATH: &str = "/oauth/token";
const REVOKE_PATH: &str = "/oauth/revoke";

/// federation-core's URL shapes (task 5.4, `_Boundary: FederationModule,
/// Bootstrap, AppState, Config_`), matching `crate::federation::urls::ActorUrls`'s
/// own literal path convention verbatim (`urls.rs`'s own doc comment:
/// "actor: `https://{domain}/users/{handle}`", "inbox: `{actor_url}/inbox`",
/// "shared inbox: `https://{domain}/inbox`", "outbox: `{actor_url}/outbox`")
/// — every one of these constants must keep matching that module's own
/// construction exactly, since a sender's HTTP Signature covers the URL
/// `ActorUrls` builds, and a mismatch here would make every inbound
/// signature verification fail on a URL the sender never actually signed.
const WEBFINGER_PATH: &str = "/.well-known/webfinger";
const NODEINFO_DISCOVERY_PATH: &str = "/.well-known/nodeinfo";
const NODEINFO_DOCUMENT_PATH: &str = "/nodeinfo/{version}";
const ACTOR_PATH: &str = "/users/{handle}";
const ACTOR_INBOX_PATH: &str = "/users/{handle}/inbox";
const ACTOR_OUTBOX_PATH: &str = "/users/{handle}/outbox";
const SHARED_INBOX_PATH: &str = "/inbox";
/// Catch-all GET route [`object_get`] is mounted on, for every local
/// object/collection URL that is not an actor URL (Requirement 6.2).
/// `ActorUrls::object_url`'s own `ObjectKind` is a deliberately open-ended,
/// caller-supplied path segment (`urls.rs`'s own doc comment: "extensible by
/// construction at any later call site, never by editing this type again"),
/// so no fixed set of literal routes can enumerate every kind a not-yet-written
/// downstream spec might register an `ObjectDocumentProvider` for. A single
/// wildcard route is therefore the only mount point that can serve every
/// current and future object/collection kind without this task guessing at
/// kinds that do not exist yet. axum's router prefers a more specific
/// static/named-param match over a wildcard one (`ACTOR_PATH`/`ACTOR_OUTBOX_PATH`/
/// etc. all win over this route for the paths they cover), so this does not
/// shadow any of the other GET routes mounted below.
const OBJECT_CATCH_ALL_PATH: &str = "/{*path}";

/// media-pipeline's three endpoints (task 5.2, `_Boundary: MediaModule
/// wiring_`, design.md's API Contract table): `POST /api/v2/media` (upload),
/// `GET`/`PUT /api/v1/media/{id}` (poll/update).
const MEDIA_UPLOAD_PATH: &str = "/api/v2/media";
const MEDIA_ITEM_PATH: &str = "/api/v1/media/{id}";

/// Rate-limit policy applied to the whole router (task 7.1, api-foundation
/// Requirements 8.1-8.4): a single-owner deployment ("一人鯖前提") has
/// exactly one legitimate caller class, so this is a generous fixed-window
/// limit — Requirement 8.4 explicitly sanctions a loose real value as long
/// as the header shape/computation convention (`crate::api::ratelimit`,
/// already reviewed, task 6.3) stays consistent, which this wiring
/// preserves unchanged.
const RATE_LIMIT_PER_WINDOW: u32 = 300;
const RATE_LIMIT_WINDOW: time::Duration = time::Duration::seconds(60);

/// Bridges `crate::state::AppState` to [`AuthState`] (task 7.1): lets the
/// Bearer auth extractors (`oauth::middleware::OptionalActor`/
/// `RequiredActor`) be used directly inside a handler mounted on
/// `Router<AppState>` — see `middleware.rs`'s own doc comment ("Axum
/// integration shape") for why those extractors need this bridge spelled
/// out explicitly rather than getting it automatically the way
/// `axum::extract::State<T>` does.
impl FromRef<AppState> for AuthState {
    fn from_ref(state: &AppState) -> Self {
        AuthState {
            pool: state.pool().clone(),
            token_hash_key: state.oauth().token_hash_key().clone(),
        }
    }
}

/// Bridges `AppState` to [`AppsEndpointState`] (task 7.1, `_Boundary:
/// AppsEndpoint_`'s own "not `AppState`" judgment call, resolved here by
/// deriving one from the other via `FromRef` rather than editing
/// `apps_endpoint.rs`'s already-reviewed state type).
impl FromRef<AppState> for AppsEndpointState {
    fn from_ref(state: &AppState) -> Self {
        AppsEndpointState {
            service: state.oauth().service().clone(),
            pool: state.pool().clone(),
            token_hash_key: state.oauth().token_hash_key().clone(),
        }
    }
}

/// Bridges `AppState` to [`AuthorizeEndpointState`] (task 7.1), mirroring
/// [`AppsEndpointState`]'s own `FromRef` bridge above.
impl FromRef<AppState> for AuthorizeEndpointState {
    fn from_ref(state: &AppState) -> Self {
        AuthorizeEndpointState {
            service: state.oauth().service().clone(),
            pool: state.pool().clone(),
            owner_credential: state.oauth().owner_credential().clone(),
            directory: state.actor().directory().clone(),
            token_hash_key: state.oauth().token_hash_key().clone(),
            runtime: state.runtime().clone(),
            cookie_secure: state.oauth().cookie_secure(),
        }
    }
}

/// Bridges `AppState` to [`TokenEndpointState`] (task 7.1), mirroring
/// [`AppsEndpointState`]'s own `FromRef` bridge above.
impl FromRef<AppState> for TokenEndpointState {
    fn from_ref(state: &AppState) -> Self {
        TokenEndpointState {
            service: state.oauth().service().clone(),
            pool: state.pool().clone(),
            token_hash_key: state.oauth().token_hash_key().clone(),
        }
    }
}

/// Bridges `AppState` to [`WebfingerState`] (task 5.4), mirroring
/// [`AppsEndpointState`]'s own `FromRef` bridge above.
impl FromRef<AppState> for WebfingerState {
    fn from_ref(state: &AppState) -> Self {
        state.federation().webfinger_state()
    }
}

/// Bridges `AppState` to [`NodeInfoState`] (task 5.4).
impl FromRef<AppState> for NodeInfoState {
    fn from_ref(state: &AppState) -> Self {
        state.federation().nodeinfo_state()
    }
}

/// Bridges `AppState` to [`ApGetState<ConcreteVerifier>`] (task 5.4). See
/// `crate::federation::module`'s own doc comment ("One concrete type per
/// non-`dyn`-safe trait") for why [`ConcreteVerifier`] specifically, and why
/// this instantiation (not a generic `impl<V> FromRef<AppState> for
/// ApGetState<V>`) is the one this crate mounts.
impl FromRef<AppState> for ApGetState<ConcreteVerifier> {
    fn from_ref(state: &AppState) -> Self {
        state.federation().ap_get_state()
    }
}

/// Bridges `AppState` to [`OutboxState`] (task 5.4).
impl FromRef<AppState> for OutboxState {
    fn from_ref(state: &AppState) -> Self {
        state.federation().outbox_state()
    }
}

/// Bridges `AppState` to [`InboxState<ConcreteVerifier, ConcreteBlockPolicy,
/// ConcreteReceivedActivityStore>`] (task 5.4). Same concrete-type-choice
/// reasoning as [`ApGetState<ConcreteVerifier>`]'s bridge above.
impl FromRef<AppState>
    for InboxState<ConcreteVerifier, ConcreteBlockPolicy, ConcreteReceivedActivityStore>
{
    fn from_ref(state: &AppState) -> Self {
        state.federation().inbox_state()
    }
}

/// Bridges `AppState` to [`MediaEndpointsState<LocalFsStore>`] (task 5.2,
/// `_Boundary: MediaModule wiring_`), mirroring [`AppsEndpointState`]'s own
/// `FromRef` bridge above. `LocalFsStore` is the one concrete `MediaStore`
/// this instance mounts every media endpoint with — see
/// `crate::media::MediaModule`'s own doc comment.
impl FromRef<AppState> for MediaEndpointsState<LocalFsStore> {
    fn from_ref(state: &AppState) -> Self {
        MediaEndpointsState {
            media_service: state.media().service(),
            store: state.media().store().clone(),
            auth: AuthState::from_ref(state),
        }
    }
}

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

/// media-pipeline's three endpoints (task 5.2), kept as a separate
/// `.merge()`-able group rather than folded directly into [`router`] itself:
/// mounting `upload_media` needs to size a [`DefaultBodyLimit`] layer from
/// `AppConfig::media.max_upload_size_bytes`, a real runtime config value
/// [`router`] never has access to (it is built before any concrete
/// `AppState` exists; only [`build_router`] is, so this function is called
/// from there instead). See `crate::media::endpoints`'s own doc comment
/// ("CONCERN for task 5.2") for why this layer exists at all: axum's
/// `Multipart` extractor applies its own hard-coded 2MB body-limit default
/// unless a `DefaultBodyLimit` layer overrides it on the specific route the
/// handler is mounted on (`MethodRouter::layer`, scoped to just this one
/// route — not [`Router::layer`], which would apply to every route
/// including unrelated ones), and that hard-coded default is smaller than
/// this config's own default (10 MiB) — so a legitimate upload between
/// those two sizes would otherwise be silently rejected by axum itself
/// before `MediaService::accept_upload`'s own size validation ever ran
/// (Requirements 1.1, 1.4).
fn media_router(upload_body_limit: usize) -> Router<AppState> {
    Router::new()
        .route(
            MEDIA_UPLOAD_PATH,
            post(media::upload_media::<LocalFsStore>)
                .layer(DefaultBodyLimit::max(upload_body_limit)),
        )
        .route(
            MEDIA_ITEM_PATH,
            get(media::show_media::<LocalFsStore>).put(media::update_media::<LocalFsStore>),
        )
}

/// Builds the foundation `Router<AppState>` (Requirement 1.1, and — as of
/// task 7.1 — api-foundation Requirements 1.1, 2.1, 3.1, 5.1): the minimal
/// `GET /health` liveness route, the four OAuth endpoints
/// (`apps`/`authorize`/`token`/`revoke`, tasks 5.1-5.3, mounted here per
/// design.md's "Modified Files" entry for this file: "ルータに OAuth/apps
/// エンドポイントを mount"), plus the mount point later specs extend by
/// `.merge()`/`.nest()`-ing their own routes onto the returned value
/// *before* a caller finalizes it with [`build_router`]'s `.with_state()`
/// step (routes must share the `AppState` state type to merge cleanly).
/// Each OAuth handler's own small state type (`AppsEndpointState`/
/// `AuthorizeEndpointState`/`TokenEndpointState`) is derived from `AppState`
/// automatically via the `impl axum::extract::FromRef<AppState>` blocks
/// above — no separate `.with_state()`/`.merge()` per route group is
/// needed.
///
/// Deliberately does not attach [`TraceLayer`] itself: middleware that needs
/// to close over a concrete `AppState` value (to draw `request_id`s from its
/// `RuntimeContext`, see module docs) can only be attached once a concrete
/// `AppState` is available, which is [`build_router`]'s job.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(HEALTH_PATH, get(health))
        .route(APPS_PATH, post(apps_endpoint::register_app))
        .route(
            APPS_VERIFY_CREDENTIALS_PATH,
            get(apps_endpoint::verify_credentials),
        )
        .route(
            AUTHORIZE_PATH,
            get(authorize_endpoint::authorize_get).post(authorize_endpoint::authorize_post),
        )
        .route(TOKEN_PATH, post(token_endpoint::exchange_token))
        .route(REVOKE_PATH, post(token_endpoint::revoke_token))
        // federation-core (task 5.4): WebFinger, NodeInfo, ActivityPub
        // actor/object/collection GET, outbox GET, per-actor and shared
        // inbox POST — see `WEBFINGER_PATH`'s own doc comment for why every
        // path constant here must match `ActorUrls`'s construction exactly.
        .route(WEBFINGER_PATH, get(webfinger))
        .route(NODEINFO_DISCOVERY_PATH, get(nodeinfo_discovery))
        .route(NODEINFO_DOCUMENT_PATH, get(nodeinfo_document))
        .route(ACTOR_PATH, get(actor_get::<ConcreteVerifier>))
        .route(ACTOR_OUTBOX_PATH, get(outbox_get))
        .route(
            ACTOR_INBOX_PATH,
            post(actor_inbox::<ConcreteVerifier, ConcreteBlockPolicy, ConcreteReceivedActivityStore>),
        )
        .route(
            SHARED_INBOX_PATH,
            post(shared_inbox::<ConcreteVerifier, ConcreteBlockPolicy, ConcreteReceivedActivityStore>),
        )
        .route(OBJECT_CATCH_ALL_PATH, get(object_get::<ConcreteVerifier>))
}

/// Builds the complete, ready-to-serve foundation router (Requirements 1.1,
/// 7.2; api-foundation Requirements 7.1-7.5, 8.1-8.4): [`router`]'s routes,
/// with the `X-RateLimit-*`-attaching rate-limit layer (task 6.3's
/// [`rate_limit_layer`], applied here per design.md's "Modified Files"
/// entry for this file: "横断レイヤー（エラー変換・RL）を全 API に適用する
/// 装着点を用意") and [`tower_http::trace::TraceLayer`] attached so every
/// request/response is logged inside a [`crate::telemetry::request_span`]
/// carrying a `request_id` drawn from `state`'s `RuntimeContext` (see module
/// docs), and `state` applied so the result is a plain `Router` ready for
/// [`axum::serve`].
///
/// Mastodon-compatible error-body conversion (task 6.1's
/// `crate::api::error::mastodon_error_body`) needs no separate layer here:
/// it is wired in as `crate::error::AppError`'s own default
/// `IntoResponse` (see `src/error.rs`'s doc comment), so every handler that
/// reports failures as a plain `AppError` — every OAuth endpoint above, and
/// any future handler — renders through it automatically.
///
/// Rate-limiting is applied *inside* `TraceLayer` (added first here, then
/// wrapped by `TraceLayer` below) so a rate-limited (429) response is still
/// logged the same way any other response is, consistent with
/// Requirement 8.1's "レート制限の対象応答" covering the over-limit response
/// too.
pub fn build_router(state: AppState) -> Router {
    let span_state = state.clone();
    let rate_limit_clock = state.runtime().clock.clone();
    let media_upload_body_limit = state.config().media.max_upload_size_bytes as usize;

    router()
        // task 5.2: mounted here (not inside `router()`) precisely because
        // sizing `DefaultBodyLimit` needs `state`'s own real config value —
        // see `media_router`'s own doc comment. Merged before the
        // rate-limit/`TraceLayer` layers below, so media responses get
        // wrapped by both exactly the way every other endpoint on this
        // router already is (Requirement 9.5).
        .merge(media_router(media_upload_body_limit))
        .layer(rate_limit_layer(
            rate_limit_clock,
            RateLimitPolicy::new(RATE_LIMIT_PER_WINDOW, RATE_LIMIT_WINDOW),
        ))
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
///
/// `pub(crate)` (not private) as of task 5.2: `src/bootstrap.rs` passes this
/// function directly as every resident `ProcessingWorker`'s own shutdown
/// signal factory (`crate::media::MediaBackgroundWorkers::spawn`). This is
/// safe to call more than once concurrently — `tokio::signal::ctrl_c`/
/// `tokio::signal::unix::signal` both support any number of independent
/// listeners for the same signal, each observing the same real OS event —
/// so every worker and [`serve_with_shutdown`]'s own internal call below
/// observe the identical shutdown trigger without needing a broadcast/watch
/// channel to fan one call out to several tasks.
pub(crate) async fn os_shutdown_signal() {
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
    serve_with_shutdown_and_signal(state, cfg, os_shutdown_signal()).await
}

/// Test/composition-root-oriented variant of [`serve_with_shutdown`] that
/// accepts an injectable shutdown trigger instead of always waiting for a
/// real OS signal ([`os_shutdown_signal`]); [`serve_with_shutdown`] itself
/// delegates here with that real signal.
///
/// This is `pub` (unlike the private [`drive_shutdown`] this module's own
/// tests drive directly) because task 7.4's Bootstrap composition root needs
/// an equivalent seam reachable from its own integration tests, which live in
/// a separate `tests/*.rs` binary/process (see
/// `tests/bootstrap_lifecycle_it.rs`'s module doc comment for why) and so
/// cannot reach a `pub(crate)`-or-narrower item. Such a test spawns
/// `crate::bootstrap::bootstrap_with_shutdown_signal` (which calls this
/// function instead of `serve_with_shutdown`), polls the configured bind
/// address until it accepts connections to prove "listen-ready"
/// (Requirement 1.1), then resolves `signal` (e.g. via a `oneshot` channel,
/// mirroring this module's own [`drive_shutdown`] tests) to trigger a clean
/// shutdown without needing to send a real OS signal to the whole test
/// process.
pub async fn serve_with_shutdown_and_signal(
    state: AppState,
    cfg: &ServerConfig,
    signal: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let listener = TcpListener::bind(cfg.bind_addr)
        .await
        .map_err(ServeError::Bind)?;
    let app = build_router(state.clone());
    drive_shutdown(listener, app, state, cfg.shutdown_grace, signal).await
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
