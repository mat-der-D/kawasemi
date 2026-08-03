//! Integration-style, DB-backed tests for `PollRepository` (Requirements
//! 13.1-13.5), per task 2.3's observable completion condition: "締切後/範囲外
//! /重複投票が拒否され集計が反映される...（リポジトリ単体テストがグリーン）".
//!
//! Mirrors `interaction_repository/tests.rs`'s established convention:
//! reuses `crate::test_harness::spawn_test_app` for an isolated,
//! already-migrated schema and a deterministic `RuntimeContext`, and inserts
//! a real target `statuses` row via `status_repository::insert_status`
//! (`polls.status_id` carries a real FK to `statuses(id)`, so a genuine
//! target status row is required).

use time::Duration;

use crate::domain::{Id, Visibility};
use crate::statuses::model::{Poll, PollOption, Status};
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

use super::{VoteOutcome, insert_poll, record_vote, tally};

/// A small, deliberate duplicate of `status_repository/tests.rs`'s own
/// `sample_status` helper (private to its own module, not visible here) —
/// same rationale as `interaction_repository/tests.rs`'s identical helper:
/// this task's Boundary forbids modifying `status_repository.rs` (including
/// loosening its private items' visibility) just to share a test helper.
fn sample_status(app: &TestApp, actor_id: Id) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    Status {
        id,
        actor_id,
        uri: format!("https://example.test/statuses/{}", id.as_i64()),
        url: Some(format!("https://example.test/@actor/{}", id.as_i64())),
        content: "what should we have for lunch?".to_string(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: Some("en".to_string()),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    }
}

/// Inserts a fresh target status, returning it — every poll in this module
/// needs a real `status_id` to reference (`polls.status_id` is a real FK).
async fn insert_target_status(app: &TestApp, actor_id: Id) -> Status {
    let status = sample_status(app, actor_id);
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    status
}

/// Builds and persists a poll with `titles.len()` options (idx 0..N),
/// `multiple` choice-ness, and `expires_at`, attached to a fresh status
/// owned by `actor_id`. Returns the created `Poll`.
async fn insert_target_poll(
    app: &TestApp,
    actor_id: Id,
    titles: &[&str],
    multiple: bool,
    expires_at: Option<time::OffsetDateTime>,
) -> Poll {
    let status = insert_target_status(app, actor_id).await;
    let poll = Poll {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        expires_at,
        multiple,
    };
    let options: Vec<PollOption> = titles
        .iter()
        .enumerate()
        .map(|(idx, title)| PollOption {
            poll_id: poll.id,
            idx: idx as i32,
            title: title.to_string(),
            votes_count: 0,
        })
        .collect();

    insert_poll(&app.pool, &poll, &options)
        .await
        .expect("insert_poll must succeed for a fresh poll");

    poll
}

// -- insert_poll / tally happy path ---------------------------------------

/// Requirement 13.1: a freshly-created poll's options are persisted and
/// readable back via `tally`, each starting at zero votes.
#[tokio::test]
async fn insert_poll_persists_options_with_zero_initial_votes() {
    let app = spawn_test_app().await;
    let actor = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, actor, &["Pizza", "Sushi", "Tacos"], false, None).await;

    let result = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");

    assert_eq!(result.poll_id, poll.id);
    assert_eq!(result.options.len(), 3);
    assert_eq!(result.options[0].title, "Pizza");
    assert_eq!(result.options[1].title, "Sushi");
    assert_eq!(result.options[2].title, "Tacos");
    assert!(result.options.iter().all(|o| o.votes_count == 0));
    assert_eq!(result.voters_count, 0);
    assert!(result.own_votes.is_empty());

    app.cleanup().await;
}

/// `tally` reports `404 Not Found` for a poll id that does not exist.
#[tokio::test]
async fn tally_reports_not_found_for_unknown_poll() {
    let app = spawn_test_app().await;
    let unknown_poll = app.runtime.ids.next_id();

    let err = tally(&app.pool, unknown_poll, None)
        .await
        .expect_err("tally of a nonexistent poll must fail");
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);

    app.cleanup().await;
}

