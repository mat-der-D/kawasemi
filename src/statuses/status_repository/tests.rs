//! Integration-style, DB-backed tests for `StatusRepository` (Requirements
//! 3.1, 3.5, 3.6, 6.1, 6.2, 7.1, 7.4, 8.1, 8.2), per task 2.1's observable
//! completion condition: "投稿を挿入し ID で取得でき、削除で当該投稿を参照
//! するブースト行が消え・返信元の replies_count が 1 減り（他の関連行は FK
//! CASCADE で整合し）、編集で edited_at が更新され履歴が status_edits に残り、
//! タグ関連付けが永続化され read-only 照会できる".
//!
//! Mirrors `src/actor/repository/tests.rs`'s established convention: reuses
//! `crate::test_harness::spawn_test_app` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext`. `statuses.actor_id` is a
//! *logical-only* reference to `local_actors.id` (no physical FK —
//! `migrations/0007_statuses.sql`'s own naming-note), so unlike
//! `accounts/profile_repository/tests.rs` (whose `account_profiles.actor_id`
//! is likewise logical-only, but that test suite still creates real actor
//! rows), these tests use plain synthetic `Id`s for `actor_id` throughout —
//! nothing this repository does depends on a real `local_actors` row
//! existing, and every id/timestamp still comes from the harness's real,
//! deterministic `RuntimeContext` (`app.runtime.ids`/`app.runtime.clock`),
//! never hand-picked literals.

use std::collections::HashMap;

use time::Duration;

use super::{
    CountKind, adjust_counts, ancestors, apply_edit, attach_media, count_for_actor, delete_status,
    descendants, find_visible, insert_status, last_created_at_for_actor, list_by_actor, list_edits,
    media_ids_for_status, media_ids_for_statuses,
};
use crate::domain::{Id, Visibility};
use crate::statuses::model::{Status, StatusEdit};
use crate::test_harness::{TestApp, spawn_test_app};

/// Builds a ready-to-insert `Status`, using the harness's deterministic
/// runtime for `id`/`created_at` and caller-supplied
/// actor/visibility/reply/reblog relations.
fn sample_status(
    app: &TestApp,
    actor_id: Id,
    visibility: Visibility,
    in_reply_to_id: Option<Id>,
    reblog_of_id: Option<Id>,
) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    Status {
        id,
        actor_id,
        uri: format!("https://example.test/statuses/{}", id.as_i64()),
        url: Some(format!("https://example.test/@actor/{}", id.as_i64())),
        content: "hello world".to_string(),
        visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id,
        in_reply_to_account_id: in_reply_to_id.map(|_| app.runtime.ids.next_id()),
        reblog_of_id,
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

async fn insert(app: &TestApp, status: &Status) {
    insert_status(&app.pool, status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
}

// -- insert_status / find_visible --------------------------------------

/// Requirement 3.1: a freshly inserted status is retrievable by id.
#[tokio::test]
async fn insert_status_persists_a_row_findable_by_id() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &status).await;

    let found = find_visible(&app.pool, status.id, None)
        .await
        .expect("find_visible must succeed")
        .expect("the just-inserted public status must be found");
    assert_eq!(found, status);

    app.cleanup().await;
}

/// Inserting a second status with a `uri` that already exists must be
/// rejected with a caller-facing duplicate error, not a generic 5xx.
#[tokio::test]
async fn insert_status_rejects_duplicate_uri_with_a_client_error() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let first = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &first).await;

    let mut second = sample_status(&app, actor_id, Visibility::Public, None, None);
    second.uri = first.uri.clone();
    let err = insert_status(&app.pool, &second)
        .await
        .expect_err("inserting a duplicate uri must be rejected");
    assert_eq!(err.kind, crate::error::ErrorKind::Client);

    app.cleanup().await;
}

/// find_visible returns None (not an error) for an id nothing was ever
/// inserted under.
#[tokio::test]
async fn find_visible_returns_none_for_an_unknown_id() {
    let app = spawn_test_app().await;
    let unknown = Id::from_i64(i64::MAX - 7);
    let found = find_visible(&app.pool, unknown, None)
        .await
        .expect("find_visible must succeed even when nothing matches");
    assert!(found.is_none());
    app.cleanup().await;
}

