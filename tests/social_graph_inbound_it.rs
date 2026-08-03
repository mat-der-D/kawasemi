//! Integration tests for task 6.2's own observable completion condition
//! (`.kiro/specs/social-graph/tasks.md`, "6.2 (P) 受信処理・署名拒否・プロバ
//! イダ統合テスト", `_Boundary: InboundHandler, BlockPolicyImpl,
//! RelProviderImpl_`) — this file covers the "InboundHandler" and
//! "BlockPolicyImpl" halves (`RelProviderImpl` is
//! `tests/social_graph_relationship_provider_it.rs`'s own job): design.md's
//! own "Integration Tests（`spawn_test_app` 上）" Testing Strategy bullets
//! "受信 Activity: 受信 Follow（承認要否分岐）・Accept/Reject・Block・
//! Undo(Follow/Block) の状態遷移と冪等（7.2–7.7, 2.5, 2.6）" and "BlockPolicy:
//! ブロック後に当該署名者の受信が federation-core で拒否され、解除後は通る
//! （6.1–6.4）".
//!
//! ## Driven through the real, live inbox HTTP endpoint, not a test-local
//! router
//! Unlike `tests/inbox_it.rs` (federation-core's own task, a test-local
//! `axum::Router` over test-double `PublicKeyResolver`/`BlockPolicy`
//! instances), this file posts genuinely signed Activity JSON bodies over
//! real TCP to `spawn_test_app()`'s own live `POST /users/{handle}/inbox` /
//! `POST /inbox` routes (`src/server.rs`'s real mounting of
//! `actor_inbox`/`shared_inbox`) — so the *whole* federation-core pipeline
//! (real `HttpSignatureVerifier<DbFederationPublicKeyResolver<
//! ReqwestFederationHttpClient>>`, the real `BlockPolicyRegistry` carrying
//! this spec's own real `BlockPolicyImpl` — task 5.2's wiring, confirmed by
//! `src/test_harness.rs`'s own `social_graph::build_social_graph_module`
//! call — real Postgres-backed dedup, real `InboundActivityDispatcher`)
//! runs for real and lands in the real, registered
//! `SocialGraphInboundHandler::handle` (`src/social_graph/inbound.rs`).
//! This is this task's own explicit guidance: "アクター個別 inbox と
//! shared inbox のそれぞれで異なる宛先コンテキストが BlockPolicy に渡る"
//! and "本 spec が処理する種別は常にアクター個別 inbox へ配送される" are
//! design.md's own claims about the *live* wiring, not about a test-local
//! substitute of it.
//!
//! ## Simulating a genuine remote signer without any real network call
//! A "remote" actor here is a real RSA-2048 keypair
//! (`kawasemi::actor::keys::material::generate_keypair`, mirrors
//! `tests/inbox_it.rs::test_keypair`) whose public key material is
//! pre-seeded directly into the `remote_public_keys` cache table (mirrors
//! `tests/signatures_it.rs::seed_cached_public_key`'s own established
//! convention) and whose account document is pre-seeded into
//! `remote_accounts` via `upsert_remote` (mirrors
//! `tests/follow_unfollow_it.rs`/`tests/mute_block_it.rs::create_test_remote`).
//! Both caches are read cache-first with the deterministic `FixedClock`'s
//! `now()` used as `fetched_at` (`src/accounts/remote_fetcher.rs::
//! fetch_and_normalize`'s own doc comment: "Reuses a valid cache entry
//! without any network call"; `src/federation/signatures/key_resolver.rs`'s
//! identical contract) — a `FixedClock` never advances, so the seeded
//! `fetched_at` can never be judged stale, and no test in this file ever
//! reaches the network.
//!
//! Draft-cavage HTTP Signatures are hand-built the same way
//! `tests/inbox_it.rs::sign_post_request` does (this spec's own boundary
//! does not include `RequestSigner`/federation-core, so this file cannot
//! reuse that production signer either — the same constraint that file
//! documents for itself), against the exact canonical `ActorUrls::
//! inbox_url`/`shared_inbox_url` this instance's own live router signs
//! against (`src/federation/endpoints/inbox.rs`'s own doc comment,
//! "Destination-context construction").
//!
//! ## No HTTP client dependency for the raw socket itself: raw sockets
//! (this crate's established `tests/*_it.rs` convention).

use std::net::SocketAddr;
use std::time::Duration;

