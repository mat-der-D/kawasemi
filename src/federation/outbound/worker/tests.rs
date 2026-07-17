//! Integration-style tests for `DeliveryWorker` (Requirements 1.1, 3.1, 3.2,
//! 3.3, 11.2, 11.3, 11.5), per task 4.3's observable completion condition:
//! "ワーカーがジョブを送信して done にし、一時失敗で再スケジュール、上限で
//! failed に遷移する統合テストが通る".
//!
//! Mirrors `src/federation/outbound/queue/tests.rs`'s (real `DeliveryQueue`
//! against a real, isolated-schema Postgres via `spawn_test_app`) and
//! `src/federation/signatures/negotiation/tests.rs`'s (real actor/signing-key
//! fixture via `ActorService::create_actor`, `MockFederationHttpClient` as
//! the network boundary) established conventions combined: this module's own
//! job is composing both already-tested real boundaries plus the mocked
//! network edge, never re-proving either boundary's own internals.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;

use super::*;
use crate::actor::model::ActorType;
use crate::actor::owner::create_owner;
use crate::actor::{NewActor, ResolvedActor};
use crate::domain::Id;
use crate::federation::outbound::queue::{DbDeliveryQueue, NewDeliveryJob};
use crate::federation::signatures::{HttpResponse, MockFederationHttpClient, RequestSigner};
use crate::federation::urls::ActorUrls;
use crate::test_harness::{TestApp, spawn_test_app};

const TARGET_INBOX: &str = "https://remote.example/inbox";

/// Creates a real owner + a real local actor (via `ActorService::create_actor`,
/// which provisions a real, currently valid RSA-2048 signing key) under
/// `handle` -- mirrors `negotiation/tests.rs`'s own `create_signable_actor`.
async fn create_signable_actor(app: &TestApp, handle: &str) -> ResolvedActor {
    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: crate::actor::Handle::new(handle).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: "Worker Test Actor".to_string(),
            summary: String::new(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    app.actor
        .directory()
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolving the just-created actor must succeed")
        .expect("the just-created actor must be resolvable")
}

/// Builds a `DeliveryWorker` wired against `app`'s own real
/// `DbDeliveryQueue`/`ActorDirectory`/`Clock`-backed `SignatureNegotiator`,
/// and `mock` as the send boundary -- mirrors
/// `negotiation/tests.rs`'s `negotiator_for`.
fn worker_for(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
) -> DeliveryWorker<DbDeliveryQueue, MockFederationHttpClient> {
    let signer = RequestSigner::new(
        app.actor.directory().clone(),
        app.runtime.keys.clone(),
        ActorUrls::new(app.state.config().server.domain.clone()),
        app.runtime.clock.clone(),
    );
    let negotiator =
        SignatureNegotiator::new(app.pool.clone(), mock, signer, app.runtime.clock.clone());
    DeliveryWorker::new(
        DbDeliveryQueue::new(app.pool.clone()),
        negotiator,
        app.runtime.clock.clone(),
        app.actor.directory().clone(),
    )
}

fn sample_activity(activity_id: &str) -> serde_json::Value {
    json!({ "@context": "https://www.w3.org/ns/activitystreams", "id": activity_id, "type": "Create" })
}

fn new_job(
    id: i64,
    sender_actor_id: Id,
    activity_id: &str,
    next_attempt_at: time::OffsetDateTime,
) -> NewDeliveryJob {
    NewDeliveryJob {
        id: Id::from_i64(id),
        sender_actor_id,
        target_inbox: TARGET_INBOX.to_string(),
        activity: sample_activity(activity_id),
        next_attempt_at,
    }
}

async fn read_job_state(pool: &sqlx::PgPool, job_id: i64) -> (String, i32, time::OffsetDateTime) {
    sqlx::query_as("SELECT status, attempts, next_attempt_at FROM delivery_jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("the job must exist in delivery_jobs")
}

fn ok_response() -> HttpResponse {
    HttpResponse {
        status: StatusCode::OK,
        headers: axum::http::HeaderMap::new(),
        body: Vec::new(),
    }
}

fn server_error_response() -> HttpResponse {
    HttpResponse {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        headers: axum::http::HeaderMap::new(),
        body: Vec::new(),
    }
}

// --- (a) success: a claimed job that gets a 2xx ends up `done`. ---

#[tokio::test]
async fn run_once_delivers_a_due_job_and_marks_it_done() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "worker_alice").await;
    let now = app.runtime.clock.now();

    let queue_setup = DbDeliveryQueue::new(app.pool.clone());
    queue_setup
        .enqueue(new_job(
            1,
            actor.id,
            "https://remote.example/activities/1",
            now,
        ))
        .await
        .expect("enqueue must succeed");

    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(ok_response());
    let worker = worker_for(&app, mock.clone());

    let summary = worker.run_once(10).await.expect("run_once must succeed");

    assert_eq!(summary.claimed, 1);
    assert_eq!(summary.done, 1);
    assert_eq!(summary.rescheduled, 0);
    assert_eq!(summary.failed, 0);

    let (status, attempts, _) = read_job_state(&app.pool, 1).await;
    assert_eq!(status, "done");
    assert_eq!(
        attempts, 0,
        "a successful first attempt must not increment attempts"
    );
    assert_eq!(mock.sent_requests().len(), 1);

    app.cleanup().await;
}