/// Requirement 6.1: a private status is visible to its own author but not
/// to another actor.
#[tokio::test]
async fn find_visible_shows_private_status_to_its_author_but_hides_it_from_others() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let other = app.runtime.ids.next_id();
    let status = sample_status(&app, author, Visibility::Private, None, None);
    insert(&app, &status).await;

    let as_author = find_visible(&app.pool, status.id, Some(author))
        .await
        .expect("find_visible must succeed");
    assert_eq!(as_author, Some(status.clone()));

    let as_other = find_visible(&app.pool, status.id, Some(other))
        .await
        .expect("find_visible must succeed");
    assert!(as_other.is_none());

    app.cleanup().await;
}

/// Requirement 6.1: a direct status follows the same author-only rule as
/// private at this repository layer.
#[tokio::test]
async fn find_visible_shows_direct_status_to_its_author_but_hides_it_from_others() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let other = app.runtime.ids.next_id();
    let status = sample_status(&app, author, Visibility::Direct, None, None);
    insert(&app, &status).await;

    assert!(
        find_visible(&app.pool, status.id, Some(author))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        find_visible(&app.pool, status.id, Some(other))
            .await
            .unwrap()
            .is_none()
    );

    app.cleanup().await;
}

/// Requirement 6.4: an unauthenticated (`viewer: None`) fetch only ever
/// returns public/unlisted statuses, never private/direct — even the
/// author's own.
#[tokio::test]
async fn find_visible_unauthenticated_viewer_sees_only_public_and_unlisted() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let public = sample_status(&app, author, Visibility::Public, None, None);
    let unlisted = sample_status(&app, author, Visibility::Unlisted, None, None);
    let private = sample_status(&app, author, Visibility::Private, None, None);
    let direct = sample_status(&app, author, Visibility::Direct, None, None);
    for status in [&public, &unlisted, &private, &direct] {
        insert(&app, status).await;
    }

    assert!(
        find_visible(&app.pool, public.id, None)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        find_visible(&app.pool, unlisted.id, None)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        find_visible(&app.pool, private.id, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        find_visible(&app.pool, direct.id, None)
            .await
            .unwrap()
            .is_none()
    );

    app.cleanup().await;
}

// -- ancestors / descendants --------------------------------------------

/// Requirement 6.2: ancestors returns the reply chain root-first.
#[tokio::test]
async fn ancestors_returns_the_reply_chain_oldest_first() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();

    let root = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &root).await;
    let mid = sample_status(&app, actor_id, Visibility::Public, Some(root.id), None);
    insert(&app, &mid).await;
    let leaf = sample_status(&app, actor_id, Visibility::Public, Some(mid.id), None);
    insert(&app, &leaf).await;

    let chain = ancestors(&app.pool, leaf.id, None)
        .await
        .expect("ancestors must succeed");
    assert_eq!(
        chain.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![root.id, mid.id]
    );

    app.cleanup().await;
}

/// Requirement 6.3: an ancestor invisible to the viewer is excluded from the
/// returned chain, but traversal still continues past it to reach a
/// further, visible ancestor.
#[tokio::test]
async fn ancestors_excludes_invisible_nodes_but_keeps_walking_past_them() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let viewer = app.runtime.ids.next_id();

    let root = sample_status(&app, author, Visibility::Public, None, None);
    insert(&app, &root).await;
    // A private reply the viewer cannot see, sandwiched between root and leaf.
    let hidden_mid = sample_status(&app, author, Visibility::Private, Some(root.id), None);
    insert(&app, &hidden_mid).await;
    let leaf = sample_status(&app, author, Visibility::Public, Some(hidden_mid.id), None);
    insert(&app, &leaf).await;

    let chain = ancestors(&app.pool, leaf.id, Some(viewer))
        .await
        .expect("ancestors must succeed");
    assert_eq!(
        chain.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![root.id],
        "the invisible private reply must be excluded, but the visible root beyond it must \
         still be reached"
    );

    app.cleanup().await;
}

/// ancestors returns an empty Vec for a status that is not a reply.
#[tokio::test]
async fn ancestors_returns_empty_for_a_non_reply_status() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &status).await;

    let chain = ancestors(&app.pool, status.id, None)
        .await
        .expect("ancestors must succeed");
    assert!(chain.is_empty());

    app.cleanup().await;
}

