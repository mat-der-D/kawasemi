//! Integration tests for `BearerAuthMiddleware` (task 6.4), per this task's
//! observable completion condition: "有効トークンで単一アクター文脈が確定し、
//! 欠如/失効で 401、スコープ不足で 403、任意認証で未認証継続することを統合
//! テストで確認できる" (Requirements 5.1-5.5, 4.2, 4.3).
//!
//! Mirrors `src/api/ratelimit/tests.rs`'s established convention for a
//! not-yet-router-wired cross-cutting component: real requests are driven
//! through a minimal, test-only axum router via `tower::ServiceExt::oneshot`
//! (not a `tests/*_it.rs` full production-router integration test, since
//! nothing wires this middleware into that router yet — that is task 7.1's
//! job). Unlike `ratelimit`'s tests, this module's router is backed by a
//! real, migrated Postgres schema via `crate::test_harness::spawn_test_app`
//! (mirroring `src/oauth/token_repository/tests.rs`'s convention), because
//! token resolution genuinely queries the database — there is nothing
//! meaningful to fake here.
//!
//! Requirements exercised:
//! - 5.1, 5.3: a freshly issued, real access token resolves (through real
//!   axum extraction, not a direct function call) to a single-actor
//!   `RequestActorContext` carrying the actor and scopes it was issued with.
//! - 5.2: a missing bearer header on a mandatory-auth route is 401; so is a
//!   garbage/never-issued token; so is a genuinely revoked token (issued via
//!   `token_repository::issue_token`, revoked via
//!   `token_repository::revoke_token`, then presented).
//! - 5.4: an optional-auth route with no bearer header continues as
//!   unauthenticated (200, not 401).
//! - 4.2, 4.3, 5.2: a valid, unrevoked token whose granted scopes do not
//!   satisfy the route's required scope is 403, distinct from every 401
//!   case above; a token whose top-level granted scope subsumes the
//!   required granular scope succeeds (proving `require_scope` reuses
//!   `scope::ScopeSet::is_satisfied_by`'s real inclusion judgment, not a
//!   reimplementation).

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;

use super::*;
use crate::config::Secret;
use crate::domain::Id;
use crate::oauth::app_repository::{self, NewApp};
use crate::oauth::hash::TokenHashKey;
use crate::oauth::model::ScopeSet as ModelScopeSet;
use crate::oauth::scope::ScopeSet as RealScopeSet;
use crate::oauth::token_repository::{self, NewAccessToken};
use crate::test_harness::spawn_test_app;

/// A fixed, non-production token-hashing key for this test module only —
/// mirrors `token_repository/tests.rs::test_token_hash_key`'s own
/// reasoning. Deliberately independent of `spawn_test_app`'s own internal
/// fixed key: `authenticate`/`AuthState` take the key as an explicit
/// parameter (this task's reconciliation of design.md's `&AppState` sketch,
/// see this module's doc comment), so these tests only need *a* fixed key
/// consistently used for both issuance and resolution, not the exact one
/// `spawn_test_app` happens to configure `AppConfig.oauth` with.
fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x77; 32])
}

