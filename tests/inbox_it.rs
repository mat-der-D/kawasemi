//! Integration test for the inbox / shared-inbox POST endpoint handlers
//! (`.kiro/specs/federation-core/tasks.md`, task 5.3 `inbox / shared inbox
//! エンドポイントを実装する`; design.md's File Structure Plan names
//! `inbox.rs`, mirrors task 5.1/5.2's `tests/webfinger_nodeinfo_it.rs`/
//! `tests/ap_get_outbox_it.rs` naming convention for this file).
//!
//! This task's own observable completion condition: "署名付き Activity が
//! 202 で受理されディスパッチされ、検証失敗が認証失敗応答になり、アクター
//! 個別 inbox と shared inbox のそれぞれで異なる宛先コンテキストが
//! `BlockPolicy` に渡ることが確認できる統合テストが通る" (Requirements
//! 7.1, 7.2), plus Requirement 7.4 (duplicate delivery is acked without
//! re-dispatch) named directly in this task's own bullet text.
//!
//! Neither handler is mounted on the live application router yet -- wiring
//! into `AppState`/`bootstrap`/`server` is task 5.4's job, out of this
//! task's boundary (see `src/federation/endpoints/inbox.rs`'s own doc
//! comment). Per task 5.1/5.2's established precedent, this test builds a
//! minimal, test-local `axum::Router` mounting just these two handlers over
//! their own narrow state type, and drives it through real HTTP-shaped
//! `Request`/`Response` values via `tower::ServiceExt::oneshot`.
//!
//! Secure signature verification is proven against the *real*
//! `HttpSignatureVerifier` (task 2.3), with a small in-file
//! `PublicKeyResolver` test double standing in for
//! `DbFederationPublicKeyResolver` (mirrors `tests/ap_get_outbox_it.rs`'s
//! own `FixedPublicKeyResolver` convention -- that type is private to its
//! own file, so this file defines its own independent equivalent). The
//! dedup ledger (`D`) uses the real, Postgres-backed
//! `DbReceivedActivityStore` (via `spawn_test_app`), matching this task's
//! own Requirement 7.4 completion condition against genuine dedup state
//! rather than a hand-rolled substitute. `BlockPolicy` (`B`) uses a small
//! in-file recording double so this test can assert on exactly which
//! `LocalRecipientContext` variant each route passes -- this spec's own
//! `BlockPolicy` has no such introspection built in (`NoopBlockPolicy`
//! never records anything), and this is precisely this task's own
//! observable completion condition to prove.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::routing::post;
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use time::macros::format_description;
use tower::ServiceExt;

use kawasemi::actor::keys::material::generate_keypair;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::error::AppError;
use kawasemi::federation::inbound::DEFAULT_RECEIVED_ACTIVITY_RETENTION;
use kawasemi::federation::signatures::{
    Digest as BodyDigest, DraftCavageSuite, HttpSignatureVerifier, PublicKeyResolver,
    RemotePublicKey, SignableRequest, SignatureSuite,
};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{
    BlockPolicy, DbReceivedActivityStore, HandleOutcome, InboundActivityDispatcher,
    InboundActivityHandler, InboundContext, InboxService, InboxState, LocalRecipientContext,
    ParsedActivity, actor_inbox, shared_inbox,
};
use kawasemi::runtime::SeededRng;
use kawasemi::test_harness::{TestApp, spawn_test_app};

const TEST_DOMAIN: &str = "kawasemi.inbox-it.internal";

// ==========================================================================
// Fixtures
// ==========================================================================

async fn insert_actor_fixture(app: &TestApp, handle_str: &str) -> kawasemi::actor::LocalActor {
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
            display_name: format!("Inbox IT {handle_str}"),
            summary: "an actor used by the inbox integration test".to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle")
}

// ==========================================================================
// Test doubles
// ==========================================================================

/// A deterministic, in-memory `PublicKeyResolver` test double resolving
/// exactly one fixed keyId (mirrors `tests/ap_get_outbox_it.rs`'s own
/// `FixedPublicKeyResolver`, independently reimplemented here since that
/// type is private to its own file).
struct FixedPublicKeyResolver {
    key: RemotePublicKey,
}