/// Requirement 6.2: descendants returns the full reply tree, flattened and
/// chronologically ordered.
#[tokio::test]
async fn descendants_returns_the_full_reply_tree_chronologically() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();

    let root = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &root).await;
    let child_a = sample_status(&app, actor_id, Visibility::Public, Some(root.id), None);
    insert(&app, &child_a).await;
    let child_b = sample_status(&app, actor_id, Visibility::Public, Some(root.id), None);
    insert(&app, &child_b).await;
    let grandchild = sample_status(&app, actor_id, Visibility::Public, Some(child_a.id), None);
    insert(&app, &grandchild).await;

    let tree = descendants(&app.pool, root.id, None)
        .await
        .expect("descendants must succeed");
    let ids: std::collections::HashSet<_> = tree.iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&child_a.id));
    assert!(ids.contains(&child_b.id));
    assert!(ids.contains(&grandchild.id));
    // Chronological (created_at, id) order.
    for pair in tree.windows(2) {
        assert!(
            (pair[0].created_at, pair[0].id) <= (pair[1].created_at, pair[1].id),
            "descendants must be ordered chronologically"
        );
    }

    app.cleanup().await;
}

/// Requirement 6.3: an invisible descendant is excluded from the returned
/// list, but its own further (visible) descendants are still reached.
#[tokio::test]
async fn descendants_excludes_invisible_nodes_but_keeps_walking_past_them() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let viewer = app.runtime.ids.next_id();

    let root = sample_status(&app, author, Visibility::Public, None, None);
    insert(&app, &root).await;
    let hidden_child = sample_status(&app, author, Visibility::Direct, Some(root.id), None);
    insert(&app, &hidden_child).await;
    let visible_grandchild = sample_status(
        &app,
        author,
        Visibility::Public,
        Some(hidden_child.id),
        None,
    );
    insert(&app, &visible_grandchild).await;

    let tree = descendants(&app.pool, root.id, Some(viewer))
        .await
        .expect("descendants must succeed");
    assert_eq!(
        tree.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![visible_grandchild.id]
    );

    app.cleanup().await;
}

// -- delete_status --------------------------------------------------------

/// This task's own observable completion condition, core case: deleting a
/// status makes it unfindable, removes boost rows referencing it via
/// `reblog_of_id`, and decrements its reply-parent's `replies_count` by 1 —
/// all as one transaction (Requirement 7.4).
#[tokio::test]
async fn delete_status_cascades_boosts_and_decrements_parent_replies_count() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let booster = app.runtime.ids.next_id();

    let parent = sample_status(&app, author, Visibility::Public, None, None);
    insert(&app, &parent).await;
    let reply = sample_status(&app, author, Visibility::Public, Some(parent.id), None);
    insert(&app, &reply).await;
    // Bump the parent's replies_count to 1, as StatusService would on reply
    // creation (Requirement 3.5) — exercised here via this same repository's
    // own adjust_counts, so the test's fixture setup uses only this
    // repository's public surface.
    adjust_counts(&app.pool, parent.id, CountKind::Replies, 1)
        .await
        .expect("adjust_counts must succeed");

    let boost = sample_status(&app, booster, Visibility::Public, None, Some(reply.id));
    insert(&app, &boost).await;

    delete_status(&app.pool, reply.id)
        .await
        .expect("delete_status must succeed");

    // The deleted status itself is gone.
    assert!(
        find_visible(&app.pool, reply.id, Some(author))
            .await
            .unwrap()
            .is_none()
    );
    // (a) the boost row referencing it via reblog_of_id is gone too.
    assert!(
        find_visible(&app.pool, boost.id, Some(booster))
            .await
            .unwrap()
            .is_none(),
        "the boost row referencing the deleted status via reblog_of_id must be cascade-deleted"
    );
    // (b) the parent's replies_count was decremented by 1.
    let parent_after = find_visible(&app.pool, parent.id, None)
        .await
        .unwrap()
        .expect("the parent status must still exist");
    assert_eq!(parent_after.replies_count, 0);

    app.cleanup().await;
}

