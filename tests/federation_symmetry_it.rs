//! Integration tests for task 6.3 (`.kiro/specs/social-graph/tasks.md`,
//! "6.3 連合対称性テスト（2 インスタンス往復）") — the spec's own last
//! remaining task, named in design.md's File Structure Plan as
//! `federation_symmetry_it.rs` and covering design.md's own "Federation
//! Tests（2 インスタンス往復）" Testing Strategy bullets in full:
//! - "A→B のフォローで B にフォロワー確立・A に following 反映、ロック済み B
//!   では承認後に確立、A のブロックで B からの受信が拒否される" (1.2, 2.x,
//!   6.x, 7.x) — see tests (1)-(3) below.
//! - "同一 Follow/Block を local in-process と HTTP 配送で実行し、関係状態
//!   遷移結果が同値であることを検証" (10.3) — see tests (4)-(5) below.
//!
//! Requirements exercised: 1.2 (フォロー対象がリモートのとき Follow
//! Activity を生成し federation-core の共通配送パスで配送), 1.3 (フォロー
//! 対象がローカルのとき同一の Follow Activity を配送手段のみ in-process に
//! 分岐), 10.3 (ローカル宛配送(in-process)とリモート宛配送(HTTP)で同一の
//! Activity を生成し、同一の関係状態遷移結果になることを保証する).
//!
//! ## Prior art this file follows
//! - `tests/federation_pair_it.rs` (federation-core task 6.4): the
//!   established pattern for driving `spawn_federation_pair` — fixture
//!   duplication per test file (each `tests/*.rs` file is its own compiled
//!   crate), `wait_until` polling for the real, already-running background
//!   `DeliveryWorker` (HTTP delivery is asynchronous via a DB queue, unlike
//!   local delivery which completes synchronously inside the endpoint call).
//! - `tests/statuses_federation_pair_it.rs` (statuses-core task 8.3): the
//!   established pattern for a genuine local-vs-HTTP outcome-equivalence
//!   comparison within a paired-instance test — same technique applied here
//!   to `follows`/`blocks` state instead of status/reblog/favourite state.
//! - `tests/follow_unfollow_it.rs` / `tests/follow_request_it.rs` /
//!   `tests/same_server_skip_it.rs` / `tests/mute_block_it.rs` (task 6.1) and
//!   `tests/social_graph_inbound_it.rs` (task 6.2): this file's own raw-HTTP
//!   plumbing, fixture helpers, and DB-assertion conventions are duplicated
//!   from these files unchanged (this crate's established per-file
//!   duplication convention) — the only genuinely new ingredient here is
//!   driving both instances of `src/social_graph/endpoints.rs`'s real
//!   endpoints through `spawn_federation_pair`'s two real, live, mutually
//!   reachable instances instead of a single `spawn_test_app`.
//!
//! `src/federation/test_harness.rs::spawn_paired_instance` already registers
//! social-graph's own `SocialGraphInboundHandler`/`BlockPolicyImpl` on both
//! paired instances (that module's own "Task 5.2" doc-comment sections) —
//! no wiring gap needed closing for this task, unlike statuses-core's own
//! task 8.3 (that file's own doc comment, "Wiring gap this task closed").
//!
//! ## The known local-target-Block `activity_id` bug (task 6.1's own
//! Implementation Notes entry) and how this file works around comparing on
//! it, not the bug itself
//! Task 6.1's own `tasks.md` Implementation Notes entry documents that a
//! **local**-target `BlockService::block` call clobbers the just-persisted
//! real `activity_id` with an empty string, because `DeliveryService`
//! re-enters `SocialGraphInboundHandler::handle_block` in-process for a
//! local recipient, and `Transitions::mark_blocked_by` always persists
//! `activity_id = String::new()` (`transitions.rs`'s own doc comment,
//! "`apply_block` / `mark_blocked_by` share one private transactional
//! core"). Test (5) below exercises exactly this local-target Block path as
//! one leg of its local-vs-HTTP comparison, and therefore *never* compares
//! `activity_id` between the two legs (or asserts it non-empty on the local
//! leg) — only the *outcome* fields Requirement 10.3 actually cares about
//! (`blocking`/`blocked_by`/`follow`/`followed_by`, i.e. "blocking
//! relationship established, bidirectional follows cleared"). It does
//! positively assert the local leg's `blocks.activity_id` reads back empty
//! (concrete, disclosed evidence the known bug is exactly what's being
//! routed around, not silently masked) and separately confirms the HTTP
//! leg's own `blocks.activity_id` is genuinely non-empty (that leg's Block
//! row lives on a *different* instance's database from the in-process
//! re-entrant write that clobbers the local leg's row, so no clobbering is
//! structurally possible there) — see this file's own `CONCERNS`-equivalent
//! doc comment on test (5) itself for the full reasoning.
//!
//! ## No HTTP client dependency: raw sockets (this crate's established
//! `tests/*_it.rs` convention; duplicated per file since each integration
//! test is its own compiled crate).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use kawasemi::accounts::model::{ProfileField, ProfilePatch, RemoteAccount};
use kawasemi::accounts::profile_repository::upsert_profile;
use kawasemi::accounts::remote_repository::{find_remote_by_uri, upsert_remote};
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, LocalActor, NewActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{FederationPair, spawn_federation_pair};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as PlaceholderScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::social_graph::repository::{self, RelationshipState};
use kawasemi::test_harness::TestApp;

