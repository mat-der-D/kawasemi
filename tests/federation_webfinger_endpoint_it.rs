//! Integration tests for the `webfinger` handler (Requirements 4.1-4.5 of
//! the federation-core spec), relocated here from
//! `src/federation/endpoints/webfinger/tests.rs` by
//! `.kiro/specs/test-placement-migration` task 2.2: every test below needs a
//! real, running instance (`spawn_test_app`), which steering
//! `structure.md`'s test layout rule places under `tests/*_it.rs`.
//!
//! The handler requires a real `ActorDirectory` (owner-non-exposing actor
//! resolution has no narrow mockable port anywhere in the federation-core
//! spec), so these tests call [`webfinger`] directly as an ordinary async
//! function against a real instance -- the same "not wired into a router
//! yet" convention `tests/oauth_apps_it.rs` established. The complementary
//! router-level coverage (the same handler reached over real HTTP-shaped
//! requests, alongside NodeInfo) lives in `tests/webfinger_nodeinfo_it.rs`
//! and is deliberately a different layer, not a duplicate.
//!
//! The pure unit tests for `parse_acct_resource` need no running instance
//! and stay at `src/federation/endpoints/webfinger/tests.rs`.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde_json::Value;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, LocalActor, NewActor};
use kawasemi::federation::endpoints::webfinger::{WebfingerQuery, WebfingerState, webfinger};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::test_harness::{TestApp, spawn_test_app};

const TEST_DOMAIN: &str = "kawasemi.webfinger-test.internal";

/// Test-local redefinition of `webfinger.rs`'s own private `JRD_MEDIA_TYPE`
/// const (visibility ladder Tier 0): it is used here only to state the
/// expected `Content-Type`, never to exercise the module's behavior, so
/// restating the one-line literal keeps the assertion identical without
/// widening any production item's visibility (Requirement 3.1).
const JRD_MEDIA_TYPE: &str = "application/jrd+json";

// ---- handler-level tests (real ActorDirectory via spawn_test_app) ----

async fn insert_actor_fixture(app: &TestApp, handle_str: &str) -> LocalActor {
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
            display_name: format!("Webfinger Test {handle_str}"),
            summary: "an actor used to test the webfinger handler".to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle")
}

fn test_state(app: &TestApp) -> WebfingerState {
    WebfingerState {
        directory: Arc::clone(app.actor.directory()),
        urls: ActorUrls::new(TEST_DOMAIN),
        domain: TEST_DOMAIN.to_string(),
    }
}

async fn response_body_json(response: axum::response::Response) -> Value {
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(JRD_MEDIA_TYPE),
        "a successful webfinger response must carry the JRD content type"
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test response body must be readable");
    serde_json::from_slice(&bytes).expect("test response body must be valid JSON")
}

/// Requirements 4.1, 4.5: a self-domain `acct:` query for a real local
/// actor resolves to a JRD with a `self` link carrying the actor's
/// ActivityPub URL and `application/activity+json` type, with no
/// owner-identifying field anywhere in the response.
#[tokio::test]
async fn webfinger_resolves_a_local_actor_to_a_jrd_self_link() {
    let app = spawn_test_app().await;
    let actor = insert_actor_fixture(&app, "alice").await;
    let state = test_state(&app);
    let resource = format!("acct:alice@{TEST_DOMAIN}");

    let response = webfinger(
        State(state),
        Query(WebfingerQuery {
            resource: resource.clone(),
        }),
    )
    .await
    .expect("a well-formed, self-domain, known-actor query must resolve");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_body_json(response).await;
    assert_eq!(body["subject"], Value::String(resource));
    let links = body["links"].as_array().expect("links must be an array");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["rel"], Value::String("self".to_string()));
    assert_eq!(
        links[0]["type"],
        Value::String("application/activity+json".to_string())
    );
    let expected_href = ActorUrls::new(TEST_DOMAIN).actor_url(&actor.handle);
    assert_eq!(links[0]["href"], Value::String(expected_href));
    assert!(
        body.get("owner").is_none(),
        "the JRD response must never carry owner-identifying information"
    );

    app.cleanup().await;
}

/// Requirement 4.2: multiple distinct local actors on the same instance
/// each resolve independently through the same handler/state.
#[tokio::test]
async fn webfinger_resolves_multiple_distinct_local_actors_independently() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_multi").await;
    let bob = insert_actor_fixture(&app, "bob_multi").await;
    let state = test_state(&app);

    let alice_response = webfinger(
        State(state.clone()),
        Query(WebfingerQuery {
            resource: format!("acct:alice_multi@{TEST_DOMAIN}"),
        }),
    )
    .await
    .expect("alice must resolve");
    let bob_response = webfinger(
        State(state.clone()),
        Query(WebfingerQuery {
            resource: format!("acct:bob_multi@{TEST_DOMAIN}"),
        }),
    )
    .await
    .expect("bob must resolve");

    let alice_body = response_body_json(alice_response).await;
    let bob_body = response_body_json(bob_response).await;
    let urls = ActorUrls::new(TEST_DOMAIN);
    assert_eq!(
        alice_body["links"][0]["href"],
        Value::String(urls.actor_url(&alice.handle))
    );
    assert_eq!(
        bob_body["links"][0]["href"],
        Value::String(urls.actor_url(&bob.handle))
    );
    assert_ne!(alice_body["links"][0]["href"], bob_body["links"][0]["href"]);

    app.cleanup().await;
}

/// Requirement 4.3: a query for a domain other than this instance's own
/// configured domain is never resolved as a local actor, even if the user
/// segment names a real local actor.
#[tokio::test]
async fn webfinger_does_not_resolve_a_non_matching_domain() {
    let app = spawn_test_app().await;
    insert_actor_fixture(&app, "alice_wrong_domain").await;
    let state = test_state(&app);

    let err = webfinger(
        State(state),
        Query(WebfingerQuery {
            resource: "acct:alice_wrong_domain@other.example".to_string(),
        }),
    )
    .await
    .expect_err("a non-matching domain must never resolve");

    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirement 4.4: a self-domain query for an actor that does not exist is
/// reported as not found.
#[tokio::test]
async fn webfinger_reports_an_unknown_actor_as_not_found() {
    let app = spawn_test_app().await;
    let state = test_state(&app);

    let err = webfinger(
        State(state),
        Query(WebfingerQuery {
            resource: format!("acct:does_not_exist@{TEST_DOMAIN}"),
        }),
    )
    .await
    .expect_err("an unknown handle must not resolve");

    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// A malformed `resource` value (not a well-formed `acct:` URI) is a bad
/// request, distinct from "not found".
#[tokio::test]
async fn webfinger_rejects_a_malformed_resource_with_bad_request() {
    let app = spawn_test_app().await;
    let state = test_state(&app);

    let err = webfinger(
        State(state),
        Query(WebfingerQuery {
            resource: "not-an-acct-uri".to_string(),
        }),
    )
    .await
    .expect_err("a malformed resource must be rejected");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    app.cleanup().await;
}