use axum::http::{HeaderName, HeaderValue, Method, header};
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use time::macros::format_description;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::model::{ProfileField, ProfilePatch, RemoteAccount};
use kawasemi::accounts::profile_repository::upsert_profile;
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::keys::material::generate_keypair;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, LocalActor, NewActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::federation::signatures::{
    Digest as BodyDigest, DraftCavageSuite, SignableRequest, SignatureSuite,
};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::runtime::SeededRng;
use kawasemi::social_graph::repository::{upsert_block, upsert_follow, upsert_request};
use kawasemi::social_graph::{Block, Follow, FollowRequest, FollowRequestDirection};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// raw HTTP plumbing (this crate's established `tests/*_it.rs` convention;
// duplicated per file since each integration test is its own compiled
// crate)
// ==========================================================================

#[derive(Debug)]
struct RawResponse {
    status: u16,
    body: String,
}

/// Unlike sibling files' own `raw_request` (which hardcodes `Host:
/// 127.0.0.1` in the request line before appending caller-supplied
/// headers), this file's signed requests must send *exactly one* `Host`
/// header carrying the value the signature itself covers
/// (`ActorUrls`'s configured domain, not the physical loopback address) —
/// so this helper takes the complete header list from the caller instead.
async fn raw_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
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

    parse_response(&String::from_utf8_lossy(&buf))
}

async fn raw_post(addr: SocketAddr, path: &str, token: &str) -> RawResponse {
    raw_request(
        addr,
        "POST",
        path,
        &[("Authorization".to_string(), bearer_header(token))],
        b"",
    )
    .await
}

fn parse_response(raw: &str) -> RawResponse {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    RawResponse {
        status,
        body: body.to_string(),
    }
}

fn body_json(response: &RawResponse) -> Value {
    serde_json::from_str(&response.body)
        .unwrap_or_else(|e| panic!("response body must be valid JSON: {e}; body: {response:?}"))
}

fn bearer_header(token: &str) -> String {
    format!("Bearer {token}")
}

// ==========================================================================
// fixtures
// ==========================================================================

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "social_graph_inbound_it Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: PlaceholderScopeSet::new(["read", "write", "follow"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

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
            display_name: format!("Social Graph Inbound IT {handle_str}"),
            summary: "a social_graph_inbound_it integration test fixture".to_string(),
        })
        .await
        .expect("create_actor must succeed for a valid owner and a fresh handle")
}

async fn lock_actor(app: &TestApp, actor_id: Id) {
    upsert_profile(
        &app.pool,
        actor_id,
        ProfilePatch {
            locked: Some(true),
            ..Default::default()
        },
        app.runtime.clock.now(),
    )
    .await
    .expect("locking the test actor's profile must succeed");
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

fn urls(app: &TestApp) -> ActorUrls {
    ActorUrls::new(app.state.config().server.domain.clone())
}

/// A simulated remote signer: a real RSA-2048 keypair whose public key is
/// pre-seeded into the real `remote_public_keys`/`remote_accounts` caches
/// so the live `HttpSignatureVerifier`/`RemoteAccountFetcher` resolve it
/// without any network call (this module's own doc comment, "Simulating a
/// genuine remote signer").
struct RemoteSigner {
    id: Id,
    actor_uri: String,
    key_id: String,
    private_key: RsaPrivateKey,
}

fn test_keypair(seed: u64) -> (RsaPrivateKey, String) {
    let generated =
        generate_keypair(&SeededRng::new(seed)).expect("test key generation must succeed");
    let private_key = RsaPrivateKey::from_pkcs8_pem(generated.private_key_pem.expose_secret())
        .expect("generated private key PEM must parse");
    (private_key, generated.public_key_pem)
}

async fn seed_remote_signer(app: &TestApp, slug: &str, seed: u64) -> RemoteSigner {
    let actor_uri = format!("https://remote.example/users/{slug}");
    let key_id = format!("{actor_uri}#main-key");
    let (private_key, public_key_pem) = test_keypair(seed);
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();

    upsert_remote(
        &app.pool,
        &RemoteAccount {
            id,
            actor_uri: actor_uri.clone(),
            username: slug.to_string(),
            domain: "remote.example".to_string(),
            display_name: format!("Remote {slug}"),
            note: String::new(),
            url: actor_uri.clone(),
            avatar_url: None,
            header_url: None,
            fields: Vec::<ProfileField>::new(),
            bot: false,
            locked: false,
            fetched_at: now,
        },
    )
    .await
    .expect("seeding the remote signer's account cache row must succeed");

    sqlx::query(
        "INSERT INTO remote_public_keys (key_id, actor_uri, public_key_pem, fetched_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&key_id)
    .bind(&actor_uri)
    .bind(&public_key_pem)
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding a cached remote public key must succeed");

    RemoteSigner {
        id,
        actor_uri,
        key_id,
        private_key,
    }
}

async fn seed_follow(app: &TestApp, follower: AccountRef, followee: AccountRef, activity_id: &str) {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_follow(
        &app.pool,
        id,
        &Follow {
            follower,
            followee,
            reblogs: true,
            notify: false,
            languages: Vec::new(),
            activity_id: activity_id.to_string(),
            created_at: now,
        },
    )
    .await
    .expect("seeding a follows row must succeed");
}

async fn seed_pending_request(
    app: &TestApp,
    requester: AccountRef,
    target: AccountRef,
    direction: FollowRequestDirection,
    activity_id: &str,
) {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_request(
        &app.pool,
        id,
        &FollowRequest {
            requester,
            target,
            direction,
            activity_id: activity_id.to_string(),
            created_at: now,
        },
    )
    .await
    .expect("seeding a follow_requests row must succeed");
}

async fn seed_block(app: &TestApp, blocker: AccountRef, blocked: AccountRef, activity_id: &str) {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_block(
        &app.pool,
        id,
        &Block {
            blocker,
            blocked,
            activity_id: activity_id.to_string(),
            created_at: now,
        },
    )
    .await
    .expect("seeding a blocks row must succeed");
}

async fn follows_row_count(app: &TestApp, follower: (&str, i64), followee: (&str, i64)) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM follows \
         WHERE follower_kind = $1 AND follower_id = $2 \
           AND followee_kind = $3 AND followee_id = $4",
    )
    .bind(follower.0)
    .bind(follower.1)
    .bind(followee.0)
    .bind(followee.1)
    .fetch_one(&app.pool)
    .await
    .expect("counting follows rows must succeed")
}

