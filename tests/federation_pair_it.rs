//! Integration tests for the 2-instance federation test harness
//! (`.kiro/specs/federation-core/tasks.md`, task 6.4, `_Boundary:
//! FederationTestHarness, federation_pair_it_`; Requirements 10.5, 13.1,
//! 13.2, 13.3, 13.4).
//!
//! Unlike every other integration test in this crate (all built on a single
//! `spawn_test_app()` instance), this file is built on
//! `kawasemi::federation::spawn_federation_pair()` — two genuinely separate,
//! genuinely reachable-over-real-loopback-TCP instances (see
//! `src/federation/test_harness.rs`'s own doc comment for the reachability
//! problem this required solving: `ActorUrls`' hardcoded `https://` scheme
//! vs. plain-HTTP test serving).
//!
//! 1. `spawn_federation_pair_boots_two_isolated_deterministic_instances`
//!    proves the harness itself satisfies Requirements 13.1 (two isolated
//!    instances) and 13.2 (deterministic injection boundaries per instance).
//! 2. `federation_pair_round_trip_verifies_signature_and_dispatch_and_local_http_equivalence`
//!    is the substantive test: instance `A` sends ONE real `deliver()` call
//!    whose recipients mix a LOCAL actor on `A` and a REMOTE actor on `B`
//!    (mirroring `tests/federation_bootstrap_it.rs`'s own mixed-recipients
//!    test, but going one step further: that file's own remote recipient is
//!    a nonexistent host and only proves the job was durably enqueued, never
//!    that anything actually arrived). Here `B` is a real, live, reachable
//!    instance, so:
//!    - The local target is handed off in-process (Requirement 10.3) and
//!      observable immediately.
//!    - The remote target is durably enqueued (Requirement 11.1) and then
//!      actually sent by `A`'s own real, live `DeliveryWorker` background
//!      loop through the real `SignatureNegotiator` /
//!      `ReqwestFederationHttpClient::insecure_loopback()` path (no
//!      hand-built HTTP request bypassing the worker) — `B`'s real
//!      `HttpSignatureVerifier`/`DbFederationPublicKeyResolver` fetch `A`'s
//!      real public key over real HTTP and verify the real signature before
//!      `B`'s own `InboxService` hands the Activity to its dispatch boundary
//!      (Requirement 13.3).
//!    - Both the local and remote target ultimately observe the *same*
//!      canonical Activity (Requirement 10.1's "one canonical Activity, one
//!      resolution" invariant — `DeliveryService::deliver` builds it exactly
//!      once, before branching on physical delivery mechanism), and both
//!      land in their own instance's `received_activities` ledger with the
//!      same `activity_id` — the observable equivalence Requirement 10.5 /
//!      13.4 ask to be verifiable, without requiring a custom registered
//!      `InboundActivityHandler` (see `src/federation/test_harness.rs`'s own
//!      doc comment, "Dispatch-success observation", for why a
//!      `received_activities` row is already as strong a "verification and
//!      dispatch hand-off succeeded" signal as a bespoke handler's own
//!      `Handled` outcome would be, given `InboundActivityDispatcher` is not
//!      live-mutable after this task's own composition-root wiring and
//!      `inbound/service.rs`/`inbound/dispatcher.rs` are both outside this
//!      task's boundary).

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, LocalActor, NewActor};
use kawasemi::federation::urls::ActorUrls;
use kawasemi::federation::{DeliveryRequest, FederationPair, Recipient, spawn_federation_pair};
use kawasemi::test_harness::TestApp;

// ==========================================================================
// Fixtures (mirrors `tests/federation_bootstrap_it.rs`'s/`tests/signatures_it.rs`'s
// own `insert_actor_fixture` convention -- each integration test file is its
// own compiled crate, so this is independently duplicated rather than
// shared).
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
            display_name: format!("Federation Pair IT {handle_str}"),
            summary: "an actor used by the federation-pair integration test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed")
}