// --- (b) transient failure: a non-2xx response reschedules with backoff,
// incrementing attempts. ---

#[tokio::test]
async fn run_once_reschedules_a_job_on_transient_failure_with_backoff_applied() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "worker_bob").await;
    let now = app.runtime.clock.now();

    let queue_setup = DbDeliveryQueue::new(app.pool.clone());
    queue_setup
        .enqueue(new_job(
            2,
            actor.id,
            "https://remote.example/activities/2",
            now,
        ))
        .await
        .expect("enqueue must succeed");

    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(server_error_response());
    let worker = worker_for(&app, mock.clone());

    let summary = worker.run_once(10).await.expect("run_once must succeed");

    assert_eq!(summary.claimed, 1);
    assert_eq!(summary.done, 0);
    assert_eq!(summary.rescheduled, 1);
    assert_eq!(summary.failed, 0);

    let (status, attempts, next_attempt_at) = read_job_state(&app.pool, 2).await;
    assert_eq!(
        status, "pending",
        "a rescheduled job must go back to 'pending'"
    );
    assert_eq!(attempts, 1);
    assert_eq!(
        next_attempt_at,
        now + backoff_delay(1),
        "next_attempt_at must be pushed forward by exactly backoff_delay(new_attempts)"
    );

    app.cleanup().await;
}

// --- (c) attempts exhausted: a job whose incremented attempts reaches the
// documented max ends up permanently failed instead of rescheduled again. ---

#[tokio::test]
async fn run_once_marks_a_job_permanently_failed_once_attempts_are_exhausted() {
    let app = spawn_test_app().await;
    let actor = create_signable_actor(&app, "worker_carol").await;
    let now = app.runtime.clock.now();

    let queue_setup = DbDeliveryQueue::new(app.pool.clone());
    queue_setup
        .enqueue(new_job(
            3,
            actor.id,
            "https://remote.example/activities/3",
            now,
        ))
        .await
        .expect("enqueue must succeed");

    // Fast-forward this job's persisted attempts count to one below the
    // documented max, so this call's own failed attempt is the one that
    // exhausts the retry budget.
    sqlx::query("UPDATE delivery_jobs SET attempts = $1 WHERE id = $2")
        .bind(DEFAULT_MAX_DELIVERY_ATTEMPTS - 1)
        .bind(3_i64)
        .execute(&app.pool)
        .await
        .expect("fast-forwarding the job's attempts count must succeed");

    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_send_response(server_error_response());
    let worker = worker_for(&app, mock.clone());

    let summary = worker.run_once(10).await.expect("run_once must succeed");

    assert_eq!(summary.claimed, 1);
    assert_eq!(summary.done, 0);
    assert_eq!(summary.rescheduled, 0);
    assert_eq!(
        summary.failed, 1,
        "a job whose incremented attempts reaches the max must be marked failed, not rescheduled"
    );

    let (status, attempts, _) = read_job_state(&app.pool, 3).await;
    assert_eq!(status, "failed");
    assert_eq!(
        attempts,
        DEFAULT_MAX_DELIVERY_ATTEMPTS - 1,
        "mark_failed must not itself alter the persisted attempts count"
    );

    app.cleanup().await;
}

// --- Edge case this task's own Id -> Handle gap resolution introduces: a
// job whose sender_actor_id no longer resolves to any local actor is marked
// permanently failed immediately, without ever attempting a send. ---

#[tokio::test]
async fn run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves() {
    let app = spawn_test_app().await;
    let now = app.runtime.clock.now();
    let nonexistent_sender_id = app.runtime.ids.next_id();

    let queue_setup = DbDeliveryQueue::new(app.pool.clone());
    queue_setup
        .enqueue(new_job(
            4,
            nonexistent_sender_id,
            "https://remote.example/activities/4",
            now,
        ))
        .await
        .expect("enqueue must succeed");

    let mock = Arc::new(MockFederationHttpClient::new());
    let worker = worker_for(&app, mock.clone());

    let summary = worker.run_once(10).await.expect("run_once must succeed");

    assert_eq!(summary.claimed, 1);
    assert_eq!(summary.failed, 1);
    assert!(
        mock.sent_requests().is_empty(),
        "no send attempt must ever be made for a sender that does not resolve"
    );

    let (status, attempts, _) = read_job_state(&app.pool, 4).await;
    assert_eq!(status, "failed");
    assert_eq!(
        attempts, 0,
        "mark_failed must not alter the persisted attempts count"
    );

    app.cleanup().await;
}
