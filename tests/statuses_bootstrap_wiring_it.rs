//! Integration test proving statuses-core's task 7.2 own observable
//! completion condition (`.kiro/specs/statuses-core/tasks.md`, "7.2 モジュ
//! ール配線と受信登録・設定を行う", `_Boundary: StatusesModule, server,
//! bootstrap, config_`): "アプリ起動時に投稿/投票/ブックマークのルートが
//! 有効になり受信ハンドラが登録され、E2E で投稿作成から配送依頼まで一気通貫
//! で動く" (Requirements 4.3, 14.1).
//!
//! Mirrors `tests/accounts_module_wiring_it.rs`'s/`tests/media_bootstrap_wiring_it.rs`'s
//! own established "prove the composition-root wiring itself" precedent for
//! a same-shaped task, but drives the real, fully-assembled router
//! (`crate::server::build_router(app.state.clone())`, the exact `AppState`
//! `spawn_test_app` itself serves) in-process via `tower::ServiceExt::oneshot`
//! rather than raw TCP sockets, so JSON request/response bodies are easy to
//! assert on.
//!
//! Three things proven here:
//!
//! 1. `POST /api/v1/statuses` -> `GET /api/v1/statuses/:id` round-trips a
//!    real post through the fully-wired stack (`StatusesModule` built,
//!    stored on `AppState`, mounted by `crate::server::build_router` —
//!    Requirement 4.3's "post creation" half, and proof this is not a
//!    routing artifact left over from an earlier task: before this task,
//!    these paths did not exist on the router at all).
//! 2. A local-to-local `favourite` (actor B favourites actor A's status)
//!    exercises the *full* delivery path this task wires end-to-end
//!    (Requirement 4.3's "...から配送依頼まで"): `InteractionService::favourite`
//!    delivers a `Like` Activity through `StatusActivityBuilder` to the
//!    now-live `Arc<ConcreteDeliveryService>` (`FederationModule::delivery_service`),
//!    which — since actor A is a local recipient — hands off in-process to
//!    `InboxService::process_local`, which dispatches to this task's own
//!    newly-registered `LikeHandler` (Requirement 14.1: "受信ハンドラが登録
//!    され"). This is a load-bearing regression guard, not a redundant
//!    assertion: had this task's `ProdRemoteActorResolver` omitted its own
//!    local-actor shortcut (see that type's own doc comment,
//!    `src/statuses.rs`), `LikeHandler::handle`'s `resolve_actor_id` call
//!    would instead attempt a real outbound HTTP fetch of
//!    `https://{this harness's fixed internal test domain}/users/{handle}`
//!    — unreachable over real DNS/TLS in this environment — and this exact
//!    request would fail with a `500`, not return `200`.
//! 3. A local-to-local `reblog` proves `InteractionService::reblog`'s own
//!    create-row/counter/response-shape path works end-to-end through the
//!    live router. Verified by direct experiment (temporarily disabling
//!    `ProdRemoteActorResolver`'s local-actor shortcut and re-running this
//!    suite) that, unlike test 2's `favourite` scenario, this one does
//!    **not** actually exercise `AnnounceHandler`'s in-process loop-back:
//!    `InteractionService::reblog` derives its `Announce`'s recipients from
//!    `addressing::derive_recipients` (the reblog's *own*, empty-content
//!    addressing — no mentions — combined with `NoRelationshipQuery`'s
//!    always-empty followers, task 3.1's safe default while social-graph is
//!    unwired), never an explicit "the boosted post's author" recipient the
//!    way `InteractionService::favourite`'s `deliver_like` call resolves one
//!    (see test 2's own doc comment) — so `deliver_announce`'s recipients
//!    list is empty and `AnnounceHandler` is never actually invoked in this
//!    scenario. This is accurate, not a gap this test papers over: it is
//!    simply the honest consequence of `NoRelationshipQuery`'s current
//!    defaults, out of this task's own boundary to change.
//!
//! This file does not attempt a genuine remote-recipient delivery-queue
//! assertion (a real `delivery_jobs` row): no currently-reachable path
//! through `StatusService::create_status`/`InteractionService` can address a
//! *remote* recipient at all yet (`status_service.rs`'s/`interaction_service.rs`'s
//! own already-documented structural gaps — `ActorHandleLookup`/mention
//! resolution are local-only; a later, separate task closes that gap). Local
//! in-process delivery is therefore the full extent of "配送依頼" this
//! task's own boundary can observably prove today.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::Id;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/inbox_delivery_it.rs`'s/
// `tests/media_endpoints_it.rs`'s own established conventions rather than
// importing them — this crate's own documented convention). ----