fn test_domain(app: &TestApp) -> String {
    app.state.config().server.domain.clone()
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

async fn delivery_job_status(
    app: &TestApp,
    target_inbox: &str,
    activity_id: &str,
) -> Option<String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT status FROM delivery_jobs WHERE target_inbox = $1 AND activity->>'id' = $2",
    )
    .bind(target_inbox)
    .bind(activity_id)
    .fetch_optional(&app.pool)
    .await
    .expect("querying delivery_jobs must succeed");
    row.map(|(status,)| status)
}

/// Polls `check` every 50ms until it returns `true` or `timeout` elapses
/// (panicking with `description` in the latter case). Needed because, unlike
/// this crate's other integration tests (which drive a `DeliveryWorker`
/// directly via a single `run_once` call for determinism, e.g.
/// `tests/inbox_delivery_it.rs`'s own `worker_for` convention), this file
/// deliberately exercises the *real*, already-running background
/// `DeliveryWorker` loop `spawn_federation_pair` starts for each instance
/// (task 6.4's own brief: "so the REAL `DeliveryWorker`/`SignatureNegotiator`/
/// `ReqwestFederationHttpClient::insecure_loopback()` path is exercised end
/// to end"), whose completion is therefore only observable asynchronously.
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
// (1) The harness itself: two isolated, deterministic instances (13.1, 13.2)
// ==========================================================================

#[tokio::test]
async fn spawn_federation_pair_boots_two_isolated_deterministic_instances() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    // Requirement 13.1: two genuinely separate instances -- distinct real
    // bound addresses, distinct isolated database schemas (each instance's
    // own connection pool is pinned to its own `search_path`, mirroring
    // `spawn_test_app`'s own per-instance isolation guarantee).
    assert_ne!(
        a.address, b.address,
        "the two paired instances must be bound to distinct real addresses"
    );

    let a_schema: (String,) = sqlx::query_as("SELECT current_schema()")
        .fetch_one(&a.pool)
        .await
        .expect("querying A's current_schema() must succeed");
    let b_schema: (String,) = sqlx::query_as("SELECT current_schema()")
        .fetch_one(&b.pool)
        .await
        .expect("querying B's current_schema() must succeed");
    assert_ne!(
        a_schema.0, b_schema.0,
        "the two paired instances must be pinned to distinct isolated Postgres schemas"
    );

    // Requirement 13.2: each instance was started with its non-determinism
    // boundaries (clock/id/rng/signing key) replaced with deterministic
    // implementations -- `RuntimeContext::deterministic`'s `FixedClock`
    // always returns the same fixed instant regardless of when/how often
    // `now()` is called, so both instances (built from the same fixed seed,
    // mirroring `spawn_test_app`'s own fixed-seed convention) must observe
    // the identical value.
    assert_eq!(
        a.runtime.clock.now(),
        b.runtime.clock.now(),
        "both paired instances must have been booted with a deterministic (fixed) clock"
    );

    a.cleanup().await;
    b.cleanup().await;
}

// ==========================================================================
// (2) A -> B signed round trip + local/HTTP delivery-result equivalence
// (10.5, 13.3, 13.4)
// ==========================================================================