// -- record_vote: happy path + aggregate reflection (13.2) -----------------

/// Requirement 13.2: a valid vote for an open, single-choice poll is
/// recorded and reflected in the poll's aggregate.
#[tokio::test]
async fn record_vote_records_a_valid_single_choice_vote_and_updates_tally() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;

    let outcome = record_vote(&app.pool, poll.id, voter, &[0], app.runtime.clock.now())
        .await
        .expect("record_vote must succeed for a valid single-choice vote");
    assert_eq!(outcome, VoteOutcome::Recorded);

    let result = tally(&app.pool, poll.id, Some(voter))
        .await
        .expect("tally must succeed");
    assert_eq!(result.options[0].votes_count, 1);
    assert_eq!(result.options[1].votes_count, 0);
    assert_eq!(result.voters_count, 1);
    assert_eq!(result.own_votes, vec![0]);

    app.cleanup().await;
}

/// A multiple-choice poll accepts more than one distinct selected option in
/// a single vote, and every selected option's tally is updated.
#[tokio::test]
async fn record_vote_records_multiple_choices_on_a_multiple_choice_poll() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Pizza", "Sushi", "Tacos"], true, None).await;

    let outcome = record_vote(&app.pool, poll.id, voter, &[0, 2], app.runtime.clock.now())
        .await
        .expect("record_vote must succeed for a valid multi-choice vote");
    assert_eq!(outcome, VoteOutcome::Recorded);

    let result = tally(&app.pool, poll.id, Some(voter))
        .await
        .expect("tally must succeed");
    assert_eq!(result.options[0].votes_count, 1);
    assert_eq!(result.options[1].votes_count, 0);
    assert_eq!(result.options[2].votes_count, 1);
    assert_eq!(
        result.voters_count, 1,
        "one distinct voter, two ballots cast"
    );
    assert_eq!(result.own_votes, vec![0, 2]);

    app.cleanup().await;
}

/// Two different actors voting for the same option each contribute
/// independently to `votes_count`/`voters_count`.
#[tokio::test]
async fn record_vote_accumulates_across_independent_voters() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter_a = app.runtime.ids.next_id();
    let voter_b = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;

    record_vote(&app.pool, poll.id, voter_a, &[0], app.runtime.clock.now())
        .await
        .expect("voter_a's vote must succeed");
    record_vote(&app.pool, poll.id, voter_b, &[0], app.runtime.clock.now())
        .await
        .expect("voter_b's vote must succeed");

    let result = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");
    assert_eq!(result.options[0].votes_count, 2);
    assert_eq!(result.voters_count, 2);

    app.cleanup().await;
}

// -- record_vote rejections: deadline (13.3) --------------------------------

/// Requirement 13.3: a vote against a poll whose deadline has already passed
/// is rejected, and no vote/aggregate change occurs.
#[tokio::test]
async fn record_vote_rejects_when_deadline_has_passed() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let already_expired = now - Duration::seconds(60);
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, Some(already_expired)).await;

    let err = record_vote(&app.pool, poll.id, voter, &[0], now)
        .await
        .expect_err("vote against a closed poll must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    let result = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");
    assert!(
        result.options.iter().all(|o| o.votes_count == 0),
        "a rejected vote must not be reflected in the tally"
    );

    app.cleanup().await;
}

/// A vote submitted exactly at (not after) the poll's `expires_at` is also
/// rejected — `now >= expires_at` is the closed boundary, not `now >
/// expires_at`.
#[tokio::test]
async fn record_vote_rejects_when_now_equals_expires_at() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, Some(now)).await;

    let err = record_vote(&app.pool, poll.id, voter, &[0], now)
        .await
        .expect_err("vote exactly at the deadline must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// A poll with no `expires_at` at all never rejects on deadline grounds.
#[tokio::test]
async fn record_vote_accepts_when_poll_has_no_deadline() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;

    let outcome = record_vote(&app.pool, poll.id, voter, &[0], app.runtime.clock.now())
        .await
        .expect("a poll with no deadline never rejects on deadline grounds");
    assert_eq!(outcome, VoteOutcome::Recorded);

    app.cleanup().await;
}