async fn follow_requests_row_count(
    app: &TestApp,
    requester: (&str, i64),
    target: (&str, i64),
) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM follow_requests \
         WHERE requester_kind = $1 AND requester_id = $2 \
           AND target_kind = $3 AND target_id = $4",
    )
    .bind(requester.0)
    .bind(requester.1)
    .bind(target.0)
    .bind(target.1)
    .fetch_one(&app.pool)
    .await
    .expect("counting follow_requests rows must succeed")
}

async fn follow_request_direction(
    app: &TestApp,
    requester: (&str, i64),
    target: (&str, i64),
) -> Option<String> {
    sqlx::query_scalar(
        "SELECT direction FROM follow_requests \
         WHERE requester_kind = $1 AND requester_id = $2 \
           AND target_kind = $3 AND target_id = $4",
    )
    .bind(requester.0)
    .bind(requester.1)
    .bind(target.0)
    .bind(target.1)
    .fetch_optional(&app.pool)
    .await
    .expect("querying the seeded follow_requests row's direction must succeed")
}

async fn blocks_row_count(app: &TestApp, blocker: (&str, i64), blocked: (&str, i64)) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM blocks \
         WHERE blocker_kind = $1 AND blocker_id = $2 AND blocked_kind = $3 AND blocked_id = $4",
    )
    .bind(blocker.0)
    .bind(blocker.1)
    .bind(blocked.0)
    .bind(blocked.1)
    .fetch_one(&app.pool)
    .await
    .expect("counting blocks rows must succeed")
}

async fn delivery_job_count_for_type(
    app: &TestApp,
    target_inbox: &str,
    activity_type: &str,
) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM delivery_jobs \
         WHERE target_inbox = $1 AND activity->>'type' = $2",
    )
    .bind(target_inbox)
    .bind(activity_type)
    .fetch_one(&app.pool)
    .await
    .expect("counting delivery_jobs rows must succeed")
}

fn remote_inbox(actor_uri: &str) -> String {
    format!("{actor_uri}/inbox")
}

// ==========================================================================
// signing helpers (mirrors `tests/inbox_it.rs`'s own established
// draft-cavage signing convention; duplicated here per this crate's
// per-file-independence convention -- see this module's own doc comment,
// "Simulating a genuine remote signer")
// ==========================================================================

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

