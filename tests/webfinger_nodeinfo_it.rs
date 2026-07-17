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
//!
//! ## Task 6.3 extension: the live-wired-app section below
//! Everything above this point is task 5.1's own original test-local-router
//! coverage (kept as-is). Task 6.3 (`_Depends: 5.4_`, this file's own
//! `_Boundary:_`) is the first task in this spec able to exercise these
//! endpoints through the *real* mounted application router
//! (`spawn_test_app`/`crate::server::build_router`, task 5.4's own wiring),
//! not a handler-only test-local one -- that is this extension's entire
//! marginal value over the tests above and over `tests/ap_get_outbox_it.rs`'s
//! (task 5.2's) own test-local-router coverage of `actor_get`/`object_get`/
//! `outbox_get`, and over `tests/federation_bootstrap_it.rs`'s (task 5.4's
//! own boundary file's) already-proven single-actor WebFinger/NodeInfo/
//! actor-GET-reachability smoke coverage. The tests below deliberately do
//! *not* re-prove what is already proven elsewhere at the unit level
//! (`src/federation/endpoints/*/tests.rs`), the test-local-router level (this
//! file's own tests above, `ap_get_outbox_it.rs`), or already thoroughly at
//! the live-router level (`federation_bootstrap_it.rs`'s own downstream-
//! registration test, which already asserts the *unregistered* 404/empty
//! defaults as its own "before" state) -- see each test's own doc comment for
//! exactly which requirement/gap it closes and why.
//!
//! ### Secure-mode authorized fetch: not re-proven here (documented scope decision)
//! Requirement 6.4's authorized-fetch behavior is already proven against the
//! real `HttpSignatureVerifier` cryptographic path by
//! `tests/ap_get_outbox_it.rs`'s `actor_get_in_secure_mode_*`/
//! `object_get_in_secure_mode_*` tests (a test-local router, real verifier).
//! Proving the *same* behavior through `spawn_test_app`'s live router
//! specifically is not practical without a production-code change out of
//! this task's `_Boundary: webfinger_nodeinfo_it_`: `src/test_harness.rs`'s
//! `spawn_test_app` hard-codes `federation.secure_mode: false` into the
//! `AppConfig` it builds (see `spawn_test_app`'s own body,
//! `FederationConfig { secure_mode: false, .. }`), and
//! `FederationWiringConfig::secure_mode` is consumed once, at
//! `build_federation_module` construction time, baked into the
//! `ApGetState::secure_mode` every mounted `actor_get`/`object_get` route
//! closes over -- there is no live per-request or per-test toggle. Adding one
//! would mean editing `src/test_harness.rs` (and possibly
//! `src/federation/module.rs`), both outside this file's boundary.
//! `ap_get_outbox_it.rs`'s existing coverage of this requirement is relied on
//! instead.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::error::AppError;
use kawasemi::federation::jsonld::ACTIVITYSTREAMS_CONTEXT;
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{
    NodeInfoState, OutboxItemsPage, OutboxSource, PageCursor, WebfingerState, nodeinfo_discovery,
    nodeinfo_document, webfinger,
};
use kawasemi::test_harness::{TestApp, spawn_test_app};

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

// ==========================================================================
// Live-wired-app coverage (task 6.3): everything below drives the real
// mounted application router via `spawn_test_app`, not a test-local one --
// see this file's own doc comment ("Task 6.3 extension") for why.
// ==========================================================================

/// Raw HTTP response plumbing (this crate has no HTTP client dependency of
/// its own; mirrors `tests/federation_bootstrap_it.rs`'s own identical
/// `RawResponse`/`raw_request` -- duplicated here rather than shared, since
/// each integration test file is its own compiled crate and cannot import
/// from another one).
struct RawResponse {
    status: u16,
    body: Vec<u8>,
}