impl PublicKeyResolver for FixedPublicKeyResolver {
    async fn resolve_public_key(
        &self,
        key_id: &str,
        _force: bool,
    ) -> Result<RemotePublicKey, AppError> {
        if key_id == self.key.key_id {
            Ok(self.key.clone())
        } else {
            Err(AppError::client(
                StatusCode::NOT_FOUND,
                "unknown test keyId",
            ))
        }
    }
}

/// A `BlockPolicy` test double recording every `LocalRecipientContext` it
/// was called with (this task's own observable completion condition:
/// "アクター個別 inbox と shared inbox のそれぞれで異なる宛先コンテキスト
/// が `BlockPolicy` に渡ることが確認できる") and optionally blocking a
/// fixed set of signer actor URIs.
#[derive(Default)]
struct RecordingBlockPolicyState {
    calls: Vec<LocalRecipientContext>,
    blocked_actor_uris: HashSet<String>,
}

struct RecordingBlockPolicy {
    state: Arc<Mutex<RecordingBlockPolicyState>>,
}

impl BlockPolicy for RecordingBlockPolicy {
    async fn is_blocked(
        &self,
        actor_uri: &str,
        local_recipient: LocalRecipientContext,
    ) -> Result<bool, AppError> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(local_recipient);
        Ok(state.blocked_actor_uris.contains(actor_uri))
    }
}

/// A counting `InboundActivityHandler` stub proving dispatch actually ran
/// (mirrors `src/federation/inbound/service/tests.rs`'s own
/// `CountingHandler`, independently reimplemented here since that type is
/// private to its own module).
struct CountingHandler {
    types: Vec<&'static str>,
    invocations: Arc<AtomicUsize>,
}

impl InboundActivityHandler for CountingHandler {
    fn activity_types(&self) -> &[&str] {
        &self.types
    }

    fn handle<'a>(
        &'a self,
        _activity: &'a ParsedActivity,
        _ctx: &'a InboundContext,
    ) -> Pin<Box<dyn Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(HandleOutcome::Handled)
        })
    }
}

// ==========================================================================
// Signing helpers
// ==========================================================================

/// Generates a deterministic real RSA-2048 key pair for test fixtures
/// (mirrors `tests/ap_get_outbox_it.rs`'s own `test_keypair`).
fn test_keypair(seed: u64) -> (RsaPrivateKey, String) {
    let generated =
        generate_keypair(&SeededRng::new(seed)).expect("test key generation must succeed");
    let private_key = RsaPrivateKey::from_pkcs8_pem(generated.private_key_pem.expose_secret())
        .expect("generated private key PEM must parse");
    (private_key, generated.public_key_pem)
}

/// HTTP-date (RFC 9110 IMF-fixdate) formatting, duplicated from
/// `verifier.rs`'s/`tests/ap_get_outbox_it.rs`'s own private
/// `HTTP_DATE_FORMAT`/formatting convention (not exported -- established
/// precedent of duplicating this exact format description across modules
/// that cannot import a private one).
const HTTP_DATE_FORMAT: &[time::format_description::BorrowedFormatItem<'_>] = format_description!(
    "[weekday repr:short], [day padding:zero] [month repr:short] [year] [hour]:[minute]:[second] GMT"
);

fn format_http_date(when: OffsetDateTime) -> String {
    when.to_offset(time::UtcOffset::UTC)
        .format(HTTP_DATE_FORMAT)
        .expect("HTTP-date formatting must not fail")
}

/// RSA-SHA256/PKCS#1v1.5 padding, duplicated from `signer.rs`'s/
/// `verifier.rs`'s own private `sha256_pkcs1v15_padding` (same `rsa`/`sha2`
/// version-mismatch constraint documented there).
const SHA256_PKCS1V15_PREFIX: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

fn sha256_pkcs1v15_padding() -> rsa::Pkcs1v15Sign {
    rsa::Pkcs1v15Sign {
        hash_len: Some(32),
        prefix: SHA256_PKCS1V15_PREFIX.to_vec().into_boxed_slice(),
    }
}

