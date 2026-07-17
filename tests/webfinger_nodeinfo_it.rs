//! Integration test for the WebFinger and NodeInfo endpoint handlers
//! (`.kiro/specs/federation-core/tasks.md`, task 5.1 `(P) WebFinger と
//! NodeInfo エンドポイントを実装する`; design.md's File Structure Plan names
//! this exact file, `tests/webfinger_nodeinfo_it.rs`).
//!
//! This task's own observable completion condition: "複数アクターがそれぞれ
//! JRD で解決され、他ドメイン照会は非解決・不在は未検出、NodeInfo に内部情
//! 報が含まれない統合テストが通る" (Requirements 4.1-4.5, 5.1-5.3).
//!
//! Neither handler is mounted on the live application router yet -- wiring
//! into `AppState`/`bootstrap`/`server` is task 5.4's job, out of this
//! task's boundary (see `src/federation/endpoints/webfinger.rs`'s/
//! `nodeinfo.rs`'s own doc comments). Per this task's own instructions
//! (no prior endpoint-shaped task exists yet in this spec to establish an
//! in-spec precedent either way), this test builds a minimal, test-local
//! `axum::Router` mounting just these four handlers over their own narrow
//! state types, and drives it through real HTTP-shaped `Request`/`Response`
//! values via `tower::ServiceExt::oneshot` (no real TCP socket, mirroring
//! this crate's `tower = { features = ["util"] }` dev-dependency's intended
//! use) -- proving the full observable behavior (status codes,
//! `Content-Type`, JSON body shape) end to end, not just the handler
//! functions' return values in isolation (which `src/federation/endpoints/
//! webfinger/tests.rs` / `nodeinfo/tests.rs` already cover at the unit
//! level).
//!
//! A real, Postgres-backed `ActorDirectory` is used throughout (via
//! `spawn_test_app`), matching every other `ActorDirectory`-touching test in
//! this spec (`endpoints/document/tests.rs`, `actor_lifecycle_it.rs`) --
//! `resolve_actor_by_handle` has no narrow mockable port introduced
//! anywhere in this spec.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use serde_json::Value;
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{
    NodeInfoState, WebfingerState, nodeinfo_discovery, nodeinfo_document, webfinger,
};
use kawasemi::test_harness::spawn_test_app;

const TEST_DOMAIN: &str = "kawasemi.webfinger-nodeinfo-it.internal";

/// Creates a real owner + active actor fixture, mirroring
/// `tests/actor_lifecycle_it.rs`'s established pattern.
async fn insert_actor_fixture(
    app: &kawasemi::test_harness::TestApp,
    handle_str: &str,
) -> kawasemi::actor::LocalActor {
    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    app.actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new(handle_str).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: format!("Webfinger/NodeInfo IT {handle_str}"),
            summary: "an actor used by the webfinger/nodeinfo integration test".to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle")
}

/// Builds the test-local router mounting exactly this task's four handlers
/// -- not the full application router (task 5.4's job, out of this task's
/// boundary).
fn test_router(app: &kawasemi::test_harness::TestApp) -> Router {
    let webfinger_state = WebfingerState {
        directory: Arc::clone(app.actor.directory()),
        urls: ActorUrls::new(TEST_DOMAIN),
        domain: TEST_DOMAIN.to_string(),
    };
    let nodeinfo_state = NodeInfoState {
        domain: TEST_DOMAIN.to_string(),
    };

    let webfinger_router = Router::new()
        .route("/.well-known/webfinger", get(webfinger))
        .with_state(webfinger_state);
    let nodeinfo_router = Router::new()
        .route("/.well-known/nodeinfo", get(nodeinfo_discovery))
        .route("/nodeinfo/{version}", get(nodeinfo_document))
        .with_state(nodeinfo_state);

    webfinger_router.merge(nodeinfo_router)
}

async fn get_request(router: &Router, uri: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .expect("a fixed GET request is always well-formed"),
        )
        .await
        .expect("the router must always produce a response, never fail as a tower::Service")
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test response body must be readable");
    serde_json::from_slice(&bytes).expect("test response body must be valid JSON")
}