async fn raw_request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> RawResponse {
    let mut stream =
        tokio::time::timeout(std::time::Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .expect("connecting to the test listener must not time out")
            .expect("connect");

    let mut request = format!("{method} {path} HTTP/1.1\r\n");
    request.push_str("Host: 127.0.0.1\r\n");
    request.push_str("Connection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut request_bytes = request.into_bytes();
    request_bytes.extend_from_slice(body);

    stream
        .write_all(&request_bytes)
        .await
        .expect("write request");

    let mut buf = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut buf),
    )
    .await
    .expect("read must not time out")
    .expect("read response");

    let text = String::from_utf8_lossy(&buf);
    let (head, body_text) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    RawResponse {
        status,
        body: body_text.as_bytes().to_vec(),
    }
}

impl std::fmt::Debug for RawResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawResponse")
            .field("status", &self.status)
            .field("body", &String::from_utf8_lossy(&self.body))
            .finish()
    }
}

fn raw_body_json(response: &RawResponse) -> Value {
    serde_json::from_slice(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

/// Minimal, purpose-built percent-encoder (this crate has no URL-encoding
/// dependency; mirrors `tests/federation_bootstrap_it.rs`'s own identical
/// `urlencode`).
fn urlencode(input: &str) -> String {
    let mut out = String::new();
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn test_domain(app: &TestApp) -> String {
    app.state.config().server.domain.clone()
}

/// Requirements 4.1, 4.2, 4.5, through the real mounted router: two distinct
/// local actors each resolve, independently, to their own owner-non-exposing
/// JRD. `federation_bootstrap_it.rs`'s own live-router WebFinger coverage
/// exercises exactly one actor; this closes the "複数アクター" (multiple
/// actors) half of this task's own completion condition that single-actor
/// smoke test does not.
#[tokio::test]
async fn webfinger_through_the_real_router_resolves_multiple_local_actors() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let alice = insert_actor_fixture(&app, "live_alice").await;
    let bob = insert_actor_fixture(&app, "live_bob").await;

    let alice_resource = format!("acct:{}@{domain}", alice.handle.as_str());
    let alice_path = format!(
        "/.well-known/webfinger?resource={}",
        urlencode(&alice_resource)
    );
    let alice_response = raw_request(app.address, "GET", &alice_path, &[], b"").await;
    assert_eq!(alice_response.status, 200);
    let alice_body = raw_body_json(&alice_response);
    assert_eq!(
        alice_body["links"][0]["href"],
        json!(urls.actor_url(&alice.handle))
    );
    assert!(
        alice_body.get("owner").is_none(),
        "WebFinger through the real router must never expose owner information (Requirement 4.5)"
    );

    let bob_resource = format!("acct:{}@{domain}", bob.handle.as_str());
    let bob_path = format!(
        "/.well-known/webfinger?resource={}",
        urlencode(&bob_resource)
    );
    let bob_response = raw_request(app.address, "GET", &bob_path, &[], b"").await;
    assert_eq!(bob_response.status, 200);
    let bob_body = raw_body_json(&bob_response);
    assert_eq!(
        bob_body["links"][0]["href"],
        json!(urls.actor_url(&bob.handle))
    );

    assert_ne!(
        alice_body["links"][0]["href"], bob_body["links"][0]["href"],
        "distinct local actors must resolve to distinct self links through the real router too"
    );

    app.cleanup().await;
}

/// Requirement 4.3, through the real mounted router: a query naming a real
/// local actor's handle under a different domain is not resolved.
#[tokio::test]
async fn webfinger_through_the_real_router_does_not_resolve_a_non_matching_domain() {
    let app = spawn_test_app().await;
    insert_actor_fixture(&app, "live_carol").await;

    let path = "/.well-known/webfinger?resource=acct:live_carol@not-this-instance.example";
    let response = raw_request(app.address, "GET", path, &[], b"").await;

    assert_eq!(response.status, 404);

    app.cleanup().await;
}

/// Requirement 4.4, through the real mounted router: a self-domain query for
/// an actor that does not exist is reported as not found.
#[tokio::test]
async fn webfinger_through_the_real_router_reports_an_absent_actor_as_not_found() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);

    let path = format!("/.well-known/webfinger?resource=acct:live_nobody@{domain}");
    let response = raw_request(app.address, "GET", &path, &[], b"").await;

    assert_eq!(response.status, 404);

    app.cleanup().await;
}

