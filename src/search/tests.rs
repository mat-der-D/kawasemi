//! Wiring-assembly tests for task 5.3 (`Boundary: SearchModule, Bootstrap,
//! AppState, Server`) — see `src/search.rs`'s own doc comment ("Task 5.3")
//! for what [`crate::search::build_search_module`] does.
//!
//! Mirrors `crate::notifications::tests`'s own established "prove the
//! composition-root wiring itself, via the real fully-assembled router"
//! technique for the structurally identical situation (task 4.2's own
//! wiring test, `src/notifications/tests.rs`'s own doc comment): kept as an
//! inline `#[cfg(test)] mod tests` rather than a new `tests/*.rs` file, and
//! deliberately narrow — proving the wiring itself connects (this task's
//! own observable completion condition: "起動後に `/api/v2/search` が一連で
//! 機能し、既定バックエンドが必須拡張なしで配線され、`X-RateLimit-*`
//! 付与・レート制限装着点に乗ることが確認できる"), not a re-verification of
//! already-approved task 1.x-5.2 business logic (that is
//! `search::service::tests`'s/`search::endpoint::tests`'s own job, driven
//! through their own hand-built test-only routers).
//!
//! Two things are proven here, both through the *real*, fully-assembled
//! router ([`crate::server::build_router`], the exact `AppState`
//! [`crate::test_harness::spawn_test_app`] itself serves — never a
//! hand-built test-only router the way task 5.1/5.2's own tests use):
//!
//! 1. [`search_endpoint_requires_bearer_auth_and_carries_rate_limit_headers`]:
//!    `GET /api/v2/search` unauthenticated returns `401` (an unmounted
//!    route would `404`, never `401` — proving the route is actually
//!    mounted and that `SearchEndpointsState`'s `AuthState` is correctly
//!    derived from the real `AppState` via `FromRef`, this task's own
//!    `src/server.rs` addition), and carries an `x-ratelimit-limit` header
//!    (proving `/api/v2/search` is merged onto the router at the same
//!    cross-cutting application point every other endpoint already is,
//!    Requirement 9.4 — not a separate/duplicate rate-limit layer of its
//!    own).
//! 2. [`search_end_to_end_through_the_real_router_with_the_default_pg_backend`]:
//!    a real, authenticated `GET /api/v2/search?q=...&type=statuses`
//!    returns a previously-created status through the default
//!    `PgSearchBackend` this task wires — proving `SearchService`'s entire
//!    parse -> match -> hydrate -> assemble pipeline (task 5.1, already
//!    reviewed) runs unmodified against the real production backend when
//!    reached through the real mounted endpoint (task 5.2, already
//!    reviewed), not merely through `search::service::tests`'s/
//!    `search::endpoint::tests`'s own StubSearchBackend/hand-built-router
//!    coverage (Requirements 7.3, 7.4). No `CREATE EXTENSION` is required
//!    anywhere in this path (Requirement 8.1) — `spawn_test_app` runs
//!    `migrate::apply_migrations` against a schema with no PostgreSQL
//!    extension privileges beyond the default ones already granted to the
//!    `kawasemi_test` role.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::actor::owner::create_owner;
use crate::actor::{ActorType, Handle};
use crate::domain::{Id, Visibility};
use crate::oauth::app_repository::{self, NewApp};
use crate::oauth::model::ScopeSet as ModelScopeSet;
use crate::oauth::token_repository::{self, NewAccessToken};
use crate::server;
use crate::statuses::model::Status;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (mirrors `crate::notifications::tests`'s own
// established helpers). ----

async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(crate::actor::NewActor {
            owner_id,
            handle: Handle::new(handle).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: "Search Wiring Test Actor".to_string(),
            summary: "an actor used by search's own task 5.3 wiring tests".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    actor.id
}

async fn create_test_status(app: &TestApp, actor_id: Id, content: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let status = Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: content.to_string(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    };
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");
    id
}

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Search Wiring Test Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn issue_test_token(app: &TestApp, app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
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

fn real_router(app: &TestApp) -> Router {
    server::build_router(app.state.clone())
}

fn req(method: &str, path: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("build request")
}

/// Requirement 9.1/9.4: proves the router is actually mounted (a
/// not-yet-mounted route 404s, never 401) and carries `X-RateLimit-*`
/// (case-insensitively `x-ratelimit-limit`) — i.e. `/api/v2/search` is
/// merged onto the same cross-cutting application point every other
/// endpoint already is, not a bespoke one.
#[tokio::test]
async fn search_endpoint_requires_bearer_auth_and_carries_rate_limit_headers() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let response = router
        .oneshot(req("GET", "/api/v2/search?q=anything", None))
        .await
        .expect("router must not fail to produce a response");

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "an unmounted route would 404, not 401"
    );
    assert!(
        response.headers().contains_key("x-ratelimit-limit"),
        "a response merged before build_router's rate_limit_layer must carry \
         X-RateLimit-* headers (Requirement 9.4): {:?}",
        response.headers()
    );

    app.cleanup().await;
}

/// Requirements 7.3, 7.4, 8.1: a real, authenticated search reaches the
/// default `PgSearchBackend` through the real mounted endpoint, end to end,
/// with no PostgreSQL extension required anywhere in the path.
#[tokio::test]
async fn search_end_to_end_through_the_real_router_with_the_default_pg_backend() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let author = create_test_actor(&app, "search_wiring_author").await;
    let status_id = create_test_status(&app, author, "hello from the wired search endpoint").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, author, &["read:search"]).await;

    let response = router
        .oneshot(req(
            "GET",
            "/api/v2/search?q=wired+search+endpoint&type=statuses",
            Some(&token),
        ))
        .await
        .expect("router must not fail to produce a response");

    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let body: Value = serde_json::from_slice(&bytes).expect("response body must be valid JSON");

    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    let expected_id = status_id.as_i64().to_string();
    let statuses = body["statuses"]
        .as_array()
        .expect("statuses field must be a JSON array");
    assert!(
        statuses.iter().any(|s| s["id"] == expected_id),
        "the default PgSearchBackend must have matched the seeded status: {body:?}"
    );
    assert_eq!(
        body["accounts"],
        json!([]),
        "type=statuses must return [] for accounts (Requirement 2.2)"
    );
    assert_eq!(
        body["hashtags"],
        json!([]),
        "type=statuses must return [] for hashtags (Requirement 2.2)"
    );

    app.cleanup().await;
}