/// Requirements 4.1, 4.2, 4.5: two distinct local actors on the same
/// instance each resolve, independently, to a JRD carrying their own
/// `self` link (`application/activity+json`, owner-non-exposing).
#[tokio::test]
async fn webfinger_resolves_multiple_local_actors_each_to_their_own_jrd() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_it").await;
    let bob = insert_actor_fixture(&app, "bob_it").await;
    let router = test_router(&app);
    let urls = ActorUrls::new(TEST_DOMAIN);

    let alice_response = get_request(
        &router,
        &format!("/.well-known/webfinger?resource=acct:alice_it@{TEST_DOMAIN}"),
    )
    .await;
    assert_eq!(alice_response.status(), StatusCode::OK);
    assert_eq!(
        alice_response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/jrd+json")
    );
    let alice_body = body_json(alice_response).await;
    assert_eq!(
        alice_body["subject"],
        Value::String(format!("acct:alice_it@{TEST_DOMAIN}"))
    );
    assert_eq!(
        alice_body["links"][0]["rel"],
        Value::String("self".to_string())
    );
    assert_eq!(
        alice_body["links"][0]["type"],
        Value::String("application/activity+json".to_string())
    );
    assert_eq!(
        alice_body["links"][0]["href"],
        Value::String(urls.actor_url(&alice.handle))
    );
    assert!(
        alice_body.get("owner").is_none(),
        "webfinger JRD must never expose owner information (Requirement 4.5)"
    );

    let bob_response = get_request(
        &router,
        &format!("/.well-known/webfinger?resource=acct:bob_it@{TEST_DOMAIN}"),
    )
    .await;
    assert_eq!(bob_response.status(), StatusCode::OK);
    let bob_body = body_json(bob_response).await;
    assert_eq!(
        bob_body["links"][0]["href"],
        Value::String(urls.actor_url(&bob.handle))
    );
    assert_ne!(
        alice_body["links"][0]["href"], bob_body["links"][0]["href"],
        "distinct local actors must resolve to distinct self links"
    );

    app.cleanup().await;
}

/// Requirement 4.3: a query naming a real local actor's handle under a
/// different domain is not resolved as this instance's actor (404), never
/// silently matched on handle alone.
#[tokio::test]
async fn webfinger_does_not_resolve_a_query_for_a_non_matching_domain() {
    let app = spawn_test_app().await;
    insert_actor_fixture(&app, "carol_it").await;
    let router = test_router(&app);

    let response = get_request(
        &router,
        "/.well-known/webfinger?resource=acct:carol_it@not-this-instance.example",
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirement 4.4: a self-domain query for an actor that does not exist is
/// reported as not found.
#[tokio::test]
async fn webfinger_reports_an_absent_actor_as_not_found() {
    let app = spawn_test_app().await;
    let router = test_router(&app);

    let response = get_request(
        &router,
        &format!("/.well-known/webfinger?resource=acct:nobody_it@{TEST_DOMAIN}"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirement 5.1: the NodeInfo discovery document links to this
/// instance's NodeInfo document location.
#[tokio::test]
async fn nodeinfo_well_known_returns_discovery_links() {
    let app = spawn_test_app().await;
    let router = test_router(&app);

    let response = get_request(&router, "/.well-known/nodeinfo").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let links = body["links"].as_array().expect("links must be an array");
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0]["href"],
        Value::String(format!("https://{TEST_DOMAIN}/nodeinfo/2.0"))
    );

    app.cleanup().await;
}

/// Requirements 5.2, 5.3: the NodeInfo document reports software
/// name/version and ActivityPub protocol support, and nothing else --
/// no internal information (no owner data, no internal counts/config).
#[tokio::test]
async fn nodeinfo_document_exposes_minimal_stats_without_internal_information() {
    let app = spawn_test_app().await;
    let router = test_router(&app);

    let response = get_request(&router, "/nodeinfo/2.0").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["software"]["name"],
        Value::String("kawasemi".to_string())
    );
    assert!(
        body["software"]["version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "software.version must be a non-empty string"
    );
    assert_eq!(body["protocols"], serde_json::json!(["activitypub"]));

    let object = body
        .as_object()
        .expect("nodeinfo document must be an object");
    let allowed_keys = ["version", "software", "protocols"];
    for key in object.keys() {
        assert!(
            allowed_keys.contains(&key.as_str()),
            "NodeInfo document must not include internal information beyond the minimal public \
             stats (Requirement 5.3); unexpected field {key:?}"
        );
    }

    app.cleanup().await;
}

/// An unsupported NodeInfo version is reported as not found, not silently
/// coerced to the supported one.
#[tokio::test]
async fn nodeinfo_document_rejects_an_unsupported_version() {
    let app = spawn_test_app().await;
    let router = test_router(&app);

    let response = get_request(&router, "/nodeinfo/1.0").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}