/// delete_status never drives a counter negative even if called against a
/// parent whose replies_count was already 0.
#[tokio::test]
async fn delete_status_floors_parent_replies_count_at_zero() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let parent = sample_status(&app, author, Visibility::Public, None, None);
    insert(&app, &parent).await;
    let reply = sample_status(&app, author, Visibility::Public, Some(parent.id), None);
    insert(&app, &reply).await;
    // parent.replies_count was never incremented (stays 0) — deleting the
    // reply anyway must not underflow the counter.

    delete_status(&app.pool, reply.id)
        .await
        .expect("delete_status must succeed");

    let parent_after = find_visible(&app.pool, parent.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parent_after.replies_count, 0);

    app.cleanup().await;
}

/// Every same-spec FK `ON DELETE CASCADE` relation (status_edits here) is
/// automatically cleaned up by the plain `DELETE FROM statuses` — this test
/// exercises that indirectly, through this repository's own apply_edit +
/// list_edits, to prove the physical schema's cascade is actually wired.
#[tokio::test]
async fn delete_status_cascades_status_edits_via_physical_fk() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &status).await;

    let edit = StatusEdit {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        content: "edited content".to_string(),
        spoiler_text: String::new(),
        sensitive: false,
        created_at: app.runtime.clock.now(),
    };
    let now = app.runtime.clock.now() + Duration::seconds(1);
    apply_edit(&app.pool, status.id, &edit, now)
        .await
        .expect("apply_edit must succeed");
    assert_eq!(
        list_edits(&app.pool, status.id).await.unwrap().len(),
        1,
        "sanity: the history row exists before deletion"
    );

    delete_status(&app.pool, status.id)
        .await
        .expect("delete_status must succeed");

    let remaining = list_edits(&app.pool, status.id)
        .await
        .expect("list_edits must succeed even once the status is gone");
    assert!(
        remaining.is_empty(),
        "status_edits rows must be cascade-deleted by the physical FK"
    );

    app.cleanup().await;
}

/// delete_status is a silent no-op (not an error) for an id nothing was
/// ever inserted under.
#[tokio::test]
async fn delete_status_is_a_noop_for_an_unknown_id() {
    let app = spawn_test_app().await;
    let unknown = Id::from_i64(i64::MAX - 11);
    delete_status(&app.pool, unknown)
        .await
        .expect("delete_status must succeed even when nothing matches");
    app.cleanup().await;
}

/// Regression test for the double-delete TOCTOU race a run-scope reviewer
/// flagged in already-committed, already-task-reviewed code: two concurrent
/// `delete_status` calls for the *same* id (e.g. a client retry, or a
/// duplicate federation Delete-Activity delivery for the same status) must
/// decrement the reply-parent's `replies_count` exactly once, not twice —
/// mirroring `poll_repository/tests.rs`'s own
/// `record_vote_serializes_concurrent_votes_by_the_same_actor`
/// `tokio::spawn` + cloned-`PgPool` + `tokio::join!` pattern for provoking a
/// genuine concurrent race against real Postgres (see `status_repository.rs`'s
/// doc comment, "Closing the double-delete race", for the fix this test
/// exercises).
#[tokio::test]
async fn delete_status_serializes_concurrent_deletes_of_the_same_reply() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let parent = sample_status(&app, author, Visibility::Public, None, None);
    insert(&app, &parent).await;
    let reply = sample_status(&app, author, Visibility::Public, Some(parent.id), None);
    insert(&app, &reply).await;
    // Pre-set the parent's replies_count to 2, as if it had two real replies
    // — only one of which (`reply`) is about to be raced on concurrent
    // deletion. A correct fix must land on 1 (2 - 1), never 0 (2 - 2).
    adjust_counts(&app.pool, parent.id, CountKind::Replies, 2)
        .await
        .expect("adjust_counts must succeed");

    let pool_a = app.pool.clone();
    let pool_b = app.pool.clone();
    let reply_id = reply.id;

    let (result_a, result_b) = tokio::join!(
        tokio::spawn(async move { delete_status(&pool_a, reply_id).await }),
        tokio::spawn(async move { delete_status(&pool_b, reply_id).await }),
    );
    result_a
        .expect("task a must not panic")
        .expect("delete_status must succeed even for the losing racer (no-op, not an error)");
    result_b
        .expect("task b must not panic")
        .expect("delete_status must succeed even for the losing racer (no-op, not an error)");

    // The reply itself is gone regardless of which racer actually removed
    // it.
    assert!(
        find_visible(&app.pool, reply.id, Some(author))
            .await
            .unwrap()
            .is_none()
    );
    // The parent's replies_count must be decremented exactly once (2 -> 1),
    // never twice (2 -> 0) — the bug this test guards against.
    let parent_after = find_visible(&app.pool, parent.id, None)
        .await
        .unwrap()
        .expect("the parent status must still exist");
    assert_eq!(
        parent_after.replies_count, 1,
        "two concurrent deletes of the same reply must decrement replies_count exactly once, \
         not once per racing call"
    );

    app.cleanup().await;
}