/// Hand-builds the real, genuinely-signed (draft-cavage) header set for a
/// `POST` of `body` to `url` -- the exact canonical inbox/shared-inbox URL
/// this instance's own live router signs against
/// (`src/federation/endpoints/inbox.rs`). Mirrors
/// `tests/inbox_it.rs::sign_post_request`.
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
                name.as_str().to_string(),
                value
                    .to_str()
                    .expect("test header values are ASCII")
                    .to_string(),
            )
        })
        .collect()
}

async fn post_signed_activity(
    app: &TestApp,
    inbox_path: &str,
    url: &str,
    signer: &RemoteSigner,
    activity: &Value,
) -> RawResponse {
    let body = serde_json::to_vec(activity).expect("serializing a test Activity body");
    let when = app.runtime.clock.now();
    let domain = app.state.config().server.domain.clone();
    let headers = sign_post_request(
        url,
        &domain,
        &signer.key_id,
        &signer.private_key,
        when,
        &body,
    );
    raw_request(app.address, "POST", inbox_path, &headers, &body).await
}

// ==========================================================================
// Activity body builders
// ==========================================================================

fn follow_activity(id: &str, actor_uri: &str, object_uri: &str) -> Value {
    json!({ "id": id, "type": "Follow", "actor": actor_uri, "object": object_uri })
}

fn accept_or_reject_activity(
    id: &str,
    outer_type: &str,
    approving_actor_uri: &str,
    inner_follow_id: &str,
    requester_uri: &str,
    target_uri: &str,
) -> Value {
    json!({
        "id": id,
        "type": outer_type,
        "actor": approving_actor_uri,
        "object": {
            "id": inner_follow_id,
            "type": "Follow",
            "actor": requester_uri,
            "object": target_uri,
        }
    })
}

fn block_activity(id: &str, actor_uri: &str, object_uri: &str) -> Value {
    json!({ "id": id, "type": "Block", "actor": actor_uri, "object": object_uri })
}

fn undo_activity(
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

fn inbox_path(handle: &Handle) -> String {
    format!("/users/{}/inbox", handle.as_str())
}

// ==========================================================================
// (1) Requirement 7.2: inbound Follow to an unlocked local actor
// establishes the follow and delivers Accept(Follow) back to the source.
// ==========================================================================

#[tokio::test]
async fn inbound_follow_to_unlocked_local_actor_establishes_follow_and_delivers_accept() {
    let app = spawn_test_app().await;
    let target = insert_actor_fixture(&app, "sgi_follow_target").await;
    let signer = seed_remote_signer(&app, "sgi_follow_source", 1).await;
    let ourls = urls(&app);
    let target_uri = ourls.actor_url(&target.handle);
    let inbox_url = ourls.inbox_url(&target.handle);

    let body = follow_activity(
        "https://remote.example/activities/sgi-follow-1",
        &signer.actor_uri,
        &target_uri,
    );
    let response = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &body,
    )
    .await;
    assert_eq!(response.status, 202, "got: {response:?}");

    assert_eq!(
        follows_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        1,
        "an inbound Follow to an unlocked local actor must establish a follows row"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(&signer.actor_uri), "Accept").await,
        1,
        "establishing the follow must enqueue exactly one Accept(Follow) back to the source"
    );

    app.cleanup().await;
}

// ==========================================================================
// (2) Requirement 7.3: inbound Follow to a locked local actor records a
// pending inbound follow request instead of establishing immediately.
// ==========================================================================

