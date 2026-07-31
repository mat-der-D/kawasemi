//! Integration tests for social-graph's inbound Activity processing
//! (`.kiro/specs/social-graph/tasks.md`, task 6.2 "受信処理・署名拒否・
//! プロバイダ統合テスト", `_Boundary: InboundHandler, BlockPolicyImpl,
//! RelProviderImpl_`), driven through the *real*, `spawn_test_app`-booted
//! application's real, HTTP-Signature-verified `/users/{handle}/inbox`
//! route — never a test-local router or a direct call into
//! `SocialGraphInboundHandler`'s own internals.
//!
//! design.md's File Structure Plan names this exact filename
//! (`inbound_activities_it.rs`: "受信 Follow/Accept/Reject/Block/Undo の状態
//! 遷移・冪等（統合）") and its own Testing Strategy bullet ("受信 Activity:
//! 受信 Follow（承認要否分岐）・Accept/Reject・Block・Undo(Follow/Block) の
//! 状態遷移と冪等（7.2–7.7, 2.5, 2.6）").
//!
//! Covers Requirements 7.2, 7.3, 7.4, 7.5, 7.6, 7.7 (this task's own assigned
//! subset of the "受信処理" bucket; 6.x/8.x are `tests/block_policy_it.rs`'s/
//! `tests/relationship_provider_it.rs`'s own assigned subsets of this same
//! task, per its own `tasks.md` bullet naming three separate files).
//!
//! ## Mechanics: real HTTP-Signature-signed POSTs against the real mounted
//! inbox route (mirrors `tests/federation_bootstrap_it.rs`'s own
//! established "signed_activity_posted_to_the_real_inbox_route..." pattern,
//! not `tests/inbox_it.rs`'s test-local-router variant)
//! `tests/federation_bootstrap_it.rs` (federation-core, task 5.4) already
//! proved a genuinely signed Activity reaches the real, bootstrap-wired
//! `InboxService` end to end. This file reuses that exact
//! sign-then-POST mechanism (`sign_post_request`/`test_keypair`/
//! `seed_remote_public_key`, each independently duplicated here per this
//! crate's established "each integration test file is its own compiled
//! crate" convention) but drives it against `SocialGraphInboundHandler`'s
//! own registered Follow/Accept/Reject/Block/Undo semantics (task 5.2 already
//! registered this handler against the live `InboundActivityDispatcher`) —
//! not federation-core's generic dispatch/dedup mechanics alone (already
//! proven elsewhere), and observes the resulting relationship state through
//! the real, already-wired `GET /api/v1/accounts/relationships` oracle
//! (mirrors `tests/mute_block_it.rs`'s/`tests/follow_request_it.rs`'s own
//! established use of this same read-only oracle) and `delivery_jobs` rows
//! (mirrors `tests/federation_bootstrap_it.rs`'s own `delivery_job_exists`/
//! `tests/follow_request_it.rs`'s own `delivery_job_count`).
//!
//! ## Idempotency (Requirement 7.7): a second, *distinct-Activity-id* Follow
//! for an already-established pair, not merely an exact-id replay
//! `src/social_graph/inbound.rs`'s own doc comment ("Idempotency...") names
//! the specific scenario worth a dedicated regression test: federation-core's
//! own dedup ledger (already covered by `tests/inbox_it.rs`'s
//! `duplicate_activity_delivery_is_acked_without_reprocessing`) only ever
//! catches an *exact* repeated `activity.id` — a second, genuinely distinct
//! Follow Activity id for a pair that already established a follow would
//! sail straight past that dedup layer and reach `SocialGraphInboundHandler::
//! handle_follow` a second time. This file's own idempotency test exercises
//! exactly that (a second Follow with a *different* id), proving the
//! handler's own pre-check (not federation-core's dedup) is what keeps a
//! second `Accept(Follow)` from being redelivered — plus, for completeness,
//! an exact-id replay of the same Follow too (exercising the federation-core
//! dedup layer through this spec's own registered handler, in one test).
//!
//! ## No HTTP client dependency: raw sockets (this crate's established
//! per-file-duplicated convention).

