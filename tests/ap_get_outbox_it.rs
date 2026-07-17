//! Integration test for the ActivityPub GET and outbox endpoint handlers
//! (`.kiro/specs/federation-core/tasks.md`, task 5.2 `(P) ActivityPub GET と
//! outbox エンドポイントを実装する`; design.md's File Structure Plan names
//! `ap_get.rs`/`outbox.rs`, mirrors task 5.1's `tests/webfinger_nodeinfo_it.rs`
//! naming convention for this file).
//!
//! This task's own observable completion condition: "AP Accept で
//! activity+json が返り owner を含まず、非 AP Accept は AP 表現を返さず、
//! セキュアモードで未署名 GET が拒否され、outbox がページで返り、下流未登録
//! のオブジェクト URL は未検出、outbox は空コレクションになる統合テストが
//! 通る" (Requirements 6.1, 6.2, 6.3, 6.4, 6.6, 8.1, 8.2, 9.4).
//!
//! Neither handler is mounted on the live application router yet -- wiring
//! into `AppState`/`bootstrap`/`server` is task 5.4's job, out of this
//! task's boundary (see `src/federation/endpoints/ap_get.rs`'s/`outbox.rs`'s
//! own doc comments). Per task 5.1's established precedent (its own
//! `tests/webfinger_nodeinfo_it.rs`), this test builds a minimal, test-local
//! `axum::Router` mounting just these handlers over their own narrow state
//! types, and drives it through real HTTP-shaped `Request`/`Response`
//! values via `tower::ServiceExt::oneshot`.
//!
//! A real, Postgres-backed `ActorDirectory` is used throughout (via
//! `spawn_test_app`), matching `webfinger_nodeinfo_it.rs`'s/
//! `endpoints/document/tests.rs`'s established pattern -- `ActorDirectory`
//! has no narrow mockable port introduced anywhere in this spec. Secure-mode
//! authorized fetch is proven against the *real* `HttpSignatureVerifier`
//! (task 2.3), with a small in-file `PublicKeyResolver` test double standing
//! in for `DbFederationPublicKeyResolver` (mirrors `verifier/tests.rs`'s own
//! `MockPublicKeyResolver` convention -- that module's test-only type is
//! private to it, so this file defines its own independent equivalent), so
//! this test exercises the real cryptographic verification path this
//! task's handlers call into, not a hand-waved stub.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::routing::get;
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use time::macros::format_description;
use tower::ServiceExt;

use kawasemi::actor::keys::material::generate_keypair;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::error::AppError;
use kawasemi::federation::signatures::{
    DEFAULT_SIGNATURE_MAX_AGE, DraftCavageSuite, HttpSignatureVerifier, IncomingRequest,
    PublicKeyResolver, RemotePublicKey, SignableRequest, SignatureSuite, SignatureVerifier,
    VerifiedSigner,
};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{
    ActivityPubDocumentBuilder, ApGetState, ObjectDocumentProvider, ObjectDocumentRegistry,
    OutboxItemsPage, OutboxSource, OutboxSourceRegistry, OutboxState, PageCursor, actor_get,
    object_get, outbox_get,
};
use kawasemi::runtime::SeededRng;
use kawasemi::test_harness::{TestApp, spawn_test_app};

const TEST_DOMAIN: &str = "kawasemi.ap-get-outbox-it.internal";

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
            display_name: format!("AP GET/outbox IT {handle_str}"),
            summary: "an actor used by the ap_get/outbox integration test".to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle")
}

/// A stub `ObjectDocumentProvider` claiming every URL under `prefix`,
/// always resolving to `body` (mirrors
/// `src/federation/endpoints/document/tests.rs`'s own `StubObjectProvider`,
/// reimplemented here since that one is private to its own module).
struct StubObjectProvider {
    prefix: &'static str,
    body: Value,
}

impl ObjectDocumentProvider for StubObjectProvider {
    fn can_resolve(&self, url: &str) -> bool {
        url.starts_with(self.prefix)
    }

    fn resolve<'a>(
        &'a self,
        _url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, AppError>> + Send + 'a>> {
        Box::pin(async move { Ok(Some(self.body.clone())) })
    }
}

/// A stub `OutboxSource` that always contributes the same fixed set of
/// items (mirrors `document/tests.rs`'s own `StubOutboxSource`).
struct StubOutboxSource {
    items: Vec<Value>,
}