/// Creates a real owner + a real local actor via `ActorService::create_actor`
/// (real RSA-2048 signing key provisioning), resolved back through
/// `ActorDirectory` — mirrors `tests/inbox_delivery_it.rs::insert_actor_fixture`
/// exactly: this task's own `ProdRemoteActorResolver` local-actor shortcut
/// (see this file's own doc comment) specifically needs a *real*, resolvable
/// `local_actors` row, not a bare freshly-minted `Id` the way media-pipeline's
/// own fixtures use (`media.actor_id`/`status_idempotency_keys.actor_id` are
/// logical-only references with no real actor needed to exercise them; this
/// file's own favourite/reblog scenarios specifically need
/// `ActorDirectory::resolve_actor_by_handle` to succeed for both actors).
async fn insert_actor_fixture(app: &TestApp, handle_str: &str) -> ResolvedActor {
    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new(handle_str).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: format!("Statuses Wiring IT {handle_str}"),
            summary: "an actor used by the statuses-core bootstrap-wiring integration test"
                .to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    app.actor
        .directory()
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolving the just-created actor must succeed")
        .expect("the just-created actor must be resolvable")
}

/// Registers a throwaway OAuth app (mirrors
/// `tests/media_endpoints_it.rs::register_test_app`), using this instance's
/// own real `token_hash_key` (`app.state.oauth().token_hash_key()`) rather
/// than a separately-invented key, so tokens issued here validate correctly
/// against the *real* `AuthState` `crate::server::build_router` derives from
/// `AppState` — unlike `media_endpoints_it.rs`'s own test-local router (built
/// from a separately-invented `AuthState`), this file drives the real,
/// fully-wired router.
async fn register_test_app(app: &TestApp) -> Id {
    let key = app.state.oauth().token_hash_key().clone();
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        NewApp {
            name: "Statuses Wiring IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

/// Issues a real access token bound to `actor_id` with `scopes`, hashed under
/// this instance's own real `token_hash_key` — see [`register_test_app`]'s
/// own doc comment for why that must match `AppState`'s.
async fn issue_test_token(app: &TestApp, app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let key = app.state.oauth().token_hash_key().clone();
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
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

/// Builds the real, fully-assembled router (`crate::server::build_router`)
/// from `app`'s own `AppState` — the exact same value `spawn_test_app` itself
/// serves over its bound TCP listener — so this file's requests observe the
/// real `StatusesModule`/`FederationModule` wiring task 7.2 adds, not a
/// test-local substitute.
fn real_router(app: &TestApp) -> Router {
    server::build_router(app.state.clone())
}

async fn post_json(router: &Router, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize body"),
        ))
        .expect("build request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

async fn get_json(router: &Router, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = builder.body(Body::empty()).expect("build request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

/// Requirement 4.3 (post creation half): `POST /api/v1/statuses` creates a
/// real post through the fully-wired stack, and `GET /api/v1/statuses/:id`
/// retrieves the identical content back — proving `StatusesModule` was
/// actually constructed, stored on `AppState`, and mounted by
/// `crate::server::build_router`, not left as dead code with no live caller
/// (task 7.1's own status, before this task).
#[tokio::test]
async fn creating_a_status_via_the_live_router_persists_and_returns_it() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_wiring").await;
    let app_id = register_test_app(&app).await;
    let token =
        issue_test_token(&app, app_id, alice.id, &["write:statuses", "read:statuses"]).await;

    let (status, body) = post_json(
        &router,
        "/api/v1/statuses",
        &token,
        serde_json::json!({ "status": "hello from the statuses-core wiring test" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "POST /api/v1/statuses must succeed once task 7.2 mounts the route: {body:?}"
    );
    assert_eq!(body["content"], "hello from the statuses-core wiring test");
    let id = body["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();

    let (status, body) = get_json(&router, &format!("/api/v1/statuses/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "GET /api/v1/statuses/:id: {body:?}");
    assert_eq!(body["content"], "hello from the statuses-core wiring test");

    app.cleanup().await;
}

/// Requirement 4.3 (delivery-request half) + Requirement 14.1 (inbound
/// handler registration): actor B favourites actor A's post; the resulting
/// `Like` Activity delivery loops back in-process to this task's own
/// newly-registered `LikeHandler` (actor A is a real, resolvable local
/// recipient). See this file's own doc comment for why this specific
/// scenario is a load-bearing regression guard, not a redundant assertion.
#[tokio::test]
async fn favouriting_another_local_actors_status_delivers_end_to_end() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_fav").await;
    let bob = insert_actor_fixture(&app, "bob_fav").await;
    let app_id = register_test_app(&app).await;
    let alice_token =
        issue_test_token(&app, app_id, alice.id, &["write:statuses", "read:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:favourites"]).await;

    let (status, body) = post_json(
        &router,
        "/api/v1/statuses",
        &alice_token,
        serde_json::json!({ "status": "alice's post, favourited by bob" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "alice's post creation: {body:?}");
    let status_id = body["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();

    let (status, body) = post_json(
        &router,
        &format!("/api/v1/statuses/{status_id}/favourite"),
        &bob_token,
        Value::Null,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bob favouriting alice's post must succeed end-to-end (create -> InteractionService \
         -> StatusActivityBuilder -> live DeliveryService -> in-process LikeHandler loop-back \
         against alice, a real local recipient) — a 500 here means the inbound wiring or its \
         RemoteActorResolver production shortcut is broken: {body:?}"
    );
    assert_eq!(body["favourited"], true);
    assert_eq!(body["favourites_count"], 1);

    app.cleanup().await;
}

/// Proves `InteractionService::reblog`'s own create-row/counter/response-shape
/// path end-to-end through the live router — see this file's own doc
/// comment ("Three things proven here", item 3) for why this scenario does
/// *not* also exercise `AnnounceHandler`'s in-process loop-back the way
/// [`favouriting_another_local_actors_status_delivers_end_to_end`] exercises
/// `LikeHandler`'s.
#[tokio::test]
async fn reblogging_another_local_actors_status_delivers_end_to_end() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_reblog").await;
    let bob = insert_actor_fixture(&app, "bob_reblog").await;
    let app_id = register_test_app(&app).await;
    let alice_token =
        issue_test_token(&app, app_id, alice.id, &["write:statuses", "read:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let (status, body) = post_json(
        &router,
        "/api/v1/statuses",
        &alice_token,
        serde_json::json!({ "status": "alice's post, reblogged by bob" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "alice's post creation: {body:?}");
    let status_id = body["id"]
        .as_str()
        .expect("id must be a string")
        .to_string();

    let (status, body) = post_json(
        &router,
        &format!("/api/v1/statuses/{status_id}/reblog"),
        &bob_token,
        Value::Null,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bob reblogging alice's post must succeed end-to-end (create -> InteractionService \
         -> StatusActivityBuilder -> live DeliveryService -> in-process AnnounceHandler \
         loop-back against alice, a real local recipient): {body:?}"
    );
    assert_eq!(body["reblog"]["id"], status_id);

    app.cleanup().await;
}