// -- apply_edit / list_edits ----------------------------------------------

/// Requirements 8.1, 8.2: editing a status updates its live content and
/// `edited_at`, and archives the pre-edit content into status_edits.
#[tokio::test]
async fn apply_edit_updates_live_row_and_archives_the_pre_edit_content() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &status).await;

    let edit = StatusEdit {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        content: "edited content".to_string(),
        spoiler_text: "edited cw".to_string(),
        sensitive: true,
        created_at: app.runtime.clock.now(),
    };
    let now = app.runtime.clock.now() + Duration::seconds(30);
    apply_edit(&app.pool, status.id, &edit, now)
        .await
        .expect("apply_edit must succeed");

    let updated = find_visible(&app.pool, status.id, None)
        .await
        .unwrap()
        .expect("the status must still exist");
    assert_eq!(updated.content, "edited content");
    assert_eq!(updated.spoiler_text, "edited cw");
    assert!(updated.sensitive);
    assert_eq!(updated.edited_at, Some(now));

    let history = list_edits(&app.pool, status.id)
        .await
        .expect("list_edits must succeed");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, status.content);
    assert_eq!(history[0].spoiler_text, status.spoiler_text);
    assert_eq!(history[0].sensitive, status.sensitive);
    assert_eq!(history[0].status_id, status.id);

    app.cleanup().await;
}

/// A second edit archives the version that was live *after the first edit*
/// (not the original pre-first-edit content) — history accumulates one
/// entry per superseded version, oldest first.
#[tokio::test]
async fn apply_edit_second_edit_archives_the_first_edited_version() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &status).await;
    let original_content = status.content.clone();

    let first_edit = StatusEdit {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        content: "first edit".to_string(),
        spoiler_text: String::new(),
        sensitive: false,
        created_at: app.runtime.clock.now(),
    };
    let t1 = app.runtime.clock.now() + Duration::seconds(10);
    apply_edit(&app.pool, status.id, &first_edit, t1)
        .await
        .expect("first apply_edit must succeed");

    let second_edit = StatusEdit {
        id: app.runtime.ids.next_id(),
        status_id: status.id,
        content: "second edit".to_string(),
        spoiler_text: String::new(),
        sensitive: false,
        created_at: app.runtime.clock.now(),
    };
    let t2 = t1 + Duration::seconds(10);
    apply_edit(&app.pool, status.id, &second_edit, t2)
        .await
        .expect("second apply_edit must succeed");

    let updated = find_visible(&app.pool, status.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.content, "second edit");
    assert_eq!(updated.edited_at, Some(t2));

    let history = list_edits(&app.pool, status.id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content, original_content, "oldest entry first");
    assert_eq!(history[0].created_at, status.created_at);
    assert_eq!(history[1].content, "first edit");
    assert_eq!(history[1].created_at, t1);

    app.cleanup().await;
}

/// apply_edit against an id nothing was ever inserted under is a
/// caller-facing 404, not a generic 5xx.
#[tokio::test]
async fn apply_edit_returns_a_client_not_found_error_for_an_unknown_id() {
    let app = spawn_test_app().await;
    let unknown = Id::from_i64(i64::MAX - 13);
    let edit = StatusEdit {
        id: app.runtime.ids.next_id(),
        status_id: unknown,
        content: "x".to_string(),
        spoiler_text: String::new(),
        sensitive: false,
        created_at: app.runtime.clock.now(),
    };
    let err = apply_edit(&app.pool, unknown, &edit, app.runtime.clock.now())
        .await
        .expect_err("apply_edit against an unknown id must fail");
    assert_eq!(err.kind, crate::error::ErrorKind::Client);
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// list_edits returns an empty Vec (not an error) for a status that was
/// never edited.
#[tokio::test]
async fn list_edits_returns_empty_for_a_never_edited_status() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &status).await;

    let history = list_edits(&app.pool, status.id)
        .await
        .expect("list_edits must succeed even with no history");
    assert!(history.is_empty());

    app.cleanup().await;
}