impl OutboxSource for StubOutboxSource {
    fn outbox_page<'a>(
        &'a self,
        _actor: &'a Handle,
        _page: PageCursor,
    ) -> Pin<Box<dyn Future<Output = Result<OutboxItemsPage, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(OutboxItemsPage {
                items: self.items.clone(),
                next: None,
            })
        })
    }
}

/// A `SignatureVerifier` that must never actually be called -- used for
/// every non-secure-mode test, where `ap_get.rs`'s own `secure_mode` gate
/// means `authorize_fetch` never runs at all.
struct UnusedVerifier;

impl SignatureVerifier for UnusedVerifier {
    async fn verify_request(&self, _req: &IncomingRequest) -> Result<VerifiedSigner, AppError> {
        unreachable!(
            "UnusedVerifier::verify_request must never be called -- these tests only use it \
             with secure_mode = false"
        )
    }
}

/// A deterministic, in-memory `PublicKeyResolver` test double resolving
/// exactly one fixed keyId (mirrors `verifier/tests.rs`'s own
/// `MockPublicKeyResolver`, independently reimplemented here since that
/// type is private to its own module).
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

/// HTTP-date (RFC 9110 IMF-fixdate) formatting, duplicated from
/// `verifier.rs`'s own private `HTTP_DATE_FORMAT`/formatting convention
/// (not exported -- see this crate's own `signer.rs`/`verifier.rs`
/// Implementation Notes for the established precedent of duplicating this
/// exact format description across modules that cannot import a private
/// one).
const HTTP_DATE_FORMAT: &[time::format_description::BorrowedFormatItem<'_>] = format_description!(
    "[weekday repr:short], [day padding:zero] [month repr:short] [year] [hour]:[minute]:[second] GMT"
);

fn format_http_date(when: OffsetDateTime) -> String {
    when.to_offset(time::UtcOffset::UTC)
        .format(HTTP_DATE_FORMAT)
        .expect("HTTP-date formatting must not fail")
}

/// RSA-SHA256/PKCS#1v1.5 padding, duplicated from `signer.rs`'s/
/// `verifier.rs`'s own private `sha256_pkcs1v15_padding` (same
/// `rsa`/`sha2` version-mismatch constraint documented there -- see this
/// crate's own `verifier.rs` doc comment, "RSA verification: same
/// hand-built `Pkcs1v15Sign` as `signer.rs`").
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

/// Generates a deterministic real RSA-2048 key pair for test fixtures
/// (mirrors `verifier/tests.rs`'s own `test_keypair` convention).
fn test_keypair(seed: u64) -> (RsaPrivateKey, String) {
    let generated =
        generate_keypair(&SeededRng::new(seed)).expect("test key generation must succeed");
    let private_key = RsaPrivateKey::from_pkcs8_pem(generated.private_key_pem.expose_secret())
        .expect("generated private key PEM must parse");
    (private_key, generated.public_key_pem)
}

