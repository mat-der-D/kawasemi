//! Unit tests for [`super::SearchHydrator`]'s module-private helpers
//! ([`account_ref_id`] and the tolerant [`TolerantPolls`] resolver).
//!
//! Every test here that needs a real, running instance was moved to
//! `tests/search_hydrator_it.rs` by
//! `.kiro/specs/test-placement-migration` task 3.2. What remains needs no
//! such instance: the pure-logic [`account_ref_id`] test, and the three
//! [`TolerantPolls`] tests, which name and construct that module-private
//! type directly — impossible from outside the crate without promoting it —
//! and are served by the lighter
//! [`crate::test_harness::db_fixture::spawn_test_db`].
//!
//! `sample_status`/`create_test_status` below are still needed by the
//! `TolerantPolls` fixtures, so the moved file carries its own copies rather
//! than these being moved out (a parameterized variant of
//! `notifications/service/tests.rs`'s identical helpers, since the tests
//! need to vary `visibility`/`content` per case).

use time::OffsetDateTime;

use super::*;
use crate::domain::Visibility;
use crate::statuses::model::{Poll, PollOption};
use crate::statuses::poll_repository::PollTally;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::db_fixture::{TestDb, spawn_test_db};

// -- pure-logic (no DB) -------------------------------------------------

/// [`account_ref_id`] recovers the same [`Id`] regardless of local/remote-
/// ness — the helper [`SearchHydrator::hydrate_accounts`]'s dedup/
/// `following`-filter logic is built on.
#[test]
fn account_ref_id_recovers_the_id_regardless_of_local_remote() {
    assert_eq!(
        account_ref_id(&AccountRef::Local(Id::from_i64(5))),
        Id::from_i64(5)
    );
    assert_eq!(
        account_ref_id(&AccountRef::Remote(Id::from_i64(9))),
        Id::from_i64(9)
    );
}

// -- DB-backed fixtures ---------------------------------------------------

fn sample_status(
    id: Id,
    actor_id: Id,
    visibility: Visibility,
    content: &str,
    created_at: OffsetDateTime,
) -> Status {
    Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: content.to_string(),
        visibility,
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
        created_at,
        edited_at: None,
    }
}

async fn create_test_status(
    pool: &PgPool,
    runtime: &RuntimeContext,
    actor_id: Id,
    visibility: Visibility,
    content: &str,
) -> Id {
    let id = runtime.ids.next_id();
    let now = runtime.clock.now();
    insert_status(pool, &sample_status(id, actor_id, visibility, content, now))
        .await
        .expect("insert_status must succeed");
    id
}

// -- `TolerantPolls` -------------------------------------------------------

/// Inserts a `polls` row carrying `titles` as options `idx 0..N`, attached
/// to a fresh `statuses` row — `polls.status_id` is a real FK, so a genuine
/// target row is required.
async fn insert_test_poll(db: &TestDb, titles: &[&str]) -> Poll {
    let actor_id = db.runtime.ids.next_id();
    let status_id = create_test_status(
        &db.pool,
        &db.runtime,
        actor_id,
        Visibility::Public,
        "what's for lunch?",
    )
    .await;
    let poll = Poll {
        id: db.runtime.ids.next_id(),
        status_id,
        expires_at: None,
        multiple: false,
    };
    let options: Vec<PollOption> = titles
        .iter()
        .enumerate()
        .map(|(idx, title)| PollOption {
            poll_id: poll.id,
            idx: idx as i32,
            title: (*title).to_string(),
            votes_count: 0,
        })
        .collect();
    poll_repository::insert_poll(&db.pool, &poll, &options)
        .await
        .expect("insert_poll must succeed for a fresh poll");
    poll
}

fn resolved_ids(resolved: &[(Id, Poll, PollTally)]) -> Vec<Id> {
    resolved.iter().map(|(id, _, _)| *id).collect()
}

fn option_titles(tally: &PollTally) -> Vec<&str> {
    tally
        .options
        .iter()
        .map(|option| option.title.as_str())
        .collect()
}