#[tokio::test]
async fn inbound_follow_to_locked_local_actor_records_pending_inbound_request() {
    let app = spawn_test_app().await;
    let target = insert_actor_fixture(&app, "sgi_locked_target").await;
    lock_actor(&app, target.id).await;
    let signer = seed_remote_signer(&app, "sgi_locked_source", 2).await;
    let ourls = urls(&app);
    let target_uri = ourls.actor_url(&target.handle);
    let inbox_url = ourls.inbox_url(&target.handle);

    let body = follow_activity(
        "https://remote.example/activities/sgi-follow-locked-1",
        &signer.actor_uri,
        &target_uri,
    );
    let response = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &body,
    )
    .await;
    assert_eq!(response.status, 202, "got: {response:?}");

    assert_eq!(
        follows_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        0,
        "a locked local actor must not establish the follow immediately"
    );
    assert_eq!(
        follow_request_direction(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await
        .as_deref(),
        Some("inbound"),
        "the pending request recorded on receipt must be direction=Inbound"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(&signer.actor_uri), "Accept").await,
        0,
        "a pending (not yet approved) follow must not deliver an immediate Accept"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Requirement 7.7 (a): the exact same inbound Follow Activity id,
// received twice, is deduplicated by federation-core's own dedup ledger --
// no second follows row, no second Accept.
// ==========================================================================

#[tokio::test]
async fn inbound_duplicate_follow_activity_id_is_deduplicated_without_a_second_accept() {
    let app = spawn_test_app().await;
    let target = insert_actor_fixture(&app, "sgi_dup_target").await;
    let signer = seed_remote_signer(&app, "sgi_dup_source", 3).await;
    let ourls = urls(&app);
    let target_uri = ourls.actor_url(&target.handle);
    let inbox_url = ourls.inbox_url(&target.handle);

    let body = follow_activity(
        "https://remote.example/activities/sgi-follow-dup-1",
        &signer.actor_uri,
        &target_uri,
    );

    let first = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &body,
    )
    .await;
    assert_eq!(first.status, 202, "got: {first:?}");
    let second = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &body,
    )
    .await;
    assert_eq!(
        second.status, 202,
        "a duplicate Activity id must still be acked (not re-dispatched), got: {second:?}"
    );

    assert_eq!(
        follows_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        1,
        "re-delivering the identical Activity id must not duplicate the follows row"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(&signer.actor_uri), "Accept").await,
        1,
        "re-delivering the identical Activity id must not enqueue a second Accept"
    );

    app.cleanup().await;
}

// ==========================================================================
// (4) Requirement 7.7 (b): a *second, distinct* Follow Activity id for an
// already-established pair is still idempotent at the state layer and does
// not rebuild/redeliver a second Accept -- this handler's own documented
// pre-check (tasks.md Implementation Notes, task 4.1: "受信 Follow の重複配
// 送抑制...は load_states による事前チェックに依存").
// ==========================================================================

#[tokio::test]
async fn inbound_follow_already_established_is_idempotent_even_with_a_new_activity_id() {
    let app = spawn_test_app().await;
    let target = insert_actor_fixture(&app, "sgi_reestablish_target").await;
    let signer = seed_remote_signer(&app, "sgi_reestablish_source", 4).await;
    let ourls = urls(&app);
    let target_uri = ourls.actor_url(&target.handle);
    let inbox_url = ourls.inbox_url(&target.handle);

    let first_body = follow_activity(
        "https://remote.example/activities/sgi-reestablish-a",
        &signer.actor_uri,
        &target_uri,
    );
    let first = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &first_body,
    )
    .await;
    assert_eq!(first.status, 202, "got: {first:?}");

    let second_body = follow_activity(
        "https://remote.example/activities/sgi-reestablish-b",
        &signer.actor_uri,
        &target_uri,
    );
    let second = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &second_body,
    )
    .await;
    assert_eq!(second.status, 202, "got: {second:?}");

    assert_eq!(
        follows_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        1,
        "a second, distinct Follow id for an already-established pair must not duplicate the follows row"
    );
    assert_eq!(
        delivery_job_count_for_type(&app, &remote_inbox(&signer.actor_uri), "Accept").await,
        1,
        "the handler's own idempotency pre-check must not rebuild/redeliver a second Accept"
    );

    app.cleanup().await;
}

// ==========================================================================
// (5) Requirement 7.4: inbound Block records blocked_by and clears the
// bidirectional follow/pending-request state between the pair, within one
// transaction.
// ==========================================================================

#[tokio::test]
async fn inbound_block_records_blocked_by_and_clears_bidirectional_relationship() {
    let app = spawn_test_app().await;
    let target = insert_actor_fixture(&app, "sgi_block_target").await;
    let signer = seed_remote_signer(&app, "sgi_block_source", 5).await;
    let target_ref = AccountRef::Local(target.id);
    let signer_ref = AccountRef::Remote(signer.id);
    let ourls = urls(&app);
    let target_uri = ourls.actor_url(&target.handle);
    let inbox_url = ourls.inbox_url(&target.handle);

    seed_follow(
        &app,
        target_ref,
        signer_ref,
        "https://remote.example/activities/sgi-block-pre-follow-out",
    )
    .await;
    seed_follow(
        &app,
        signer_ref,
        target_ref,
        "https://remote.example/activities/sgi-block-pre-follow-in",
    )
    .await;
    seed_pending_request(
        &app,
        target_ref,
        signer_ref,
        FollowRequestDirection::Outbound,
        "https://remote.example/activities/sgi-block-pre-pending-out",
    )
    .await;
    seed_pending_request(
        &app,
        signer_ref,
        target_ref,
        FollowRequestDirection::Inbound,
        "https://remote.example/activities/sgi-block-pre-pending-in",
    )
    .await;

    let body = block_activity(
        "https://remote.example/activities/sgi-block-1",
        &signer.actor_uri,
        &target_uri,
    );
    let response = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &body,
    )
    .await;
    assert_eq!(response.status, 202, "got: {response:?}");

    assert_eq!(
        blocks_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        1,
        "the inbound Block must record blocked_by (blocker=signer, blocked=target)"
    );
    assert_eq!(
        follows_row_count(
            &app,
            ("local", target.id.as_i64()),
            ("remote", signer.id.as_i64())
        )
        .await,
        0,
        "an inbound Block must clear the outbound-direction follow"
    );
    assert_eq!(
        follows_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        0,
        "an inbound Block must clear the inbound-direction follow"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("local", target.id.as_i64()),
            ("remote", signer.id.as_i64())
        )
        .await,
        0,
        "an inbound Block must clear the outbound pending request"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        0,
        "an inbound Block must clear the inbound pending request"
    );

    app.cleanup().await;
}