/// Hand-builds a genuinely signed (draft-cavage) `HeaderMap` for a bodyless
/// GET request to `url` (Requirement 6.4's authorized fetch: "署名付き取得
/// 要求", not necessarily carrying an Activity body) -- exercises the exact
/// same `SignatureSuite` surface a real `RequestSigner` would, mirroring
/// `verifier/tests.rs`'s own `build_signed_request` but for a GET with no
/// body/digest component.
fn sign_get_request(
    url: &str,
    host: &str,
    key_id: &str,
    private_key: &RsaPrivateKey,
    when: OffsetDateTime,
) -> HeaderMap {
    let suite = DraftCavageSuite::new();

    let mut headers = HeaderMap::new();
    headers.insert(
        header::HOST,
        HeaderValue::from_str(host).expect("valid host header value"),
    );
    headers.insert(
        header::DATE,
        HeaderValue::from_str(&format_http_date(when)).expect("valid date header value"),
    );

    let signable = SignableRequest {
        method: Method::GET,
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

// ==========================================================================
// Router builders
// ==========================================================================

// Deliberately *not* one generic `fn ap_get_router<V: SignatureVerifier>(..)`:
// `actor_get::<V>`/`object_get::<V>` only satisfy axum's `Handler` bound
// (which requires their returned future to be `Send`) once `V` is a
// *concrete* type -- `SignatureVerifier::verify_request` has no `+ Send`
// bound in its own trait signature (see `ap_get.rs`'s own doc comment,
// "`ApGetState<V>`"), so the compiler can only prove that `Send`-ness by
// inspecting a concrete, monomorphized `V`'s actual async body, never for
// an as-yet-generic type parameter still being type-checked inside a
// generic helper function. Each concrete `V` this test file ever mounts a
// router with therefore gets its own small, non-generic builder function
// below.

fn ap_get_router_insecure(state: ApGetState<UnusedVerifier>) -> Router {
    Router::new()
        .route("/users/{handle}", get(actor_get::<UnusedVerifier>))
        .route("/statuses/{id}", get(object_get::<UnusedVerifier>))
        .with_state(state)
}

type TestSecureVerifier = HttpSignatureVerifier<FixedPublicKeyResolver>;

fn ap_get_router_secure(state: ApGetState<TestSecureVerifier>) -> Router {
    Router::new()
        .route("/users/{handle}", get(actor_get::<TestSecureVerifier>))
        .route("/statuses/{id}", get(object_get::<TestSecureVerifier>))
        .with_state(state)
}

fn outbox_router(state: OutboxState) -> Router {
    Router::new()
        .route("/users/{handle}/outbox", get(outbox_get))
        .with_state(state)
}

fn insecure_ap_get_state(app: &TestApp) -> ApGetState<UnusedVerifier> {
    let urls = ActorUrls::new(TEST_DOMAIN);
    ApGetState {
        directory: Arc::clone(app.actor.directory()),
        document_builder: Arc::new(ActivityPubDocumentBuilder::new(
            urls,
            Arc::clone(app.actor.directory()),
            OutboxSourceRegistry::new(),
        )),
        object_documents: Arc::new(ObjectDocumentRegistry::new()),
        domain: TEST_DOMAIN.to_string(),
        secure_mode: false,
        verifier: Arc::new(UnusedVerifier),
    }
}

async fn get_request(router: &Router, uri: &str, headers: HeaderMap) -> axum::response::Response {
    let mut builder = Request::builder().method("GET").uri(uri);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    router
        .clone()
        .oneshot(
            builder
                .body(Body::empty())
                .expect("a GET request with test headers is always well-formed"),
        )
        .await
        .expect("the router must always produce a response, never fail as a tower::Service")
}

fn ap_accept_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("application/activity+json"),
    );
    headers
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test response body must be readable");
    serde_json::from_slice(&bytes).expect("test response body must be valid JSON")
}

// ==========================================================================
// actor_get
// ==========================================================================

/// Requirements 6.1, 6.5, 9.4: an AP-Accept GET on a local actor's URL
/// returns `application/activity+json` with the actor's id/inbox/outbox,
/// and never any owner-identifying field.
#[tokio::test]
async fn actor_get_with_ap_accept_returns_activity_json_without_owner() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_it").await;
    let urls = ActorUrls::new(TEST_DOMAIN);
    let router = ap_get_router_insecure(insecure_ap_get_state(&app));

    let response = get_request(
        &router,
        &format!("/users/{}", alice.handle.as_str()),
        ap_accept_headers(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/activity+json")
    );
    let body = body_json(response).await;
    assert_eq!(body["id"], json!(urls.actor_url(&alice.handle)));
    assert_eq!(body["inbox"], json!(urls.inbox_url(&alice.handle)));
    assert_eq!(body["outbox"], json!(urls.outbox_url(&alice.handle)));

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
            "actor document must never expose an owner-identifying field beyond the documented \
             ActivityPub shape (Requirement 6.5); unexpected top-level field {key:?}"
        );
    }

    app.cleanup().await;
}

/// Requirements 6.3, 9.4: a non-AP `Accept` header must not receive the
/// ActivityPub representation.
#[tokio::test]
async fn actor_get_with_non_ap_accept_does_not_return_activity_json() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "bob_it").await;
    let router = ap_get_router_insecure(insecure_ap_get_state(&app));

    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));

    let response = get_request(
        &router,
        &format!("/users/{}", alice.handle.as_str()),
        headers,
    )
    .await;

    assert_ne!(
        response.status(),
        StatusCode::OK,
        "a non-AP Accept header must never receive the ActivityPub representation"
    );
    assert_ne!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/activity+json"),
        "a non-AP Accept header must never receive an activity+json Content-Type"
    );

    app.cleanup().await;
}