/// Hand-builds a genuinely signed (draft-cavage) `HeaderMap` for a POST of
/// `body` to `url` (the canonical inbox/shared-inbox URL this handler will
/// reconstruct server-side -- see `inbox.rs`'s own doc comment,
/// "Destination-context construction") -- exercises the exact same
/// `SignatureSuite` surface a real `RequestSigner` would, mirroring
/// `tests/ap_get_outbox_it.rs`'s own `sign_get_request` but for a POST with
/// a body/`Digest` component.
fn sign_post_request(
    url: &str,
    host: &str,
    key_id: &str,
    private_key: &RsaPrivateKey,
    when: OffsetDateTime,
    body: &[u8],
) -> axum::http::HeaderMap {
    let suite = DraftCavageSuite::new();

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        header::HOST,
        HeaderValue::from_str(host).expect("valid host header value"),
    );
    headers.insert(
        header::DATE,
        HeaderValue::from_str(&format_http_date(when)).expect("valid date header value"),
    );
    headers.insert(
        HeaderName::from_static("digest"),
        HeaderValue::from_str(&BodyDigest::compute(body).header_value())
            .expect("valid digest header value"),
    );

    let signable = SignableRequest {
        method: Method::POST,
        url: url.to_string(),
        key_id: key_id.to_string(),
        headers: headers.clone(),
    };
    let signing_input = suite.build_signing_input(&signable);
    let hashed = Sha256::digest(signing_input.signing_string.as_bytes());
    let signature = private_key
        .sign(sha256_pkcs1v15_padding(), hashed.as_slice())
        .expect("test signing must succeed");

    for (name, value) in suite.assemble_headers(key_id, &signature, &signing_input) {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
            HeaderValue::from_str(&value).expect("valid header value"),
        );
    }
    headers
}

fn activity_body(id: &str, activity_type: &str) -> Vec<u8> {
    json!({ "id": id, "type": activity_type })
        .to_string()
        .into_bytes()
}

// ==========================================================================
// Router / harness plumbing
// ==========================================================================

type TestVerifier = HttpSignatureVerifier<FixedPublicKeyResolver>;
type TestInboxService = InboxService<TestVerifier, RecordingBlockPolicy, DbReceivedActivityStore>;
type TestInboxState = InboxState<TestVerifier, RecordingBlockPolicy, DbReceivedActivityStore>;

fn inbox_router(state: TestInboxState) -> Router {
    Router::new()
        .route(
            "/users/{handle}/inbox",
            post(actor_inbox::<TestVerifier, RecordingBlockPolicy, DbReceivedActivityStore>),
        )
        .route(
            "/inbox",
            post(shared_inbox::<TestVerifier, RecordingBlockPolicy, DbReceivedActivityStore>),
        )
        .with_state(state)
}

/// Assembles a full, real `InboxService` (real `HttpSignatureVerifier` over
/// a fixed test keypair, the recording `BlockPolicy` double, the real
/// Postgres-backed `DbReceivedActivityStore`, and a dispatcher pre-loaded
/// with `handler`) plus the `InboxState`/`ActorUrls` this task's handlers
/// need, and the shared block-policy/dispatch-count handles the test itself
/// inspects afterward.
struct Harness {
    router: Router,
    urls: ActorUrls,
    block_state: Arc<Mutex<RecordingBlockPolicyState>>,
    invocations: Arc<AtomicUsize>,
    signer_key_id: String,
    private_key: RsaPrivateKey,
}

