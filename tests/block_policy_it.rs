//! Integration tests for social-graph's `BlockPolicy` delegation-boundary
//! implementation actually taking effect end to end
//! (`.kiro/specs/social-graph/tasks.md`, task 6.2 "受信処理・署名拒否・
//! プロバイダ統合テスト", `_Boundary: InboundHandler, BlockPolicyImpl,
//! RelProviderImpl_`), driven through the *real*, `spawn_test_app`-booted
//! application's real, mounted `/users/{handle}/inbox` and `/inbox` routes
//! and real HTTP-Signature verification pipeline -- never a test-local
//! `InboxService` instantiation with a hand-rolled `BlockPolicy` double.
//!
//! design.md's File Structure Plan names this exact filename
//! (`block_policy_it.rs`: "ブロック先署名拒否（BlockPolicy 実装が
//! federation-core に効く）（統合）") and its own Testing Strategy bullet
//! ("BlockPolicy: ブロック後に当該署名者の受信が federation-core で拒否さ
//! れ、解除後は通る（6.1–6.4）").
//!
//! Covers Requirements 6.1, 6.2, 6.3, 6.4.
//!
//! ## Why this file exists on top of `tests/inbox_it.rs`
//! `tests/inbox_it.rs` (federation-core, task 5.3) already proves the
//! *mechanism* -- that the per-actor inbox and shared inbox pass different
//! `LocalRecipientContext` variants to whatever `BlockPolicy` is configured
//! -- but does so against a hand-rolled `RecordingBlockPolicy` test double
//! over a test-local `InboxService`/`axum::Router`, never the real,
//! bootstrap-registered `BlockPolicyRegistry` -> `BlockPolicyImpl` ->
//! `blocks`-table pipeline (task 5.2's own wiring). This file instead drives
//! the real, live-mounted router (`spawn_test_app`/`src/server.rs`) and
//! actually calls the real `POST /api/v1/accounts/:id/block`/`.../unblock`
//! endpoints (task 5.1) to flip the underlying `blocks` row, then observes
//! the *only* externally-observable effect a blocked signer has on this
//! pipeline: `src/federation/inbound/service.rs::InboxService::
//! process_verified`'s own documented "a blocked signer is rejected (403,
//! Requirement 12.2) before deduplication or dispatch ever observes the
//! Activity" contract -- i.e. this file asserts on the real HTTP status code
//! (`403` while blocked, `202` otherwise) and, for the rejected case, that
//! the Activity id never even reaches `received_activities` (proving it was
//! turned away *before* dedup/dispatch, not merely dropped afterward).
//!
//! ## Activity body: a generic, handler-agnostic `Arrive`, not `Follow`
//! Mirrors `tests/federation_bootstrap_it.rs`'s own documented choice for
//! the identical reason: this file's own concern is exclusively the
//! block-judgment stage of the pipeline (upstream of dispatch), not
//! `SocialGraphInboundHandler`'s own Follow/Accept/Reject/Block/Undo
//! semantics (`tests/inbound_activities_it.rs`'s own boundary) -- an
//! `Arrive` body needs no `actor`/`object` shape to be a well-formed,
//! acceptable (if unhandled) Activity.
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

use kawasemi::accounts::model::{ProfileField, RemoteAccount};
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