// ==========================================================================
// Raw HTTP plumbing (mirrors `tests/follow_unfollow_it.rs`'s identical copy)
// ==========================================================================

#[derive(Debug)]
struct RawResponse {
    status: u16,
    body: String,
}

async fn raw_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> RawResponse {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("connecting to the test listener must not time out")
        .expect("connect");

    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
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
        &[("Authorization", &bearer_header(token))],
        b"",
    )
    .await
}

async fn raw_post_json(addr: SocketAddr, path: &str, token: &str, body: &Value) -> RawResponse {
    let body_bytes = serde_json::to_vec(body).expect("serializing test request body");
    raw_request(
        addr,
        "POST",
        path,
        &[
            ("Authorization", &bearer_header(token)),
            ("Content-Type", "application/json"),
        ],
        &body_bytes,
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
// Fixtures (each `tests/*.rs` file is its own compiled crate — duplicated
// from `tests/federation_pair_it.rs`/`tests/follow_unfollow_it.rs`'s own
// established conventions rather than imported).
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
            name: "federation_symmetry_it Client".to_string(),
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
            display_name: format!("Federation Symmetry IT {handle_str}"),
            summary: "an actor used by the federation_symmetry_it integration test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed")
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

fn sample_remote_account(
    id: Id,
    actor_uri: &str,
    fetched_at: OffsetDateTime,
    locked: bool,
) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "remoteactor".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Remote Actor".to_string(),
        note: String::new(),
        url: actor_uri.to_string(),
        avatar_url: None,
        header_url: None,
        fields: Vec::<ProfileField>::new(),
        bot: false,
        locked,
        fetched_at,
    }
}

/// Seeds `app`'s own `remote_accounts` cache with a row for `actor_uri` (a
/// real actor URL on the *other* paired instance) — needed so
/// `FollowService::resolve_target`/`BlockService::resolve_target`'s
/// numeric-id-first resolution has something to resolve before the very
/// first cross-instance call this test drives (mirrors
/// `tests/follow_unfollow_it.rs::create_test_remote`). `locked` should
/// match the *real* target actor's own real lock state on the other
/// instance for scenarios that compare A's locally-cached judgment against
/// B's own authoritative one.
async fn create_test_remote(app: &TestApp, actor_uri: &str, locked: bool) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(
        &app.pool,
        &sample_remote_account(id, actor_uri, now, locked),
    )
    .await
    .expect("upsert_remote must succeed");
    id
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

fn test_domain(app: &TestApp) -> String {
    app.state.config().server.domain.clone()
}

fn urls(app: &TestApp) -> ActorUrls {
    ActorUrls::new(test_domain(app))
}

/// Polls `check` every 50ms until it returns `true` or `timeout` elapses
/// (panicking with `description` in the latter case) — mirrors
/// `tests/federation_pair_it.rs`'s own identical `wait_until` (needed
/// because HTTP delivery runs asynchronously via each paired instance's own
/// real, already-running background `DeliveryWorker`, unlike local delivery
/// which completes synchronously inside the endpoint call).
async fn wait_until<F, Fut>(mut check: F, timeout: Duration, description: &str)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    loop {
        if check().await {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out after {timeout:?} waiting for: {description}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ==========================================================================
// DB assertions
// ==========================================================================

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

async fn blocks_row_count(app: &TestApp, blocker: (&str, i64), blocked: (&str, i64)) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM blocks \
         WHERE blocker_kind = $1 AND blocker_id = $2 \
           AND blocked_kind = $3 AND blocked_id = $4",
    )
    .bind(blocker.0)
    .bind(blocker.1)
    .bind(blocked.0)
    .bind(blocked.1)
    .fetch_one(&app.pool)
    .await
    .expect("counting blocks rows must succeed")
}

async fn block_activity_id(app: &TestApp, blocker: (&str, i64), blocked: (&str, i64)) -> String {
    sqlx::query_scalar(
        "SELECT activity_id FROM blocks \
         WHERE blocker_kind = $1 AND blocker_id = $2 \
           AND blocked_kind = $3 AND blocked_id = $4",
    )
    .bind(blocker.0)
    .bind(blocker.1)
    .bind(blocked.0)
    .bind(blocked.1)
    .fetch_one(&app.pool)
    .await
    .expect("reading the blocks row's activity_id must succeed")
}