fn build_harness(app: &TestApp, blocked_actor_uris: &[&str], key_seed: u64) -> Harness {
    let urls = ActorUrls::new(TEST_DOMAIN);

    let (private_key, public_key_pem) = test_keypair(key_seed);
    let signer_key_id = "https://remote.example/users/mallory#main-key".to_string();
    let resolver = FixedPublicKeyResolver {
        key: RemotePublicKey {
            key_id: signer_key_id.clone(),
            actor_uri: "https://remote.example/users/mallory".to_string(),
            public_key_pem,
        },
    };
    let verifier = HttpSignatureVerifier::new(
        Arc::new(resolver),
        app.runtime.clock.clone(),
        kawasemi::federation::signatures::DEFAULT_SIGNATURE_MAX_AGE,
    );

    let block_state = Arc::new(Mutex::new(RecordingBlockPolicyState {
        calls: Vec::new(),
        blocked_actor_uris: blocked_actor_uris.iter().map(|s| s.to_string()).collect(),
    }));
    let block_policy = RecordingBlockPolicy {
        state: Arc::clone(&block_state),
    };

    let dedup = DbReceivedActivityStore::new(
        app.pool.clone(),
        app.runtime.clock.clone(),
        DEFAULT_RECEIVED_ACTIVITY_RETENTION,
    );

    let invocations = Arc::new(AtomicUsize::new(0));
    let mut dispatcher = InboundActivityDispatcher::new();
    dispatcher.register(Arc::new(CountingHandler {
        types: vec!["Follow"],
        invocations: Arc::clone(&invocations),
    }));

    let service: TestInboxService =
        InboxService::new(verifier, block_policy, dedup, dispatcher, urls.clone());

    let state = InboxState {
        inbox: Arc::new(service),
        urls: urls.clone(),
    };

    Harness {
        router: inbox_router(state),
        urls,
        block_state,
        invocations,
        signer_key_id,
        private_key,
    }
}

async fn post_signed(
    router: &Router,
    path: &str,
    url: &str,
    host: &str,
    harness: &Harness,
    when: OffsetDateTime,
    body: Vec<u8>,
) -> axum::response::Response {
    let mut headers = sign_post_request(
        url,
        host,
        &harness.signer_key_id,
        &harness.private_key,
        when,
        &body,
    );
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/activity+json"),
    );

    let mut builder = Request::builder().method("POST").uri(path);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    router
        .clone()
        .oneshot(
            builder
                .body(Body::from(body))
                .expect("a well-formed signed POST request"),
        )
        .await
        .expect("the router must always produce a response, never fail as a tower::Service")
}

// ==========================================================================
// Tests
// ==========================================================================

/// Requirements 7.1, 7.3: a validly signed Activity POSTed to a local
/// actor's per-actor inbox is accepted (`202`) and actually dispatched.
#[tokio::test]
async fn actor_inbox_accepts_and_dispatches_a_validly_signed_activity() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_it").await;
    let harness = build_harness(&app, &[], 1);

    let url = harness.urls.inbox_url(&alice.handle);
    let body = activity_body("https://remote.example/activities/1", "Follow");
    let response = post_signed(
        &harness.router,
        &format!("/users/{}/inbox", alice.handle.as_str()),
        &url,
        TEST_DOMAIN,
        &harness,
        app.runtime.clock.now(),
        body,
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        harness.invocations.load(Ordering::SeqCst),
        1,
        "a fresh, allowed Activity must be dispatched exactly once (Requirement 7.3)"
    );

    app.cleanup().await;
}

/// Requirement 7.2: a POST with an invalid/unverifiable signature is
/// rejected as an authentication failure, never accepted.
#[tokio::test]
async fn actor_inbox_rejects_an_invalid_signature_with_auth_failure() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "bob_it").await;
    let harness = build_harness(&app, &[], 2);

    let url = harness.urls.inbox_url(&alice.handle);
    let body = activity_body("https://remote.example/activities/2", "Follow");
    let mut headers = sign_post_request(
        &url,
        TEST_DOMAIN,
        &harness.signer_key_id,
        &harness.private_key,
        app.runtime.clock.now(),
        &body,
    );
    // Tamper with the signature header after signing so the receiver's
    // recomputed signing string no longer matches (Requirement 2.6's
    // "改ざん" bucket, surfaced here as Requirement 7.2's auth failure).
    headers.insert(
        HeaderName::from_static("signature"),
        HeaderValue::from_static(
            "keyId=\"https://remote.example/users/mallory#main-key\",algorithm=\"rsa-sha256\",\
             headers=\"(request-target) host date digest\",signature=\"dGFtcGVyZWQ=\"",
        ),
    );
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/activity+json"),
    );

    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/users/{}/inbox", alice.handle.as_str()));
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    let response = harness
        .router
        .clone()
        .oneshot(
            builder
                .body(Body::from(body))
                .expect("a well-formed (if tampered) POST request"),
        )
        .await
        .expect("the router must always produce a response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        harness.invocations.load(Ordering::SeqCst),
        0,
        "an unverifiable signature must never reach dispatch"
    );

    app.cleanup().await;
}