// ==========================================================================
// (6) Requirement 7.5: inbound Undo(Follow) removes the established follow.
// ==========================================================================

#[tokio::test]
async fn inbound_undo_follow_removes_the_established_follow() {
    let app = spawn_test_app().await;
    let target = insert_actor_fixture(&app, "sgi_undo_follow_target").await;
    let signer = seed_remote_signer(&app, "sgi_undo_follow_source", 6).await;
    let target_ref = AccountRef::Local(target.id);
    let signer_ref = AccountRef::Remote(signer.id);
    let ourls = urls(&app);
    let target_uri = ourls.actor_url(&target.handle);
    let inbox_url = ourls.inbox_url(&target.handle);
    let original_follow_id = "https://remote.example/activities/sgi-undo-follow-original";

    seed_follow(&app, signer_ref, target_ref, original_follow_id).await;
    assert_eq!(
        follows_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        1,
        "precondition: the follow must exist before Undo"
    );

    let body = undo_activity(
        "https://remote.example/activities/sgi-undo-follow-1",
        &signer.actor_uri,
        "Follow",
        original_follow_id,
        &target_uri,
    );
    let response = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &body,
    )
    .await;
    assert_eq!(response.status, 202, "got: {response:?}");

    assert_eq!(
        follows_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        0,
        "Undo(Follow) must remove the source's follow of the target"
    );

    app.cleanup().await;
}

// ==========================================================================
// (7) Requirement 7.6: inbound Undo(Block) clears the blocked_by state.
// ==========================================================================

#[tokio::test]
async fn inbound_undo_block_clears_blocked_by() {
    let app = spawn_test_app().await;
    let target = insert_actor_fixture(&app, "sgi_undo_block_target").await;
    let signer = seed_remote_signer(&app, "sgi_undo_block_source", 7).await;
    let target_ref = AccountRef::Local(target.id);
    let signer_ref = AccountRef::Remote(signer.id);
    let ourls = urls(&app);
    let target_uri = ourls.actor_url(&target.handle);
    let inbox_url = ourls.inbox_url(&target.handle);
    let original_block_id = "https://remote.example/activities/sgi-undo-block-original";

    seed_block(&app, signer_ref, target_ref, original_block_id).await;
    assert_eq!(
        blocks_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        1,
        "precondition: the blocked_by row must exist before Undo"
    );

    let body = undo_activity(
        "https://remote.example/activities/sgi-undo-block-1",
        &signer.actor_uri,
        "Block",
        original_block_id,
        &target_uri,
    );
    let response = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &body,
    )
    .await;
    assert_eq!(response.status, 202, "got: {response:?}");

    assert_eq!(
        blocks_row_count(
            &app,
            ("remote", signer.id.as_i64()),
            ("local", target.id.as_i64())
        )
        .await,
        0,
        "Undo(Block) must clear the source's block of the target"
    );

    app.cleanup().await;
}

// ==========================================================================
// (8) Requirement 2.5 (bonus -- named directly in task 6.2's own bullet
// text, "受信...Accept...の状態遷移", even though not itemized on its
// `_Requirements:_` line): inbound Accept promotes a pending outbound
// follow request to an established follow.
// ==========================================================================

