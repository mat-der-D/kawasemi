//! Tests for this module's [`super::TolerantPolls`] — the [`PollResolver`]
//! `StatusHydrator` hands the assembler.
//!
//! Scoped to the resolver rather than to `hydrate` as a whole: end-to-end
//! timeline hydration is already covered by `tests/timeline_hydrator_it.rs`
//! and `tests/timeline_status_contract_it.rs`, neither of which can reach
//! the one behavior that distinguishes *this* resolver from the four others
//! implementing the same trait — what happens to a `poll_id` whose `polls`
//! row is not there. A rendered timeline never contains one.
//!
//! DB-backed against `crate::test_harness::spawn_test_app`, mirroring
//! `statuses/poll_repository/tests.rs`'s fixture convention: a poll needs a
//! real `statuses` row to reference (`polls.status_id` is a genuine FK), but
//! `statuses.actor_id` is not FK-constrained, so a bare id stands in for the
//! author.

use super::*;
use crate::domain::Visibility;
use crate::statuses::model::{Poll, PollOption};
use crate::statuses::poll_repository::PollTally;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

fn sample_status(app: &TestApp, actor_id: Id) -> Status {
    let id = app.runtime.ids.next_id();
    Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: "what should we have for lunch?".to_string(),
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
        created_at: app.runtime.clock.now(),
        edited_at: None,
    }
}

/// Inserts a `polls` row carrying `titles` as options `idx 0..N`, attached
/// to a fresh `statuses` row.
async fn insert_test_poll(app: &TestApp, titles: &[&str]) -> Poll {
    let status = sample_status(app, app.runtime.ids.next_id());
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");

    let poll = Poll {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
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
    poll_repository::insert_poll(&app.pool, &poll, &options)
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
/// matching no `polls` row is dropped from the result, never an error, so
/// one dangling poll cannot fail a whole page of timeline — the degradation
/// this module's own doc comment commits to.
///
/// Pinned as its own test because that degradation is the entire difference
/// between this implementation and `statuses::account_provider`'s strict
/// one, and a batched lookup makes "absent from the result" the ordinary
/// shape of a miss, which is exactly when dropping-vs-raising is easiest to
/// get wrong.
#[tokio::test]
async fn resolve_many_drops_a_dangling_poll_id_instead_of_failing() {
    let app = spawn_test_app().await;
    let polls = TolerantPolls {
        pool: app.pool.clone(),
    };

    let first = insert_test_poll(&app, &["Yes", "No"]).await;
    let second = insert_test_poll(&app, &["Pizza", "Sushi"]).await;
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

    app.cleanup().await;
}

/// The returned `Vec` follows `poll_ids`, not whatever order the rows come
/// back in. Requested here in an order that is neither ascending nor
/// descending by id, so a lookup keyed by a `HashMap` that let its own
/// iteration order through could not pass by luck.
#[tokio::test]
async fn resolve_many_returns_polls_in_the_requested_order() {
    let app = spawn_test_app().await;
    let polls = TolerantPolls {
        pool: app.pool.clone(),
    };

    let first = insert_test_poll(&app, &["a"]).await;
    let second = insert_test_poll(&app, &["b"]).await;
    let third = insert_test_poll(&app, &["c"]).await;

    let requested = [third.id, first.id, second.id];
    let resolved = polls
        .resolve_many(&requested, None)
        .await
        .expect("resolve_many must succeed for three existing polls");

    assert_eq!(resolved_ids(&resolved), requested.to_vec());
    assert_eq!(option_titles(&resolved[0].2), vec!["c"]);
    assert_eq!(option_titles(&resolved[1].2), vec!["a"]);
    assert_eq!(option_titles(&resolved[2].2), vec!["b"]);

    app.cleanup().await;
}

/// `viewer` reaches the tally: their own selections come back in
/// `own_votes`, and an unauthenticated read gets an empty one while still
/// seeing the same public `voters_count`.
#[tokio::test]
async fn resolve_many_reports_the_viewers_own_votes() {
    let app = spawn_test_app().await;
    let polls = TolerantPolls {
        pool: app.pool.clone(),
    };

    let poll = insert_test_poll(&app, &["Yes", "No"]).await;
    let viewer = app.runtime.ids.next_id();
    poll_repository::record_vote(&app.pool, poll.id, viewer, &[1], app.runtime.clock.now())
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

    app.cleanup().await;
}