/// Registers a real `oauth_applications` row and returns its `Id`, so tests
/// can satisfy `oauth_access_tokens.app_id`'s FK constraint (mirrors
/// `token_repository/tests.rs::register_test_app`).
async fn register_test_app(pool: &sqlx::PgPool, runtime: &crate::runtime::RuntimeContext) -> Id {
    let key = test_token_hash_key();
    let now = runtime.clock.now();
    let registered = app_repository::register_app(
        pool,
        runtime.ids.as_ref(),
        runtime.rng.as_ref(),
        &key,
        now,
        NewApp {
            name: "Bearer Middleware Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Issues a real access token bound to `actor_id` with `scopes`, returning
/// its plaintext bearer value. Never hand-constructs an `AccessToken`/
/// `RequestActorContext` — every token these tests present was actually
/// persisted and hashed through `token_repository::issue_token`.
async fn issue_test_token(
    pool: &sqlx::PgPool,
    runtime: &crate::runtime::RuntimeContext,
    app_id: Id,
    actor_id: Id,
    scopes: &[&str],
) -> String {
    let key = test_token_hash_key();
    let now = runtime.clock.now();
    let issued = token_repository::issue_token(
        pool,
        runtime.ids.as_ref(),
        runtime.rng.as_ref(),
        &key,
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ModelScopeSet::new(scopes.iter().copied()),
        },
    )
    .await
    .expect("issue_token must succeed");
    issued.plaintext.expose_secret().to_string()
}

/// Test-only JSON body every probe handler responds with, so a test can
/// assert on both "was a `RequestActorContext` supplied" and "which actor".
#[derive(Debug, Serialize, Deserialize)]
struct ProbeBody {
    authenticated: bool,
    actor_id: Option<i64>,
}

/// `GET /optional`: exercises [`OptionalActor`] (Requirement 5.4) — never
/// rejects merely for a missing/absent bearer token.
async fn optional_probe(OptionalActor(ctx): OptionalActor) -> Json<ProbeBody> {
    Json(ProbeBody {
        authenticated: ctx.is_some(),
        actor_id: ctx.map(|c| c.actor_id.as_i64()),
    })
}

/// `GET /required`: exercises [`RequiredActor`] alone (Requirement 5.2) —
/// missing/invalid/revoked bearer tokens never reach the handler body.
async fn required_probe(RequiredActor(ctx): RequiredActor) -> Json<ProbeBody> {
    Json(ProbeBody {
        authenticated: true,
        actor_id: Some(ctx.actor_id.as_i64()),
    })
}

/// `GET /scoped`: exercises [`RequiredActor`] plus [`require_scope`]
/// against a fixed `write:statuses` requirement (Requirements 4.2, 4.3,
/// 5.2's 403 case) — insufficient scope must reject before any success body
/// is produced.
async fn scoped_probe(RequiredActor(ctx): RequiredActor) -> Result<Json<ProbeBody>, AppError> {
    let required = RealScopeSet::parse("write:statuses").expect("valid scope literal");
    require_scope(&ctx, &required)?;
    Ok(Json(ProbeBody {
        authenticated: true,
        actor_id: Some(ctx.actor_id.as_i64()),
    }))
}

fn test_router(state: AuthState) -> Router {
    Router::new()
        .route("/optional", get(optional_probe))
        .route("/required", get(required_probe))
        .route("/scoped", get(scoped_probe))
        .with_state(state)
}

fn request(path: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("valid test request")
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test response body should be readable");
    serde_json::from_slice(&bytes).expect("test response body should be valid JSON")
}

// ---- Requirement 5.4: optional-auth continuation ----

#[tokio::test]
async fn optional_route_with_no_bearer_header_continues_unauthenticated() {
    let app = spawn_test_app().await;
    let router = test_router(AuthState {
        pool: app.pool.clone(),
        token_hash_key: test_token_hash_key(),
    });

    let response = router
        .oneshot(request("/optional", None))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["authenticated"], Value::Bool(false));
    assert_eq!(body["actor_id"], Value::Null);

    app.cleanup().await;
}

// ---- Requirements 5.1, 5.3: real single-actor context on success ----

#[tokio::test]
async fn optional_route_with_a_real_valid_token_resolves_the_single_bound_actor() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let actor_id = app.runtime.ids.next_id();
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["read", "write"],
    )
    .await;

    let router = test_router(AuthState {
        pool: app.pool.clone(),
        token_hash_key: test_token_hash_key(),
    });
    let response = router
        .oneshot(request("/optional", Some(&token)))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["authenticated"], Value::Bool(true));
    assert_eq!(body["actor_id"], Value::from(actor_id.as_i64()));

    app.cleanup().await;
}

// ---- Requirement 5.2: missing/invalid/revoked -> 401 ----

