//! Integration tests for task 6.3 (`.kiro/specs/social-graph/tasks.md`,
//! "6.3 連合対称性テスト（2 インスタンス往復）", `_Depends: 6.1, 6.2_`, no
//! `_Boundary:_` line -- scoped to this new test file only, mirroring 6.1/6.2's
//! own identical situation): proves that a 2-instance Follow/Block round trip
//! (establish / approve / receive-a-rejected-request-after-block) behaves
//! correctly over **real** signed HTTP between two genuinely separate,
//! genuinely reachable instances, and -- the central claim -- that the SAME
//! Follow/Block Activity, executed by the SAME `FollowService`/`BlockService`
//! call, produces the IDENTICAL relationship-state-transition result whether
//! the recipient is LOCAL (in-process delivery, same instance) or REMOTE
//! (real signed HTTP delivery to a genuinely separate instance).
//!
//! Requirements 1.2, 1.3, 10.3 (this task's own assigned subset). design.md's
//! Testing Strategy -> "Federation Tests（2 インスタンス往復）" names both
//! bullets this file covers verbatim:
//! - "A→B のフォローで B にフォロワー確立・A に following 反映、ロック済み B
//!   では承認後に確立、A のブロックで B からの受信が拒否される" ->
//!   [`a_follows_unlocked_b_establishes_symmetric_follower_and_following_state`],
//!   [`a_follows_locked_b_requires_authorize_before_establishing`],
//!   [`a_blocks_b_rejects_bs_subsequent_signed_follow_toward_a`].
//! - "同一 Follow/Block を local in-process と HTTP 配送で実行し、関係状態
//!   遷移結果が同値であることを検証" (Requirement 10.3, the central claim) ->
//!   [`identical_follow_activity_produces_identical_relationship_transition_regardless_of_local_or_http_delivery`],
//!   [`identical_block_activity_produces_identical_blocked_by_result_regardless_of_local_or_http_delivery`].
//!
//! ## Prior art this file follows
//! - `tests/federation_pair_it.rs` (federation-core task 6.4): the
//!   established pattern for driving `spawn_federation_pair` -- per-file
//!   fixture duplication, `wait_until` polling for the real background
//!   `DeliveryWorker` (HTTP delivery is asynchronous via a DB queue, unlike
//!   local delivery which completes synchronously inside `deliver()`), and
//!   observing successful HTTP delivery+dispatch without a bespoke
//!   `InboundActivityHandler`.
//! - `tests/statuses_federation_pair_it.rs` (statuses-core task 8.3): the
//!   closest precedent for a downstream spec building its own
//!   federation-symmetry test on this shared harness, including its own
//!   "Wiring gap this task closed" doc-comment convention.
//! - `src/federation/test_harness.rs`'s own doc comment for the `https://`
//!   vs. plain-HTTP loopback reachability problem this harness solves, and
//!   what it exposes (`spawn_federation_pair`/`spawn_paired_instance`).
//!
//! ## No harness extension was needed (unlike statuses-core's task 8.3)
//! Before writing this file, `src/federation/test_harness.rs::spawn_paired_instance`
//! was read in full to check whether it registers social-graph's own
//! `InboundActivityHandler` (`SocialGraphInboundHandler`) on both paired
//! instances -- statuses-core's task 8.3 had to add exactly this kind of
//! registration for its own six handlers, since federation-core's original
//! task 6.4 harness registered none at all. Empirically, **it already
//! does**: that function's own body assembles
//! `social_graph::register_downstream_handlers(...)` and folds it into the
//! same `register_downstream` closure `statuses::register_downstream_handlers`
//! already contributes to (search `spawn_paired_instance` for
//! `social_graph_register_downstream`/`social_graph_pending_delivery`), and
//! that function's own doc comment explicitly attributes this wiring to
//! "social-graph task 5.2" (task 5.2's own Implementation Note in
//! `tasks.md` independently confirms `SocialGraphModule`'s bootstrap wiring
//! registers its inbound handlers the same way for every other composition
//! root in this crate). This task's boundary is therefore exactly what
//! `tasks.md` implies for 6.1/6.2's identical situation (no `_Boundary:_`
//! line -> test-file-only): no `src/federation/test_harness.rs` change was
//! made or was necessary.
//!
//! ## Driving `FollowService`/`BlockService`/`FollowRequestService` directly,
//! not through the HTTP endpoint layer
//! Every scenario below calls `app.state.social_graph().follow()`/`.block()`/
//! `.follow_requests()` directly -- the exact same production `Arc<Concrete*Service>`
//! handles `src/social_graph/endpoints.rs`'s own HTTP handlers call, only
//! skipping the Bearer-auth/scope/JSON-request-body plumbing (task 5.1's own
//! boundary, already covered by `src/social_graph/endpoints/tests.rs` and
//! `tests/follow_unfollow_it.rs`/`tests/follow_request_it.rs`/
//! `tests/mute_block_it.rs`, none of which this task re-tests). This mirrors
//! `tests/statuses_federation_pair_it.rs`'s own established "call the real
//! business-service layer directly, bypass the HTTP surface" convention for
//! the identical reason: this task's own concern is
//! `ActivityBuilder`/`Transitions`/`InboundHandler` symmetry across a real
//! two-instance boundary, not endpoint-layer plumbing.
//!
//! ## Seeding a real, reachable remote-account row (not a fake `remote.example`)
//! `FollowService`/`BlockService`'s own `target: &str` resolution
//! (`follow_service.rs`'s own doc comment, "`target: &str` resolution")
//! requires an already-cached `remote_accounts` row for a remote target --
//! this crate's own established fixture convention for a *single-instance*
//! remote target (`tests/follow_unfollow_it.rs`/`tests/block_policy_it.rs`)
//! seeds one pointing at an intentionally-unreachable `https://remote.example/...`
//! URI. This file instead seeds that same kind of row with the *paired
//! instance's own real actor URI* (`ActorUrls::new(other_domain).actor_url(&handle)`)
//! -- so the `{actor_uri}/inbox` convention `FollowService`/`BlockService`
//! already use resolves to the OTHER paired instance's real, live, reachable
//! inbox route, making every "remote" leg below a genuine signed HTTP round
//! trip, never a durably-enqueued-but-never-sent job the way the
//! single-instance tests' own `remote.example` fixtures are.
//!
//! ## Why direct `repository::load_states` calls, not the HTTP relationships
//! oracle
//! `tests/relationship_provider_it.rs`/task 6.1's own Implementation Note use
//! `GET /api/v1/accounts/relationships` as a read-only oracle. This file
//! instead calls `social_graph::repository::load_states` directly on each
//! instance's own pool -- the same already-implemented, already-reviewed
//! function `RelProviderImpl`/`FollowService`/`BlockService` themselves call
//! internally -- avoiding a second OAuth app/token-issuing rig purely to read
//! back state this task's own two business services already expose a
//! perfectly good, directly callable Rust API for.
//!
//! ## No HTTP client dependency: no raw sockets either (unlike this crate's
//! other integration tests)
//! Every real over-the-wire leg here is exercised *indirectly*, through each
//! paired instance's own real background `DeliveryWorker`/`InboxService`
//! pipeline (exactly like `tests/federation_pair_it.rs`/
//! `tests/statuses_federation_pair_it.rs`) -- this file itself never opens a
//! raw `TcpStream` or hand-signs a request.