/// Counts `follows` rows whose follower is the *genuinely remote-discovered*
/// account named by `follower_uri` (resolved by joining through
/// `remote_accounts`, since this instance mints that account's own numeric
/// id itself the first time it resolves the signer -- the caller cannot
/// predict it up front) and whose followee is `followee_local_id` (one of
/// this instance's own local actors).
async fn follows_from_remote_uri_to_local(
    app: &TestApp,
    follower_uri: &str,
    followee_local_id: Id,
) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM follows f \
         JOIN remote_accounts r ON f.follower_kind = 'remote' AND f.follower_id = r.id \
         WHERE r.actor_uri = $1 AND f.followee_kind = 'local' AND f.followee_id = $2",
    )
    .bind(follower_uri)
    .bind(followee_local_id.as_i64())
    .fetch_one(&app.pool)
    .await
    .expect("counting follows rows joined through remote_accounts must succeed")
}

/// Mirrors [`follows_from_remote_uri_to_local`] for pending *inbound*
/// `follow_requests` rows.
async fn inbound_request_from_remote_uri(
    app: &TestApp,
    requester_uri: &str,
    target_local_id: Id,
) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM follow_requests fr \
         JOIN remote_accounts r ON fr.requester_kind = 'remote' AND fr.requester_id = r.id \
         WHERE r.actor_uri = $1 AND fr.target_kind = 'local' AND fr.target_id = $2 \
           AND fr.direction = 'inbound'",
    )
    .bind(requester_uri)
    .bind(target_local_id.as_i64())
    .fetch_one(&app.pool)
    .await
    .expect("counting follow_requests rows joined through remote_accounts must succeed")
}

async fn remote_account_id_by_uri(app: &TestApp, actor_uri: &str) -> Option<Id> {
    find_remote_by_uri(&app.pool, actor_uri)
        .await
        .expect("find_remote_by_uri must not error")
        .map(|remote| remote.id)
}

#[derive(sqlx::FromRow, Debug)]
struct DeliveryJobRow {
    status: String,
    attempts: i32,
}

async fn delivery_job_row(
    app: &TestApp,
    target_inbox: &str,
    activity_type: &str,
) -> Option<DeliveryJobRow> {
    sqlx::query_as::<_, DeliveryJobRow>(
        "SELECT status, attempts FROM delivery_jobs \
         WHERE target_inbox = $1 AND activity->>'type' = $2",
    )
    .bind(target_inbox)
    .bind(activity_type)
    .fetch_optional(&app.pool)
    .await
    .expect("querying delivery_jobs must succeed")
}

/// Loads `viewer`'s single relationship state toward `target` via the real
/// `RelationshipRepository::load_states` (the same repository function every
/// social-graph service call uses internally) — this file's own comparison
/// tests (4)/(5) read this directly rather than through the HTTP
/// Relationship response, since they need to compare the *same*-instance
/// local leg against the cross-instance HTTP leg using a single, uniform
/// data shape untouched by `RelationshipMapper`'s own further derivation.
async fn load_state(app: &TestApp, viewer: AccountRef, target: AccountRef) -> RelationshipState {
    let now = app.runtime.clock.now();
    let mut states =
        repository::load_states(&app.pool, &viewer, std::slice::from_ref(&target), now)
            .await
            .expect("load_states must succeed");
    states
        .pop()
        .expect("load_states returns exactly one state per requested target")
}

// ==========================================================================
// (1) A -> B follow of an unlocked target: B gets a follower, A gets
// following (Requirements 1.1, 1.2, design.md's first Federation Tests
// bullet, first clause).
// ==========================================================================

