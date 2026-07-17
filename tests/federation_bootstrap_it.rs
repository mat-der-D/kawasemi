//! Integration tests for federation-core's Composition Root wiring
//! (`.kiro/specs/federation-core/tasks.md`, task 5.4, `_Boundary:
//! FederationModule, Bootstrap, AppState, Config_`, Requirements 7.3, 10.1,
//! 11.1, 11.2).
//!
//! Tasks 5.1-5.3 (WebFinger/NodeInfo, AP GET/outbox, inbox/shared-inbox) were
//! each implemented and reviewed already, but every one of their own doc
//! comments says the same thing: none of them are mounted on any router yet
//! (see e.g. `src/federation/endpoints/webfinger.rs`'s "Not wired into a
//! router" section), and their own tests build a minimal test-local
//! `axum::Router` rather than exercising the real, live-serving instance.
//! This task's own job is exactly that mounting — plus starting the
//! delivery-worker/pruning background tasks and exposing the
//! downstream-registration surface (`FederationModule::object_documents`/
//! `outbox_sources`/`delivery_service`) design.md's own completion condition
//! names.
//!
//! This file proves, through the *real* mounted router
//! (`spawn_test_app`/`crate::server::build_router`, not a test-local one):
//! 1. WebFinger, NodeInfo, and ActivityPub actor GET are reachable and
//!    return correctly-shaped responses.
//! 2. A genuinely signed Activity POSTed to a local actor's inbox is
//!    accepted (`202`) and reaches `InboxService`'s own idempotency ledger
//!    (`received_activities`), proving the full receive pipeline — not just
//!    the handler function in isolation — is live behind the real router.
//! 3. `FederationModule::delivery_service()` (the port design.md's
//!    completion condition names for "下流が...配送サービスへ配送依頼できる")
//!    is reachable from outside `AppState` and genuinely delivers to both a
//!    local recipient (observed via `received_activities`, the same
//!    convergence point Requirement 10.3/10.5 requires) and a remote
//!    recipient (observed via a persisted `delivery_jobs` row — this test
//!    does not wait for the delivery worker to actually attempt a network
//!    send to a nonexistent host, only that the common-part `deliver()` call
//!    durably enqueued it, per Requirement 11.1's "依頼元の処理を配送完了ま
//!    で待たせない").
//! 4. `FederationModule::object_documents()`/`outbox_sources()` are
//!    genuinely live-mutable *after* `spawn_test_app()` has already returned
//!    a fully-serving instance: a stub provider/source registered post-hoc
//!    is observably picked up by a subsequent request through the real
//!    router — design.md's own completion condition's literal requirement,
//!    and this task's own resolution to the `AppState`-immutability-vs-
//!    `&mut self`-registration tension for these two ports (see
//!    `crate::federation::module`'s doc comment, "Downstream registration
//!    surface", for the parallel `InboundActivityDispatcher` limitation this
//!    test does *not* attempt to demonstrate, since it is not live-mutable
//!    by this task's own documented, boundary-driven design).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::http::{HeaderName, HeaderValue, Method, header};
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use time::macros::format_description;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::actor::keys::material::generate_keypair;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::Id;
use kawasemi::error::AppError;
use kawasemi::federation::signatures::{
    Digest as BodyDigest, DraftCavageSuite, SignableRequest, SignatureSuite,
};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{
    DeliveryRequest, ObjectDocumentProvider, ObjectKind, OutboxItemsPage, OutboxSource, PageCursor,
    Recipient,
};
use kawasemi::runtime::SeededRng;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// Raw HTTP plumbing (this crate has no HTTP client dependency of its own;
// mirrors `tests/api_foundation_wiring_it.rs`'s own `raw_request`/
// `RawResponse` -- duplicated here rather than shared, since integration
// tests are each their own compiled crate and cannot import from one
// another).
// ==========================================================================

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

    // A signed request's own `headers` already carries a `Host` value (the
    // one the sender's signature actually covers, e.g. this instance's
    // configured domain) -- only fall back to the loopback default when the
    // caller did not supply one, so a signed request never ends up with two
    // conflicting `Host` header lines (which would make the receiver's
    // reconstructed signing string diverge from what the sender actually
    // signed, surfacing as a spurious 401 unrelated to this crate's own
    // verification logic).
    let has_host_header = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"));
    let mut request = format!("{method} {path} HTTP/1.1\r\n");
    if !has_host_header {
        request.push_str("Host: 127.0.0.1\r\n");
    }
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