// -- record_vote rejections: range / single-vs-multiple (13.4) -------------

/// Requirement 13.4: an out-of-range option index is rejected.
#[tokio::test]
async fn record_vote_rejects_out_of_range_choice_index() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;

    let err = record_vote(&app.pool, poll.id, voter, &[7], app.runtime.clock.now())
        .await
        .expect_err("an out-of-range option index must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    let result = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");
    assert!(result.options.iter().all(|o| o.votes_count == 0));

    app.cleanup().await;
}

/// A negative option index is likewise out of range and rejected.
#[tokio::test]
async fn record_vote_rejects_negative_choice_index() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;

    let err = record_vote(&app.pool, poll.id, voter, &[-1], app.runtime.clock.now())
        .await
        .expect_err("a negative option index must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

/// Requirement 13.4: multiple selected choices against a single-choice poll
/// are rejected outright — no partial application of the first choice.
#[tokio::test]
async fn record_vote_rejects_multiple_choices_on_a_single_choice_poll() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No", "Maybe"], false, None).await;

    let err = record_vote(&app.pool, poll.id, voter, &[0, 1], app.runtime.clock.now())
        .await
        .expect_err("multiple choices on a single-choice poll must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    let result = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");
    assert!(
        result.options.iter().all(|o| o.votes_count == 0),
        "a rejected vote must not partially apply"
    );

    app.cleanup().await;
}

/// An empty `choices` slice is rejected (no option selected at all).
#[tokio::test]
async fn record_vote_rejects_an_empty_choice_list() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;

    let err = record_vote(&app.pool, poll.id, voter, &[], app.runtime.clock.now())
        .await
        .expect_err("an empty choice list must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    app.cleanup().await;
}

// -- record_vote rejections: duplicate vote (13.5) --------------------------

/// Requirement 13.5: an actor who already voted cannot vote again — the
/// resubmission is rejected and does not double-count.
#[tokio::test]
async fn record_vote_rejects_a_duplicate_vote_by_the_same_actor() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;
    let now = app.runtime.clock.now();

    record_vote(&app.pool, poll.id, voter, &[0], now)
        .await
        .expect("first vote must succeed");

    let err = record_vote(&app.pool, poll.id, voter, &[0], now)
        .await
        .expect_err("a resubmitted vote by the same actor must be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    let result = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");
    assert_eq!(
        result.options[0].votes_count, 1,
        "the duplicate resubmission must not be double-counted"
    );
    assert_eq!(result.voters_count, 1);

    app.cleanup().await;
}

/// Requirement 13.5's duplicate rejection also applies when the resubmission
/// selects a *different* option than the actor's original vote — any prior
/// vote by this actor in this poll blocks a further vote, not merely an
/// exact repeat of the same choice.
#[tokio::test]
async fn record_vote_rejects_a_resubmission_with_a_different_choice() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;
    let now = app.runtime.clock.now();

    record_vote(&app.pool, poll.id, voter, &[0], now)
        .await
        .expect("first vote must succeed");

    let err = record_vote(&app.pool, poll.id, voter, &[1], now)
        .await
        .expect_err("a resubmission with a different choice must still be rejected");
    assert_eq!(err.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);

    let result = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");
    assert_eq!(result.options[0].votes_count, 1);
    assert_eq!(result.options[1].votes_count, 0);

    app.cleanup().await;
}

/// A duplicate-choice-within-one-request (`[0, 0]`) on a multiple-choice
/// poll is deduplicated to a single ballot, not rejected as a
/// single-vs-multiple violation and not double-counted.
#[tokio::test]
async fn record_vote_deduplicates_repeated_choice_within_one_request() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], true, None).await;

    let outcome = record_vote(&app.pool, poll.id, voter, &[0, 0], app.runtime.clock.now())
        .await
        .expect("a request repeating the same choice index must not be rejected");
    assert_eq!(outcome, VoteOutcome::Recorded);

    let result = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");
    assert_eq!(
        result.options[0].votes_count, 1,
        "a repeated index within one request must not be double-counted"
    );

    app.cleanup().await;
}

