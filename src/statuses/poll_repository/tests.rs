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

use std::collections::HashMap;

use time::Duration;

use crate::domain::{Id, Visibility};
use crate::error::ErrorTag;
use crate::statuses::model::{Poll, PollOption, Status};
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

use super::{
    PollTally, VoteOutcome, find_poll_by_id, find_polls_by_ids, insert_poll, record_vote, tally,
    tally_many,
};

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

/// Like [`insert_target_poll`], but takes explicit `(idx, title)` pairs **in
/// the order they are to be inserted** rather than deriving `idx` from that
/// order. The batched `tally_many` has to preserve each poll's option
/// order exactly as `tally` does (`ORDER BY idx`), and that can only be
/// detected when `idx` order, physical insertion order, and title
/// alphabetical order all disagree, which is what this helper makes
/// expressible.
async fn insert_target_poll_with_options(
    app: &TestApp,
    actor_id: Id,
    options: &[(i32, &str)],
    multiple: bool,
) -> Poll {
    let status = insert_target_status(app, actor_id).await;
    let poll = Poll {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        expires_at: None,
        multiple,
    };
    let rows: Vec<PollOption> = options
        .iter()
        .map(|(idx, title)| PollOption {
            poll_id: poll.id,
            idx: *idx,
            title: (*title).to_string(),
            votes_count: 0,
        })
        .collect();

    insert_poll(&app.pool, &poll, &rows)
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
    assert_eq!(
        err.tag, None,
        "a closed poll is a genuine client error, not the already-applied case \
         the inbound handler treats as idempotent"
    );

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
    assert_eq!(
        err.tag, None,
        "an out-of-range choice is a genuine client error, not the \
         already-applied case the inbound handler treats as idempotent"
    );

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
    assert_eq!(
        err.tag, None,
        "a single-vs-multiple violation is a genuine client error, not the \
         already-applied case the inbound handler treats as idempotent"
    );

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
    assert_eq!(
        err.public_message, "actor has already voted in this poll",
        "the wire-visible wording is part of the API contract and must not drift"
    );
    assert_eq!(
        err.tag,
        Some(ErrorTag::DuplicateVote),
        "the duplicate-vote rejection must be recognizable by tag, not by message text"
    );

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

// -- batched poll reads ---------------------------------------------------

/// The fixture the `tally_many` comparison test builds: polls whose
/// option order, vote pattern, and viewer dimension are all arranged so that
/// a batched implementation cannot agree with the singular one by accident.
struct BatchFixture {
    viewer: Id,
    other_actor: Id,
    /// Multiple-choice, options inserted out of `idx` order.
    ordered: Poll,
    /// Single-choice, voted differently by each of the two actors.
    single: Poll,
    /// Exists, has options, nobody voted in it.
    quiet: Poll,
    /// Exists, has no `poll_options` rows at all.
    optionless: Poll,
    /// Matches no `polls` row.
    unknown: Id,
    ids: Vec<Id>,
}

async fn batch_fixture(app: &TestApp) -> BatchFixture {
    let owner = app.runtime.ids.next_id();
    let viewer = app.runtime.ids.next_id();
    let other_actor = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();

    // Inserted as idx 2, 0, 3, 1, with titles whose alphabetical order is the
    // reverse of their idx order: neither the physical row order (what
    // dropping `ORDER BY idx` surfaces) nor an `ORDER BY title` can
    // coincidentally agree with the `ORDER BY idx` the singular `tally`
    // promises.
    let ordered = insert_target_poll_with_options(
        app,
        owner,
        &[(2, "delta"), (0, "zulu"), (3, "alpha"), (1, "mike")],
        true,
    )
    .await;
    let single = insert_target_poll_with_options(app, owner, &[(0, "Yes"), (1, "No")], false).await;
    let quiet = insert_target_poll_with_options(app, owner, &[(0, "Yes"), (1, "No")], false).await;
    let optionless = insert_target_poll_with_options(app, owner, &[], false).await;

    // `viewer` casts a genuine multiple-choice ballot (two `poll_votes` rows
    // for one voter) and `other_actor` a single one: three rows, two distinct
    // voters, so a `voters_count` that lost its `DISTINCT` — or that a join
    // inflated — reads 3 rather than 2.
    record_vote(&app.pool, ordered.id, viewer, &[0, 2], now)
        .await
        .expect("viewer's multi-choice vote must succeed");
    record_vote(&app.pool, ordered.id, other_actor, &[1], now)
        .await
        .expect("other_actor's vote must succeed");
    record_vote(&app.pool, single.id, viewer, &[1], now)
        .await
        .expect("viewer's vote must succeed");
    record_vote(&app.pool, single.id, other_actor, &[0], now)
        .await
        .expect("other_actor's vote must succeed");

    let unknown = Id::from_i64(i64::MAX - 23);
    let ids = vec![ordered.id, single.id, quiet.id, optionless.id, unknown];

    BatchFixture {
        viewer,
        other_actor,
        ordered,
        single,
        quiet,
        optionless,
        unknown,
        ids,
    }
}

/// Calls the singular [`tally`] once per id, collecting what it returns.
/// A poll that does not exist is a `404` from the singular version rather
/// than a value, which is exactly the "no row -> key absent" shape the
/// batched version reports — so it contributes no entry here.
async fn tallies_per_call(app: &TestApp, ids: &[Id], viewer: Option<Id>) -> HashMap<Id, PollTally> {
    let mut per_call: HashMap<Id, PollTally> = HashMap::new();
    for &poll_id in ids {
        match tally(&app.pool, poll_id, viewer).await {
            Ok(result) => {
                per_call.insert(poll_id, result);
            }
            Err(err) => assert_eq!(
                err.status,
                axum::http::StatusCode::NOT_FOUND,
                "the only tolerated singular failure here is the nonexistent poll"
            ),
        }
    }
    per_call
}

/// N singular calls and one batched call must agree: `find_polls_by_ids`
/// returns, for every id, exactly what
/// `find_poll_by_id` returns for that same id on its own — same `WHERE`
/// scoping, same treatment of an id matching no row.
#[tokio::test]
async fn find_polls_by_ids_matches_calling_the_singular_version_per_poll() {
    let app = spawn_test_app().await;
    let owner = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();

    // Deliberately varied along both of a `polls` row's own dimensions
    // (`multiple`, `expires_at`), so a batched read that dropped or
    // transposed a column could not agree with the singular one.
    let open = insert_target_poll(&app, owner, &["Yes", "No"], false, None).await;
    let multiple = insert_target_poll(&app, owner, &["Pizza", "Sushi"], true, None).await;
    let expiring = insert_target_poll(
        &app,
        owner,
        &["Yes"],
        false,
        Some(now + Duration::seconds(600)),
    )
    .await;
    let unknown = Id::from_i64(i64::MAX - 23);
    let ids = [open.id, multiple.id, expiring.id, unknown];

    let mut per_call: HashMap<Id, Poll> = HashMap::new();
    for &poll_id in &ids {
        let singular = find_poll_by_id(&app.pool, poll_id)
            .await
            .expect("find_poll_by_id must succeed");
        if let Some(poll) = singular {
            per_call.insert(poll_id, poll);
        }
    }

    let batched = find_polls_by_ids(&app.pool, &ids)
        .await
        .expect("find_polls_by_ids must succeed");
    assert_eq!(
        batched, per_call,
        "one batched call must agree with N singular calls"
    );

    // Spelled out too, so the comparison above cannot pass vacuously if both
    // sides were to degrade the same way.
    assert_eq!(batched.get(&open.id), Some(&open));
    assert_eq!(batched.get(&multiple.id), Some(&multiple));
    assert_eq!(batched.get(&expiring.id), Some(&expiring));
    assert_eq!(
        batched.get(&unknown),
        None,
        "an id matching no `polls` row is absent from the map, not an error"
    );

    app.cleanup().await;
}

/// The same agreement, for the aggregate half: `tally_many` returns, for
/// every id and for every shape of `viewer`, exactly what `tally` returns for
/// that same id on its own — same option order, same per-option counts, same
/// `voters_count`, same `own_votes`.
#[tokio::test]
async fn tally_many_matches_calling_the_singular_version_per_poll() {
    let app = spawn_test_app().await;
    let fixture = batch_fixture(&app).await;

    // All three viewer shapes, because `own_votes` is the one part of a
    // tally that moves with the viewer: a batched query that dropped its
    // `actor_id` predicate would still agree with the singular version for
    // whichever single viewer happened to be tested alone.
    for viewer in [Some(fixture.viewer), Some(fixture.other_actor), None] {
        let per_call = tallies_per_call(&app, &fixture.ids, viewer).await;
        let batched = tally_many(&app.pool, &fixture.ids, viewer)
            .await
            .expect("tally_many must succeed");
        assert_eq!(
            batched, per_call,
            "one batched call must agree with N singular calls (viewer = {viewer:?})"
        );
        assert_eq!(
            batched.get(&fixture.unknown),
            None,
            "an id matching no `polls` row is absent from the map, not an error"
        );
    }

    // Spelled out too, so the comparisons above cannot pass vacuously if both
    // sides were to degrade the same way.
    let mine = tally_many(&app.pool, &fixture.ids, Some(fixture.viewer))
        .await
        .expect("tally_many must succeed");
    let theirs = tally_many(&app.pool, &fixture.ids, Some(fixture.other_actor))
        .await
        .expect("tally_many must succeed");
    let anonymous = tally_many(&app.pool, &fixture.ids, None)
        .await
        .expect("tally_many must succeed");

    let ordered = mine
        .get(&fixture.ordered.id)
        .expect("the multiple-choice poll must be present");
    assert_eq!(
        ordered
            .options
            .iter()
            .map(|option| (option.idx, option.title.as_str(), option.votes_count))
            .collect::<Vec<_>>(),
        vec![
            (0, "zulu", 1),
            (1, "mike", 1),
            (2, "delta", 1),
            (3, "alpha", 0)
        ],
        "options must come back in `idx` order — not insertion or title order — \
         each carrying its own real count"
    );
    assert_eq!(
        ordered.voters_count, 2,
        "three ballots cast by two distinct actors: a non-DISTINCT or \
         join-inflated count would read 3"
    );
    assert_eq!(
        ordered.own_votes,
        vec![0, 2],
        "a multiple-choice voter's own selections, ascending"
    );

    let ordered_theirs = theirs
        .get(&fixture.ordered.id)
        .expect("the multiple-choice poll must be present for the other viewer too");
    assert_eq!(
        ordered_theirs.own_votes,
        vec![1],
        "a different viewer must yield a different voted set, not the first viewer's"
    );
    assert_eq!(
        ordered_theirs.options, ordered.options,
        "the aggregate half of a tally must not move with the viewer"
    );

    let ordered_anonymous = anonymous
        .get(&fixture.ordered.id)
        .expect("the multiple-choice poll must be present without a viewer too");
    assert!(
        ordered_anonymous.own_votes.is_empty(),
        "no viewer means no own votes"
    );
    assert_eq!(ordered_anonymous.voters_count, 2);

    assert_eq!(
        mine.get(&fixture.single.id)
            .expect("the single-choice poll must be present")
            .own_votes,
        vec![1]
    );
    assert_eq!(
        theirs
            .get(&fixture.single.id)
            .expect("the single-choice poll must be present")
            .own_votes,
        vec![0],
        "each viewer's own selection on the single-choice poll differs"
    );

    let quiet = mine
        .get(&fixture.quiet.id)
        .expect("a poll nobody voted in must still be present");
    assert_eq!(quiet.voters_count, 0);
    assert!(quiet.own_votes.is_empty());
    assert_eq!(quiet.options.len(), 2);
    assert!(quiet.options.iter().all(|option| option.votes_count == 0));

    let optionless = mine.get(&fixture.optionless.id).expect(
        "an existing poll with no options must still be present — the map is \
         keyed off `polls`, not off option rows",
    );
    assert!(optionless.options.is_empty());
    assert_eq!(optionless.voters_count, 0);

    app.cleanup().await;
}

/// For both functions at once: an empty `poll_ids` returns an empty map
/// *without issuing a query*. Closing the pool first is
/// what makes that second half observable — every statement against a closed
/// `PgPool` fails with `sqlx::Error::PoolClosed`, so an `Ok` here can only
/// mean the function short-circuited before touching the database. Both
/// share one closed pool rather than one test app each: the assertion is
/// identical and a test app is the scarce
/// resource, since each holds its own connection pool.
#[tokio::test]
async fn batched_poll_reads_return_empty_for_an_empty_slice_without_querying() {
    let app = spawn_test_app().await;
    let viewer = app.runtime.ids.next_id();
    app.pool.close().await;

    assert!(
        find_polls_by_ids(&app.pool, &[])
            .await
            .expect("an empty slice must succeed even against a closed pool")
            .is_empty()
    );
    assert!(
        tally_many(&app.pool, &[], Some(viewer))
            .await
            .expect("an empty slice must succeed even against a closed pool")
            .is_empty()
    );
    assert!(
        tally_many(&app.pool, &[], None)
            .await
            .expect("an empty slice must succeed even against a closed pool")
            .is_empty()
    );

    app.cleanup().await;
}

// -- executor genericity ---------------------------------------------------

/// [`insert_poll`] accepts an open transaction, and rolling that transaction
/// back leaves neither the `polls` row nor its `poll_options` rows behind —
/// the property `StatusService::create_status` needs so a failed post
/// creation cannot strand a poll.
#[tokio::test]
async fn insert_poll_accepts_a_transaction_and_rolls_back() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, actor_id).await;

    let poll = Poll {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        expires_at: None,
        multiple: false,
    };
    let options = vec![
        PollOption {
            poll_id: poll.id,
            idx: 0,
            title: "ramen".to_string(),
            votes_count: 0,
        },
        PollOption {
            poll_id: poll.id,
            idx: 1,
            title: "curry".to_string(),
            votes_count: 0,
        },
    ];

    let mut tx = app.pool.begin().await.expect("begin must succeed");
    insert_poll(&mut *tx, &poll, &options)
        .await
        .expect("insert_poll must succeed against a transaction");
    tx.rollback().await.expect("rollback must succeed");

    let found = find_poll_by_id(&app.pool, poll.id)
        .await
        .expect("find_poll_by_id must succeed");
    assert!(
        found.is_none(),
        "a rolled-back insert_poll must leave no row"
    );

    app.cleanup().await;
}