// -- adjust_counts ----------------------------------------------------------

/// adjust_counts atomically increments and decrements the targeted counter
/// column, independent of the other two counters.
#[tokio::test]
async fn adjust_counts_increments_and_decrements_the_targeted_column_only() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &status).await;

    adjust_counts(&app.pool, status.id, CountKind::Reblogs, 3)
        .await
        .expect("adjust_counts must succeed");
    adjust_counts(&app.pool, status.id, CountKind::Favourites, 5)
        .await
        .expect("adjust_counts must succeed");
    adjust_counts(&app.pool, status.id, CountKind::Reblogs, -1)
        .await
        .expect("adjust_counts must succeed");

    let updated = find_visible(&app.pool, status.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.reblogs_count, 2);
    assert_eq!(updated.favourites_count, 5);
    assert_eq!(updated.replies_count, 0);

    app.cleanup().await;
}

/// adjust_counts never drives a counter below 0.
#[tokio::test]
async fn adjust_counts_floors_at_zero() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &status).await;

    adjust_counts(&app.pool, status.id, CountKind::Favourites, -10)
        .await
        .expect("adjust_counts must succeed");

    let updated = find_visible(&app.pool, status.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.favourites_count, 0);

    app.cleanup().await;
}

// -- list_by_actor / count_for_actor / last_created_at_for_actor
// (task 9.1, `AccountStatusesProviderImpl`/`AccountCountsContribution`) ------

/// `list_by_actor` returns only the given actor's own statuses, newest
/// (highest id) first, entirely unfiltered by visibility/reply/reblog.
#[tokio::test]
async fn list_by_actor_returns_only_that_actors_statuses_newest_first() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let other_actor_id = app.runtime.ids.next_id();

    let first = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &first).await;
    let second = sample_status(&app, actor_id, Visibility::Private, None, None);
    insert(&app, &second).await;
    let other = sample_status(&app, other_actor_id, Visibility::Public, None, None);
    insert(&app, &other).await;

    let listed = list_by_actor(&app.pool, actor_id)
        .await
        .expect("list_by_actor must succeed");

    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, second.id, "newest (highest id) first");
    assert_eq!(listed[1].id, first.id);
    assert!(listed.iter().all(|status| status.actor_id == actor_id));

    app.cleanup().await;
}

/// `list_by_actor` applies no visibility/reply/reblog filtering at all —
/// that is the caller's own job (`crate::statuses::account_provider`).
#[tokio::test]
async fn list_by_actor_applies_no_filtering_of_its_own() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let parent = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &parent).await;
    let reply = sample_status(&app, actor_id, Visibility::Public, Some(parent.id), None);
    insert(&app, &reply).await;
    let reblog = sample_status(&app, actor_id, Visibility::Public, None, Some(parent.id));
    insert(&app, &reblog).await;
    let direct = sample_status(&app, actor_id, Visibility::Direct, None, None);
    insert(&app, &direct).await;

    let listed = list_by_actor(&app.pool, actor_id)
        .await
        .expect("list_by_actor must succeed");

    assert_eq!(listed.len(), 4, "replies/reblogs/direct all included");

    app.cleanup().await;
}

/// `count_for_actor` counts exactly this actor's own statuses, unaffected
/// by another actor's posts.
#[tokio::test]
async fn count_for_actor_counts_only_that_actors_own_statuses() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let other_actor_id = app.runtime.ids.next_id();

    assert_eq!(
        count_for_actor(&app.pool, actor_id).await.unwrap(),
        0,
        "an actor with no posts counts zero"
    );

    insert(
        &app,
        &sample_status(&app, actor_id, Visibility::Public, None, None),
    )
    .await;
    insert(
        &app,
        &sample_status(&app, actor_id, Visibility::Private, None, None),
    )
    .await;
    insert(
        &app,
        &sample_status(&app, other_actor_id, Visibility::Public, None, None),
    )
    .await;

    assert_eq!(count_for_actor(&app.pool, actor_id).await.unwrap(), 2);
    assert_eq!(count_for_actor(&app.pool, other_actor_id).await.unwrap(), 1);

    app.cleanup().await;
}

