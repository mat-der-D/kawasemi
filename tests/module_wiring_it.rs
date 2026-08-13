//! structural-refactor task 6.5 / Requirements 7.3, 7.4: the runtime test
//! that fails when `crate::bootstrap::wiring::compose_modules`'s ordering
//! constraint is broken.
//!
//! design.md's `#### compose_modules` records an invariant the type system
//! does not enforce: `AccountPortsRegistry` holds exactly one replaceable
//! slot per port, so the **last** `set_*` call for a given slot is the one
//! observed at request time. Two of the eleven wiring stages write into the
//! account-ports registry:
//!
//! - stage 7, `statuses::register_account_ports`, replaces
//!   `build_accounts_module`'s built-in `EmptyStatusesProvider` /
//!   `ZeroCountsProvider` defaults with statuses-core's real
//!   `AccountStatusesProviderImpl` / `AccountCountsContribution`, and
//! - stage 8, `social_graph::build_social_graph_module`, then registers
//!   `CombinedAccountCountsProvider` — the *composed* counts provider that
//!   merges social-graph's `followers`/`following` half with statuses-core's
//!   `statuses`/`last_status_at` half into the single registered value.
//!
//! Reordering those stages still compiles and still boots. design.md's
//! `#### 配線順序の検証テスト` therefore requires a test that observes the
//! *effect* of the ordering through the real, `spawn_test_app`-booted router
//! (which since task 6.3 runs `compose_modules` itself), not merely that
//! startup succeeds.
//!
//! ## Which fields are actually order-sensitive (measured, not assumed)
//! tasks.md's own prose for task 6.5 ("アカウントポート登録が呼ばれていなけれ
//! ば 0 になり") is only half right, and task 6.4's review already corrected
//! it. The precise, empirically-verified sensitivities are:
//!
//! - **`followers_count` / `following_count` are sensitive to the stage
//!   7 <-> stage 8 order.** If social-graph registers *before*
//!   `register_account_ports`, the last writer of the counts slot is stage
//!   7's bare `AccountCountsContribution`, which honestly zeroes the two
//!   fields it does not own (`src/statuses/account_provider.rs:460-465`) —
//!   so both counts silently revert to `0` while `statuses_count` stays
//!   correct. This is the assertion a *swap* (rather than a deletion) makes
//!   fail.
//! - **`statuses_count` is NOT sensitive to that swap.** Stage 8 builds its
//!   own `AccountCountsContribution::new(pool)` inside
//!   `CombinedAccountCountsProvider` (`src/social_graph.rs:614-617`) rather
//!   than reading back whatever stage 7 registered, so the statuses half
//!   survives either ordering. It is asserted here anyway, as the guard
//!   against the counts slot regressing to `ZeroCountsProvider` if both
//!   registrations were ever dropped.
//! - **The account's statuses page is sensitive to stage 7 being run at
//!   all.** The `AccountStatusesProvider` slot has exactly one real
//!   registrant (stage 7); stage 8 never touches it. Dropping stage 7 —
//!   precisely the defect task 6.4 found in the federation-pair harness —
//!   leaves `EmptyStatusesProvider` in place and the page silently empty
//!   while the account still resolves with a 200.
//!
//! Together the two halves pin both failure modes: a swapped stage 7/8 pair
//! (counts) and a missing stage 7 (statuses page and, via
//! `ZeroCountsProvider`, all three counts).
//!
//! ## Fixture shape
//! One actor with **both** followers and posts, plus a distinct followee, so
//! all three counts are non-zero *and* mutually distinguishable (2 posts /
//! 1 follower / 1 followed) — an all-ones fixture could not tell a real
//! count from a constant. Every fixture row is written through the real
//! production repository functions (`status_repository::insert_status`,
//! `social_graph::repository::upsert_follow`), never a stub, so the
//! assertions compare rendered output against genuine data.
//!
//! ## No shared test module
//! Each `tests/*.rs` file is its own compiled binary with no shared module,
//! so the fixture/HTTP plumbing below is duplicated by this repo's
//! established convention (see `tests/federation_pair_it.rs`'s own note).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, LocalActor, NewActor};
use kawasemi::domain::{AccountRef, Visibility};
use kawasemi::federation::urls::{ActorUrls, ObjectKind};
use kawasemi::server;
use kawasemi::social_graph::model::Follow;
use kawasemi::social_graph::repository::upsert_follow;
use kawasemi::statuses::model::Status;
use kawasemi::statuses::status_repository;
use kawasemi::test_harness::{TestApp, spawn_test_app};

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
            display_name: format!("Module Wiring IT {handle_str}"),
            summary: "an actor used by the module-wiring ordering test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed")
}

/// Inserts a public, local status authored by `actor` through the real
/// production repository function.
async fn insert_local_status_fixture(app: &TestApp, actor: &LocalActor, content: &str) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let domain = app.state.config().server.domain.clone();
    let uri = ActorUrls::new(domain).object_url(ObjectKind::new("statuses"), id);

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