fn body_json(response: &RawResponse) -> Value {
    serde_json::from_slice(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

// ==========================================================================
// Signing helpers (duplicated from `tests/federation_bootstrap_it.rs`).
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
            display_name: format!("Block Policy IT {handle_str}"),
            summary: "an actor used by the block_policy_it integration test".to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle")
}

struct RemoteSigner {
    id: Id,
    #[allow(dead_code)]
    actor_uri: String,
    key_id: String,
    private_key: RsaPrivateKey,
}

async fn seed_remote_signer(app: &TestApp, seed: u64, username: &str) -> RemoteSigner {
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
            locked: false,
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

fn arrive_body(id: &str) -> Vec<u8> {
    json!({ "id": id, "type": "Arrive" })
        .to_string()
        .into_bytes()
}

async fn deliver_to_actor_inbox(
    app: &TestApp,
    domain: &str,
    signer: &RemoteSigner,
    recipient: &Handle,
    activity_id: &str,
) -> RawResponse {
    let urls = ActorUrls::new(domain.to_string());
    let url = urls.inbox_url(recipient);
    let body = arrive_body(activity_id);
    let headers = sign_post_request(
        &url,
        domain,
        &signer.key_id,
        &signer.private_key,
        app.runtime.clock.now(),
        &body,
    );
    let path = format!("/users/{}/inbox", recipient.as_str());
    raw_request(app.address, "POST", &path, &headers, &body).await
}

async fn deliver_to_shared_inbox(
    app: &TestApp,
    domain: &str,
    signer: &RemoteSigner,
    activity_id: &str,
) -> RawResponse {
    let urls = ActorUrls::new(domain.to_string());
    let url = urls.shared_inbox_url();
    let body = arrive_body(activity_id);
    let headers = sign_post_request(
        &url,
        domain,
        &signer.key_id,
        &signer.private_key,
        app.runtime.clock.now(),
        &body,
    );
    raw_request(app.address, "POST", "/inbox", &headers, &body).await
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

fn test_domain(app: &TestApp) -> String {
    app.state.config().server.domain.clone()
}

// ---- OAuth / block-unblock API surface -----------------------------------

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Block Policy IT Client".to_string(),
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

async fn block_via_api(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/block", target_id.as_i64());
    raw_request(
        app.address,
        "POST",
        &path,
        &[("Authorization".to_string(), bearer_header(token))],
        b"",
    )
    .await
}

async fn unblock_via_api(app: &TestApp, token: &str, target_id: Id) -> RawResponse {
    let path = format!("/api/v1/accounts/{}/unblock", target_id.as_i64());
    raw_request(
        app.address,
        "POST",
        &path,
        &[("Authorization".to_string(), bearer_header(token))],
        b"",
    )
    .await
}

// ==========================================================================
// (1) Requirements 6.1, 6.2, 6.3, 6.4: a blocked signer's request is
// rejected (403) via the real BlockPolicyRegistry -> BlockPolicyImpl ->
// `blocks`-table pipeline, and unblocking restores acceptance.
// ==========================================================================

#[tokio::test]
async fn blocked_signer_is_rejected_and_unblocking_restores_acceptance() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let hank = insert_actor_fixture(&app, "bp_hank").await;
    let attacker = seed_remote_signer(&app, 101, "bp_attacker").await;

    let oauth_app_id = register_test_app(&app).await;
    let hank_token = issue_token(&app, oauth_app_id, hank.id, &["follow"]).await;

    // Baseline: not yet blocked, a signed request is accepted.
    let baseline = deliver_to_actor_inbox(
        &app,
        &domain,
        &attacker,
        &hank.handle,
        "https://remote.example/activities/bp-baseline-1",
    )
    .await;
    assert_eq!(
        baseline.status, 202,
        "an unblocked signer's request must be accepted, got: {baseline:?}"
    );

    // hank blocks the attacker via the real API.
    let block_response = block_via_api(&app, &hank_token, attacker.id).await;
    assert_eq!(block_response.status, 200, "got: {block_response:?}");
    assert_eq!(body_json(&block_response)["blocking"].as_bool(), Some(true));

    // A subsequent signed request from the now-blocked signer is rejected.
    let rejected_id = "https://remote.example/activities/bp-rejected-1";
    let rejected =
        deliver_to_actor_inbox(&app, &domain, &attacker, &hank.handle, rejected_id).await;
    assert_eq!(
        rejected.status, 403,
        "a signed request from a blocked signer must be rejected with 403 (Requirement 6.2), \
         got: {rejected:?}"
    );
    assert!(
        !received_activity_exists(&app, rejected_id).await,
        "a rejected Activity must never reach the dedup/dispatch ledger (rejected before those \
         stages, Requirement 6.2/6.3)"
    );

    // hank unblocks the attacker.
    let unblock_response = unblock_via_api(&app, &hank_token, attacker.id).await;
    assert_eq!(unblock_response.status, 200, "got: {unblock_response:?}");
    assert_eq!(
        body_json(&unblock_response)["blocking"].as_bool(),
        Some(false)
    );

    // A subsequent signed request from the now-unblocked signer is accepted
    // again (Requirement 6.4).
    let recovered = deliver_to_actor_inbox(
        &app,
        &domain,
        &attacker,
        &hank.handle,
        "https://remote.example/activities/bp-recovered-1",
    )
    .await;
    assert_eq!(
        recovered.status, 202,
        "after unblocking, the same signer's requests must be accepted again \
         (Requirement 6.4), got: {recovered:?}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirements 6.1, 6.2: the block judgment is scoped to the blocking
// local actor's own perspective (never global), and a shared-inbox
// delivery is never bulk-rejected even while the signer is blocked by one
// local actor.
// ==========================================================================

#[tokio::test]
async fn block_is_scoped_per_local_actor_and_shared_inbox_is_never_bulk_rejected() {
    let app = spawn_test_app().await;
    let domain = test_domain(&app);
    let hank = insert_actor_fixture(&app, "bp_scope_hank").await;
    let irene = insert_actor_fixture(&app, "bp_scope_irene").await;
    let attacker = seed_remote_signer(&app, 102, "bp_scope_attacker").await;

    let oauth_app_id = register_test_app(&app).await;
    let hank_token = issue_token(&app, oauth_app_id, hank.id, &["follow"]).await;

    let block_response = block_via_api(&app, &hank_token, attacker.id).await;
    assert_eq!(block_response.status, 200, "got: {block_response:?}");

    // hank (who blocked) rejects the signer.
    let to_hank = deliver_to_actor_inbox(
        &app,
        &domain,
        &attacker,
        &hank.handle,
        "https://remote.example/activities/bp-scope-hank-1",
    )
    .await;
    assert_eq!(to_hank.status, 403, "got: {to_hank:?}");

    // irene (who never blocked the signer) still accepts it -- the block
    // judgment is per-destination-local-actor, never global.
    let to_irene = deliver_to_actor_inbox(
        &app,
        &domain,
        &attacker,
        &irene.handle,
        "https://remote.example/activities/bp-scope-irene-1",
    )
    .await;
    assert_eq!(
        to_irene.status, 202,
        "a local actor who never blocked the signer must still accept the signer's requests, \
         got: {to_irene:?}"
    );

    // The shared inbox (destination not yet resolved) never bulk-rejects,
    // even though hank currently has this signer blocked.
    let to_shared = deliver_to_shared_inbox(
        &app,
        &domain,
        &attacker,
        "https://remote.example/activities/bp-scope-shared-1",
    )
    .await;
    assert_eq!(
        to_shared.status, 202,
        "a shared-inbox delivery must never be bulk-rejected on a per-actor block \
         (LocalRecipientContext::SharedInbox always answers false), got: {to_shared:?}"
    );

    app.cleanup().await;
}