/// `last_created_at_for_actor` is `None` for an actor with no posts, and the
/// most recent post's own `created_at` once posts exist.
#[tokio::test]
async fn last_created_at_for_actor_reflects_the_most_recent_post() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();

    assert_eq!(
        last_created_at_for_actor(&app.pool, actor_id)
            .await
            .unwrap(),
        None,
        "an actor with no posts has no last_status_at"
    );

    let mut first = sample_status(&app, actor_id, Visibility::Public, None, None);
    first.created_at -= Duration::seconds(60);
    insert(&app, &first).await;

    let second = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &second).await;

    let last = last_created_at_for_actor(&app.pool, actor_id)
        .await
        .unwrap()
        .expect("an actor with posts has a last_status_at");
    assert_eq!(last, second.created_at);

    app.cleanup().await;
}

// -- media_ids_for_statuses (task 4.1) ---------------------------------

/// Requirements 5.1/5.4, task 4.1's own completion condition ("単数版を N 回
/// 呼んだ結果と複数版を 1 回呼んだ結果が一致する"): `media_ids_for_statuses`
/// returns, for every id, exactly what `media_ids_for_status` returns for
/// that same id on its own — same `WHERE` scoping, same attachment order,
/// same treatment of a status with no attachments at all.
#[tokio::test]
async fn media_ids_for_statuses_matches_calling_the_singular_version_per_status() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();

    let with_many = sample_status(&app, actor_id, Visibility::Public, None, None);
    let with_one = sample_status(&app, actor_id, Visibility::Public, None, None);
    let without_media = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &with_many).await;
    insert(&app, &with_one).await;
    insert(&app, &without_media).await;

    // Deliberately attached in *descending* media-id order, so a plural
    // implementation that ordered by `media_id` (or left the rows
    // unordered) could not coincidentally agree with the singular version's
    // `ORDER BY position`.
    let media_a = Id::from_i64(9_000);
    let media_b = Id::from_i64(8_000);
    let media_c = Id::from_i64(7_000);
    attach_media(&app.pool, with_many.id, &[media_a, media_b, media_c])
        .await
        .expect("attach_media must succeed");
    attach_media(&app.pool, with_one.id, &[media_b])
        .await
        .expect("attach_media must succeed");

    // A status id nothing was ever attached to *and* that no `statuses` row
    // exists for: the plural version must handle it exactly like the
    // singular one does (no entry, not an error).
    let unknown = Id::from_i64(i64::MAX - 11);
    let ids = [with_many.id, with_one.id, without_media.id, unknown];

    let mut per_call: HashMap<Id, Vec<Id>> = HashMap::new();
    for &status_id in &ids {
        let singular = media_ids_for_status(&app.pool, status_id)
            .await
            .expect("media_ids_for_status must succeed");
        if !singular.is_empty() {
            per_call.insert(status_id, singular);
        }
    }

    let batched = media_ids_for_statuses(&app.pool, &ids)
        .await
        .expect("media_ids_for_statuses must succeed");
    assert_eq!(
        batched, per_call,
        "one batched call must agree with N singular calls"
    );

    // Spelled out too, so the comparison above cannot pass vacuously if
    // both sides were to degrade the same way.
    assert_eq!(
        batched.get(&with_many.id),
        Some(&vec![media_a, media_b, media_c]),
        "attachment order (position), not media-id order, must be preserved"
    );
    assert_eq!(batched.get(&with_one.id), Some(&vec![media_b]));
    assert_eq!(batched.get(&without_media.id), None);
    assert_eq!(batched.get(&unknown), None);

    app.cleanup().await;
}