#[tokio::test]
async fn inbound_accept_promotes_pending_outbound_follow_to_established() {
    let app = spawn_test_app().await;
    let requester = insert_actor_fixture(&app, "sgi_accept_requester").await;
    let approver = seed_remote_signer(&app, "sgi_accept_approver", 8).await;
    let requester_ref = AccountRef::Local(requester.id);
    let approver_ref = AccountRef::Remote(approver.id);
    let ourls = urls(&app);
    let requester_uri = ourls.actor_url(&requester.handle);
    let inbox_url = ourls.inbox_url(&requester.handle);
    let original_follow_id = "https://local.example/activities/sgi-accept-original";

    seed_pending_request(
        &app,
        requester_ref,
        approver_ref,
        FollowRequestDirection::Outbound,
        original_follow_id,
    )
    .await;

    let body = accept_or_reject_activity(
        "https://remote.example/activities/sgi-accept-1",
        "Accept",
        &approver.actor_uri,
        original_follow_id,
        &requester_uri,
        &approver.actor_uri,
    );
    let response = post_signed_activity(
        &app,
        &inbox_path(&requester.handle),
        &inbox_url,
        &approver,
        &body,
    )
    .await;
    assert_eq!(response.status, 202, "got: {response:?}");

    assert_eq!(
        follows_row_count(
            &app,
            ("local", requester.id.as_i64()),
            ("remote", approver.id.as_i64())
        )
        .await,
        1,
        "receiving Accept must establish the previously-pending outbound follow"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("local", requester.id.as_i64()),
            ("remote", approver.id.as_i64())
        )
        .await,
        0,
        "receiving Accept must consume the pending outbound request"
    );

    app.cleanup().await;
}

// ==========================================================================
// (9) Requirement 2.6 (bonus, same rationale as (8)): inbound Reject drops
// the pending outbound follow request without establishing a follow.
// ==========================================================================

#[tokio::test]
async fn inbound_reject_drops_pending_outbound_follow_without_establishing() {
    let app = spawn_test_app().await;
    let requester = insert_actor_fixture(&app, "sgi_reject_requester").await;
    let approver = seed_remote_signer(&app, "sgi_reject_approver", 9).await;
    let requester_ref = AccountRef::Local(requester.id);
    let approver_ref = AccountRef::Remote(approver.id);
    let ourls = urls(&app);
    let requester_uri = ourls.actor_url(&requester.handle);
    let inbox_url = ourls.inbox_url(&requester.handle);
    let original_follow_id = "https://local.example/activities/sgi-reject-original";

    seed_pending_request(
        &app,
        requester_ref,
        approver_ref,
        FollowRequestDirection::Outbound,
        original_follow_id,
    )
    .await;

    let body = accept_or_reject_activity(
        "https://remote.example/activities/sgi-reject-1",
        "Reject",
        &approver.actor_uri,
        original_follow_id,
        &requester_uri,
        &approver.actor_uri,
    );
    let response = post_signed_activity(
        &app,
        &inbox_path(&requester.handle),
        &inbox_url,
        &approver,
        &body,
    )
    .await;
    assert_eq!(response.status, 202, "got: {response:?}");

    assert_eq!(
        follows_row_count(
            &app,
            ("local", requester.id.as_i64()),
            ("remote", approver.id.as_i64())
        )
        .await,
        0,
        "receiving Reject must never establish a follow"
    );
    assert_eq!(
        follow_requests_row_count(
            &app,
            ("local", requester.id.as_i64()),
            ("remote", approver.id.as_i64())
        )
        .await,
        0,
        "receiving Reject must consume (delete) the pending outbound request"
    );

    app.cleanup().await;
}

// ==========================================================================
// (10) Requirements 6.1, 6.2, 6.3: once a signer is blocked (via the real
// block API, persisting a real `blocks` row `BlockPolicyImpl` reads), the
// live federation-core pipeline rejects that signer's per-actor inbox
// delivery with 403, repeatedly, for as long as the block is active -- and
// the same signer, addressed via the shared inbox instead, is *not*
// bulk-rejected (`LocalRecipientContext::SharedInbox` always returns
// false, design.md's own documented contract).
// ==========================================================================