/// The commit half of [`insert_poll_accepts_a_transaction_and_rolls_back`]:
/// committed through a transaction, `insert_poll` persists the poll and its
/// options exactly as the pool-driven path does.
#[tokio::test]
async fn insert_poll_committed_through_a_transaction_persists_normally() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, actor_id).await;

    let poll = Poll {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        expires_at: None,
        multiple: false,
    };
    let options = vec![
        PollOption {
            poll_id: poll.id,
            idx: 0,
            title: "ramen".to_string(),
            votes_count: 0,
        },
        PollOption {
            poll_id: poll.id,
            idx: 1,
            title: "curry".to_string(),
            votes_count: 0,
        },
    ];

    let mut tx = app.pool.begin().await.expect("begin must succeed");
    insert_poll(&mut *tx, &poll, &options)
        .await
        .expect("insert_poll must succeed against a transaction");
    tx.commit().await.expect("commit must succeed");

    let found = find_poll_by_id(&app.pool, poll.id)
        .await
        .expect("find_poll_by_id must succeed");
    assert_eq!(found.as_ref(), Some(&poll));

    let tallied = tally(&app.pool, poll.id, None)
        .await
        .expect("tally must succeed");
    let titles: Vec<&str> = tallied
        .options
        .iter()
        .map(|option| option.title.as_str())
        .collect();
    assert_eq!(titles, vec!["ramen", "curry"]);

    app.cleanup().await;
}