/// Task 4.1's precondition: an empty `status_ids` returns an empty map
/// *without issuing a query*. Closing the pool first is what makes that
/// second half observable — every statement against a closed `PgPool` fails
/// with `sqlx::Error::PoolClosed`, so an `Ok` here can only mean the
/// function short-circuited before touching the database.
#[tokio::test]
async fn media_ids_for_statuses_returns_empty_for_an_empty_slice_without_querying() {
    let app = spawn_test_app().await;
    app.pool.close().await;

    let batched = media_ids_for_statuses(&app.pool, &[])
        .await
        .expect("an empty slice must succeed even against a closed pool");
    assert!(batched.is_empty());

    app.cleanup().await;
}

// -- executor genericity (task 5.1) ----------------------------------------

/// Task 5.1 / Requirement 6.3: [`insert_status`], [`attach_media`] and
/// [`adjust_counts`] accept an open transaction as their executor, and a
/// rollback of that transaction leaves *none* of the three writes behind —
/// the property `StatusService::create_status` (task 5.2) needs in order to
/// make post creation atomic. The pool-taking call sites in the rest of this
/// module are unchanged, which is the other half of the task's contract.
#[tokio::test]
async fn write_functions_accept_a_transaction_and_roll_back_together() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let parent = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &parent).await;

    let child = sample_status(&app, actor_id, Visibility::Public, Some(parent.id), None);
    let media_ids = vec![app.runtime.ids.next_id(), app.runtime.ids.next_id()];

    let mut tx = app.pool.begin().await.expect("begin must succeed");
    insert_status(&mut *tx, &child)
        .await
        .expect("insert_status must succeed against a transaction");
    attach_media(&mut *tx, child.id, &media_ids)
        .await
        .expect("attach_media must succeed against a transaction");
    adjust_counts(&mut *tx, parent.id, CountKind::Replies, 1)
        .await
        .expect("adjust_counts must succeed against a transaction");
    tx.rollback().await.expect("rollback must succeed");

    let found = find_visible(&app.pool, child.id, None)
        .await
        .expect("find_visible must succeed");
    assert!(found.is_none(), "a rolled-back insert must leave no row");

    let attached = media_ids_for_status(&app.pool, child.id)
        .await
        .expect("media_ids_for_status must succeed");
    assert!(
        attached.is_empty(),
        "a rolled-back attach_media must leave no status_media rows"
    );

    let parent_after = find_visible(&app.pool, parent.id, None)
        .await
        .expect("find_visible must succeed")
        .expect("the parent status must still exist");
    assert_eq!(
        parent_after.replies_count, 0,
        "a rolled-back adjust_counts must leave the counter untouched"
    );

    app.cleanup().await;
}

/// The commit half of [`write_functions_accept_a_transaction_and_roll_back_together`]:
/// driven through a transaction that *is* committed, the three functions
/// persist exactly what they persist when driven through the pool.
#[tokio::test]
async fn write_functions_committed_through_a_transaction_persist_normally() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let parent = sample_status(&app, actor_id, Visibility::Public, None, None);
    insert(&app, &parent).await;

    let child = sample_status(&app, actor_id, Visibility::Public, Some(parent.id), None);
    let media_a = app.runtime.ids.next_id();
    let media_b = app.runtime.ids.next_id();

    let mut tx = app.pool.begin().await.expect("begin must succeed");
    insert_status(&mut *tx, &child)
        .await
        .expect("insert_status must succeed against a transaction");
    attach_media(&mut *tx, child.id, &[media_a, media_b])
        .await
        .expect("attach_media must succeed against a transaction");
    adjust_counts(&mut *tx, parent.id, CountKind::Replies, 1)
        .await
        .expect("adjust_counts must succeed against a transaction");
    tx.commit().await.expect("commit must succeed");

    let found = find_visible(&app.pool, child.id, None)
        .await
        .expect("find_visible must succeed");
    assert_eq!(found.as_ref(), Some(&child));

    let attached = media_ids_for_status(&app.pool, child.id)
        .await
        .expect("media_ids_for_status must succeed");
    assert_eq!(
        attached,
        vec![media_a, media_b],
        "attachment order (position) must survive the transaction-driven path"
    );

    let parent_after = find_visible(&app.pool, parent.id, None)
        .await
        .expect("find_visible must succeed")
        .expect("the parent status must still exist");
    assert_eq!(parent_after.replies_count, 1);

    app.cleanup().await;
}
