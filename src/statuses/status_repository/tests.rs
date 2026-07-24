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

use time::Duration;

use super::{
    CountKind, adjust_counts, ancestors, apply_edit, delete_status, descendants, find_visible,
    insert_status, list_edits,
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