#[tokio::test]
async fn blocked_signer_is_rejected_from_the_actor_inbox_but_shared_inbox_is_not_bulk_rejected() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let target = insert_actor_fixture(&app, "sgi_bp_target").await;
    let owner_token = issue_token(&app, oauth_app_id, target.id, &["follow"]).await;
    let signer = seed_remote_signer(&app, "sgi_bp_source", 10).await;
    let ourls = urls(&app);
    let target_uri = ourls.actor_url(&target.handle);
    let inbox_url = ourls.inbox_url(&target.handle);
    let shared_inbox_url = ourls.shared_inbox_url();

    // Baseline: unblocked signer is accepted normally.
    let baseline = follow_activity(
        "https://remote.example/activities/sgi-bp-baseline",
        &signer.actor_uri,
        &target_uri,
    );
    let baseline_response = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &baseline,
    )
    .await;
    assert_eq!(baseline_response.status, 202, "got: {baseline_response:?}");

    // Block the signer for real, through the live block API (Requirement
    // 6.1: this spec's own real block relationship, exactly the state
    // `BlockPolicyImpl::is_blocked` reads via `repository::is_blocked`).
    let block_response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/block", signer.id.as_i64()),
        &owner_token,
    )
    .await;
    assert_eq!(block_response.status, 200, "got: {block_response:?}");
    assert_eq!(
        body_json(&block_response)["blocking"].as_bool(),
        Some(true),
        "got: {block_response:?}"
    );

    // Requirement 6.2/6.3: repeated per-actor-inbox attempts from the
    // blocked signer stay rejected for as long as the block is active.
    for attempt in 1..=2 {
        let body = follow_activity(
            &format!("https://remote.example/activities/sgi-bp-blocked-{attempt}"),
            &signer.actor_uri,
            &target_uri,
        );
        let response = post_signed_activity(
            &app,
            &inbox_path(&target.handle),
            &inbox_url,
            &signer,
            &body,
        )
        .await;
        assert_eq!(
            response.status, 403,
            "attempt {attempt} from a blocked signer's per-actor inbox must be rejected, got: {response:?}"
        );
    }

    // The identical blocked signer, addressed via the shared inbox
    // instead, must not be bulk-rejected (design.md: "共有 inbox 文脈では
    // 常に偽を返し...一括拒否しない").
    let shared_body = follow_activity(
        "https://remote.example/activities/sgi-bp-shared",
        &signer.actor_uri,
        &target_uri,
    );
    let shared_response =
        post_signed_activity(&app, "/inbox", &shared_inbox_url, &signer, &shared_body).await;
    assert_eq!(
        shared_response.status, 202,
        "the shared inbox must never bulk-reject a blocked signer, got: {shared_response:?}"
    );

    app.cleanup().await;
}

// ==========================================================================
// (11) Requirement 6.4: unblocking restores acceptance from the previously
// blocked signer.
// ==========================================================================

#[tokio::test]
async fn unblocking_restores_acceptance_from_the_previously_blocked_signer() {
    let app = spawn_test_app().await;
    let oauth_app_id = register_test_app(&app).await;
    let target = insert_actor_fixture(&app, "sgi_unbp_target").await;
    let owner_token = issue_token(&app, oauth_app_id, target.id, &["follow"]).await;
    let signer = seed_remote_signer(&app, "sgi_unbp_source", 11).await;
    let ourls = urls(&app);
    let target_uri = ourls.actor_url(&target.handle);
    let inbox_url = ourls.inbox_url(&target.handle);

    let block_response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/block", signer.id.as_i64()),
        &owner_token,
    )
    .await;
    assert_eq!(block_response.status, 200, "got: {block_response:?}");

    let while_blocked = follow_activity(
        "https://remote.example/activities/sgi-unbp-while-blocked",
        &signer.actor_uri,
        &target_uri,
    );
    let rejected = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &while_blocked,
    )
    .await;
    assert_eq!(rejected.status, 403, "got: {rejected:?}");

    let unblock_response = raw_post(
        app.address,
        &format!("/api/v1/accounts/{}/unblock", signer.id.as_i64()),
        &owner_token,
    )
    .await;
    assert_eq!(unblock_response.status, 200, "got: {unblock_response:?}");
    assert_eq!(
        body_json(&unblock_response)["blocking"].as_bool(),
        Some(false),
        "got: {unblock_response:?}"
    );

    let after_unblock = follow_activity(
        "https://remote.example/activities/sgi-unbp-after-unblock",
        &signer.actor_uri,
        &target_uri,
    );
    let accepted = post_signed_activity(
        &app,
        &inbox_path(&target.handle),
        &inbox_url,
        &signer,
        &after_unblock,
    )
    .await;
    assert_eq!(
        accepted.status, 202,
        "after unblocking, the same signer must be accepted again, got: {accepted:?}"
    );

    app.cleanup().await;
}