/// Requirement 6.6: a syntactically valid but unregistered handle is
/// reported as not found.
#[tokio::test]
async fn actor_get_for_an_unknown_actor_returns_not_found() {
    let app = spawn_test_app().await;
    let router = ap_get_router_insecure(insecure_ap_get_state(&app));

    let response = get_request(&router, "/users/nobody_it", ap_accept_headers()).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirement 6.4: in secure mode, an unsigned GET to a local actor's URL
/// is rejected and never receives the ActivityPub representation.
#[tokio::test]
async fn actor_get_in_secure_mode_rejects_an_unsigned_get() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "carol_it").await;

    let (_private_key, public_key_pem) = test_keypair(1);
    let resolver = FixedPublicKeyResolver {
        key: RemotePublicKey {
            key_id: "https://remote.example/users/mallory#main-key".to_string(),
            actor_uri: "https://remote.example/users/mallory".to_string(),
            public_key_pem,
        },
    };
    let verifier = HttpSignatureVerifier::new(
        Arc::new(resolver),
        app.runtime.clock.clone(),
        DEFAULT_SIGNATURE_MAX_AGE,
    );
    let state = ApGetState {
        directory: Arc::clone(app.actor.directory()),
        document_builder: Arc::new(ActivityPubDocumentBuilder::new(
            ActorUrls::new(TEST_DOMAIN),
            Arc::clone(app.actor.directory()),
            OutboxSourceRegistry::new(),
        )),
        object_documents: Arc::new(ObjectDocumentRegistry::new()),
        domain: TEST_DOMAIN.to_string(),
        secure_mode: true,
        verifier: Arc::new(verifier),
    };
    let router = ap_get_router_secure(state);

    let response = get_request(
        &router,
        &format!("/users/{}", alice.handle.as_str()),
        ap_accept_headers(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

/// Requirement 6.4: in secure mode, a validly signed GET (authorized fetch)
/// still receives the ActivityPub representation -- proving secure mode
/// gates on signature validity, not merely on being turned on.
#[tokio::test]
async fn actor_get_in_secure_mode_accepts_a_validly_signed_get() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "dave_it").await;
    let urls = ActorUrls::new(TEST_DOMAIN);
    let actor_url = urls.actor_url(&alice.handle);

    let (private_key, public_key_pem) = test_keypair(2);
    let signer_key_id = "https://remote.example/users/erin#main-key";
    let resolver = FixedPublicKeyResolver {
        key: RemotePublicKey {
            key_id: signer_key_id.to_string(),
            actor_uri: "https://remote.example/users/erin".to_string(),
            public_key_pem,
        },
    };
    let verifier = HttpSignatureVerifier::new(
        Arc::new(resolver),
        app.runtime.clock.clone(),
        DEFAULT_SIGNATURE_MAX_AGE,
    );
    let state = ApGetState {
        directory: Arc::clone(app.actor.directory()),
        document_builder: Arc::new(ActivityPubDocumentBuilder::new(
            urls,
            Arc::clone(app.actor.directory()),
            OutboxSourceRegistry::new(),
        )),
        object_documents: Arc::new(ObjectDocumentRegistry::new()),
        domain: TEST_DOMAIN.to_string(),
        secure_mode: true,
        verifier: Arc::new(verifier),
    };
    let router = ap_get_router_secure(state);

    let mut headers = sign_get_request(
        &actor_url,
        TEST_DOMAIN,
        signer_key_id,
        &private_key,
        app.runtime.clock.now(),
    );
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("application/activity+json"),
    );

    let response = get_request(
        &router,
        &format!("/users/{}", alice.handle.as_str()),
        headers,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    app.cleanup().await;
}

// ==========================================================================
// object_get
// ==========================================================================

