//! Unit tests for this module's own [`super::RequiredPolls`] — the
//! **strict** [`crate::statuses::render_assembler::PollResolver`] a
//! notification page's status embedding is assembled with.
//!
//! These run on the lighter `crate::test_harness::db_fixture::spawn_test_db`
//! fixture: they exercise SQL against a real schema but never stand a server
//! up, so per steering `structure.md`'s test layout rule they belong here
//! rather than under `tests/`.
//!
//! [`NotificationService`][super::NotificationService]'s own `list`/`show`/
//! `dismiss`/`clear` tests, which do require a real running instance, live
//! in `tests/notifications_service_it.rs` — moved there by
//! `.kiro/specs/test-placement-migration` task 4.2. The `sample_status` /
//! `create_test_status` fixtures below are shared with that file and are
//! therefore duplicated in both, each file being its own compiled crate.

use super::*;
use crate::domain::Visibility;
use crate::statuses::model::{Poll, PollOption};
use crate::statuses::poll_repository::PollTally;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::db_fixture::{TestDb, spawn_test_db};

fn sample_status(id: Id, actor_id: Id, created_at: time::OffsetDateTime) -> Status {
    Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: "hello from a notification-embedded status".to_string(),
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
        created_at,
        edited_at: None,
    }
}

async fn create_test_status(pool: &PgPool, runtime: &RuntimeContext, actor_id: Id) -> Id {
    let id = runtime.ids.next_id();
    let now = runtime.clock.now();
    insert_status(pool, &sample_status(id, actor_id, now))
        .await
        .expect("insert_status must succeed");
    id
}

// -- `RequiredPolls` -------------------------------------------------------

/// Inserts a `polls` row carrying `titles` as options `idx 0..N`, attached
/// to a fresh `statuses` row — `polls.status_id` is a real FK, so a genuine
/// target row is required.
async fn insert_test_poll(db: &TestDb, titles: &[&str]) -> Poll {
    let actor_id = db.runtime.ids.next_id();
    let status_id = create_test_status(&db.pool, &db.runtime, actor_id).await;
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

/// This module supplies the **strict** [`PollResolver`]: a `poll_id`
/// matching no `polls` row is this module's own [`poll_not_found`], not a
/// silently poll-less status. Asserted on the exact status and message
/// because `statuses::account_provider`'s equally strict resolver raises a
/// *differently worded* 404 for the same condition, and the two are
/// deliberately not unified.
///
/// Checked with the dangling id in both positions: the resolver must raise
/// whether or not a resolvable poll precedes it.
#[tokio::test]
async fn resolve_many_raises_this_modules_not_found_for_a_dangling_poll_id() {
    let db = spawn_test_db().await;
    let polls = RequiredPolls {
        pool: db.pool.clone(),
    };

    let existing = insert_test_poll(&db, &["Yes", "No"]).await;
    let dangling = Id::from_i64(i64::MAX - 41);

    for requested in [[existing.id, dangling], [dangling, existing.id]] {
        let err = polls
            .resolve_many(&requested, None)
            .await
            .expect_err("a dangling poll id must fail the strict resolver");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.public_message, "poll not found");
    }

    db.cleanup().await;
}

/// The strict resolver is not trivially failing: every id that does resolve
/// comes back, in `poll_ids` order rather than whatever order the rows
/// arrive in. Requested in an order that is neither ascending nor descending
/// by id, so a lookup that let a `HashMap`'s iteration order through could
/// not pass by luck.
#[tokio::test]
async fn resolve_many_returns_every_existing_poll_in_the_requested_order() {
    let db = spawn_test_db().await;
    let polls = RequiredPolls {
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
    let polls = RequiredPolls {
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
