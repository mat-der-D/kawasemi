//! Unit tests for the `webfinger` handler (Requirements 4.1-4.5, task 5.1,
//! `Boundary: webfinger`).
//!
//! `parse_acct_resource` is pure and tested with plain `#[test]`s. The
//! handler itself requires a real `ActorDirectory` (owner-non-exposing
//! actor resolution has no narrow mockable port introduced anywhere in this
//! spec -- mirrors `endpoints/document/tests.rs`'s established precedent of
//! using a real, Postgres-backed `ActorDirectory` via `spawn_test_app`
//! rather than inventing one here), so those tests call [`webfinger`]
//! directly as an ordinary async function against a real instance (mirrors
//! `src/oauth/apps_endpoint.rs`'s tests / `tests/oauth_apps_it.rs`'s
//! established "not wired into a router yet" testing convention).

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde_json::Value;

use super::*;
use crate::actor::owner::create_owner;
use crate::actor::{ActorType, NewActor};
use crate::federation::urls::ActorUrls;
use crate::test_harness::spawn_test_app;

const TEST_DOMAIN: &str = "kawasemi.webfinger-test.internal";

// ---- parse_acct_resource (pure) ----

#[test]
fn parse_acct_resource_accepts_a_well_formed_acct_uri() {
    assert_eq!(
        parse_acct_resource("acct:alice@example.com"),
        Some(("alice", "example.com"))
    );
}

#[test]
fn parse_acct_resource_rejects_a_missing_acct_prefix() {
    assert_eq!(parse_acct_resource("alice@example.com"), None);
}

#[test]
fn parse_acct_resource_rejects_a_missing_at_separator() {
    assert_eq!(parse_acct_resource("acct:alice.example.com"), None);
}

#[test]
fn parse_acct_resource_rejects_an_empty_user_segment() {
    assert_eq!(parse_acct_resource("acct:@example.com"), None);
}

#[test]
fn parse_acct_resource_rejects_an_empty_domain_segment() {
    assert_eq!(parse_acct_resource("acct:alice@"), None);
}

// ---- handler-level tests (real ActorDirectory via spawn_test_app) ----

async fn insert_actor_fixture(
    app: &crate::test_harness::TestApp,
    handle_str: &str,
) -> crate::actor::LocalActor {
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

fn test_state(app: &crate::test_harness::TestApp) -> WebfingerState {
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