/// This task's own headline observable completion condition: the
/// per-actor inbox and the shared inbox pass different
/// `LocalRecipientContext` variants to `BlockPolicy`.
#[tokio::test]
async fn actor_inbox_and_shared_inbox_pass_different_local_recipient_context_to_block_policy() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "carol_it").await;
    let harness = build_harness(&app, &[], 3);

    let actor_url = harness.urls.inbox_url(&alice.handle);
    let actor_body = activity_body("https://remote.example/activities/3", "Follow");
    let actor_response = post_signed(
        &harness.router,
        &format!("/users/{}/inbox", alice.handle.as_str()),
        &actor_url,
        TEST_DOMAIN,
        &harness,
        app.runtime.clock.now(),
        actor_body,
    )
    .await;
    assert_eq!(actor_response.status(), StatusCode::ACCEPTED);

    let shared_url = harness.urls.shared_inbox_url();
    let shared_body = activity_body("https://remote.example/activities/4", "Follow");
    let shared_response = post_signed(
        &harness.router,
        "/inbox",
        &shared_url,
        TEST_DOMAIN,
        &harness,
        app.runtime.clock.now(),
        shared_body,
    )
    .await;
    assert_eq!(shared_response.status(), StatusCode::ACCEPTED);

    let calls = harness.block_state.lock().unwrap().calls.clone();
    assert_eq!(
        calls.len(),
        2,
        "both deliveries must have queried BlockPolicy"
    );
    assert_eq!(
        calls[0],
        LocalRecipientContext::Actor {
            actor_uri: harness.urls.actor_url(&alice.handle)
        },
        "the per-actor inbox must pass an Actor context naming the destination local actor"
    );
    assert_eq!(
        calls[1],
        LocalRecipientContext::SharedInbox,
        "the shared inbox must pass a SharedInbox context (destination unresolved)"
    );

    app.cleanup().await;
}

/// Requirement 7.4: a repeated delivery of the same Activity id is not
/// reprocessed (dispatch does not run again) but still receives a
/// successful receipt response.
#[tokio::test]
async fn duplicate_activity_delivery_is_acked_without_reprocessing() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "dave_it").await;
    let harness = build_harness(&app, &[], 4);

    let url = harness.urls.inbox_url(&alice.handle);
    let body = activity_body("https://remote.example/activities/dup-1", "Follow");
    let when = app.runtime.clock.now();

    let first = post_signed(
        &harness.router,
        &format!("/users/{}/inbox", alice.handle.as_str()),
        &url,
        TEST_DOMAIN,
        &harness,
        when,
        body.clone(),
    )
    .await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert_eq!(harness.invocations.load(Ordering::SeqCst), 1);

    let second = post_signed(
        &harness.router,
        &format!("/users/{}/inbox", alice.handle.as_str()),
        &url,
        TEST_DOMAIN,
        &harness,
        when,
        body,
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::ACCEPTED,
        "a duplicate delivery must still be acked as a successful receipt (Requirement 7.4)"
    );
    assert_eq!(
        harness.invocations.load(Ordering::SeqCst),
        1,
        "a duplicate delivery must not be re-dispatched (Requirement 7.4)"
    );

    app.cleanup().await;
}

/// Requirement 7.2: a POST with no body at all is rejected as malformed,
/// never dispatched.
#[tokio::test]
async fn actor_inbox_rejects_a_bodyless_post() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "erin_it").await;
    let harness = build_harness(&app, &[], 5);

    let url = harness.urls.inbox_url(&alice.handle);
    let headers = sign_post_request(
        &url,
        TEST_DOMAIN,
        &harness.signer_key_id,
        &harness.private_key,
        app.runtime.clock.now(),
        &[],
    );

    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/users/{}/inbox", alice.handle.as_str()));
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    let response = harness
        .router
        .clone()
        .oneshot(
            builder
                .body(Body::empty())
                .expect("a well-formed bodyless POST request"),
        )
        .await
        .expect("the router must always produce a response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(harness.invocations.load(Ordering::SeqCst), 0);

    app.cleanup().await;
}