#[tokio::test]
async fn federation_pair_round_trip_verifies_signature_and_dispatch_and_local_http_equivalence() {
    let FederationPair { a, b } = spawn_federation_pair().await;

    // `alice` sends, on instance A. `carol` is a LOCAL recipient (also on
    // A). `bob` is a REMOTE recipient: a real local actor on instance B,
    // addressed by its real inbox URL on B.
    let alice = insert_actor_fixture(&a, "pair_alice").await;
    let carol = insert_actor_fixture(&a, "pair_carol").await;
    let bob = insert_actor_fixture(&b, "pair_bob").await;

    let b_domain = test_domain(&b);
    let b_urls = ActorUrls::new(b_domain);
    let bob_inbox = b_urls.inbox_url(&bob.handle);

    let a_domain = test_domain(&a);
    let activity_id = format!("https://{a_domain}/activities/federation-pair-1");
    let activity_body: Value = json!({ "id": activity_id, "type": "Follow" });

    // Requirement 10.1/10.2's "one canonical Activity, one resolution"
    // invariant: this single `deliver()` call builds exactly one canonical
    // Activity from `activity_body` and branches only on physical delivery
    // mechanism per recipient -- the local and remote targets below
    // therefore observe the identical Activity, not two independently
    // constructed ones.
    let request = DeliveryRequest {
        activity: activity_body,
        sender: alice.handle.clone(),
        recipients: vec![
            Recipient::Local(carol.handle.clone()),
            Recipient::Remote {
                inbox: bob_inbox.clone(),
                shared_inbox: None,
            },
        ],
    };

    a.state
        .federation()
        .delivery_service()
        .deliver(request)
        .await
        .expect(
            "deliver() must succeed for a mix of a resolvable local recipient and a real, \
             reachable remote inbox URL",
        );

    // --- Local path (Requirement 10.3): in-process, observable immediately ---
    assert!(
        received_activity_exists(&a, &activity_id).await,
        "the local recipient (carol, on A) must have been reached in-process through \
         InboxService::process_local, synchronously within the deliver() call"
    );

    // --- Remote path (Requirements 11.1, 13.3): durably enqueued first ---
    assert!(
        delivery_job_status(&a, &bob_inbox, &activity_id)
            .await
            .is_some(),
        "the remote recipient must have been durably enqueued onto A's real DeliveryQueue \
         before deliver() returned (Requirement 11.1)"
    );

    // A's own real, already-running DeliveryWorker background loop (started
    // by `spawn_federation_pair`) claims the job, signs it via the real
    // SignatureNegotiator/RequestSigner, and sends it via the real
    // ReqwestFederationHttpClient::insecure_loopback() to B's real, live
    // inbox route -- B's real HttpSignatureVerifier/DbFederationPublicKeyResolver
    // fetch A's real public key over real HTTP (downgraded from
    // `https://{a_domain}/...` to plain HTTP by `insecure_loopback`) and
    // verify the real signature before B's InboxService hands the Activity
    // to its dispatch boundary (Requirement 13.3). None of this is
    // hand-built: it is the exact same code path a genuine remote federation
    // send/receive would use.
    wait_until(
        || async { received_activity_exists(&b, &activity_id).await },
        Duration::from_secs(10),
        "B's received_activities to record the Activity A's real DeliveryWorker sent over real \
         signed HTTP",
    )
    .await;

    wait_until(
        || async {
            delivery_job_status(&a, &bob_inbox, &activity_id)
                .await
                .as_deref()
                == Some("done")
        },
        Duration::from_secs(10),
        "A's delivery_jobs row for bob's inbox to reach status 'done'",
    )
    .await;

    // --- Equivalence (Requirements 10.5, 13.4) ---
    // The SAME Activity (`activity_id`), delivered by the SAME `deliver()`
    // call, via two different physical delivery mechanisms (in-process
    // function call vs. signed HTTP POST across two real, separate
    // instances), produced the same observable business-processing result
    // on each receiving side: a `received_activities` row recording that
    // exact Activity id -- i.e. the receive pipeline's
    // block-check -> dedup -> dispatch tail
    // (`InboxService::process_verified`, shared verbatim by both
    // `process_inbound` and `process_local`) ran to the same successful
    // conclusion regardless of which entry point reached it.
    assert!(
        received_activity_exists(&a, &activity_id).await
            && received_activity_exists(&b, &activity_id).await,
        "the local delivery path (on A) and the HTTP federation delivery path (on B) must both \
         have recorded the identical Activity id as successfully received"
    );

    a.cleanup().await;
    b.cleanup().await;
}