/// This module supplies the **tolerant** [`PollResolver`]: a `poll_id`
/// matching no `polls` row is dropped from the result, never an error, so a
/// search hit whose poll has gone still renders — just without its poll.
///
/// Pinned as its own test because that degradation is the entire difference
/// between this implementation and `statuses::account_provider`'s strict
/// one, and it is invisible in any end-to-end search test (which never has a
/// dangling `poll_id` to begin with). A batched lookup makes "absent from
/// the result" the ordinary shape of a miss, which is exactly when the
/// difference between dropping it and raising on it is easiest to get wrong.
#[tokio::test]
async fn resolve_many_drops_a_dangling_poll_id_instead_of_failing() {
    let db = spawn_test_db().await;
    let polls = TolerantPolls {
        pool: db.pool.clone(),
    };

    let first = insert_test_poll(&db, &["Yes", "No"]).await;
    let second = insert_test_poll(&db, &["Pizza", "Sushi"]).await;
    let dangling = Id::from_i64(i64::MAX - 41);

    let resolved = polls
        .resolve_many(&[first.id, dangling, second.id], None)
        .await
        .expect("a dangling poll id must not fail the tolerant resolver");

    assert_eq!(
        resolved_ids(&resolved),
        vec![first.id, second.id],
        "the dangling id is the only one omitted"
    );
    assert_eq!(option_titles(&resolved[0].2), vec!["Yes", "No"]);
    assert_eq!(option_titles(&resolved[1].2), vec!["Pizza", "Sushi"]);

    db.cleanup().await;
}

/// The returned `Vec` follows `poll_ids`, not whatever order the rows come
/// back in. Requested here in an order that is neither ascending nor
/// descending by id, so a lookup keyed by a `HashMap` that let its own
/// iteration order through could not pass by luck.
#[tokio::test]
async fn resolve_many_returns_polls_in_the_requested_order() {
    let db = spawn_test_db().await;
    let polls = TolerantPolls {
        pool: db.pool.clone(),
    };

    let first = insert_test_poll(&db, &["a"]).await;
    let second = insert_test_poll(&db, &["b"]).await;
    let third = insert_test_poll(&db, &["c"]).await;

    let requested = [third.id, first.id, second.id];
    let resolved = polls
        .resolve_many(&requested, None)
        .await
        .expect("resolve_many must succeed for three existing polls");

    assert_eq!(resolved_ids(&resolved), requested.to_vec());
    assert_eq!(option_titles(&resolved[0].2), vec!["c"]);
    assert_eq!(option_titles(&resolved[1].2), vec!["a"]);
    assert_eq!(option_titles(&resolved[2].2), vec!["b"]);

    db.cleanup().await;
}

/// `viewer` reaches the tally: their own selections come back in
/// `own_votes`, and an unauthenticated read gets an empty one while still
/// seeing the same public `voters_count`.
#[tokio::test]
async fn resolve_many_reports_the_viewers_own_votes() {
    let db = spawn_test_db().await;
    let polls = TolerantPolls {
        pool: db.pool.clone(),
    };

    let poll = insert_test_poll(&db, &["Yes", "No"]).await;
    let viewer = db.runtime.ids.next_id();
    poll_repository::record_vote(&db.pool, poll.id, viewer, &[1], db.runtime.clock.now())
        .await
        .expect("record_vote must succeed");

    let seen = polls
        .resolve_many(&[poll.id], Some(viewer))
        .await
        .expect("resolve_many must succeed");
    assert_eq!(seen[0].2.own_votes, vec![1]);
    assert_eq!(seen[0].2.voters_count, 1);

    let anonymous = polls
        .resolve_many(&[poll.id], None)
        .await
        .expect("resolve_many must succeed without a viewer");
    assert!(anonymous[0].2.own_votes.is_empty());
    assert_eq!(anonymous[0].2.voters_count, 1);

    db.cleanup().await;
}