fn body_json(response: &RawResponse) -> Value {
    serde_json::from_slice(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

impl std::fmt::Debug for RawResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawResponse")
            .field("status", &self.status)
            .field("body", &String::from_utf8_lossy(&self.body))
            .finish()
    }
}

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
            display_name: format!("Federation Bootstrap IT {handle_str}"),
            summary: "an actor used by the federation bootstrap wiring integration test"
                .to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle")
}

fn test_domain(app: &TestApp) -> String {
    app.state.config().server.domain.clone()
}

// ==========================================================================
// Signing helpers (mirrors `tests/inbox_it.rs`'s own identical helpers,
// independently duplicated here for the same "each integration test file is
// its own crate" reason).
// ==========================================================================

fn test_keypair(seed: u64) -> (RsaPrivateKey, String) {
    let generated =
        generate_keypair(&SeededRng::new(seed)).expect("test key generation must succeed");
    let private_key = RsaPrivateKey::from_pkcs8_pem(generated.private_key_pem.expose_secret())
        .expect("generated private key PEM must parse");
    (private_key, generated.public_key_pem)
}

const HTTP_DATE_FORMAT: &[time::format_description::BorrowedFormatItem<'_>] = format_description!(
    "[weekday repr:short], [day padding:zero] [month repr:short] [year] [hour]:[minute]:[second] GMT"
);

fn format_http_date(when: OffsetDateTime) -> String {
    when.to_offset(time::UtcOffset::UTC)
        .format(HTTP_DATE_FORMAT)
        .expect("HTTP-date formatting must not fail")
}

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

/// Hand-builds a genuinely signed (draft-cavage) header list for a POST of
/// `body` to `url`, mirroring `tests/inbox_it.rs`'s own `sign_post_request`.
fn sign_post_request(
    url: &str,
    host: &str,
    key_id: &str,
    private_key: &RsaPrivateKey,
    when: OffsetDateTime,
    body: &[u8],
) -> Vec<(String, String)> {
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
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/activity+json"),
    );

    headers
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                value
                    .to_str()
                    .expect("test header values are ASCII")
                    .to_string(),
            )
        })
        .collect()
}

/// Inserts a pre-cached remote public key (mirrors this file's own doc
/// comment, item 2: avoids a real network fetch, exercising the cache-hit
/// path `DbFederationPublicKeyResolver` already has dedicated unit/
/// integration coverage for at task 2.1).
async fn seed_remote_public_key(app: &TestApp, key_id: &str, actor_uri: &str, pem: &str) {
    sqlx::query(
        "INSERT INTO remote_public_keys (key_id, actor_uri, public_key_pem, fetched_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(key_id)
    .bind(actor_uri)
    .bind(pem)
    .bind(app.runtime.clock.now())
    .execute(&app.pool)
    .await
    .expect("seeding a cached remote public key must succeed");
}

async fn received_activity_exists(app: &TestApp, activity_id: &str) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT activity_id FROM received_activities WHERE activity_id = $1")
            .bind(activity_id)
            .fetch_optional(&app.pool)
            .await
            .expect("querying received_activities must succeed");
    row.is_some()
}

async fn delivery_job_exists(app: &TestApp, target_inbox: &str, activity_id: &str) -> bool {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM delivery_jobs WHERE target_inbox = $1 AND activity->>'id' = $2",
    )
    .bind(target_inbox)
    .bind(activity_id)
    .fetch_optional(&app.pool)
    .await
    .expect("querying delivery_jobs must succeed");
    row.is_some()
}

// ==========================================================================
// (1) WebFinger / NodeInfo / actor GET reachable through the real router
// ==========================================================================