use std::time::Duration;

use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use time::macros::format_description;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::model::{ProfileField, ProfilePatch, RemoteAccount};
use kawasemi::accounts::profile_repository;
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::keys::material::generate_keypair;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, LocalActor, NewActor};
use kawasemi::domain::Id;
use kawasemi::federation::signatures::{
    Digest as BodyDigest, DraftCavageSuite, SignableRequest, SignatureSuite,
};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::runtime::SeededRng;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// Raw HTTP plumbing (duplicated per-file; see this module's doc comment).
// ==========================================================================

struct RawResponse {
    status: u16,
    body: Vec<u8>,
}

impl std::fmt::Debug for RawResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawResponse")
            .field("status", &self.status)
            .field("body", &String::from_utf8_lossy(&self.body))
            .finish()
    }
}

async fn raw_request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

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
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
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

async fn raw_get(addr: std::net::SocketAddr, path: &str, headers: &[(&str, &str)]) -> RawResponse {
    let owned: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    raw_request(addr, "GET", path, &owned, b"").await
}

async fn raw_post_empty(
    addr: std::net::SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
) -> RawResponse {
    let owned: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    raw_request(addr, "POST", path, &owned, b"").await
}

fn body_json(response: &RawResponse) -> Value {
    serde_json::from_slice(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

// ==========================================================================
// Signing helpers (duplicated from `tests/federation_bootstrap_it.rs`/
// `tests/inbox_it.rs`; see this module's doc comment).
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
        axum::http::header::HOST,
        axum::http::HeaderValue::from_str(host).expect("valid host header value"),
    );
    headers.insert(
        axum::http::header::DATE,
        axum::http::HeaderValue::from_str(&format_http_date(when))
            .expect("valid date header value"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("digest"),
        axum::http::HeaderValue::from_str(&BodyDigest::compute(body).header_value())
            .expect("valid digest header value"),
    );

    let signable = SignableRequest {
        method: axum::http::Method::POST,
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
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
            axum::http::HeaderValue::from_str(&value).expect("valid header value"),
        );
    }
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/activity+json"),
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

// ==========================================================================
// Fixtures
// ==========================================================================

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
            display_name: format!("Inbound Activities IT {handle_str}"),
            summary: "an actor used by the inbound_activities_it integration test".to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle")
}

async fn lock_actor(app: &TestApp, actor_id: Id) {
    let now = app.runtime.clock.now();
    profile_repository::upsert_profile(
        &app.pool,
        actor_id,
        ProfilePatch {
            locked: Some(true),
            ..Default::default()
        },
        now,
    )
    .await
    .expect("locking the actor's profile must succeed");
}

/// A remote signer fixture: a real RSA keypair plus a cached
/// `remote_public_keys`/`remote_accounts` row pair, so `HttpSignatureVerifier`
/// (cache-hit path) and `ProdActorUriResolver`/`BlockPolicyImpl` (their own
/// `remote_accounts` lookups) both resolve this signer without a live
/// network fetch (mirrors `tests/federation_bootstrap_it.rs`'s own
/// `seed_remote_public_key` doc comment for exactly this reason).
struct RemoteSigner {
    id: Id,
    actor_uri: String,
    key_id: String,
    private_key: RsaPrivateKey,
}

