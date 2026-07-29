//! Integration tests for task 8.3 (`.kiro/specs/statuses-core/tasks.md`,
//! "8.3 連合テスト（2 インスタンス・ローカル/HTTP 同値）を整備する",
//! documented as the spec's single most important risk, "最重要リスク"):
//! `spawn_federation_pair` round trips of post-create/reblog/favourite/
//! delete/edit between two genuinely separate instances, and — the
//! documented central claim — that the *same* canonical Activity, delivered
//! by the *same* kind of `StatusActivityBuilder` call, produces the *same*
//! observable domain-state result whether its recipient is LOCAL
//! (in-process delivery) or REMOTE (real signed HTTP delivery across
//! `spawn_federation_pair`'s two instances) (Requirements 4.4, 4.5, 14.1,
//! 14.2, 14.3, 14.4, 14.5; `Boundary: StatusActivityBuilder, InboundHandlers`).
//!
//! ## Prior art this file follows
//! - `tests/federation_pair_it.rs` (federation-core task 6.4): the
//!   established pattern for driving `spawn_federation_pair` — fixture
//!   duplication per test file, `wait_until` polling for the real
//!   background `DeliveryWorker` (HTTP delivery is asynchronous via a DB
//!   queue, unlike local delivery which completes synchronously inside the
//!   `deliver()` call), and observing successful HTTP delivery+dispatch
//!   without a bespoke `InboundActivityHandler`.
//! - `src/federation/test_harness.rs`'s own doc comment ("Why not
//!   `spawn_test_app`", "Dispatch-success observation") for the
//!   reachability machinery this file's tests depend on.
//! - `tests/status_crud_it.rs`/`tests/interactions_it.rs`/`tests/polls_it.rs`
//!   (task 8.1, out of this task's boundary, not modified here): this file
//!   deliberately does *not* drive status creation/reblog/favourite through
//!   the HTTP endpoint layer or `StatusService`/`InteractionService` the way
//!   those files do — see "Why this file calls `StatusActivityBuilder`
//!   directly" below.
//!
//! ## Wiring gap this task closed (outside this task's own two source files,
//! justified below)
//! `src/federation/test_harness.rs::spawn_paired_instance` (federation-core
//! task 6.4) registered no downstream `InboundActivityHandler`s at all — its
//! own doc comment explicitly said so and explicitly named *this* task
//! ("8.3, `_Depends: 7.2_`") as the one expected to extend it. Without
//! statuses-core's six handlers ([`inbound_handlers::register_status_handlers`])
//! registered on both paired instances' dispatchers, neither instance could
//! ever reflect a received post-related Activity in its own domain state —
//! the dispatcher would silently no-op every one (`dispatcher.rs`'s own
//! documented "unregistered outer types are a safe no-op" behavior),
//! making this task's entire assignment unobservable. This task therefore
//! made the one-line-equivalent edit `spawn_paired_instance`'s own doc
//! comment already earmarked for it: replacing that function's no-op
//! `register_downstream` closure argument with
//! `statuses::register_downstream_handlers(...)`, built the same way
//! `crate::test_harness::spawn_test_app`'s own production-mirroring
//! composition already does (only the `RemoteAccountFetcher`'s
//! `FederationHttpClient` differs: the caller-supplied
//! `insecure_loopback()` instance, not a fresh `ReqwestFederationHttpClient::new()`,
//! for the identical loopback-reachability reason every other per-instance
//! dependency in that module already follows). This is a pure composition-
//! root wiring change (no `InboxService`/`dispatcher.rs`/signature/delivery
//! logic touched) that only calls an already-implemented, already-reviewed
//! function this task's own boundary owns
//! ([`inbound_handlers::register_status_handlers`]) — not a new capability
//! authored outside this task's boundary.
//!
//! ## Why this file calls `StatusActivityBuilder` directly, not
//! `StatusService`/`InteractionService`
//! `tasks.md`'s own Implementation Notes for 5.1/5.2/5.3 (already reviewed,
//! out of this task's boundary to change) document several structural gaps
//! that make it impossible to reach a *genuinely remote* recipient through
//! those services' current wiring:
//! - `InteractionService::reblog`'s own `Announce` addressing is the
//!   reblog's *own followers* (via `RelationshipQuery::followers_of`), never
//!   the target's author — and `StatusesModule`'s wiring (task 7.2) always
//!   uses `NoRelationshipQuery` (social-graph has not landed), so every
//!   `Announce` this service dispatches carries *zero* recipients,
//!   regardless of locality (5.2's own note, "reblog 自体の Announce は
//!   リブースト者自身のフォロワー宛のため影響なし", combined with 3.1's
//!   documented `NoRelationshipQuery` default).
//! - `InteractionService::favourite`/`unfavourite`/`unreblog`'s target-author
//!   resolution ([`ActorHandleLookup`], reused from task 4.1) only resolves
//!   **local** actors — favouriting a remote-authored post therefore fails
//!   outright (5.2's own note, "Resolving a target's author is local-only").
//! - `StatusService::create_status`'s mention resolution is local-only too
//!   (5.1's own note): a remote mention is extracted but never resolved to a
//!   recipient.
//!
//! None of these are `StatusActivityBuilder`/`InboundHandlers` bugs (this
//! task's own boundary) — they are already-reviewed, already-documented
//! boundary decisions in `StatusService`/`InteractionService` (tasks 5.1/5.2,
//! explicitly out of this task's boundary and explicitly excluded from
//! remediation by this task's own instructions). This task's job is to prove
//! Requirement 4.4/4.5's "同一の正規 Activity...ローカル/HTTP 同値" claim at
//! exactly the level design.md's own `#### StatusActivityBuilder` component
//! and this task's own `_Boundary: StatusActivityBuilder, InboundHandlers_`
//! name — so every test below builds a [`kawasemi::statuses::ConcreteStatusActivityBuilder`]
//! directly (the same concrete instantiation `build_statuses_module`
//! assembles in production, `ActorDirectory` + the paired instance's own
//! real `DeliveryService`) and calls its `deliver_*` methods with an
//! explicitly-chosen local-or-remote `Recipient`/`ActorRef`, exactly
//! mirroring `tests/federation_pair_it.rs`'s own established "one `deliver()`
//! call, hand-supplied recipients" technique — this sidesteps every one of
//! the `StatusService`/`InteractionService` gaps above entirely, since
//! recipient resolution for those gaps is `InteractionService`'s/
//! `StatusService`'s own responsibility, not `StatusActivityBuilder`'s.
//! Status fixtures this file needs as reblog/favourite/delete/update
//! *targets* are inserted directly via `status_repository::insert_status`
//! (mirroring `tests/polls_it.rs`'s own established "insert fixtures
//! directly, bypass the creating service" convention) rather than created
//! through the HTTP endpoint layer.
//!
//! ## Coverage decision (see also each test's own doc comment)
//! - **Round-trip reception** (a): full genuine A→B coverage for all five
//!   named operations — create, reblog (Announce), favourite (Like), delete,
//!   edit (Update).
//! - **Local/HTTP equivalence** (b, the "最重要リスク" claim): full domain-
//!   state-shape comparison for reblog and favourite (both naturally support
//!   an apples-to-apples "same operation, target's home instance differs"
//!   comparison — see `reblog_announce_...`'s own doc comment). Create gets
//!   a lighter equivalence check (one call, mixed local+remote recipients,
//!   proving canonical-Activity reuse and that the local leg's structurally-
//!   guaranteed idempotent no-op does not corrupt state while the remote leg
//!   performs genuine ingestion — see that test's own doc comment for why a
//!   full local-vs-remote *state-change* comparison is not meaningful for
//!   `Create` specifically). Delete/Update do not get a local-recipient
//!   equivalence variant: `DeleteHandler`/`UpdateHandler` only ever act on a
//!   target this instance itself ingested from a remote origin
//!   (`target.local == false`, `inbound_handlers.rs`'s own documented gate)
//!   — a condition that cannot naturally coexist with an in-process/
//!   same-instance "local recipient" scenario without fabricating a
//!   deliberately-inconsistent fixture row (`local == false` yet owned by a
//!   local actor), which would not be representative of any real delivery
//!   path. Their genuine cross-instance round trip (the only path their own
//!   domain logic is ever reachable through in the first place) is covered
//!   in full below; combined with `deliver_delete`/`deliver_update` sharing
//!   the identical `deliver_one` → `DeliveryService::deliver` code path
//!   `deliver_create`/`deliver_announce`/`deliver_like` already prove
//!   locality-independent (`activity_builder.rs`'s own doc comment, "one
//!   canonical Activity, one `deliver()` call" — structural, not per-method),
//!   this is judged sufficient evidence without a non-representative
//!   fabricated-fixture test.
//! - Poll voting (`deliver_vote`) is out of this file's scope: the task's own
//!   literal instruction text names only "投稿/ブースト/お気に入り/削除/編集"
//!   (create/reblog/favourite/delete/edit), not voting.
//! - `Undo` (unreblog/unfavourite) is likewise out of this file's scope for
//!   the same reason.