/// Requirements 5.2, 5.3, through the real mounted router: the NodeInfo
/// document exposes exactly the minimal public stats and nothing else.
/// `federation_bootstrap_it.rs`'s own live-router NodeInfo coverage only
/// checks `software.name`; this closes the "内部情報が含まれない" (no
/// internal information) half of this task's own completion condition that
/// partial check does not.
#[tokio::test]
async fn nodeinfo_document_through_the_real_router_exposes_only_minimal_public_stats() {
    let app = spawn_test_app().await;

    let response = raw_request(app.address, "GET", "/nodeinfo/2.0", &[], b"").await;

    assert_eq!(response.status, 200);
    let body = raw_body_json(&response);
    assert_eq!(body["protocols"], json!(["activitypub"]));
    let object = body
        .as_object()
        .expect("nodeinfo document must be an object");
    let allowed_keys = ["version", "software", "protocols"];
    for key in object.keys() {
        assert!(
            allowed_keys.contains(&key.as_str()),
            "NodeInfo document through the real router must not include internal information \
             beyond the minimal public stats (Requirement 5.3); unexpected field {key:?}"
        );
    }

    app.cleanup().await;
}

/// Requirements 6.1, 6.5, 9.1, 9.4, through the real mounted router: an
/// actor's ActivityPub document carries `@context` (9.1) and its identifying
/// fields (6.1), never any owner-identifying field (6.5) beyond the
/// documented shape, and is served for the `application/ld+json` Accept
/// value (9.4's second accepted media type) -- `ap_get_outbox_it.rs`'s own
/// owner-non-exposure assertion only ever exercises `application/
/// activity+json`, and only through its own test-local router; this proves
/// both the alternate accepted media type and the live-router angle, and adds
/// the `@context` assertion no test anywhere makes for this specific
/// (real-router, HTTP-response) surface (`@context` is proven only at the
/// `ActivityPubDocumentBuilder` unit level, by
/// `src/federation/endpoints/document/tests.rs`'s own
/// `build_actor_document_includes_the_activitystreams_context`).
#[tokio::test]
async fn actor_get_through_the_real_router_never_exposes_owner_information_and_carries_context() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let alice = insert_actor_fixture(&app, "live_dave").await;

    let actor_path = urls
        .actor_url(&alice.handle)
        .replacen(&format!("https://{domain}"), "", 1);
    let response = raw_request(
        app.address,
        "GET",
        &actor_path,
        &[("Accept".to_string(), "application/ld+json".to_string())],
        b"",
    )
    .await;

    assert_eq!(
        response.status, 200,
        "application/ld+json must be accepted as an ActivityPub representation request \
         (Requirement 9.4), got: {response:?}"
    );
    let body = raw_body_json(&response);
    assert_eq!(body["id"], json!(urls.actor_url(&alice.handle)));
    assert_eq!(body["inbox"], json!(urls.inbox_url(&alice.handle)));
    assert_eq!(
        body["@context"],
        json!(ACTIVITYSTREAMS_CONTEXT),
        "the actor document served through the real router must carry @context (Requirement 9.1)"
    );

    let object = body.as_object().expect("actor document must be an object");
    let allowed_top_level_keys = [
        "@context",
        "id",
        "type",
        "preferredUsername",
        "name",
        "summary",
        "inbox",
        "outbox",
        "publicKey",
    ];
    for key in object.keys() {
        assert!(
            allowed_top_level_keys.contains(&key.as_str()),
            "the actor document served through the real router must never expose an \
             owner-identifying field beyond the documented ActivityPub shape (Requirement 6.5); \
             unexpected top-level field {key:?}"
        );
    }

    app.cleanup().await;
}