#[tokio::test]
async fn required_route_with_no_bearer_header_is_401() {
    let app = spawn_test_app().await;
    let router = test_router(AuthState {
        pool: app.pool.clone(),
        token_hash_key: test_token_hash_key(),
    });

    let response = router
        .oneshot(request("/required", None))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn required_route_with_a_garbage_never_issued_token_is_401() {
    let app = spawn_test_app().await;
    let router = test_router(AuthState {
        pool: app.pool.clone(),
        token_hash_key: test_token_hash_key(),
    });

    let response = router
        .oneshot(request("/required", Some("this-token-was-never-issued")))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

#[tokio::test]
async fn required_route_with_a_genuinely_revoked_token_is_401_not_a_crash_or_success() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let actor_id = app.runtime.ids.next_id();
    let token = issue_test_token(
        &app.pool,
        &app.runtime,
        app_id,
        actor_id,
        &["read", "write"],
    )
    .await;

    let revoked = token_repository::revoke_token(&app.pool, &test_token_hash_key(), &token)
        .await
        .expect("revoke_token must succeed");
    assert!(revoked, "revoke_token must actually flip a real row");

    let router = test_router(AuthState {
        pool: app.pool.clone(),
        token_hash_key: test_token_hash_key(),
    });
    let response = router
        .oneshot(request("/required", Some(&token)))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

// ---- Requirements 4.2, 4.3, 5.2: scope sufficiency ----

#[tokio::test]
async fn scoped_route_with_a_valid_unrevoked_token_missing_the_required_scope_is_403() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let actor_id = app.runtime.ids.next_id();
    // Grants only `read`; the route requires `write:statuses`.
    let token = issue_test_token(&app.pool, &app.runtime, app_id, actor_id, &["read"]).await;

    let router = test_router(AuthState {
        pool: app.pool.clone(),
        token_hash_key: test_token_hash_key(),
    });
    let response = router
        .oneshot(request("/scoped", Some(&token)))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "insufficient scope must be 403, distinct from every 401 case"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn scoped_route_with_a_top_level_scope_subsuming_the_required_granular_scope_succeeds() {
    let app = spawn_test_app().await;
    let app_id = register_test_app(&app.pool, &app.runtime).await;
    let actor_id = app.runtime.ids.next_id();
    // Grants top-level `write`, which must subsume the required
    // `write:statuses` per `scope::ScopeSet::is_satisfied_by`'s established
    // inclusion judgment (Requirement 4.4) -- this test would fail if
    // `require_scope` reimplemented inclusion instead of reusing it.
    let token = issue_test_token(&app.pool, &app.runtime, app_id, actor_id, &["write"]).await;

    let router = test_router(AuthState {
        pool: app.pool.clone(),
        token_hash_key: test_token_hash_key(),
    });
    let response = router
        .oneshot(request("/scoped", Some(&token)))
        .await
        .expect("oneshot dispatch must succeed");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["authenticated"], Value::Bool(true));
    assert_eq!(body["actor_id"], Value::from(actor_id.as_i64()));

    app.cleanup().await;
}

// ---- Pure-function unit coverage of `require_scope` (no DB needed): would
// fail if the argument order to `is_satisfied_by` were ever flipped. ----

#[test]
fn require_scope_allows_when_the_required_scope_is_satisfied_by_the_granted_scope() {
    let ctx = RequestActorContext {
        actor_id: Id::from_i64(1),
        scopes: ModelScopeSet::new(["write"]),
    };
    let required = RealScopeSet::parse("write:media").expect("valid scope literal");
    assert!(require_scope(&ctx, &required).is_ok());
}

#[test]
fn require_scope_rejects_with_403_when_the_required_scope_is_missing() {
    let ctx = RequestActorContext {
        actor_id: Id::from_i64(1),
        scopes: ModelScopeSet::new(["read"]),
    };
    let required = RealScopeSet::parse("write:media").expect("valid scope literal");
    let err = require_scope(&ctx, &required).expect_err("missing scope must be rejected");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
}