#[tokio::test]
async fn webfinger_nodeinfo_and_actor_get_are_reachable_through_the_real_router() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let alice = insert_actor_fixture(&app, "wf_alice").await;

    // WebFinger.
    let resource = format!("acct:{}@{}", alice.handle.as_str(), domain);
    let webfinger_path = format!("/.well-known/webfinger?resource={}", urlencode(&resource));
    let webfinger_response = raw_request(app.address, "GET", &webfinger_path, &[], b"").await;
    assert_eq!(
        webfinger_response.status, 200,
        "WebFinger must be reachable through the real mounted router, got: {webfinger_response:?}"
    );
    let webfinger_body = body_json(&webfinger_response);
    assert_eq!(webfinger_body["subject"], json!(resource));
    let self_link = webfinger_body["links"][0]["href"]
        .as_str()
        .expect("WebFinger response must carry a self link href");
    assert_eq!(self_link, urls.actor_url(&alice.handle));

    // NodeInfo discovery + document.
    let discovery_response =
        raw_request(app.address, "GET", "/.well-known/nodeinfo", &[], b"").await;
    assert_eq!(discovery_response.status, 200);
    let nodeinfo_response = raw_request(app.address, "GET", "/nodeinfo/2.0", &[], b"").await;
    assert_eq!(nodeinfo_response.status, 200);
    let nodeinfo_body = body_json(&nodeinfo_response);
    assert_eq!(nodeinfo_body["software"]["name"], json!("kawasemi"));

    // Actor GET.
    let actor_path = urls
        .actor_url(&alice.handle)
        .replacen(&format!("https://{domain}"), "", 1);
    let actor_response = raw_request(
        app.address,
        "GET",
        &actor_path,
        &[(
            "Accept".to_string(),
            "application/activity+json".to_string(),
        )],
        b"",
    )
    .await;
    assert_eq!(
        actor_response.status, 200,
        "actor GET must be reachable through the real mounted router, got: {actor_response:?}"
    );
    let actor_body = body_json(&actor_response);
    assert_eq!(actor_body["id"], json!(urls.actor_url(&alice.handle)));
    assert_eq!(actor_body["inbox"], json!(urls.inbox_url(&alice.handle)));
    assert!(
        actor_body.get("publicKey").is_some(),
        "a freshly created actor must have an active signing key, so its actor document must \
         carry a publicKey: {actor_body}"
    );

    app.cleanup().await;
}

/// Minimal, purpose-built percent-encoder (this crate has no URL-encoding
/// dependency; mirrors `tests/api_foundation_wiring_it.rs`'s own `url_encode`).
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

// ==========================================================================
// (2) A genuinely signed Activity posted to the real, mounted inbox route
// ==========================================================================

#[tokio::test]
async fn signed_activity_posted_to_the_real_inbox_route_is_accepted_and_recorded() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let alice = insert_actor_fixture(&app, "inbox_alice").await;

    let (private_key, public_key_pem) = test_keypair(1);
    let signer_key_id = "https://remote.example/users/mallory#main-key";
    let signer_actor_uri = "https://remote.example/users/mallory";
    seed_remote_public_key(&app, signer_key_id, signer_actor_uri, &public_key_pem).await;

    let inbox_url = urls.inbox_url(&alice.handle);
    let activity_id = "https://remote.example/activities/bootstrap-it-1";
    let body = json!({ "id": activity_id, "type": "Follow" })
        .to_string()
        .into_bytes();
    let headers = sign_post_request(
        &inbox_url,
        &domain,
        signer_key_id,
        &private_key,
        app.runtime.clock.now(),
        &body,
    );

    let path = format!("/users/{}/inbox", alice.handle.as_str());
    let response = raw_request(app.address, "POST", &path, &headers, &body).await;
    assert_eq!(
        response.status, 202,
        "a validly signed Activity POSTed to the real mounted inbox route must be accepted, \
         got: {response:?}"
    );

    assert!(
        received_activity_exists(&app, activity_id).await,
        "InboxService's own idempotency ledger must record the accepted Activity id"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) DeliveryService, reached through FederationModule, delivers to both a
// local and a remote recipient
// ==========================================================================

#[tokio::test]
async fn delivery_service_reached_through_federation_module_delivers_locally_and_enqueues_remotely()
{
    let app = spawn_test_app().await;
    let sender = insert_actor_fixture(&app, "deliver_sender").await;
    let recipient = insert_actor_fixture(&app, "deliver_recipient").await;

    let activity_id = "https://kawasemi.bootstrap-it.internal/activities/deliver-1";
    let remote_inbox = "https://remote.example/users/nobody/inbox";
    let request = DeliveryRequest {
        activity: json!({ "id": activity_id, "type": "Follow" }),
        sender: sender.handle.clone(),
        recipients: vec![
            Recipient::Local(recipient.handle.clone()),
            Recipient::Remote {
                inbox: remote_inbox.to_string(),
                shared_inbox: None,
            },
        ],
    };

    app.state
        .federation()
        .delivery_service()
        .deliver(request)
        .await
        .expect(
            "deliver() must succeed for a mix of a resolvable local recipient and an already-\
             known remote inbox URL",
        );

    assert!(
        received_activity_exists(&app, activity_id).await,
        "the local recipient must have been reached in-process through InboxService::process_local \
         (Requirement 10.3)"
    );
    assert!(
        delivery_job_exists(&app, remote_inbox, activity_id).await,
        "the remote recipient must have been durably enqueued onto the real DeliveryQueue \
         (Requirement 11.1) -- this assertion does not wait for the delivery worker to actually \
         attempt (and fail) a network send to a nonexistent host, only that deliver() itself \
         returned after persisting the job"
    );

    app.cleanup().await;
}