async fn seed_remote_signer(
    app: &TestApp,
    seed: u64,
    username: &str,
    locked: bool,
) -> RemoteSigner {
    let (private_key, public_key_pem) = test_keypair(seed);
    let actor_uri = format!("https://remote.example/users/{username}");
    let key_id = format!("{actor_uri}#main-key");

    sqlx::query(
        "INSERT INTO remote_public_keys (key_id, actor_uri, public_key_pem, fetched_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&key_id)
    .bind(&actor_uri)
    .bind(&public_key_pem)
    .bind(app.runtime.clock.now())
    .execute(&app.pool)
    .await
    .expect("seeding a cached remote public key must succeed");

    let id = app.runtime.ids.next_id();
    upsert_remote(
        &app.pool,
        &RemoteAccount {
            id,
            actor_uri: actor_uri.clone(),
            username: username.to_string(),
            domain: "remote.example".to_string(),
            display_name: format!("Remote {username}"),
            note: String::new(),
            url: actor_uri.clone(),
            avatar_url: None,
            header_url: None,
            fields: Vec::<ProfileField>::new(),
            bot: false,
            locked,
            fetched_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("seeding the cached remote account must succeed");

    RemoteSigner {
        id,
        actor_uri,
        key_id,
        private_key,
    }
}

/// Signs `body` as `signer` and POSTs it to `recipient`'s per-actor inbox
/// through the real, mounted router.
async fn deliver_to_inbox(
    app: &TestApp,
    domain: &str,
    signer: &RemoteSigner,
    recipient: &Handle,
    body: &Value,
) -> RawResponse {
    let urls = ActorUrls::new(domain.to_string());
    let url = urls.inbox_url(recipient);
    let body_bytes = body.to_string().into_bytes();
    let headers = sign_post_request(
        &url,
        domain,
        &signer.key_id,
        &signer.private_key,
        app.runtime.clock.now(),
        &body_bytes,
    );
    let path = format!("/users/{}/inbox", recipient.as_str());
    raw_request(app.address, "POST", &path, &headers, &body_bytes).await
}

fn follow_body(id: &str, actor_uri: &str, object_uri: &str) -> Value {
    json!({ "id": id, "type": "Follow", "actor": actor_uri, "object": object_uri })
}

fn block_body(id: &str, actor_uri: &str, object_uri: &str) -> Value {
    json!({ "id": id, "type": "Block", "actor": actor_uri, "object": object_uri })
}

fn accept_or_reject_body(
    id: &str,
    outer_type: &str,
    approver_uri: &str,
    follow_id: &str,
    requester_uri: &str,
    target_uri: &str,
) -> Value {
    json!({
        "id": id,
        "type": outer_type,
        "actor": approver_uri,
        "object": {
            "id": follow_id,
            "type": "Follow",
            "actor": requester_uri,
            "object": target_uri,
        }
    })
}

fn undo_body(
    id: &str,
    actor_uri: &str,
    inner_type: &str,
    inner_id: &str,
    inner_object_uri: &str,
) -> Value {
    json!({
        "id": id,
        "type": "Undo",
        "actor": actor_uri,
        "object": {
            "id": inner_id,
            "type": inner_type,
            "actor": actor_uri,
            "object": inner_object_uri,
        }
    })
}

// ---- OAuth / relationships oracle (mirrors `tests/mute_block_it.rs`'s own
// established pattern) -----------------------------------------------------

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Inbound Activities IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: PlaceholderScopeSet::new(["read", "write", "follow"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn issue_token(app: &TestApp, oauth_app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewAccessToken {
            app_id: oauth_app_id,
            actor_id,
            scopes: PlaceholderScopeSet::new(scopes.iter().copied()),
        },
    )
    .await
    .expect("issuing a real access token must succeed");
    issued.plaintext.expose_secret().clone()
}

fn bearer_header(token: &str) -> String {
    format!("Bearer {token}")
}

async fn relationship_to(app: &TestApp, viewer_token: &str, target_id: Id) -> Value {
    let response = raw_get(
        app.address,
        &format!("/api/v1/accounts/relationships?id={}", target_id.as_i64()),
        &[("Authorization", &bearer_header(viewer_token))],
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    body.as_array()
        .expect("relationships must return an array")
        .first()
        .cloned()
        .expect("relationships must return exactly one entry for one requested id")
}

async fn delivery_job_count(app: &TestApp, target_inbox: &str, activity_type: &str) -> i64 {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM delivery_jobs WHERE target_inbox = $1 AND activity->>'type' = $2",
    )
    .bind(target_inbox)
    .bind(activity_type)
    .fetch_one(&app.pool)
    .await
    .expect("querying delivery_jobs must succeed");
    row.0
}

async fn follow_via_api(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/follow", target_id.as_i64());
    raw_post_empty(
        app.address,
        &path,
        &[("Authorization", &bearer_header(token))],
    )
    .await
}

fn test_domain(app: &TestApp) -> String {
    app.state.config().server.domain.clone()
}

// ==========================================================================
// (1) Requirement 7.2: received Follow to an unlocked local actor
// establishes the follow immediately and delivers Accept(Follow) back.
// ==========================================================================

#[tokio::test]
async fn received_follow_to_unlocked_local_actor_establishes_and_delivers_accept() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let target = insert_actor_fixture(&app, "ia_unlocked_target").await;
    let requester = seed_remote_signer(&app, 1, "ia_follow_requester", false).await;

    let oauth_app_id = register_test_app(&app).await;
    let target_token = issue_token(&app, oauth_app_id, target.id, &["read:follows"]).await;

    let target_uri = urls.actor_url(&target.handle);
    let body = follow_body(
        "https://remote.example/activities/ia-follow-1",
        &requester.actor_uri,
        &target_uri,
    );
    let response = deliver_to_inbox(&app, &domain, &requester, &target.handle, &body).await;
    assert_eq!(response.status, 202, "got: {response:?}");

    let rel = relationship_to(&app, &target_token, requester.id).await;
    assert_eq!(
        rel["followed_by"].as_bool(),
        Some(true),
        "the unlocked target must have established the follow immediately, got: {rel}"
    );
    assert_eq!(rel["requested_by"].as_bool(), Some(false), "got: {rel}");

    let requester_inbox = format!("{}/inbox", requester.actor_uri);
    assert_eq!(
        delivery_job_count(&app, &requester_inbox, "Accept").await,
        1,
        "an unlocked target must deliver Accept(Follow) back to the requester"
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirement 7.3: received Follow to a locked local actor is recorded
// as a pending request, with no Accept delivered.
// ==========================================================================

#[tokio::test]
async fn received_follow_to_locked_local_actor_records_pending_without_accept() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let target = insert_actor_fixture(&app, "ia_locked_target").await;
    lock_actor(&app, target.id).await;
    let requester = seed_remote_signer(&app, 2, "ia_pending_requester", false).await;

    let oauth_app_id = register_test_app(&app).await;
    let target_token =
        issue_token(&app, oauth_app_id, target.id, &["read:follows", "follow"]).await;

    let target_uri = urls.actor_url(&target.handle);
    let body = follow_body(
        "https://remote.example/activities/ia-follow-2",
        &requester.actor_uri,
        &target_uri,
    );
    let response = deliver_to_inbox(&app, &domain, &requester, &target.handle, &body).await;
    assert_eq!(response.status, 202, "got: {response:?}");

    let rel = relationship_to(&app, &target_token, requester.id).await;
    assert_eq!(
        rel["requested_by"].as_bool(),
        Some(true),
        "a locked target must record the Follow as pending, got: {rel}"
    );
    assert_eq!(
        rel["followed_by"].as_bool(),
        Some(false),
        "a locked target must not establish the follow immediately, got: {rel}"
    );

    let requester_inbox = format!("{}/inbox", requester.actor_uri);
    assert_eq!(
        delivery_job_count(&app, &requester_inbox, "Accept").await,
        0,
        "a locked target must not deliver an immediate Accept(Follow)"
    );

    // The pending request must be visible through the owner's own
    // `list_follow_requests` surface too.
    let list_response = raw_get(
        app.address,
        "/api/v1/follow_requests",
        &[("Authorization", &bearer_header(&target_token))],
    )
    .await;
    assert_eq!(list_response.status, 200, "got: {list_response:?}");
    let list_body = body_json(&list_response);
    let items = list_body.as_array().expect("must be an array");
    assert_eq!(items.len(), 1, "got: {list_body}");
    assert_eq!(
        items[0]["id"].as_str(),
        Some(requester.id.as_i64().to_string()).as_deref()
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Requirement 2.5 / 7.1: received Accept(Follow) promotes the matching
// outbound pending follow request to an established follow.
// ==========================================================================

#[tokio::test]
async fn received_accept_promotes_outbound_pending_to_established() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let alice = insert_actor_fixture(&app, "ia_accept_alice").await;
    // Locked remote target: `alice`'s own outbound follow becomes a pending
    // request awaiting the remote target's own Accept, rather than
    // establishing immediately.
    let target = seed_remote_signer(&app, 3, "ia_accept_target", true).await;

    let oauth_app_id = register_test_app(&app).await;
    let alice_write_token = issue_token(&app, oauth_app_id, alice.id, &["follow"]).await;
    let alice_read_token = issue_token(&app, oauth_app_id, alice.id, &["read:follows"]).await;

    let follow_response = follow_via_api(&app, &alice_write_token, target.id).await;
    assert_eq!(follow_response.status, 200, "got: {follow_response:?}");
    let follow_body_json = body_json(&follow_response);
    assert_eq!(
        follow_body_json["requested"].as_bool(),
        Some(true),
        "a locked remote target must leave the follow pending, got: {follow_body_json}"
    );

    let alice_uri = urls.actor_url(&alice.handle);
    let target_uri = target.actor_uri.clone();
    let accept_body = accept_or_reject_body(
        "https://remote.example/activities/ia-accept-1",
        "Accept",
        &target_uri,
        "https://kawasemi.inbound-it.internal/activities/original-follow-1",
        &alice_uri,
        &target_uri,
    );
    let response = deliver_to_inbox(&app, &domain, &target, &alice.handle, &accept_body).await;
    assert_eq!(response.status, 202, "got: {response:?}");

    let rel = relationship_to(&app, &alice_read_token, target.id).await;
    assert_eq!(
        rel["following"].as_bool(),
        Some(true),
        "Accept must promote the outbound pending request to an established follow, got: {rel}"
    );
    assert_eq!(rel["requested"].as_bool(), Some(false), "got: {rel}");

    app.cleanup().await;
}

// ==========================================================================
// (4) Requirement 2.6 / 7.1: received Reject(Follow) drops the outbound
// pending request without ever establishing the follow.
// ==========================================================================

#[tokio::test]
async fn received_reject_drops_outbound_pending_without_establishing() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let bob = insert_actor_fixture(&app, "ia_reject_bob").await;
    let target = seed_remote_signer(&app, 4, "ia_reject_target", true).await;

    let oauth_app_id = register_test_app(&app).await;
    let bob_write_token = issue_token(&app, oauth_app_id, bob.id, &["follow"]).await;
    let bob_read_token = issue_token(&app, oauth_app_id, bob.id, &["read:follows"]).await;

    let follow_response = follow_via_api(&app, &bob_write_token, target.id).await;
    assert_eq!(follow_response.status, 200, "got: {follow_response:?}");
    assert_eq!(
        body_json(&follow_response)["requested"].as_bool(),
        Some(true)
    );

    let bob_uri = urls.actor_url(&bob.handle);
    let target_uri = target.actor_uri.clone();
    let reject_body = accept_or_reject_body(
        "https://remote.example/activities/ia-reject-1",
        "Reject",
        &target_uri,
        "https://kawasemi.inbound-it.internal/activities/original-follow-2",
        &bob_uri,
        &target_uri,
    );
    let response = deliver_to_inbox(&app, &domain, &target, &bob.handle, &reject_body).await;
    assert_eq!(response.status, 202, "got: {response:?}");

    let rel = relationship_to(&app, &bob_read_token, target.id).await;
    assert_eq!(
        rel["following"].as_bool(),
        Some(false),
        "a Reject must never establish the follow, got: {rel}"
    );
    assert_eq!(
        rel["requested"].as_bool(),
        Some(false),
        "a Reject must drop the pending request, got: {rel}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) Requirement 7.4: received Block records `blocked_by` and tears down
// established follows in both directions.
// ==========================================================================

#[tokio::test]
async fn received_block_records_blocked_by_and_clears_both_direction_follows() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let carol = insert_actor_fixture(&app, "ia_block_carol").await;
    let attacker = seed_remote_signer(&app, 5, "ia_block_attacker", false).await;

    let oauth_app_id = register_test_app(&app).await;
    let carol_write_token = issue_token(&app, oauth_app_id, carol.id, &["follow"]).await;
    let carol_read_token = issue_token(&app, oauth_app_id, carol.id, &["read:follows"]).await;

    // carol -> attacker (unlocked remote, establishes immediately).
    let follow_response = follow_via_api(&app, &carol_write_token, attacker.id).await;
    assert_eq!(follow_response.status, 200, "got: {follow_response:?}");
    assert_eq!(
        body_json(&follow_response)["following"].as_bool(),
        Some(true)
    );

    // attacker -> carol (unlocked local, establishes immediately + Accept).
    let carol_uri = urls.actor_url(&carol.handle);
    let inbound_follow = follow_body(
        "https://remote.example/activities/ia-block-follow-1",
        &attacker.actor_uri,
        &carol_uri,
    );
    let follow_in =
        deliver_to_inbox(&app, &domain, &attacker, &carol.handle, &inbound_follow).await;
    assert_eq!(follow_in.status, 202, "got: {follow_in:?}");

    let mutual = relationship_to(&app, &carol_read_token, attacker.id).await;
    assert_eq!(mutual["following"].as_bool(), Some(true), "got: {mutual}");
    assert_eq!(mutual["followed_by"].as_bool(), Some(true), "got: {mutual}");

    // Now attacker blocks carol.
    let block = block_body(
        "https://remote.example/activities/ia-block-1",
        &attacker.actor_uri,
        &carol_uri,
    );
    let response = deliver_to_inbox(&app, &domain, &attacker, &carol.handle, &block).await;
    assert_eq!(response.status, 202, "got: {response:?}");

    let rel = relationship_to(&app, &carol_read_token, attacker.id).await;
    assert_eq!(
        rel["blocked_by"].as_bool(),
        Some(true),
        "a received Block must record blocked_by, got: {rel}"
    );
    assert_eq!(
        rel["following"].as_bool(),
        Some(false),
        "a received Block must clear carol's own follow of the blocker, got: {rel}"
    );
    assert_eq!(
        rel["followed_by"].as_bool(),
        Some(false),
        "a received Block must clear the blocker's own follow of carol too, got: {rel}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (6) Requirement 7.5: received Undo(Follow) removes the follower
// registration.
// ==========================================================================

#[tokio::test]
async fn received_undo_follow_removes_follower_registration() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let dave = insert_actor_fixture(&app, "ia_undo_follow_dave").await;
    let attacker = seed_remote_signer(&app, 6, "ia_undo_follow_attacker", false).await;

    let oauth_app_id = register_test_app(&app).await;
    let dave_read_token = issue_token(&app, oauth_app_id, dave.id, &["read:follows"]).await;

    let dave_uri = urls.actor_url(&dave.handle);
    let original_follow_id = "https://remote.example/activities/ia-undo-follow-original-1";
    let follow_in = follow_body(original_follow_id, &attacker.actor_uri, &dave_uri);
    let follow_response =
        deliver_to_inbox(&app, &domain, &attacker, &dave.handle, &follow_in).await;
    assert_eq!(follow_response.status, 202, "got: {follow_response:?}");

    let before = relationship_to(&app, &dave_read_token, attacker.id).await;
    assert_eq!(before["followed_by"].as_bool(), Some(true), "got: {before}");

    let undo = undo_body(
        "https://remote.example/activities/ia-undo-follow-1",
        &attacker.actor_uri,
        "Follow",
        original_follow_id,
        &dave_uri,
    );
    let response = deliver_to_inbox(&app, &domain, &attacker, &dave.handle, &undo).await;
    assert_eq!(response.status, 202, "got: {response:?}");

    let after = relationship_to(&app, &dave_read_token, attacker.id).await;
    assert_eq!(
        after["followed_by"].as_bool(),
        Some(false),
        "Undo(Follow) must remove the follower registration, got: {after}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (7) Requirement 7.6: received Undo(Block) clears the `blocked_by` state.
// ==========================================================================

#[tokio::test]
async fn received_undo_block_clears_blocked_by() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let erin = insert_actor_fixture(&app, "ia_undo_block_erin").await;
    let attacker = seed_remote_signer(&app, 7, "ia_undo_block_attacker", false).await;

    let oauth_app_id = register_test_app(&app).await;
    let erin_read_token = issue_token(&app, oauth_app_id, erin.id, &["read:follows"]).await;

    let erin_uri = urls.actor_url(&erin.handle);
    let original_block_id = "https://remote.example/activities/ia-undo-block-original-1";
    let block = block_body(original_block_id, &attacker.actor_uri, &erin_uri);
    let block_response = deliver_to_inbox(&app, &domain, &attacker, &erin.handle, &block).await;
    assert_eq!(block_response.status, 202, "got: {block_response:?}");

    let before = relationship_to(&app, &erin_read_token, attacker.id).await;
    assert_eq!(before["blocked_by"].as_bool(), Some(true), "got: {before}");

    let undo = undo_body(
        "https://remote.example/activities/ia-undo-block-1",
        &attacker.actor_uri,
        "Block",
        original_block_id,
        &erin_uri,
    );
    let response = deliver_to_inbox(&app, &domain, &attacker, &erin.handle, &undo).await;
    assert_eq!(response.status, 202, "got: {response:?}");

    let after = relationship_to(&app, &erin_read_token, attacker.id).await;
    assert_eq!(
        after["blocked_by"].as_bool(),
        Some(false),
        "Undo(Block) must clear blocked_by, got: {after}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (8) Requirement 7.7: idempotency -- an exact-id replay never re-applies,
// and a second, distinct-id Follow for an already-established pair never
// redelivers a second Accept(Follow).
// ==========================================================================

#[tokio::test]
async fn received_follow_replay_and_a_second_distinct_follow_never_redeliver_accept() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let urls = ActorUrls::new(domain.clone());
    let frank = insert_actor_fixture(&app, "ia_idem_frank").await;
    let attacker = seed_remote_signer(&app, 8, "ia_idem_attacker", false).await;

    let oauth_app_id = register_test_app(&app).await;
    let frank_read_token = issue_token(&app, oauth_app_id, frank.id, &["read:follows"]).await;

    let frank_uri = urls.actor_url(&frank.handle);
    let requester_inbox = format!("{}/inbox", attacker.actor_uri);

    // First Follow (id X): establishes + delivers exactly one Accept.
    let first_id = "https://remote.example/activities/ia-idem-follow-1";
    let first = follow_body(first_id, &attacker.actor_uri, &frank_uri);
    let first_response = deliver_to_inbox(&app, &domain, &attacker, &frank.handle, &first).await;
    assert_eq!(first_response.status, 202, "got: {first_response:?}");
    assert_eq!(
        delivery_job_count(&app, &requester_inbox, "Accept").await,
        1
    );

    // Exact-id replay of the same Follow (federation-core's own dedup layer):
    // still acked, never redispatched, Accept count unchanged.
    let replay_response = deliver_to_inbox(&app, &domain, &attacker, &frank.handle, &first).await;
    assert_eq!(replay_response.status, 202, "got: {replay_response:?}");
    assert_eq!(
        delivery_job_count(&app, &requester_inbox, "Accept").await,
        1,
        "an exact-id replay must not redeliver a second Accept"
    );

    // A second, genuinely distinct Activity id for the same already-
    // established pair (this spec's own pre-check, not federation-core's
    // dedup layer): still acked, Accept count still unchanged.
    let second_id = "https://remote.example/activities/ia-idem-follow-2";
    let second = follow_body(second_id, &attacker.actor_uri, &frank_uri);
    let second_response = deliver_to_inbox(&app, &domain, &attacker, &frank.handle, &second).await;
    assert_eq!(second_response.status, 202, "got: {second_response:?}");
    assert_eq!(
        delivery_job_count(&app, &requester_inbox, "Accept").await,
        1,
        "a second, distinct-id Follow for an already-established pair must not redeliver a \
         second Accept (Requirement 7.7)"
    );

    let rel = relationship_to(&app, &frank_read_token, attacker.id).await;
    assert_eq!(
        rel["followed_by"].as_bool(),
        Some(true),
        "the relationship state itself must remain correctly established, got: {rel}"
    );

    app.cleanup().await;
}