/// Two independent polls keep fully independent vote state — a vote on one
/// poll never affects another poll's tally, even for the same actor.
#[tokio::test]
async fn record_vote_is_scoped_to_its_own_poll() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll_a = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;
    let poll_b = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;

    record_vote(&app.pool, poll_a.id, voter, &[0], app.runtime.clock.now())
        .await
        .expect("vote on poll_a must succeed");
    // The same actor voting on a *different* poll is not a duplicate.
    record_vote(&app.pool, poll_b.id, voter, &[1], app.runtime.clock.now())
        .await
        .expect("vote on poll_b must succeed independently of poll_a");

    let tally_a = tally(&app.pool, poll_a.id, None).await.unwrap();
    let tally_b = tally(&app.pool, poll_b.id, None).await.unwrap();
    assert_eq!(tally_a.options[0].votes_count, 1);
    assert_eq!(tally_a.options[1].votes_count, 0);
    assert_eq!(tally_b.options[0].votes_count, 0);
    assert_eq!(tally_b.options[1].votes_count, 1);

    app.cleanup().await;
}

/// `record_vote` reports `404 Not Found` for a poll id that does not exist.
#[tokio::test]
async fn record_vote_reports_not_found_for_unknown_poll() {
    let app = spawn_test_app().await;
    let voter = app.runtime.ids.next_id();
    let unknown_poll = app.runtime.ids.next_id();

    let err = record_vote(
        &app.pool,
        unknown_poll,
        voter,
        &[0],
        app.runtime.clock.now(),
    )
    .await
    .expect_err("voting on a nonexistent poll must fail");
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);

    app.cleanup().await;
}

// -- record_vote: concurrent-duplicate regression (review round 1) ---------

/// Regression test for the TOCTOU race review round 1 flagged: two
/// concurrent `record_vote` calls for the *same* `(poll_id, actor_id)` (here
/// with different `choice`s, the scenario the plain SELECT-then-INSERT check
/// could not catch) must let exactly one succeed, never both — mirroring
/// `code_repository/tests.rs`'s own
/// `concurrent_consumption_of_the_same_code_lets_exactly_one_caller_win`
/// `tokio::spawn` + cloned-`PgPool` pattern for provoking a genuine
/// concurrent race against real Postgres (see `poll_repository.rs`'s doc
/// comment, "Closing the duplicate-vote race", for the `FOR UPDATE` fix this
/// test exercises).
#[tokio::test]
async fn record_vote_serializes_concurrent_votes_by_the_same_actor() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let voter = app.runtime.ids.next_id();
    let poll = insert_target_poll(&app, owner, &["Yes", "No"], true, None).await;
    let now = app.runtime.clock.now();

    let pool_a = app.pool.clone();
    let pool_b = app.pool.clone();

    let (result_a, result_b) = tokio::join!(
        tokio::spawn(async move { record_vote(&pool_a, poll.id, voter, &[0], now).await }),
        tokio::spawn(async move { record_vote(&pool_b, poll.id, voter, &[1], now).await }),
    );

    let outcomes = [
        result_a.expect("task a must not panic"),
        result_b.expect("task b must not panic"),
    ];
    let successes = outcomes.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        successes, 1,
        "exactly one of two concurrent votes by the same actor must succeed, never zero or both"
    );
    let loser = outcomes
        .iter()
        .find(|r| r.is_err())
        .expect("exactly one outcome must be an error");
    assert_eq!(
        loser.as_ref().unwrap_err().status,
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        "the losing concurrent vote must be rejected as a duplicate, not a different failure"
    );

    let result = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");
    let total_votes: i64 = result.options.iter().map(|o| o.votes_count).sum();
    assert_eq!(
        total_votes, 1,
        "only one vote may ever be recorded for this actor, regardless of scheduling"
    );
    assert_eq!(
        result.voters_count, 1,
        "exactly one distinct voter must be recorded"
    );

    app.cleanup().await;
}