use std::time::{Duration, Instant};

use time::OffsetDateTime;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorDirectory, ActorType, Handle, LocalActor, NewActor};
use kawasemi::domain::{Id, Visibility};
use kawasemi::federation::urls::{ActorUrls, ObjectKind};
use kawasemi::federation::{FederationPair, Recipient, spawn_federation_pair};
use kawasemi::statuses::ConcreteStatusActivityBuilder;
use kawasemi::statuses::addressing::{self, ActorRef};
use kawasemi::statuses::model::Status;
use kawasemi::statuses::status_repository;
use kawasemi::test_harness::TestApp;

// ==========================================================================
// Fixtures (each `tests/*.rs` file is its own compiled crate — this
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
            display_name: format!("Statuses Federation Pair IT {handle_str}"),
            summary: "an actor used by the statuses_federation_pair_it integration test"
                .to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed")
}

fn test_domain(app: &TestApp) -> String {
    app.state.config().server.domain.clone()
}

/// Builds the one concrete `StatusActivityBuilder` instantiation this crate's
/// own production wiring uses (`src/statuses.rs::build_statuses_module`'s own
/// doc comment: `ActorDirectory` for `ActorHandleLookup`, the paired
/// instance's own real `DeliveryService`), bound to `app`'s own domain/pool/
/// runtime/delivery service — see this file's own doc comment ("Why this
/// file calls `StatusActivityBuilder` directly") for why tests construct
/// this themselves rather than going through `StatusService`/
/// `InteractionService`. `ActorDirectory::new` is a thin, stateless `PgPool`
/// wrapper (`build_statuses_module`'s own doc comment) — constructing a
/// fresh one per call is the established, cheap, correct convention.
fn activity_builder(app: &TestApp) -> ConcreteStatusActivityBuilder {
    ConcreteStatusActivityBuilder::new(
        ActorUrls::new(test_domain(app)),
        app.runtime.ids.clone(),
        ActorDirectory::new(app.pool.clone()),
        std::sync::Arc::clone(app.state.federation().delivery_service()),
    )
}