/// Requirement 6.6: an object/collection URL with no registered
/// `ObjectDocumentProvider` is reported as not found (this task's own
/// "下流未登録のオブジェクト URL は未検出" completion condition).
#[tokio::test]
async fn object_get_with_no_registered_provider_returns_not_found() {
    let app = spawn_test_app().await;
    let router = ap_get_router_insecure(insecure_ap_get_state(&app));

    let response = get_request(&router, "/statuses/1", ap_accept_headers()).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirement 6.2: an object/collection URL a registered
/// `ObjectDocumentProvider` claims is resolved via that provider's own
/// document.
#[tokio::test]
async fn object_get_delegates_to_a_registered_provider() {
    let app = spawn_test_app().await;
    let body = json!({
        "id": format!("https://{TEST_DOMAIN}/statuses/1"),
        "type": "Note",
        "content": "hello federation"
    });
    let mut registry = ObjectDocumentRegistry::new();
    registry.register(Arc::new(StubObjectProvider {
        prefix: "https://kawasemi.ap-get-outbox-it.internal/statuses/",
        body: body.clone(),
    }));

    let state = ApGetState {
        directory: Arc::clone(app.actor.directory()),
        document_builder: Arc::new(ActivityPubDocumentBuilder::new(
            ActorUrls::new(TEST_DOMAIN),
            Arc::clone(app.actor.directory()),
            OutboxSourceRegistry::new(),
        )),
        object_documents: Arc::new(registry),
        domain: TEST_DOMAIN.to_string(),
        secure_mode: false,
        verifier: Arc::new(UnusedVerifier),
    };
    let router = ap_get_router_insecure(state);

    let response = get_request(&router, "/statuses/1", ap_accept_headers()).await;

    assert_eq!(response.status(), StatusCode::OK);
    let response_body = body_json(response).await;
    assert_eq!(response_body, body);

    app.cleanup().await;
}

/// Requirement 6.4: in secure mode, an unsigned GET to a local
/// object/collection URL is rejected and never receives the
/// `ObjectDocumentProvider`'s document -- mirrors
/// `actor_get_in_secure_mode_rejects_an_unsigned_get`, but for `object_get`'s
/// own delegation path (design.md's `#### Endpoints` Responsibilities groups
/// "アクター・オブジェクト・コレクション" together under Requirement 6.4,
/// not actors alone).
#[tokio::test]
async fn object_get_in_secure_mode_rejects_an_unsigned_get() {
    let app = spawn_test_app().await;

    let body = json!({
        "id": format!("https://{TEST_DOMAIN}/statuses/2"),
        "type": "Note",
        "content": "secure mode must gate this too"
    });
    let mut registry = ObjectDocumentRegistry::new();
    registry.register(Arc::new(StubObjectProvider {
        prefix: "https://kawasemi.ap-get-outbox-it.internal/statuses/",
        body,
    }));

    let (_private_key, public_key_pem) = test_keypair(3);
    let resolver = FixedPublicKeyResolver {
        key: RemotePublicKey {
            key_id: "https://remote.example/users/mallory#main-key".to_string(),
            actor_uri: "https://remote.example/users/mallory".to_string(),
            public_key_pem,
        },
    };
    let verifier = HttpSignatureVerifier::new(
        Arc::new(resolver),
        app.runtime.clock.clone(),
        DEFAULT_SIGNATURE_MAX_AGE,
    );
    let state = ApGetState {
        directory: Arc::clone(app.actor.directory()),
        document_builder: Arc::new(ActivityPubDocumentBuilder::new(
            ActorUrls::new(TEST_DOMAIN),
            Arc::clone(app.actor.directory()),
            OutboxSourceRegistry::new(),
        )),
        object_documents: Arc::new(registry),
        domain: TEST_DOMAIN.to_string(),
        secure_mode: true,
        verifier: Arc::new(verifier),
    };
    let router = ap_get_router_secure(state);

    let response = get_request(&router, "/statuses/2", ap_accept_headers()).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

/// Requirement 6.4: in secure mode, a validly signed GET (authorized fetch)
/// to a local object/collection URL still receives the
/// `ObjectDocumentProvider`'s document -- the `object_get` counterpart of
/// `actor_get_in_secure_mode_accepts_a_validly_signed_get`, proving secure
/// mode gates the object/collection delegation path on signature validity,
/// not merely on being turned on.
#[tokio::test]
async fn object_get_in_secure_mode_accepts_a_validly_signed_get() {
    let app = spawn_test_app().await;

    let object_url = format!("https://{TEST_DOMAIN}/statuses/3");
    let body = json!({
        "id": object_url,
        "type": "Note",
        "content": "a validly signed authorized fetch may read this"
    });
    let mut registry = ObjectDocumentRegistry::new();
    registry.register(Arc::new(StubObjectProvider {
        prefix: "https://kawasemi.ap-get-outbox-it.internal/statuses/",
        body: body.clone(),
    }));

    let (private_key, public_key_pem) = test_keypair(4);
    let signer_key_id = "https://remote.example/users/erin#main-key";
    let resolver = FixedPublicKeyResolver {
        key: RemotePublicKey {
            key_id: signer_key_id.to_string(),
            actor_uri: "https://remote.example/users/erin".to_string(),
            public_key_pem,
        },
    };
    let verifier = HttpSignatureVerifier::new(
        Arc::new(resolver),
        app.runtime.clock.clone(),
        DEFAULT_SIGNATURE_MAX_AGE,
    );
    let state = ApGetState {
        directory: Arc::clone(app.actor.directory()),
        document_builder: Arc::new(ActivityPubDocumentBuilder::new(
            ActorUrls::new(TEST_DOMAIN),
            Arc::clone(app.actor.directory()),
            OutboxSourceRegistry::new(),
        )),
        object_documents: Arc::new(registry),
        domain: TEST_DOMAIN.to_string(),
        secure_mode: true,
        verifier: Arc::new(verifier),
    };
    let router = ap_get_router_secure(state);

    let mut headers = sign_get_request(
        &object_url,
        TEST_DOMAIN,
        signer_key_id,
        &private_key,
        app.runtime.clock.now(),
    );
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("application/activity+json"),
    );

    let response = get_request(&router, "/statuses/3", headers).await;

    assert_eq!(response.status(), StatusCode::OK);
    let response_body = body_json(response).await;
    assert_eq!(response_body, body);

    app.cleanup().await;
}

// ==========================================================================
// outbox_get
// ==========================================================================

/// Requirements 8.1, 8.2: the outbox is returned as a paged
/// `OrderedCollectionPage`, sourced from registered `OutboxSource`s.
#[tokio::test]
async fn outbox_get_returns_a_page_with_registered_source_items() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "frank_it").await;
    let urls = ActorUrls::new(TEST_DOMAIN);

    let item = json!({
        "id": "https://kawasemi.ap-get-outbox-it.internal/statuses/1",
        "type": "Create",
        "published": "2024-01-01T00:00:00Z"
    });
    let mut outbox_sources = OutboxSourceRegistry::new();
    outbox_sources.register(Arc::new(StubOutboxSource {
        items: vec![item.clone()],
    }));

    let state = OutboxState {
        document_builder: Arc::new(ActivityPubDocumentBuilder::new(
            urls.clone(),
            Arc::clone(app.actor.directory()),
            outbox_sources,
        )),
    };
    let router = outbox_router(state);

    let response = get_request(
        &router,
        &format!("/users/{}/outbox", alice.handle.as_str()),
        ap_accept_headers(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["type"], json!("OrderedCollectionPage"));
    assert_eq!(body["partOf"], json!(urls.outbox_url(&alice.handle)));
    assert_eq!(body["orderedItems"], json!([item]));

    app.cleanup().await;
}