// ==========================================================================
// (4) Downstream registration after startup is observed by a later request
// ==========================================================================

struct StubObjectProvider {
    prefix: String,
    body: Value,
}

impl ObjectDocumentProvider for StubObjectProvider {
    fn can_resolve(&self, url: &str) -> bool {
        url.starts_with(&self.prefix)
    }

    fn resolve<'a>(
        &'a self,
        _url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, AppError>> + Send + 'a>> {
        Box::pin(async move { Ok(Some(self.body.clone())) })
    }
}

struct StubOutboxSource {
    item: Value,
}

impl OutboxSource for StubOutboxSource {
    fn outbox_page<'a>(
        &'a self,
        _actor: &'a Handle,
        _page: PageCursor,
    ) -> Pin<Box<dyn Future<Output = Result<OutboxItemsPage, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(OutboxItemsPage {
                items: vec![self.item.clone()],
                next: None,
            })
        })
    }
}

#[tokio::test]
async fn downstream_registration_after_startup_is_observed_by_a_subsequent_request() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let alice = insert_actor_fixture(&app, "registration_alice").await;

    // --- ObjectDocumentProvider: 404 before registration, 200 after ---
    let object_url = urls.object_url(ObjectKind::new("statuses"), Id::from_i64(1));
    let object_path = object_url.replacen(&format!("https://{domain}"), "", 1);

    let before = raw_request(
        app.address,
        "GET",
        &object_path,
        &[(
            "Accept".to_string(),
            "application/activity+json".to_string(),
        )],
        b"",
    )
    .await;
    assert_eq!(
        before.status, 404,
        "an object URL with no registered ObjectDocumentProvider must be not-found, got: {before:?}"
    );

    let stub_body =
        json!({ "id": object_url, "type": "Note", "content": "hello from a downstream spec" });
    app.state
        .federation()
        .object_documents()
        .register(Arc::new(StubObjectProvider {
            prefix: format!("https://{domain}/statuses/"),
            body: stub_body.clone(),
        }));

    let after = raw_request(
        app.address,
        "GET",
        &object_path,
        &[(
            "Accept".to_string(),
            "application/activity+json".to_string(),
        )],
        b"",
    )
    .await;
    assert_eq!(
        after.status, 200,
        "a provider registered AFTER spawn_test_app() returned must be observed by a subsequent \
         request through the real router, got: {after:?}"
    );
    assert_eq!(body_json(&after), stub_body);

    // --- OutboxSource: empty before registration, populated after ---
    let outbox_path = format!("/users/{}/outbox", alice.handle.as_str());
    let outbox_before = raw_request(
        app.address,
        "GET",
        &outbox_path,
        &[(
            "Accept".to_string(),
            "application/activity+json".to_string(),
        )],
        b"",
    )
    .await;
    assert_eq!(outbox_before.status, 200);
    assert_eq!(body_json(&outbox_before)["orderedItems"], json!([]));

    let outbox_item =
        json!({ "type": "Create", "id": "https://kawasemi.bootstrap-it.internal/statuses/1" });
    app.state
        .federation()
        .outbox_sources()
        .register(Arc::new(StubOutboxSource {
            item: outbox_item.clone(),
        }));

    let outbox_after = raw_request(
        app.address,
        "GET",
        &outbox_path,
        &[(
            "Accept".to_string(),
            "application/activity+json".to_string(),
        )],
        b"",
    )
    .await;
    assert_eq!(outbox_after.status, 200);
    let after_items = body_json(&outbox_after)["orderedItems"].clone();
    assert_eq!(
        after_items,
        json!([outbox_item]),
        "a source registered AFTER spawn_test_app() returned must be observed by a subsequent \
         outbox request through the real router"
    );

    app.cleanup().await;
}