/// Records a genuine `follower -> followee` follow via the real production
/// repository function — the ground truth
/// `social_graph::providers::AccountCountsProviderImpl` reads from.
async fn insert_follow_fixture(app: &TestApp, follower: &LocalActor, followee: &LocalActor) {
    let follow_id = app.runtime.ids.next_id();
    upsert_follow(
        &app.pool,
        follow_id,
        &Follow {
            follower: AccountRef::Local(follower.id),
            followee: AccountRef::Local(followee.id),
            reblogs: true,
            notify: false,
            languages: Vec::new(),
            activity_id: format!("https://kawasemi.example/activities/{}", follow_id.as_i64()),
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_follow must succeed for a fresh (follower, followee) pair");
}

async fn get_json_unauthenticated(router: &Router, path: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("build request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

// ==========================================================================
// The ordering assertions (Requirements 7.3, 7.4)
// ==========================================================================

/// Requirement 7.3/7.4: for an actor that genuinely has followers, a
/// followee and authored posts, every count in the rendered Account must
/// reflect that real data.
///
/// `followers_count`/`following_count` are the fields that go to `0` if
/// wiring stage 8 (`build_social_graph_module`) is moved *before* stage 7
/// (`register_account_ports`), because stage 7's bare
/// `AccountCountsContribution` would then be the last writer of the single
/// counts slot and it zeroes the two social-graph fields. `statuses_count`
/// is asserted alongside them as the guard against the slot regressing all
/// the way to `ZeroCountsProvider`. See this file's module doc comment for
/// the measured breakdown.
#[tokio::test]
async fn account_counts_reflect_real_data_when_the_wiring_order_is_intact() {
    let app = spawn_test_app().await;

    let subject = insert_actor_fixture(&app, "wiring_subject").await;
    let follower = insert_actor_fixture(&app, "wiring_follower").await;
    let followee = insert_actor_fixture(&app, "wiring_followee").await;

    insert_local_status_fixture(&app, &subject, "the first authored post").await;
    insert_local_status_fixture(&app, &subject, "the second authored post").await;
    insert_follow_fixture(&app, &follower, &subject).await;
    insert_follow_fixture(&app, &subject, &followee).await;

    let router = server::build_router(app.state.clone());
    let (status, body) = get_json_unauthenticated(
        &router,
        &format!("/api/v1/accounts/{}", subject.id.as_i64()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    assert_eq!(
        body["statuses_count"].as_i64(),
        Some(2),
        "statuses_count must reflect the 2 real authored posts; a 0 here means the \
         AccountCountsProvider slot holds neither statuses-core's contribution nor \
         social-graph's composed provider (wiring stages 7 and 8 both missing), got: {body}"
    );
    assert_eq!(
        body["followers_count"].as_i64(),
        Some(1),
        "followers_count must reflect the 1 real follows row; a 0 here means \
         social_graph::build_social_graph_module (wiring stage 8) is no longer the LAST \
         writer of the AccountCountsProvider slot — statuses::register_account_ports \
         (stage 7) clobbered its composed provider, got: {body}"
    );
    assert_eq!(
        body["following_count"].as_i64(),
        Some(1),
        "following_count must reflect the 1 real follows row; a 0 here means the same \
         stage 7 / stage 8 ordering break as followers_count, got: {body}"
    );

    app.cleanup().await;
}

/// Requirement 7.3/7.4, second half: the `AccountStatusesProvider` slot must
/// hold statuses-core's real implementation, not
/// `accounts::ports::EmptyStatusesProvider`.
///
/// Stage 7 (`statuses::register_account_ports`) is that slot's only real
/// registrant, and nothing later overwrites it — so this is the assertion
/// that fails when stage 7 is dropped from the sequence entirely, which is
/// exactly the defect task 6.4 found in the federation-pair harness. The
/// built-in default returns a 200 with an empty page, so only asserting on
/// the *contents* can distinguish it.
#[tokio::test]
async fn account_statuses_page_is_not_the_built_in_empty_default() {
    let app = spawn_test_app().await;

    let subject = insert_actor_fixture(&app, "wiring_author").await;
    let first = insert_local_status_fixture(&app, &subject, "the first authored post").await;
    let second = insert_local_status_fixture(&app, &subject, "the second authored post").await;

    let router = server::build_router(app.state.clone());
    let (status, body) = get_json_unauthenticated(
        &router,
        &format!("/api/v1/accounts/{}/statuses", subject.id.as_i64()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let page = body
        .as_array()
        .expect("the account statuses endpoint must return a JSON array");
    assert_eq!(
        page.len(),
        2,
        "the account's statuses page must carry the 2 real authored posts; an empty page \
         means accounts' built-in EmptyStatusesProvider is still installed, i.e. \
         statuses::register_account_ports (wiring stage 7) never ran, got: {body}"
    );

    let returned_ids: Vec<Option<&str>> = page.iter().map(|s| s["id"].as_str()).collect();
    for expected in [first.id, second.id] {
        let expected = expected.as_i64().to_string();
        assert!(
            returned_ids.contains(&Some(expected.as_str())),
            "the page must contain the really-inserted status {expected}, got: {returned_ids:?}"
        );
    }

    app.cleanup().await;
}