/// Inserts a local `Status` fixture directly via `StatusRepository`
/// (bypassing `StatusService`/the HTTP endpoint layer — see this file's own
/// doc comment). Always `public`, unreplied, poll-free, media-free: every
/// test below only needs a plain post to act as a reblog/favourite/delete/
/// edit target or source.
async fn insert_local_status_fixture(app: &TestApp, actor: &LocalActor, content: &str) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let uri = ActorUrls::new(test_domain(app)).object_url(ObjectKind::new("statuses"), id);

    let status = Status {
        id,
        actor_id: actor.id,
        uri: uri.clone(),
        url: Some(uri),
        content: content.to_string(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    };
    status_repository::insert_status(&app.pool, &status)
        .await
        .expect("inserting the local status fixture must succeed");
    status
}

/// A `Status` value only used to feed `StatusActivityBuilder::deliver_announce`'s
/// `reblog: &Status` parameter — that method only ever reads `.actor_id`/
/// `.created_at` off it (see `activity_builder.rs::deliver_announce`), so
/// this value is never itself persisted; every other field is a structurally
/// required but functionally unread placeholder.
fn reblog_stub(app: &TestApp, booster: &LocalActor, now: OffsetDateTime) -> Status {
    Status {
        id: app.runtime.ids.next_id(),
        actor_id: booster.id,
        uri: String::new(),
        url: None,
        content: String::new(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    }
}

fn actor_ref_local(app: &TestApp, actor: &LocalActor) -> ActorRef {
    ActorRef {
        uri: ActorUrls::new(test_domain(app)).actor_url(&actor.handle),
        recipient: Recipient::Local(actor.handle.clone()),
    }
}

fn actor_ref_remote(app: &TestApp, actor: &LocalActor) -> ActorRef {
    let urls = ActorUrls::new(test_domain(app));
    ActorRef {
        uri: urls.actor_url(&actor.handle),
        recipient: Recipient::Remote {
            inbox: urls.inbox_url(&actor.handle),
            shared_inbox: None,
        },
    }
}

fn followers_uri(app: &TestApp, actor: &LocalActor) -> String {
    format!(
        "{}/followers",
        ActorUrls::new(test_domain(app)).actor_url(&actor.handle)
    )
}

async fn refetch(app: &TestApp, id: Id) -> Status {
    status_repository::find_by_id(&app.pool, id)
        .await
        .expect("find_by_id must not error")
        .expect("the status must still exist")
}

async fn find_by_uri(app: &TestApp, uri: &str) -> Option<Status> {
    status_repository::find_by_uri(&app.pool, uri)
        .await
        .expect("find_by_uri must not error")
}

async fn status_row_count_for_uri(app: &TestApp, uri: &str) -> i64 {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM statuses WHERE uri = $1")
        .bind(uri)
        .fetch_one(&app.pool)
        .await
        .expect("counting statuses by uri must succeed");
    count
}

#[derive(sqlx::FromRow, Debug, PartialEq, Eq)]
struct ReblogShape {
    content: String,
    sensitive: bool,
    spoiler_text: String,
    local: bool,
    poll_id: Option<i64>,
}

/// Reads the (sole, by construction in these tests) reblog row referencing
/// `target_id` — a raw query rather than `interaction_repository::find_reblog`
/// (which requires already knowing the booster's *resolved* `Id` on the
/// receiving instance; for a remote booster that id is a freshly-minted
/// `remote_accounts` row this test does not otherwise need to know) — proves
/// the receiving instance's own domain state changed, independent of which
/// physical delivery path produced it.
async fn reblog_of(app: &TestApp, target_id: Id) -> ReblogShape {
    sqlx::query_as::<_, ReblogShape>(
        "SELECT content, sensitive, spoiler_text, local, poll_id FROM statuses \
         WHERE reblog_of_id = $1",
    )
    .bind(target_id.as_i64())
    .fetch_one(&app.pool)
    .await
    .expect("exactly one reblog row referencing target_id must exist")
}

async fn favourite_count_for(app: &TestApp, status_id: Id) -> i64 {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM favourites WHERE status_id = $1")
        .bind(status_id.as_i64())
        .fetch_one(&app.pool)
        .await
        .expect("counting favourites must succeed");
    count
}

/// Polls `check` every 50ms until it returns `true` or `timeout` elapses —
/// mirrors `tests/federation_pair_it.rs`'s own identical `wait_until`
/// (needed because HTTP delivery runs asynchronously via each paired
/// instance's own real, already-running background `DeliveryWorker`, unlike
/// local delivery which completes synchronously inside `deliver()`).
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
// Create (Requirements 14.1, 14.2)
// ==========================================================================

/// (a) Round-trip reception: A's `Create(Note)` is received and reflected as
/// a new, remote-origin `Status` on B.
///
/// (b) Local/HTTP equivalence, lighter form: ONE `deliver_create` call whose
/// recipients mix a LOCAL actor (on A, alongside the post's own author) and
/// a REMOTE actor (on B) — mirrors `tests/federation_pair_it.rs`'s own
/// established "one `deliver()` call, mixed recipients" technique, proving
/// the identical canonical Activity reaches both. A full local-vs-remote
/// *domain-state-change* comparison (as done below for reblog/favourite) is
/// not meaningful for `Create` specifically: the status being created
/// necessarily already exists under that exact `uri` in the *sending*
/// instance's own database (its own author just inserted it) before this
/// call is even made, so any LOCAL recipient's in-process reflection is
/// structurally always the already-documented idempotent no-op
/// (`inbound_handlers.rs`'s own "Idempotent re-delivery" section) — there is
/// no meaningful "fresh creation" state change to compare on the local leg
/// the way there is a fresh `reblogs_count`/favourite row on the remote leg.
/// What *is* meaningfully provable here — and is exactly what this test
/// asserts — is that the local leg's idempotent no-op does not corrupt
/// already-correct state (no duplicate row) while the remote leg performs a
/// genuine first-time ingestion from the *same* `deliver_create` call.
#[tokio::test]
async fn create_round_trips_and_local_recipient_stays_idempotent_while_remote_recipient_ingests_a_new_copy()
 {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let alice = insert_actor_fixture(&a, "sfp_create_alice").await;
    let carol = insert_actor_fixture(&a, "sfp_create_carol").await; // local recipient on A
    let dan_b = insert_actor_fixture(&b, "sfp_create_dan").await; // remote recipient (on B)

    let status = insert_local_status_fixture(&a, &alice, "hello, federated world").await;
    let addressing = addressing::derive_addressing(&status, &[], &followers_uri(&a, &alice));

    let builder = activity_builder(&a);
    builder
        .deliver_create(
            &status,
            &addressing,
            vec![
                Recipient::Local(carol.handle.clone()),
                Recipient::Remote {
                    inbox: ActorUrls::new(test_domain(&b)).inbox_url(&dan_b.handle),
                    shared_inbox: None,
                },
            ],
            None,
            None,
        )
        .await
        .expect("deliver_create must succeed for a mix of a local and a real remote recipient");

    // --- Local leg: synchronous, and idempotent (no duplicate row) ---
    assert_eq!(
        status_row_count_for_uri(&a, &status.uri).await,
        1,
        "the local in-process dispatch of A's own just-created post must not duplicate it"
    );
    let a_copy = refetch(&a, status.id).await;
    assert_eq!(a_copy.content, status.content);
    assert!(a_copy.local, "A's own post remains its own local post");

    // --- Remote leg: asynchronous (real DeliveryWorker + real HTTP) ---
    wait_until(
        || async { find_by_uri(&b, &status.uri).await.is_some() },
        Duration::from_secs(10),
        "B to ingest A's Create(Note) as a new remote Status",
    )
    .await;

    let b_copy = find_by_uri(&b, &status.uri)
        .await
        .expect("B must have ingested the post");
    assert_eq!(
        b_copy.content, status.content,
        "content must round-trip verbatim"
    );
    assert_eq!(b_copy.sensitive, status.sensitive);
    assert_eq!(b_copy.spoiler_text, status.spoiler_text);
    assert_eq!(b_copy.visibility, status.visibility);
    assert!(
        !b_copy.local,
        "B's copy must be reflected as remote-origin, not locally authored"
    );

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// Reblog / Announce (Requirements 9.2, 9.4, 14.1, 14.3) — the primary
// local/HTTP equivalence proof
// ==========================================================================

/// (a) Round-trip reception + (b) full local/HTTP equivalence.
///
/// Unlike `Create`, a reblog's `Announce` is naturally addressed to the
/// *target's author* (Requirement 9.2's "被お気に入り元アクターへ...配送" —
/// mirrored here for `Announce`; see this file's own doc comment for why
/// `InteractionService::reblog`'s current production wiring cannot actually
/// reach this recipient itself, a documented out-of-boundary gap this test
/// sidesteps by calling `StatusActivityBuilder` directly). This gives a
/// genuinely apples-to-apples comparison: the SAME booster (`dave`, on A)
/// boosts two DIFFERENT targets whose only difference is *where the target's
/// home instance is* — one authored locally on A (so the Announce is
/// delivered in-process), one authored locally on B (so the Announce is
/// delivered over real signed HTTP) — `AnnounceHandler` only ever accepts a
/// target it is itself the home instance for (`target.local == true`,
/// `inbound_handlers.rs`'s own documented gate), so each leg's Announce is
/// processed by *its target's own* home instance, exactly mirroring how a
/// real boost is always addressed to wherever the boosted post actually
/// lives.
#[tokio::test]
async fn reblog_announce_reflects_on_the_targets_home_instance_with_local_and_remote_recipients_producing_equivalent_results()
 {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let dave = insert_actor_fixture(&a, "sfp_reblog_dave").await; // the one booster, always on A
    let erin_a = insert_actor_fixture(&a, "sfp_reblog_erin").await; // target author, local to A
    let frank_b = insert_actor_fixture(&b, "sfp_reblog_frank").await; // target author, local to B

    let target_local = insert_local_status_fixture(&a, &erin_a, "erin's local post").await;
    let target_remote = insert_local_status_fixture(&b, &frank_b, "frank's remote post").await;

    let builder = activity_builder(&a);

    // --- Local leg: booster and target's home instance are the same (A) ---
    let addressing_local =
        addressing::derive_addressing(&target_local, &[], &followers_uri(&a, &erin_a));
    let now_a = a.runtime.clock.now();
    builder
        .deliver_announce(
            &reblog_stub(&a, &dave, now_a),
            &target_local,
            &addressing_local,
            vec![Recipient::Local(erin_a.handle.clone())],
        )
        .await
        .expect("deliver_announce must succeed for a local target/recipient");

    let target_local_after = refetch(&a, target_local.id).await;
    assert_eq!(
        target_local_after.reblogs_count, 1,
        "the local target's reblogs_count must reflect the in-process Announce immediately"
    );
    let local_reblog = reblog_of(&a, target_local.id).await;

    // --- Remote leg: booster stays on A, target's home instance is B ---
    let addressing_remote =
        addressing::derive_addressing(&target_remote, &[], &followers_uri(&b, &frank_b));
    let now_a2 = a.runtime.clock.now();
    builder
        .deliver_announce(
            &reblog_stub(&a, &dave, now_a2),
            &target_remote,
            &addressing_remote,
            vec![Recipient::Remote {
                inbox: ActorUrls::new(test_domain(&b)).inbox_url(&frank_b.handle),
                shared_inbox: None,
            }],
        )
        .await
        .expect("deliver_announce must succeed for a real remote target/recipient");

    wait_until(
        || async { refetch(&b, target_remote.id).await.reblogs_count == 1 },
        Duration::from_secs(10),
        "B's target status to reflect the Announce A's DeliveryWorker sent over real signed HTTP",
    )
    .await;
    let remote_reblog = reblog_of(&b, target_remote.id).await;

    // --- Equivalence (Requirements 4.4, 4.5, the "最重要リスク" claim) ---
    // Both legs used the SAME `StatusActivityBuilder::deliver_announce` call
    // shape for the SAME booster; only physical delivery mechanism (local
    // in-process vs. real signed HTTP) and the target's own identity
    // differed. The resulting reblog row's shape is asserted identical:
    assert_eq!(
        local_reblog, remote_reblog,
        "the reblog row AnnounceHandler creates must have the identical shape (content/\
         sensitive/spoiler_text/local/poll_id) regardless of whether the Announce was \
         delivered in-process or over real HTTP"
    );
    assert!(
        !local_reblog.local,
        "AnnounceHandler always reflects an inbound boost as remote-origin, even when delivered in-process to its own sending instance"
    );

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// Favourite / Like (Requirements 10.2, 14.1, 14.3)
// ==========================================================================

/// (a) Round-trip reception + (b) full local/HTTP equivalence — same
/// structure as the reblog test above, applied to `Like`/`LikeHandler`.
#[tokio::test]
async fn favourite_like_reflects_on_the_targets_home_instance_with_local_and_remote_recipients_producing_equivalent_results()
 {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let dave = insert_actor_fixture(&a, "sfp_fav_dave").await; // the one favouriter, always on A
    let erin_a = insert_actor_fixture(&a, "sfp_fav_erin").await;
    let frank_b = insert_actor_fixture(&b, "sfp_fav_frank").await;

    let target_local =
        insert_local_status_fixture(&a, &erin_a, "erin's local post to favourite").await;
    let target_remote =
        insert_local_status_fixture(&b, &frank_b, "frank's remote post to favourite").await;

    let builder = activity_builder(&a);

    // --- Local leg ---
    builder
        .deliver_like(dave.id, &target_local, actor_ref_local(&a, &erin_a))
        .await
        .expect("deliver_like must succeed for a local target/recipient");

    let target_local_after = refetch(&a, target_local.id).await;
    assert_eq!(
        target_local_after.favourites_count, 1,
        "the local target's favourites_count must reflect the in-process Like immediately"
    );
    assert_eq!(favourite_count_for(&a, target_local.id).await, 1);

    // --- Remote leg ---
    builder
        .deliver_like(dave.id, &target_remote, actor_ref_remote(&b, &frank_b))
        .await
        .expect("deliver_like must succeed for a real remote target/recipient");

    wait_until(
        || async { refetch(&b, target_remote.id).await.favourites_count == 1 },
        Duration::from_secs(10),
        "B's target status to reflect the Like A's DeliveryWorker sent over real signed HTTP",
    )
    .await;

    // --- Equivalence ---
    assert_eq!(
        favourite_count_for(&a, target_local.id).await,
        favourite_count_for(&b, target_remote.id).await,
        "exactly one favourite row must exist on each target's own home instance, regardless \
         of whether the Like was delivered in-process or over real HTTP"
    );

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// Delete (Requirements 7.3, 14.1, 14.4) — round-trip reception only, see
// this file's own doc comment ("Coverage decision") for why
// ==========================================================================

/// (a) Round-trip reception: A's `Create(Note)` is first federated to B (so
/// B holds a remote-origin copy, `local == false` — the only condition under
/// which `DeleteHandler`'s own local-ness gate ever activates), then A's
/// `Delete` for that same post is federated to B and removes B's copy.
#[tokio::test]
async fn delete_round_trips_from_a_to_b_and_removes_the_remote_mirrored_copy() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let alice = insert_actor_fixture(&a, "sfp_delete_alice").await;
    let dan_b = insert_actor_fixture(&b, "sfp_delete_dan").await;
    let dan_b_inbox = ActorUrls::new(test_domain(&b)).inbox_url(&dan_b.handle);

    let status = insert_local_status_fixture(&a, &alice, "a post that will be deleted").await;
    let addressing = addressing::derive_addressing(&status, &[], &followers_uri(&a, &alice));
    let builder = activity_builder(&a);

    builder
        .deliver_create(
            &status,
            &addressing,
            vec![Recipient::Remote {
                inbox: dan_b_inbox.clone(),
                shared_inbox: None,
            }],
            None,
            None,
        )
        .await
        .expect("deliver_create must succeed");

    wait_until(
        || async { find_by_uri(&b, &status.uri).await.is_some() },
        Duration::from_secs(10),
        "B to ingest A's post before it can be deleted",
    )
    .await;

    builder
        .deliver_delete(
            &status,
            &addressing,
            vec![Recipient::Remote {
                inbox: dan_b_inbox,
                shared_inbox: None,
            }],
        )
        .await
        .expect("deliver_delete must succeed");

    wait_until(
        || async { find_by_uri(&b, &status.uri).await.is_none() },
        Duration::from_secs(10),
        "B to reflect the Delete A's DeliveryWorker sent over real signed HTTP",
    )
    .await;

    // A's own copy is untouched by this test (StatusService's own
    // delete_status, out of this task's boundary, owns local deletion) —
    // only B's mirrored-copy reflection is this test's concern.
    assert!(find_by_uri(&a, &status.uri).await.is_some());

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// Edit / Update (Requirements 8.4, 14.1, 14.4) — round-trip reception only,
// see this file's own doc comment ("Coverage decision") for why
// ==========================================================================

/// (a) Round-trip reception: same Create-then-act shape as the delete test
/// above, applied to `Update`/`UpdateHandler` — B's mirrored copy's content
/// and `edited_at` must reflect A's edit.
#[tokio::test]
async fn edit_round_trips_from_a_to_b_and_updates_the_remote_mirrored_copys_content() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    let alice = insert_actor_fixture(&a, "sfp_edit_alice").await;
    let dan_b = insert_actor_fixture(&b, "sfp_edit_dan").await;
    let dan_b_inbox = ActorUrls::new(test_domain(&b)).inbox_url(&dan_b.handle);

    let status = insert_local_status_fixture(&a, &alice, "the original content").await;
    let addressing = addressing::derive_addressing(&status, &[], &followers_uri(&a, &alice));
    let builder = activity_builder(&a);

    builder
        .deliver_create(
            &status,
            &addressing,
            vec![Recipient::Remote {
                inbox: dan_b_inbox.clone(),
                shared_inbox: None,
            }],
            None,
            None,
        )
        .await
        .expect("deliver_create must succeed");

    wait_until(
        || async { find_by_uri(&b, &status.uri).await.is_some() },
        Duration::from_secs(10),
        "B to ingest A's post before it can be edited",
    )
    .await;

    let edited = Status {
        content: "the edited content".to_string(),
        edited_at: Some(a.runtime.clock.now()),
        ..status.clone()
    };
    let edited_addressing = addressing::derive_addressing(&edited, &[], &followers_uri(&a, &alice));

    builder
        .deliver_update(
            &edited,
            &edited_addressing,
            vec![Recipient::Remote {
                inbox: dan_b_inbox,
                shared_inbox: None,
            }],
            None,
        )
        .await
        .expect("deliver_update must succeed");

    wait_until(
        || async {
            find_by_uri(&b, &status.uri)
                .await
                .map(|s| s.content == edited.content)
                .unwrap_or(false)
        },
        Duration::from_secs(10),
        "B to reflect the Update A's DeliveryWorker sent over real signed HTTP",
    )
    .await;

    let b_copy = find_by_uri(&b, &status.uri)
        .await
        .expect("B's mirrored copy must still exist");
    assert_eq!(b_copy.content, edited.content);
    assert!(
        b_copy.edited_at.is_some(),
        "B's mirrored copy must record that an edit occurred"
    );

    a.cleanup().await;
    b.cleanup().await;
}