/// This task's own completion condition: while no `OutboxSource` is
/// registered, the outbox is an empty collection.
#[tokio::test]
async fn outbox_get_with_no_registered_source_returns_an_empty_collection() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "grace_it").await;

    let state = OutboxState {
        document_builder: Arc::new(ActivityPubDocumentBuilder::new(
            ActorUrls::new(TEST_DOMAIN),
            Arc::clone(app.actor.directory()),
            OutboxSourceRegistry::new(),
        )),
    };
    let router = outbox_router(state);

    let response = get_request(
        &router,
        &format!("/users/{}/outbox", alice.handle.as_str()),
        ap_accept_headers(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["orderedItems"],
        json!([]),
        "outbox must be an empty collection while no OutboxSource is registered"
    );

    app.cleanup().await;
}

/// Requirement 9.4: a non-AP `Accept` header must not receive the outbox's
/// ActivityPub representation either.
#[tokio::test]
async fn outbox_get_with_non_ap_accept_does_not_return_activity_json() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "heidi_it").await;

    let state = OutboxState {
        document_builder: Arc::new(ActivityPubDocumentBuilder::new(
            ActorUrls::new(TEST_DOMAIN),
            Arc::clone(app.actor.directory()),
            OutboxSourceRegistry::new(),
        )),
    };
    let router = outbox_router(state);

    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));

    let response = get_request(
        &router,
        &format!("/users/{}/outbox", alice.handle.as_str()),
        headers,
    )
    .await;

    assert_ne!(response.status(), StatusCode::OK);

    app.cleanup().await;
}