use std::time::{Duration, Instant};

use time::OffsetDateTime;

use kawasemi::accounts::model::{ProfileField, ProfilePatch, RemoteAccount};
use kawasemi::accounts::profile_repository;
use kawasemi::accounts::remote_repository;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, LocalActor, NewActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{FederationPair, spawn_federation_pair};
use kawasemi::social_graph::model::FollowOptions;
use kawasemi::social_graph::repository::{self, RelationshipState};
use kawasemi::test_harness::TestApp;

// ==========================================================================
// Fixtures (each `tests/*.rs` file is its own compiled crate -- this
// deliberately duplicates `tests/federation_pair_it.rs`'s own established
// conventions rather than importing them, per this crate's documented
// per-file-duplication convention).
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
            display_name: format!("Federation Symmetry IT {handle_str}"),
            summary: "an actor used by the federation_symmetry_it integration test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed")
}

fn test_domain(app: &TestApp) -> String {
    app.state.config().server.domain.clone()
}

/// Seeds a `remote_accounts` row on `app` pointing at `actor_uri` -- see
/// this file's own doc comment ("Seeding a real, reachable remote-account
/// row") for why `actor_uri` is always the OTHER paired instance's own real
/// actor URL in this file, never a fake `remote.example` one.
async fn insert_remote_fixture(
    app: &TestApp,
    actor_uri: &str,
    username: &str,
    domain: &str,
    locked: bool,
) -> Id {
    let id = app.runtime.ids.next_id();
    remote_repository::upsert_remote(
        &app.pool,
        &RemoteAccount {
            id,
            actor_uri: actor_uri.to_string(),
            username: username.to_string(),
            domain: domain.to_string(),
            display_name: format!("Federation Symmetry IT (remote) {username}"),
            note: String::new(),
            url: actor_uri.to_string(),
            avatar_url: None,
            header_url: None,
            fields: Vec::<ProfileField>::new(),
            bot: false,
            locked,
            fetched_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("seeding a real-remote-actor cache row must succeed");
    id
}

/// Locks `actor_id`'s profile (mirrors `tests/follow_request_it.rs`'s own
/// identical `lock_actor` helper).
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

/// Thin wrapper around `repository::load_states`'s batched interface for
/// this file's always-one-target case (mirrors
/// `follow_service.rs::FollowService::load_state`'s identical private
/// helper).
async fn relationship_state(
    pool: &sqlx::PgPool,
    viewer: AccountRef,
    target: AccountRef,
    now: OffsetDateTime,
) -> RelationshipState {
    let mut states = repository::load_states(pool, &viewer, std::slice::from_ref(&target), now)
        .await
        .expect("load_states must succeed");
    states
        .pop()
        .expect("load_states returns exactly one state per requested target")
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

/// Reads back the (`attempts`, Activity `id`) of the (by this file's own
/// construction, unique) `delivery_jobs` row addressed to `target_inbox` --
/// used by the block-rejection scenario to know when the real
/// `DeliveryWorker` has attempted at least one send, and which Activity id
/// to check `received_activities` for.
async fn delivery_job_attempts(app: &TestApp, target_inbox: &str) -> Option<(i32, String)> {
    let row: Option<(i32, String)> = sqlx::query_as(
        "SELECT attempts, activity->>'id' FROM delivery_jobs WHERE target_inbox = $1",
    )
    .bind(target_inbox)
    .fetch_optional(&app.pool)
    .await
    .expect("querying delivery_jobs must succeed");
    row
}

/// Polls `check` every 50ms until it returns `true` or `timeout` elapses --
/// mirrors `tests/federation_pair_it.rs`'s/`tests/statuses_federation_pair_it.rs`'s
/// own identical `wait_until` (needed because HTTP delivery runs
/// asynchronously via each paired instance's own real, already-running
/// background `DeliveryWorker`, unlike local delivery which completes
/// synchronously inside `deliver()`).
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

fn mastodon_default_opts() -> FollowOptions {
    // Matches `inbound.rs::handle_follow`'s own hardcoded receiving-side
    // Establish-branch defaults exactly (`transitions.rs`'s own doc
    // comment, "`promote_pending`'s follow options") -- passing this same
    // value for a locally-initiated follow makes the local-target and
    // remote-target legs of the equivalence tests below apples-to-apples
    // comparable.
    FollowOptions {
        reblogs: true,
        notify: false,
        languages: Vec::new(),
    }
}

// ==========================================================================
// (1) A -> B follow, B unlocked: B gets a follower, A gets a following
// (design.md's Federation Tests bullet 1; Requirement 1.2).
// ==========================================================================

#[tokio::test]
async fn a_follows_unlocked_b_establishes_symmetric_follower_and_following_state() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let alice = insert_actor_fixture(&a, "fsym_u_alice").await;
    let bob = insert_actor_fixture(&b, "fsym_u_bob").await;

    let b_domain = test_domain(&b);
    let bob_uri = ActorUrls::new(b_domain.clone()).actor_url(&bob.handle);
    let bob_on_a = insert_remote_fixture(&a, &bob_uri, "fsym_u_bob", &b_domain, false).await;

    a.state
        .social_graph()
        .follow()
        .follow(
            alice.id,
            &bob_on_a.as_i64().to_string(),
            mastodon_default_opts(),
        )
        .await
        .expect("alice's follow of bob (a real remote target) must succeed");

    // A's own outbound state: established synchronously, before HTTP
    // delivery to B even completes (`follow_service.rs::FollowService::follow`
    // calls `Transitions::establish_follow` before `DeliveryService::deliver`).
    let now_a = a.runtime.clock.now();
    let alice_state = relationship_state(
        &a.pool,
        AccountRef::Local(alice.id),
        AccountRef::Remote(bob_on_a),
        now_a,
    )
    .await;
    assert!(
        alice_state.follow.is_some(),
        "alice must already show bob as followed on A itself, before HTTP delivery even \
         completes (Requirement 1.2)"
    );
    assert!(
        !alice_state.requested,
        "bob unlocked -> established immediately, never a pending request"
    );

    // B's own state: only observable once the real DeliveryWorker's signed
    // HTTP send and B's real SocialGraphInboundHandler have run.
    let a_domain = test_domain(&a);
    let alice_uri = ActorUrls::new(a_domain).actor_url(&alice.handle);
    wait_until(
        || async {
            match remote_repository::find_remote_by_uri(&b.pool, &alice_uri)
                .await
                .expect("querying B's remote_accounts must succeed")
            {
                Some(remote) => {
                    let now_b = b.runtime.clock.now();
                    relationship_state(
                        &b.pool,
                        AccountRef::Local(bob.id),
                        AccountRef::Remote(remote.id),
                        now_b,
                    )
                    .await
                    .followed_by
                }
                None => false,
            }
        },
        Duration::from_secs(10),
        "B to record alice as a follower after receiving and processing the real signed Follow",
    )
    .await;

    let alice_remote_on_b = remote_repository::find_remote_by_uri(&b.pool, &alice_uri)
        .await
        .expect("querying B's remote_accounts must succeed")
        .expect("alice must be cached on B by now");
    let now_b = b.runtime.clock.now();
    let bob_state = relationship_state(
        &b.pool,
        AccountRef::Local(bob.id),
        AccountRef::Remote(alice_remote_on_b.id),
        now_b,
    )
    .await;
    assert!(
        bob_state.followed_by,
        "bob must show alice as a follower after real signed HTTP delivery + dispatch"
    );
    assert!(
        !bob_state.requested_by,
        "bob unlocked -> established directly, never left as a pending inbound request"
    );

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// (2) A -> B follow, B locked: pending until B authorizes, then established
// on both sides (design.md's Federation Tests bullet 1; Requirement 2.x).
// ==========================================================================

#[tokio::test]
async fn a_follows_locked_b_requires_authorize_before_establishing() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let alice = insert_actor_fixture(&a, "fsym_l_alice").await;
    let bob = insert_actor_fixture(&b, "fsym_l_bob").await;
    lock_actor(&b, bob.id).await;

    let b_domain = test_domain(&b);
    let bob_uri = ActorUrls::new(b_domain.clone()).actor_url(&bob.handle);
    let bob_on_a = insert_remote_fixture(&a, &bob_uri, "fsym_l_bob", &b_domain, true).await;

    a.state
        .social_graph()
        .follow()
        .follow(
            alice.id,
            &bob_on_a.as_i64().to_string(),
            mastodon_default_opts(),
        )
        .await
        .expect("a follow request toward a locked remote target must still succeed (recorded as pending)");

    let now_a = a.runtime.clock.now();
    let alice_state = relationship_state(
        &a.pool,
        AccountRef::Local(alice.id),
        AccountRef::Remote(bob_on_a),
        now_a,
    )
    .await;
    assert!(
        alice_state.requested,
        "bob locked -> pending on A's own outbound state, not established"
    );
    assert!(alice_state.follow.is_none());

    let a_domain = test_domain(&a);
    let alice_uri = ActorUrls::new(a_domain.clone()).actor_url(&alice.handle);
    wait_until(
        || async {
            match remote_repository::find_remote_by_uri(&b.pool, &alice_uri)
                .await
                .expect("querying B's remote_accounts must succeed")
            {
                Some(remote) => {
                    let now_b = b.runtime.clock.now();
                    relationship_state(
                        &b.pool,
                        AccountRef::Local(bob.id),
                        AccountRef::Remote(remote.id),
                        now_b,
                    )
                    .await
                    .requested_by
                }
                None => false,
            }
        },
        Duration::from_secs(10),
        "B to record a pending inbound follow request from alice after real signed HTTP delivery",
    )
    .await;

    let alice_remote_on_b = remote_repository::find_remote_by_uri(&b.pool, &alice_uri)
        .await
        .expect("querying B's remote_accounts must succeed")
        .expect("alice must be cached on B by now");

    // bob authorizes -- the SAME production `FollowRequestService` the real
    // `POST /api/v1/follow_requests/:id/authorize` endpoint uses.
    b.state
        .social_graph()
        .follow_requests()
        .authorize_request(bob.id, &alice_remote_on_b.id.as_i64().to_string())
        .await
        .expect("authorize_request must succeed for a genuinely pending inbound request");

    let now_b = b.runtime.clock.now();
    let bob_state = relationship_state(
        &b.pool,
        AccountRef::Local(bob.id),
        AccountRef::Remote(alice_remote_on_b.id),
        now_b,
    )
    .await;
    assert!(
        bob_state.followed_by,
        "authorizing must establish the follow on B's own side immediately"
    );
    assert!(!bob_state.requested_by);

    // A only learns of bob's Accept(Follow) asynchronously, over real signed
    // HTTP.
    wait_until(
        || async {
            let now_a2 = a.runtime.clock.now();
            relationship_state(
                &a.pool,
                AccountRef::Local(alice.id),
                AccountRef::Remote(bob_on_a),
                now_a2,
            )
            .await
            .follow
            .is_some()
        },
        Duration::from_secs(10),
        "A to receive bob's real signed Accept(Follow) and establish the follow",
    )
    .await;

    let now_a2 = a.runtime.clock.now();
    let alice_state_final = relationship_state(
        &a.pool,
        AccountRef::Local(alice.id),
        AccountRef::Remote(bob_on_a),
        now_a2,
    )
    .await;
    assert!(alice_state_final.follow.is_some());
    assert!(!alice_state_final.requested);

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// (3) A blocks B: B's subsequent signed Follow toward A is rejected
// (design.md's Federation Tests bullet 1's third clause; Requirements 5.x,
// 6.x -- now proven across a genuine two-instance boundary, not merely
// `tests/block_policy_it.rs`'s single-instance hand-signed case).
// ==========================================================================

#[tokio::test]
async fn a_blocks_b_rejects_bs_subsequent_signed_follow_toward_a() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let carol = insert_actor_fixture(&a, "fsym_b_carol").await; // the blocker, on A
    let dave = insert_actor_fixture(&b, "fsym_b_dave").await; // the one blocked, on B

    let b_domain = test_domain(&b);
    let dave_uri = ActorUrls::new(b_domain.clone()).actor_url(&dave.handle);
    let dave_on_a = insert_remote_fixture(&a, &dave_uri, "fsym_b_dave", &b_domain, false).await;

    a.state
        .social_graph()
        .block()
        .block(carol.id, &dave_on_a.as_i64().to_string())
        .await
        .expect("carol's block of dave (a real remote target) must succeed");

    let now_a = a.runtime.clock.now();
    let carol_state = relationship_state(
        &a.pool,
        AccountRef::Local(carol.id),
        AccountRef::Remote(dave_on_a),
        now_a,
    )
    .await;
    assert!(carol_state.blocking);

    let a_domain = test_domain(&a);
    let carol_uri = ActorUrls::new(a_domain.clone()).actor_url(&carol.handle);

    // Wait for B to learn (via the real signed Block delivery) that carol
    // has blocked dave.
    wait_until(
        || async {
            match remote_repository::find_remote_by_uri(&b.pool, &carol_uri)
                .await
                .expect("querying B's remote_accounts must succeed")
            {
                Some(remote) => {
                    let now_b = b.runtime.clock.now();
                    relationship_state(
                        &b.pool,
                        AccountRef::Local(dave.id),
                        AccountRef::Remote(remote.id),
                        now_b,
                    )
                    .await
                    .blocked_by
                }
                None => false,
            }
        },
        Duration::from_secs(10),
        "B to record dave as blocked_by carol after receiving the real signed Block",
    )
    .await;

    let carol_on_b = remote_repository::find_remote_by_uri(&b.pool, &carol_uri)
        .await
        .expect("querying B's remote_accounts must succeed")
        .expect("carol must now be cached on B");

    // dave, undeterred (or simply not yet aware), still attempts to follow
    // carol -- a genuinely signed, real HTTP Follow request delivered
    // straight to carol's own inbox on A.
    b.state
        .social_graph()
        .follow()
        .follow(
            dave.id,
            &carol_on_b.id.as_i64().to_string(),
            mastodon_default_opts(),
        )
        .await
        .expect(
            "dave's own follow() call succeeds locally on B (delivery is durably enqueued, not \
             synchronously rejected by the sender's own side)",
        );

    let carol_inbox_on_a = ActorUrls::new(a_domain).inbox_url(&carol.handle);
    wait_until(
        || async {
            delivery_job_attempts(&b, &carol_inbox_on_a)
                .await
                .map(|(attempts, _)| attempts >= 1)
                .unwrap_or(false)
        },
        Duration::from_secs(10),
        "B's real DeliveryWorker to attempt delivering dave's Follow to carol's real inbox on A",
    )
    .await;

    let (_, rejected_activity_id) = delivery_job_attempts(&b, &carol_inbox_on_a)
        .await
        .expect("the delivery job must exist by now");
    assert!(
        !received_activity_exists(&a, &rejected_activity_id).await,
        "carol's block of dave must cause A's federation-core BlockPolicy to reject dave's real \
         signed Follow before it ever reaches dedup/dispatch (Requirements 6.1-6.3), now proven \
         across a genuine two-instance boundary rather than a single hand-signed request"
    );

    // And carol's own relationship state toward dave must show the Follow
    // was never applied -- no new pending/established relationship.
    let now_a2 = a.runtime.clock.now();
    let carol_state_after = relationship_state(
        &a.pool,
        AccountRef::Local(carol.id),
        AccountRef::Remote(dave_on_a),
        now_a2,
    )
    .await;
    assert!(!carol_state_after.followed_by);
    assert!(!carol_state_after.requested_by);

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// (4) The central claim (Requirement 10.3), Follow: the SAME Follow call
// produces the identical relationship-state-transition result whether its
// target is local (in-process delivery) or a real remote instance (HTTP
// delivery).
// ==========================================================================

#[tokio::test]
async fn identical_follow_activity_produces_identical_relationship_transition_regardless_of_local_or_http_delivery()
 {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let dave = insert_actor_fixture(&a, "fsym_eq_dave").await; // the one follower, always on A
    let carol = insert_actor_fixture(&a, "fsym_eq_carol").await; // LOCAL target, on A
    let erin = insert_actor_fixture(&b, "fsym_eq_erin").await; // REMOTE target, a real actor on B

    let b_domain = test_domain(&b);
    let erin_uri = ActorUrls::new(b_domain.clone()).actor_url(&erin.handle);
    let erin_on_a = insert_remote_fixture(&a, &erin_uri, "fsym_eq_erin", &b_domain, false).await;

    // The exact same `FollowOptions` value for both calls (this file's own
    // `mastodon_default_opts` helper, matching `inbound.rs::handle_follow`'s
    // own hardcoded receiving-side defaults) -- so the receiving side's own
    // established follow row and the local target's directly-established
    // row are comparable apples to apples.
    a.state
        .social_graph()
        .follow()
        .follow(
            dave.id,
            &carol.id.as_i64().to_string(),
            mastodon_default_opts(),
        )
        .await
        .expect("dave's follow of carol (local target) must succeed");
    a.state
        .social_graph()
        .follow()
        .follow(
            dave.id,
            &erin_on_a.as_i64().to_string(),
            mastodon_default_opts(),
        )
        .await
        .expect("dave's follow of erin (a real remote target) must succeed");

    // --- Sender-side comparison (Requirements 1.2, 1.3): dave's own
    // outbound Follow row toward each target, immediately after follow()
    // returns -- both established SYNCHRONOUSLY by FollowService itself,
    // before delivery (local in-process vs. HTTP) even runs.
    let now_a = a.runtime.clock.now();
    let dave_to_carol = relationship_state(
        &a.pool,
        AccountRef::Local(dave.id),
        AccountRef::Local(carol.id),
        now_a,
    )
    .await;
    let dave_to_erin = relationship_state(
        &a.pool,
        AccountRef::Local(dave.id),
        AccountRef::Remote(erin_on_a),
        now_a,
    )
    .await;

    assert!(
        dave_to_carol.follow.is_some() && dave_to_erin.follow.is_some(),
        "both follows must be established immediately regardless of the target's locality"
    );
    let carol_follow_shape = dave_to_carol
        .follow
        .as_ref()
        .map(|f| (f.reblogs, f.notify, f.languages.clone()));
    let erin_follow_shape = dave_to_erin
        .follow
        .as_ref()
        .map(|f| (f.reblogs, f.notify, f.languages.clone()));
    assert_eq!(
        carol_follow_shape, erin_follow_shape,
        "the SAME Follow call (same viewer, same opts) must produce the identical established \
         follow shape (reblogs/notify/languages) regardless of whether the target is local or \
         remote -- Requirement 1.2/1.3's 'identical Activity/state-transition, only delivery \
         mechanism branches' claim"
    );
    assert_eq!(dave_to_carol.requested, dave_to_erin.requested);

    // --- Receiver-side comparison (Requirement 10.3): each target's OWN
    // relationship state, observed on its OWN home instance -- carol's, on
    // A, reached via `InboxService::process_local`'s in-process dispatch
    // (the local leg); erin's, on B, reached via a real signed HTTP POST to
    // a genuinely separate instance and its own real
    // `InboxService::process_inbound` (the HTTP leg).
    let now_a2 = a.runtime.clock.now();
    let carol_state = relationship_state(
        &a.pool,
        AccountRef::Local(carol.id),
        AccountRef::Local(dave.id),
        now_a2,
    )
    .await;
    assert!(
        carol_state.followed_by,
        "carol (local target) must show dave as a follower, in-process, immediately"
    );
    assert!(!carol_state.requested_by);

    let a_domain = test_domain(&a);
    let dave_uri = ActorUrls::new(a_domain).actor_url(&dave.handle);
    wait_until(
        || async {
            match remote_repository::find_remote_by_uri(&b.pool, &dave_uri)
                .await
                .expect("querying B's remote_accounts must succeed")
            {
                Some(remote) => {
                    let now_b = b.runtime.clock.now();
                    relationship_state(
                        &b.pool,
                        AccountRef::Local(erin.id),
                        AccountRef::Remote(remote.id),
                        now_b,
                    )
                    .await
                    .followed_by
                }
                None => false,
            }
        },
        Duration::from_secs(10),
        "B (erin, the real remote target) to reflect dave's follow after real signed HTTP \
         delivery + dispatch",
    )
    .await;
    let dave_remote_on_b = remote_repository::find_remote_by_uri(&b.pool, &dave_uri)
        .await
        .expect("querying B's remote_accounts must succeed")
        .expect("dave must be cached on B by now");
    let now_b = b.runtime.clock.now();
    let erin_state = relationship_state(
        &b.pool,
        AccountRef::Local(erin.id),
        AccountRef::Remote(dave_remote_on_b.id),
        now_b,
    )
    .await;

    assert_eq!(
        (carol_state.followed_by, carol_state.requested_by),
        (erin_state.followed_by, erin_state.requested_by),
        "the SAME kind of Follow Activity, delivered in-process (to carol, local) vs. over real \
         signed HTTP to a genuinely separate instance (to erin, remote), must produce the \
         identical relationship-state-transition RESULT on each target's own home instance \
         (Requirement 10.3) -- both established (followed_by=true, requested_by=false), never \
         merely 'both happen to work in isolation'"
    );

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// (5) The central claim (Requirement 10.3), Block: the SAME Block call
// produces the identical `blocked_by` result whether its target is local or
// a real remote instance.
// ==========================================================================

#[tokio::test]
async fn identical_block_activity_produces_identical_blocked_by_result_regardless_of_local_or_http_delivery()
 {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let frank = insert_actor_fixture(&a, "fsym_beq_frank").await; // the one blocker, always on A
    let grace = insert_actor_fixture(&a, "fsym_beq_grace").await; // LOCAL blocked target, on A
    let heidi = insert_actor_fixture(&b, "fsym_beq_heidi").await; // REMOTE blocked target, a real actor on B

    let b_domain = test_domain(&b);
    let heidi_uri = ActorUrls::new(b_domain.clone()).actor_url(&heidi.handle);
    let heidi_on_a =
        insert_remote_fixture(&a, &heidi_uri, "fsym_beq_heidi", &b_domain, false).await;

    a.state
        .social_graph()
        .block()
        .block(frank.id, &grace.id.as_i64().to_string())
        .await
        .expect("frank's block of grace (local target) must succeed");
    a.state
        .social_graph()
        .block()
        .block(frank.id, &heidi_on_a.as_i64().to_string())
        .await
        .expect("frank's block of heidi (a real remote target) must succeed");

    // Sender-side: frank's own blocking state toward each target,
    // established synchronously by `BlockService` itself in both cases.
    let now_a = a.runtime.clock.now();
    let frank_to_grace = relationship_state(
        &a.pool,
        AccountRef::Local(frank.id),
        AccountRef::Local(grace.id),
        now_a,
    )
    .await;
    let frank_to_heidi = relationship_state(
        &a.pool,
        AccountRef::Local(frank.id),
        AccountRef::Remote(heidi_on_a),
        now_a,
    )
    .await;
    assert!(frank_to_grace.blocking && frank_to_heidi.blocking);

    // Receiver-side: grace's own blocked_by state, in-process (synchronous);
    // heidi's own blocked_by state, over real signed HTTP (asynchronous).
    let now_a2 = a.runtime.clock.now();
    let grace_state = relationship_state(
        &a.pool,
        AccountRef::Local(grace.id),
        AccountRef::Local(frank.id),
        now_a2,
    )
    .await;
    assert!(
        grace_state.blocked_by,
        "grace (local target) must be blocked_by frank in-process, immediately"
    );

    let a_domain = test_domain(&a);
    let frank_uri = ActorUrls::new(a_domain).actor_url(&frank.handle);
    wait_until(
        || async {
            match remote_repository::find_remote_by_uri(&b.pool, &frank_uri)
                .await
                .expect("querying B's remote_accounts must succeed")
            {
                Some(remote) => {
                    let now_b = b.runtime.clock.now();
                    relationship_state(
                        &b.pool,
                        AccountRef::Local(heidi.id),
                        AccountRef::Remote(remote.id),
                        now_b,
                    )
                    .await
                    .blocked_by
                }
                None => false,
            }
        },
        Duration::from_secs(10),
        "B (heidi, the real remote target) to reflect frank's block after real signed HTTP \
         delivery + dispatch",
    )
    .await;
    let frank_remote_on_b = remote_repository::find_remote_by_uri(&b.pool, &frank_uri)
        .await
        .expect("querying B's remote_accounts must succeed")
        .expect("frank must be cached on B by now");
    let now_b = b.runtime.clock.now();
    let heidi_state = relationship_state(
        &b.pool,
        AccountRef::Local(heidi.id),
        AccountRef::Remote(frank_remote_on_b.id),
        now_b,
    )
    .await;

    assert_eq!(
        grace_state.blocked_by, heidi_state.blocked_by,
        "the SAME kind of Block Activity, delivered in-process vs. over real signed HTTP to a \
         genuinely separate instance, must produce the identical blocked_by result on each \
         target's own home instance (Requirement 10.3, applied to Block per Requirement 5.5)"
    );

    a.cleanup().await;
    b.cleanup().await;
}