/// A downstream `OutboxSource` scoped to one specific actor's handle --
/// contributes its fixed `item` only for that actor, an empty page for any
/// other (mirrors how a real downstream spec's own per-actor-scoped source
/// would behave, e.g. "this actor's own Create/Announce activities").
struct ScopedOutboxSource {
    scope: Handle,
    item: Value,
}

impl OutboxSource for ScopedOutboxSource {
    fn outbox_page<'a>(
        &'a self,
        actor: &'a Handle,
        _page: PageCursor,
    ) -> Pin<Box<dyn Future<Output = Result<OutboxItemsPage, AppError>> + Send + 'a>> {
        let page = if *actor == self.scope {
            OutboxItemsPage {
                items: vec![self.item.clone()],
                next: None,
            }
        } else {
            OutboxItemsPage {
                items: vec![],
                next: None,
            }
        };
        Box::pin(async move { Ok(page) })
    }
}

/// Requirements 8.3, 9.1, through the real mounted router: the outbox never
/// includes an Activity that is out of the requested actor's own scope, and
/// the served `OrderedCollectionPage` carries `@context`.
/// `federation_bootstrap_it.rs`'s own downstream-registration test proves a
/// *single* actor's outbox picks up a registered source's contribution; it
/// never registers a source that is scoped to a *different* actor, so it
/// never actually exercises exclusion -- only inclusion. Neither that test
/// nor `ap_get_outbox_it.rs`'s own outbox tests assert `@context` on the
/// outbox page at all (unlike the actor document, no test anywhere proves
/// `build_outbox_page` itself stamps `@context`).
#[tokio::test]
async fn outbox_get_through_the_real_router_excludes_out_of_scope_items_and_carries_context() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "live_erin").await;
    let bob = insert_actor_fixture(&app, "live_frank").await;

    let alice_item = json!({
        "id": "https://kawasemi.webfinger-nodeinfo-it.internal/statuses/live-1",
        "type": "Create",
        "published": "2024-01-01T00:00:00Z"
    });
    app.state
        .federation()
        .outbox_sources()
        .register(Arc::new(ScopedOutboxSource {
            scope: alice.handle.clone(),
            item: alice_item.clone(),
        }));

    let ap_accept = [(
        "Accept".to_string(),
        "application/activity+json".to_string(),
    )];

    // In scope: alice's own outbox carries the item scoped to her.
    let alice_path = format!("/users/{}/outbox", alice.handle.as_str());
    let alice_response = raw_request(app.address, "GET", &alice_path, &ap_accept, b"").await;
    assert_eq!(alice_response.status, 200);
    let alice_body = raw_body_json(&alice_response);
    assert_eq!(alice_body["orderedItems"], json!([alice_item]));
    assert_eq!(
        alice_body["@context"],
        json!(ACTIVITYSTREAMS_CONTEXT),
        "the outbox page served through the real router must carry @context (Requirement 9.1)"
    );

    // Out of scope: bob's own outbox must not include alice's item -- proving
    // the outbox never includes an Activity outside the requested actor's
    // scope (Requirement 8.3), not merely that an empty registry yields an
    // empty page.
    let bob_path = format!("/users/{}/outbox", bob.handle.as_str());
    let bob_response = raw_request(app.address, "GET", &bob_path, &ap_accept, b"").await;
    assert_eq!(bob_response.status, 200);
    let bob_body = raw_body_json(&bob_response);
    assert_eq!(
        bob_body["orderedItems"],
        json!([]),
        "an Activity scoped to a different actor must never leak into this actor's outbox \
         (Requirement 8.3)"
    );

    app.cleanup().await;
}