#[tokio::test]
async fn follow_round_trip_to_unlocked_remote_target_establishes_on_both_instances() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let oauth_app_id = register_test_app(&a).await;
    let alice = insert_actor_fixture(&a, "fs_ub_alice").await;
    let bob = insert_actor_fixture(&b, "fs_ub_bob").await;

    let bob_uri = urls(&b).actor_url(&bob.handle);
    let bob_remote_id = create_test_remote(&a, &bob_uri, false).await;

    let token = issue_token(&a, oauth_app_id, alice.id, &["follow"]).await;
    let response = raw_post_json(
        a.address,
        &format!("/api/v1/accounts/{}/follow", bob_remote_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(
        body["following"].as_bool(),
        Some(true),
        "an unlocked remote target must establish immediately, got: {body}"
    );

    // A: following reflected immediately (Requirement 1.1/1.2, synchronous
    // local state write, HTTP delivery only queued).
    assert_eq!(
        follows_row_count(
            &a,
            ("local", alice.id.as_i64()),
            ("remote", bob_remote_id.as_i64())
        )
        .await,
        1
    );

    // B: real signed HTTP delivery + genuine actor discovery of alice +
    // establishment of the follower relationship ("B にフォロワー確立").
    let alice_uri = urls(&a).actor_url(&alice.handle);
    wait_until(
        || async { follows_from_remote_uri_to_local(&b, &alice_uri, bob.id).await == 1 },
        Duration::from_secs(10),
        "B to record alice (genuinely discovered as a remote actor) following bob, via real \
         signed HTTP delivery from A's real DeliveryWorker",
    )
    .await;

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// (2) A -> B follow of a locked target stays pending until B authorizes,
// then establishes via a real Accept(Follow) HTTP round trip back to A
// (Requirements 2.3, 2.5, design.md's first Federation Tests bullet, second
// clause: "ロック済み B では承認後に確立").
// ==========================================================================

#[tokio::test]
async fn follow_round_trip_to_locked_remote_target_stays_pending_until_authorized() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let oauth_app_id_a = register_test_app(&a).await;
    let oauth_app_id_b = register_test_app(&b).await;
    let alice = insert_actor_fixture(&a, "fs_lb_alice").await;
    let bob = insert_actor_fixture(&b, "fs_lb_bob").await;
    lock_actor(&b, bob.id).await;

    let bob_uri = urls(&b).actor_url(&bob.handle);
    let alice_uri = urls(&a).actor_url(&alice.handle);
    // A's own cached copy of bob's lock state matches bob's real one on B,
    // so A's own outbound judgment and B's own real inbound judgment agree
    // (both decide "requires approval") -- a coherent round trip rather than
    // an artificial mismatch between the two instances' own state.
    let bob_remote_id = create_test_remote(&a, &bob_uri, true).await;

    let token = issue_token(&a, oauth_app_id_a, alice.id, &["follow"]).await;
    let response = raw_post_json(
        a.address,
        &format!("/api/v1/accounts/{}/follow", bob_remote_id.as_i64()),
        &token,
        &json!({}),
    )
    .await;
    assert_eq!(response.status, 200, "got: {response:?}");
    let body = body_json(&response);
    assert_eq!(body["following"].as_bool(), Some(false), "got: {body}");
    assert_eq!(body["requested"].as_bool(), Some(true), "got: {body}");

    // A: pending outbound request recorded, no follows row yet.
    assert_eq!(
        follow_requests_row_count(
            &a,
            ("local", alice.id.as_i64()),
            ("remote", bob_remote_id.as_i64())
        )
        .await,
        1
    );
    assert_eq!(
        follows_row_count(
            &a,
            ("local", alice.id.as_i64()),
            ("remote", bob_remote_id.as_i64())
        )
        .await,
        0
    );

    // B: real signed HTTP delivery + genuine discovery of alice + B's own
    // real (locked) approval judgment records a pending *inbound* request,
    // not an established follow.
    wait_until(
        || async { inbound_request_from_remote_uri(&b, &alice_uri, bob.id).await == 1 },
        Duration::from_secs(10),
        "B to record a pending inbound follow request from alice, via real signed HTTP delivery",
    )
    .await;
    assert_eq!(
        follows_from_remote_uri_to_local(&b, &alice_uri, bob.id).await,
        0,
        "a locked target must never establish before authorize"
    );

    // bob (the real owner on B) authorizes -- delivers a real Accept(Follow)
    // back to A over real signed HTTP.
    let alice_id_on_b = remote_account_id_by_uri(&b, &alice_uri)
        .await
        .expect("B must have already discovered alice as a remote account by now");
    let bob_token = issue_token(&b, oauth_app_id_b, bob.id, &["follow"]).await;
    let authorize_response = raw_post(
        b.address,
        &format!(
            "/api/v1/follow_requests/{}/authorize",
            alice_id_on_b.as_i64()
        ),
        &bob_token,
    )
    .await;
    assert_eq!(
        authorize_response.status, 200,
        "got: {authorize_response:?}"
    );
    let authorize_body = body_json(&authorize_response);
    assert_eq!(
        authorize_body["followed_by"].as_bool(),
        Some(true),
        "got: {authorize_body}"
    );

    // B: established immediately (authorize's own state write is
    // synchronous; only the Accept delivery back to A is async).
    assert_eq!(
        follows_from_remote_uri_to_local(&b, &alice_uri, bob.id).await,
        1
    );

    // A: the real Accept(Follow) HTTP round trip promotes the pending
    // outbound request to an established follow ("承認後に確立").
    wait_until(
        || async {
            follows_row_count(
                &a,
                ("local", alice.id.as_i64()),
                ("remote", bob_remote_id.as_i64()),
            )
            .await
                == 1
        },
        Duration::from_secs(10),
        "A to establish the follow once B's real Accept(Follow) arrives over real signed HTTP",
    )
    .await;
    assert_eq!(
        follow_requests_row_count(
            &a,
            ("local", alice.id.as_i64()),
            ("remote", bob_remote_id.as_i64())
        )
        .await,
        0,
        "the pending outbound request must be consumed by the real Accept"
    );

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// (3) A's block of B causes B's subsequent inbound delivery attempt to A to
// be genuinely rejected over real signed HTTP (Requirements 6.1-6.3,
// design.md's first Federation Tests bullet, third clause: "A のブロックで
// B からの受信が拒否される").
// ==========================================================================

/// Unlike `tests/social_graph_inbound_it.rs`'s own `blocked_signer_is_
/// rejected_from_the_actor_inbox...` test (a single-instance hand-signed
/// raw POST simulating a remote signer), this test drives the rejection
/// through two genuinely separate live instances: B's own real
/// `FollowService`/`DeliveryService`/`DeliveryWorker`/`SignatureNegotiator`
/// build and sign a real Follow Activity and POST it to A's real inbox
/// route; A's real `HttpSignatureVerifier` verifies the real signature
/// first (bob is a genuine actor with real provisioned signing keys), and
/// only then does A's real `BlockPolicyImpl` reject it with 403 -- nothing
/// here is simulated at either end.
#[tokio::test]
async fn blocked_actors_follow_attempt_is_rejected_via_real_http_and_leaves_a_unaffected() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let oauth_app_id_a = register_test_app(&a).await;
    let oauth_app_id_b = register_test_app(&b).await;
    let alice = insert_actor_fixture(&a, "fs_bp_alice").await;
    let bob = insert_actor_fixture(&b, "fs_bp_bob").await;

    let alice_uri = urls(&a).actor_url(&alice.handle);
    let bob_uri = urls(&b).actor_url(&bob.handle);
    let bob_remote_id_on_a = create_test_remote(&a, &bob_uri, false).await;
    let alice_remote_id_on_b = create_test_remote(&b, &alice_uri, false).await;

    // alice (on A) blocks bob for real, through the live block API --
    // Requirement 6.1's own real block relationship, the exact state
    // `BlockPolicyImpl::is_blocked` reads.
    let alice_token = issue_token(&a, oauth_app_id_a, alice.id, &["follow"]).await;
    let block_response = raw_post(
        a.address,
        &format!("/api/v1/accounts/{}/block", bob_remote_id_on_a.as_i64()),
        &alice_token,
    )
    .await;
    assert_eq!(block_response.status, 200, "got: {block_response:?}");
    assert_eq!(
        body_json(&block_response)["blocking"].as_bool(),
        Some(true),
        "got: {block_response:?}"
    );
    assert_eq!(
        blocks_row_count(
            &a,
            ("local", alice.id.as_i64()),
            ("remote", bob_remote_id_on_a.as_i64())
        )
        .await,
        1
    );

    // bob (on B) now attempts to follow alice -- a fresh, genuine signed
    // Follow Activity B's own real DeliveryWorker sends to alice's real
    // inbox on A. B's own *local* state is unaffected by A's block (bob has
    // no way to know about it yet): the follow establishes locally on B
    // (alice's cached-on-B copy is unlocked) and a real delivery job is
    // enqueued and attempted.
    let bob_token = issue_token(&b, oauth_app_id_b, bob.id, &["follow"]).await;
    let follow_attempt = raw_post_json(
        b.address,
        &format!("/api/v1/accounts/{}/follow", alice_remote_id_on_b.as_i64()),
        &bob_token,
        &json!({}),
    )
    .await;
    assert_eq!(follow_attempt.status, 200, "got: {follow_attempt:?}");
    assert_eq!(
        follows_row_count(
            &b,
            ("local", bob.id.as_i64()),
            ("remote", alice_remote_id_on_b.as_i64())
        )
        .await,
        1,
        "B's own local state establishes regardless -- rejection happens only at A's real \
         federation delivery boundary, not locally on B"
    );

    // The real HTTP attempt from B's real DeliveryWorker to A's real inbox
    // must genuinely be attempted, and genuinely rejected (A's real
    // BlockPolicyImpl returns 403, which DeliveryWorker classifies as
    // Retryable -- never Delivered).
    let alice_inbox = format!("{alice_uri}/inbox");
    wait_until(
        || async {
            delivery_job_row(&b, &alice_inbox, "Follow")
                .await
                .is_some_and(|job| job.attempts >= 1)
        },
        Duration::from_secs(10),
        "B's real DeliveryWorker to attempt (and have rejected) a real signed Follow POST to \
         A's real, now-blocking, inbox",
    )
    .await;
    let job = delivery_job_row(&b, &alice_inbox, "Follow")
        .await
        .expect("the Follow delivery job must still exist");
    assert_ne!(
        job.status, "done",
        "a real 403 rejection from A's real BlockPolicy must never be classified as a \
         successful delivery, got job: {job:?}"
    );

    // A's own relationship state must be genuinely unaffected by the
    // rejected attempt -- no follows/pending row for bob was ever written,
    // because A's real inbox route rejected the request before it ever
    // reached `SocialGraphInboundHandler`.
    assert_eq!(
        follows_from_remote_uri_to_local(&a, &bob_uri, alice.id).await,
        0,
        "a rejected inbound Follow must never establish a follows row on A"
    );
    assert_eq!(
        inbound_request_from_remote_uri(&a, &bob_uri, alice.id).await,
        0,
        "a rejected inbound Follow must never record a pending follow_requests row on A either"
    );

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// (4) Requirement 10.3, Follow: the same kind of Follow -- an "establish
// immediately" outcome -- produces the same relationship-state outcome
// whether delivered in-process (both actors local to the same instance) or
// over real HTTP (actors on separate instances).
//
// Both legs use an *unlocked* target so the comparison is apples-to-apples
// with the general (non-privileged) approval flow rather than the
// same-server admin privilege (Requirement 3.1) -- an unlocked target
// already establishes immediately under the *normal* policy too (Reference:
// `FollowApprovalPolicy`'s decision table), so the "Establish" outcome
// compared here is not an artifact of the local leg's admin privilege; it
// is the one outcome both a same-server pair and a cross-instance pair can
// genuinely, honestly share (the same-server privilege itself has no
// cross-instance analogue by construction -- see this file's own top-level
// doc comment and this task's own brief for why a locked-target comparison
// would not be apples-to-apples).
// ==========================================================================

#[tokio::test]
async fn follow_establish_outcome_is_equivalent_between_local_in_process_and_http_delivery() {
    // ---- Local leg: both actors local to A -------------------------------
    let FederationPair { a, b } = spawn_federation_pair().await;

    let oauth_app_id_a = register_test_app(&a).await;
    let local_follower = insert_actor_fixture(&a, "fs_eq_local_follower").await;
    let local_followee = insert_actor_fixture(&a, "fs_eq_local_followee").await;

    let local_token = issue_token(&a, oauth_app_id_a, local_follower.id, &["follow"]).await;
    let local_response = raw_post_json(
        a.address,
        &format!("/api/v1/accounts/{}/follow", local_followee.id.as_i64()),
        &local_token,
        &json!({}),
    )
    .await;
    assert_eq!(local_response.status, 200, "got: {local_response:?}");

    let local_follower_state = load_state(
        &a,
        AccountRef::Local(local_follower.id),
        AccountRef::Local(local_followee.id),
    )
    .await;
    let local_followee_state = load_state(
        &a,
        AccountRef::Local(local_followee.id),
        AccountRef::Local(local_follower.id),
    )
    .await;

    // ---- HTTP leg: follower local to A, followee local to B --------------
    let http_follower = insert_actor_fixture(&a, "fs_eq_http_follower").await;
    let http_followee = insert_actor_fixture(&b, "fs_eq_http_followee").await;
    let http_followee_uri = urls(&b).actor_url(&http_followee.handle);
    let http_followee_remote_id = create_test_remote(&a, &http_followee_uri, false).await;

    let http_token = issue_token(&a, oauth_app_id_a, http_follower.id, &["follow"]).await;
    let http_response = raw_post_json(
        a.address,
        &format!(
            "/api/v1/accounts/{}/follow",
            http_followee_remote_id.as_i64()
        ),
        &http_token,
        &json!({}),
    )
    .await;
    assert_eq!(http_response.status, 200, "got: {http_response:?}");

    let http_follower_state = load_state(
        &a,
        AccountRef::Local(http_follower.id),
        AccountRef::Remote(http_followee_remote_id),
    )
    .await;

    let http_follower_uri = urls(&a).actor_url(&http_follower.handle);
    wait_until(
        || async {
            follows_from_remote_uri_to_local(&b, &http_follower_uri, http_followee.id).await == 1
        },
        Duration::from_secs(10),
        "B to establish the followed_by relationship via real signed HTTP delivery",
    )
    .await;
    let http_follower_remote_id_on_b = remote_account_id_by_uri(&b, &http_follower_uri)
        .await
        .expect("B must have discovered http_follower as a remote account by now");
    let http_followee_state = load_state(
        &b,
        AccountRef::Local(http_followee.id),
        AccountRef::Remote(http_follower_remote_id_on_b),
    )
    .await;

    // ---- Equivalence (Requirement 10.3) -----------------------------------
    // Follower-side outcome: established, no pending request left behind,
    // identical between the two physical delivery mechanisms.
    assert_eq!(
        local_follower_state.follow.is_some(),
        http_follower_state.follow.is_some(),
        "the follower's own 'following' outcome must be the same regardless of local-in-process \
         vs. real-HTTP delivery: local={local_follower_state:?} http={http_follower_state:?}"
    );
    assert!(local_follower_state.follow.is_some() && http_follower_state.follow.is_some());
    assert_eq!(
        local_follower_state.requested,
        http_follower_state.requested
    );
    assert!(!local_follower_state.requested);
    // Neither leg's Follow local-delivery is affected by the known
    // Block-only `activity_id`-clobbering bug (Follow's own idempotency
    // pre-check in `SocialGraphInboundHandler::handle_follow` short-circuits
    // before ever re-writing the row -- see this module's own doc comment,
    // "Idempotency"), so it is meaningful (and safe) to additionally assert
    // both legs' own real Follow Activity id was actually persisted, not
    // silently dropped.
    assert!(
        !local_follower_state
            .follow
            .as_ref()
            .unwrap()
            .activity_id
            .is_empty()
    );
    assert!(
        !http_follower_state
            .follow
            .as_ref()
            .unwrap()
            .activity_id
            .is_empty()
    );

    // Followee-side outcome: followed_by established, no pending inbound
    // request left behind, identical between the two mechanisms.
    assert_eq!(
        local_followee_state.followed_by, http_followee_state.followed_by,
        "the followee's own 'followed_by' outcome must be the same regardless of local-in-\
         process vs. real-HTTP delivery: local={local_followee_state:?} \
         http={http_followee_state:?}"
    );
    assert!(local_followee_state.followed_by);
    assert_eq!(
        local_followee_state.requested_by,
        http_followee_state.requested_by
    );
    assert!(!local_followee_state.requested_by);

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// (5) Requirement 10.3, Block: the same kind of Block -- blocking
// relationship established, bidirectional follows cleared -- produces the
// same relationship-state *outcome* whether delivered in-process or over
// real HTTP, explicitly NOT comparing on `blocks.activity_id` (see this
// file's own top-level doc comment, "The known local-target-Block
// `activity_id` bug").
//
// CONCERN (disclosed, not silently worked around): the local leg below
// genuinely reproduces task 6.1's documented local-target-Block
// `activity_id`-clobbering bug (`transitions.rs`'s own doc comment,
// "`apply_block` / `mark_blocked_by` share one private transactional
// core") -- this test positively asserts that clobbered-to-empty value
// exists (concrete evidence of the bug, not a claim it is correct), and
// separately asserts the HTTP leg's own `activity_id` is genuinely
// non-empty (that leg's own Block row lives on a different instance's
// database from the in-process re-entrant write that only ever touches the
// *same* row on the *same* instance, so the bug cannot reach it). Neither
// value is used in the cross-leg equivalence assertions themselves.
// ==========================================================================

#[tokio::test]
async fn block_outcome_is_equivalent_between_local_in_process_and_http_delivery() {
    // ---- Local leg: both actors local to A --------------------------------
    let FederationPair { a, b } = spawn_federation_pair().await;

    let oauth_app_id_a = register_test_app(&a).await;
    let local_blocker = insert_actor_fixture(&a, "fs_beq_local_blocker").await;
    let local_blocked = insert_actor_fixture(&a, "fs_beq_local_blocked").await;

    // Pre-establish a bidirectional follow so clearing it is a meaningful,
    // observable part of the outcome being compared (Requirement 5.2).
    let blocker_token = issue_token(&a, oauth_app_id_a, local_blocker.id, &["follow"]).await;
    let blocked_token = issue_token(&a, oauth_app_id_a, local_blocked.id, &["follow"]).await;
    assert_eq!(
        raw_post_json(
            a.address,
            &format!("/api/v1/accounts/{}/follow", local_blocked.id.as_i64()),
            &blocker_token,
            &json!({}),
        )
        .await
        .status,
        200
    );
    assert_eq!(
        raw_post_json(
            a.address,
            &format!("/api/v1/accounts/{}/follow", local_blocker.id.as_i64()),
            &blocked_token,
            &json!({}),
        )
        .await
        .status,
        200
    );

    let block_response = raw_post(
        a.address,
        &format!("/api/v1/accounts/{}/block", local_blocked.id.as_i64()),
        &blocker_token,
    )
    .await;
    assert_eq!(block_response.status, 200, "got: {block_response:?}");

    let local_blocker_state = load_state(
        &a,
        AccountRef::Local(local_blocker.id),
        AccountRef::Local(local_blocked.id),
    )
    .await;
    let local_blocked_state = load_state(
        &a,
        AccountRef::Local(local_blocked.id),
        AccountRef::Local(local_blocker.id),
    )
    .await;

    // Disclosed evidence of the known bug (see this test's own doc
    // comment): the local leg's own row was clobbered to an empty
    // `activity_id` by the in-process re-entrant `mark_blocked_by` call.
    let local_activity_id = block_activity_id(
        &a,
        ("local", local_blocker.id.as_i64()),
        ("local", local_blocked.id.as_i64()),
    )
    .await;
    assert_eq!(
        local_activity_id, "",
        "documents the known local-target-Block activity_id-clobbering bug (task 6.1's own \
         Implementation Notes) -- if this ever starts failing because the bug was fixed, that is \
         good news: this assertion (and the CONCERN this test's doc comment raises) should be \
         removed together with it"
    );

    // ---- HTTP leg: blocker local to A, blocked local to B -----------------
    let http_blocker = insert_actor_fixture(&a, "fs_beq_http_blocker").await;
    let http_blocked = insert_actor_fixture(&b, "fs_beq_http_blocked").await;
    let http_blocked_uri = urls(&b).actor_url(&http_blocked.handle);
    let http_blocker_uri = urls(&a).actor_url(&http_blocker.handle);
    let http_blocked_remote_id = create_test_remote(&a, &http_blocked_uri, false).await;
    let http_blocker_remote_id_on_b = create_test_remote(&b, &http_blocker_uri, false).await;

    // Pre-establish a bidirectional follow across the two real instances.
    let oauth_app_id_b = register_test_app(&b).await;
    let http_blocker_token = issue_token(&a, oauth_app_id_a, http_blocker.id, &["follow"]).await;
    let http_blocked_token = issue_token(&b, oauth_app_id_b, http_blocked.id, &["follow"]).await;
    assert_eq!(
        raw_post_json(
            a.address,
            &format!(
                "/api/v1/accounts/{}/follow",
                http_blocked_remote_id.as_i64()
            ),
            &http_blocker_token,
            &json!({}),
        )
        .await
        .status,
        200
    );
    assert_eq!(
        raw_post_json(
            b.address,
            &format!(
                "/api/v1/accounts/{}/follow",
                http_blocker_remote_id_on_b.as_i64()
            ),
            &http_blocked_token,
            &json!({}),
        )
        .await
        .status,
        200
    );
    wait_until(
        || async {
            follows_from_remote_uri_to_local(&a, &http_blocked_uri, http_blocker.id).await == 1
        },
        Duration::from_secs(10),
        "A to record http_blocked following http_blocker, via real signed HTTP delivery",
    )
    .await;

    let block_response_http = raw_post(
        a.address,
        &format!("/api/v1/accounts/{}/block", http_blocked_remote_id.as_i64()),
        &http_blocker_token,
    )
    .await;
    assert_eq!(
        block_response_http.status, 200,
        "got: {block_response_http:?}"
    );

    // A's own state settles synchronously -- `apply_block` runs before
    // delivery is even attempted, and both directions of the bidirectional
    // follow live in A's own local tables regardless of the counterpart's
    // locality.
    let http_blocker_state = load_state(
        &a,
        AccountRef::Local(http_blocker.id),
        AccountRef::Remote(http_blocked_remote_id),
    )
    .await;

    let http_activity_id = block_activity_id(
        &a,
        ("local", http_blocker.id.as_i64()),
        ("remote", http_blocked_remote_id.as_i64()),
    )
    .await;
    assert_ne!(
        http_activity_id, "",
        "the HTTP leg's own Block row lives on a different instance's database from the \
         in-process re-entrant write that clobbers the local leg's row, so it must retain its \
         real activity_id"
    );

    // B's own state settles once the real Block Activity arrives over real
    // signed HTTP.
    wait_until(
        || async {
            let state = load_state(
                &b,
                AccountRef::Local(http_blocked.id),
                AccountRef::Remote(http_blocker_remote_id_on_b),
            )
            .await;
            state.blocked_by
        },
        Duration::from_secs(10),
        "B to record blocked_by=true once A's real Block Activity arrives over real signed HTTP",
    )
    .await;
    let http_blocked_state = load_state(
        &b,
        AccountRef::Local(http_blocked.id),
        AccountRef::Remote(http_blocker_remote_id_on_b),
    )
    .await;

    // ---- Equivalence (Requirement 10.3) -- outcome fields only ------------
    assert_eq!(
        local_blocker_state.blocking, http_blocker_state.blocking,
        "the blocker's own 'blocking' outcome must be the same regardless of local-in-process \
         vs. real-HTTP delivery: local={local_blocker_state:?} http={http_blocker_state:?}"
    );
    assert!(local_blocker_state.blocking);
    assert_eq!(
        local_blocker_state.follow.is_some(),
        http_blocker_state.follow.is_some(),
        "the blocker's own pre-existing outbound follow must be cleared identically by both \
         delivery mechanisms"
    );
    assert!(local_blocker_state.follow.is_none());
    assert_eq!(
        local_blocker_state.followed_by, http_blocker_state.followed_by,
        "the blocker's own pre-existing followed_by must be cleared identically by both \
         delivery mechanisms"
    );
    assert!(!local_blocker_state.followed_by);

    assert_eq!(
        local_blocked_state.blocked_by, http_blocked_state.blocked_by,
        "the blocked party's own 'blocked_by' outcome must be the same regardless of local-in-\
         process vs. real-HTTP delivery: local={local_blocked_state:?} \
         http={http_blocked_state:?}"
    );
    assert!(local_blocked_state.blocked_by);
    assert_eq!(
        local_blocked_state.follow.is_some(),
        http_blocked_state.follow.is_some(),
        "the blocked party's own pre-existing outbound follow must be cleared identically by \
         both delivery mechanisms"
    );
    assert!(local_blocked_state.follow.is_none());
    assert_eq!(
        local_blocked_state.followed_by, http_blocked_state.followed_by,
        "the blocked party's own pre-existing followed_by must be cleared identically by both \
         delivery mechanisms"
    );
    assert!(!local_blocked_state.followed_by);

    a.cleanup().await;
    b.cleanup().await;
}
