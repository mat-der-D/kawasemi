//! Integration-style, DB-backed tests for `InteractionRepository`
//! (Requirements 9.1, 9.3, 9.4, 10.1, 10.3, 10.4, 11.1, 11.2, 11.3, 12.1,
//! 12.2), per task 2.2's observable completion condition: "同一 (actor,
//! status) への二重登録が抑止され、ブックマーク一覧がブックマーク固有カー
//! ソルで取得できる（リポジトリ単体テストがグリーン）".
//!
//! Mirrors `status_repository/tests.rs`'s established convention: reuses
//! `crate::test_harness::spawn_test_app` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext`, and inserts real `statuses`
//! rows via `status_repository::insert_status` (this module's own writes
//! carry a real FK to `statuses(id)`, unlike `statuses.actor_id`'s
//! logical-only reference, so a real target status row is required — actor
//! ids stay plain synthetic `Id`s, same as `status_repository/tests.rs`,
//! since nothing here depends on a real `local_actors` row existing).

use crate::api::pagination::PageParams;
use crate::domain::{Id, Visibility};
use crate::statuses::model::Status;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

use super::{
    add_bookmark, add_favourite, exists_bookmark, exists_favourite, exists_pin, find_reblog,
    list_bookmarks, remove_bookmark, remove_favourite, set_pin,
};

/// Builds a ready-to-insert `Status`, using the harness's deterministic
/// runtime for `id`/`created_at` and caller-supplied actor/reblog relation.
/// A small, deliberate duplicate of `status_repository/tests.rs`'s own
/// `sample_status` helper: that helper is private to its module (`mod
/// tests` with no `pub`), so it is not visible from this sibling module —
/// mirrors `interaction_repository.rs`'s own "Row-to-`Status`
/// reconstruction...is a small, deliberate duplicate" doc-comment rationale
/// for the same reason (this task's Boundary: do not modify
/// `status_repository.rs`, including loosening its private items'
/// visibility, just to share a test helper).
fn sample_status(app: &TestApp, actor_id: Id, reblog_of_id: Option<Id>) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    Status {
        id,
        actor_id,
        uri: format!("https://example.test/statuses/{}", id.as_i64()),
        url: Some(format!("https://example.test/@actor/{}", id.as_i64())),
        content: "hello world".to_string(),
        visibility: Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
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

/// Inserts a fresh, visible `statuses` row owned by `actor_id`, returning
/// it — a shared building block for every test in this module that needs a
/// real `status_id` to reference (favourites/bookmarks/pins all carry a
/// real FK to `statuses(id)`).
async fn insert_target_status(app: &TestApp, actor_id: Id) -> Status {
    let status = sample_status(app, actor_id, None);
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    status
}

// -- favourite ------------------------------------------------------------

/// Requirement 10.1: a fresh favourite is recorded and reported as new.
#[tokio::test]
async fn add_favourite_records_a_new_favourite() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, app.runtime.ids.next_id()).await;

    let created = add_favourite(&app.pool, actor_id, status.id, app.runtime.clock.now())
        .await
        .expect("add_favourite must succeed");
    assert!(created, "first favourite of a status must be reported new");

    let exists = exists_favourite(&app.pool, actor_id, status.id)
        .await
        .expect("exists_favourite must succeed");
    assert!(exists);

    app.cleanup().await;
}

/// Requirement 10.4: a duplicate favourite by the same actor for the same
/// status is silently suppressed (not a second row, not an error).
#[tokio::test]
async fn add_favourite_suppresses_duplicate_registration() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, app.runtime.ids.next_id()).await;
    let now = app.runtime.clock.now();

    let first = add_favourite(&app.pool, actor_id, status.id, now)
        .await
        .expect("first add_favourite must succeed");
    assert!(first);

    let second = add_favourite(&app.pool, actor_id, status.id, now)
        .await
        .expect("second add_favourite must succeed, not error");
    assert!(
        !second,
        "duplicate favourite registration must be reported as not-new"
    );

    app.cleanup().await;
}

/// Requirement 10.3: unfavouriting removes the record and is reflected by
/// `exists_favourite`; unfavouriting again is an idempotent no-op.
#[tokio::test]
async fn remove_favourite_revokes_and_is_idempotent() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, app.runtime.ids.next_id()).await;
    add_favourite(&app.pool, actor_id, status.id, app.runtime.clock.now())
        .await
        .expect("add_favourite must succeed");

    let removed = remove_favourite(&app.pool, actor_id, status.id)
        .await
        .expect("remove_favourite must succeed");
    assert!(removed);
    assert!(
        !exists_favourite(&app.pool, actor_id, status.id)
            .await
            .expect("exists_favourite must succeed")
    );

    let removed_again = remove_favourite(&app.pool, actor_id, status.id)
        .await
        .expect("removing an absent favourite must succeed, not error");
    assert!(!removed_again, "second removal must report no change made");

    app.cleanup().await;
}

/// Two different actors favouriting the same status each get their own,
/// independent record (the unique key is `(actor_id, status_id)`, not
/// `status_id` alone).
#[tokio::test]
async fn favourites_are_independent_per_actor() {
    let app = spawn_test_app().await;
    let actor_a = app.runtime.ids.next_id();
    let actor_b = app.runtime.ids.next_id();
    let status = insert_target_status(&app, app.runtime.ids.next_id()).await;
    let now = app.runtime.clock.now();

    assert!(
        add_favourite(&app.pool, actor_a, status.id, now)
            .await
            .expect("add_favourite must succeed")
    );
    assert!(
        add_favourite(&app.pool, actor_b, status.id, now)
            .await
            .expect("add_favourite must succeed")
    );

    assert!(
        exists_favourite(&app.pool, actor_a, status.id)
            .await
            .unwrap()
    );
    assert!(
        exists_favourite(&app.pool, actor_b, status.id)
            .await
            .unwrap()
    );

    remove_favourite(&app.pool, actor_a, status.id)
        .await
        .expect("remove_favourite must succeed");
    assert!(
        !exists_favourite(&app.pool, actor_a, status.id)
            .await
            .unwrap()
    );
    assert!(
        exists_favourite(&app.pool, actor_b, status.id)
            .await
            .unwrap(),
        "actor_b's favourite must be unaffected by actor_a's removal"
    );

    app.cleanup().await;
}

// -- bookmark ---------------------------------------------------------------

/// Requirement 11.1: a fresh bookmark is recorded and reported as new.
#[tokio::test]
async fn add_bookmark_records_a_new_bookmark() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, app.runtime.ids.next_id()).await;
    let bookmark_id = app.runtime.ids.next_id();

    let created = add_bookmark(
        &app.pool,
        bookmark_id,
        actor_id,
        status.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");
    assert!(created);
    assert!(
        exists_bookmark(&app.pool, actor_id, status.id)
            .await
            .unwrap()
    );

    app.cleanup().await;
}

/// Requirement 11.1's own "(actor_id, status_id) 一意": a duplicate
/// bookmark registration is silently suppressed, not a second row or error.
#[tokio::test]
async fn add_bookmark_suppresses_duplicate_registration() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, app.runtime.ids.next_id()).await;
    let now = app.runtime.clock.now();

    let first_id = app.runtime.ids.next_id();
    let first = add_bookmark(&app.pool, first_id, actor_id, status.id, now)
        .await
        .expect("first add_bookmark must succeed");
    assert!(first);

    // A second attempt using a *different* caller-minted bookmark id must
    // still be suppressed by the (actor_id, status_id) unique constraint,
    // not merely by reusing the same id.
    let second_id = app.runtime.ids.next_id();
    let second = add_bookmark(&app.pool, second_id, actor_id, status.id, now)
        .await
        .expect("second add_bookmark must succeed, not error");
    assert!(
        !second,
        "duplicate bookmark registration must be reported as not-new"
    );

    app.cleanup().await;
}

/// Requirement 11.2: unbookmarking removes the record; repeating it is an
/// idempotent no-op.
#[tokio::test]
async fn remove_bookmark_revokes_and_is_idempotent() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, app.runtime.ids.next_id()).await;
    let bookmark_id = app.runtime.ids.next_id();
    add_bookmark(
        &app.pool,
        bookmark_id,
        actor_id,
        status.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");

    let removed = remove_bookmark(&app.pool, actor_id, status.id)
        .await
        .expect("remove_bookmark must succeed");
    assert!(removed);
    assert!(
        !exists_bookmark(&app.pool, actor_id, status.id)
            .await
            .unwrap()
    );

    let removed_again = remove_bookmark(&app.pool, actor_id, status.id)
        .await
        .expect("removing an absent bookmark must succeed, not error");
    assert!(!removed_again);

    app.cleanup().await;
}

/// Requirement 11.3: `list_bookmarks` returns bookmarked statuses ordered
/// by the bookmark's own creation order (newest bookmark first), which is
/// deliberately the *opposite* of the statuses' own creation order here —
/// proving the cursor is bookmark-specific, not the status id/created_at.
#[tokio::test]
async fn list_bookmarks_orders_by_bookmark_creation_not_status_creation() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();

    // Create statuses in order: older, newer (older has the smaller id /
    // earlier created_at).
    let older = insert_target_status(&app, app.runtime.ids.next_id()).await;
    let newer = insert_target_status(&app, app.runtime.ids.next_id()).await;

    // Bookmark them in the *reverse* order: `newer` first, `older` second —
    // so bookmark-creation order and status-creation order disagree.
    let bookmark_of_newer = app.runtime.ids.next_id();
    add_bookmark(
        &app.pool,
        bookmark_of_newer,
        actor_id,
        newer.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");

    let bookmark_of_older = app.runtime.ids.next_id();
    add_bookmark(
        &app.pool,
        bookmark_of_older,
        actor_id,
        older.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");

    let page = list_bookmarks(&app.pool, actor_id, PageParams::default())
        .await
        .expect("list_bookmarks must succeed");

    let ids: Vec<Id> = page.items.iter().map(|s| s.id).collect();
    assert_eq!(
        ids,
        vec![older.id, newer.id],
        "list_bookmarks must return items in bookmark-creation order (most \
         recently bookmarked first: `older` was bookmarked second here), not \
         status id/created_at order"
    );

    app.cleanup().await;
}

/// `list_bookmarks` only returns the requesting actor's own bookmarks.
#[tokio::test]
async fn list_bookmarks_is_scoped_to_the_requesting_actor() {
    let app = spawn_test_app().await;
    let actor_a = app.runtime.ids.next_id();
    let actor_b = app.runtime.ids.next_id();
    let status = insert_target_status(&app, app.runtime.ids.next_id()).await;

    let bookmark_id = app.runtime.ids.next_id();
    add_bookmark(
        &app.pool,
        bookmark_id,
        actor_a,
        status.id,
        app.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");

    let page_a = list_bookmarks(&app.pool, actor_a, PageParams::default())
        .await
        .expect("list_bookmarks must succeed");
    assert_eq!(page_a.items.len(), 1);

    let page_b = list_bookmarks(&app.pool, actor_b, PageParams::default())
        .await
        .expect("list_bookmarks must succeed");
    assert!(page_b.items.is_empty());

    app.cleanup().await;
}

/// `list_bookmarks` respects `limit`, and its `next_cursor` can be handed
/// back as `max_id` to walk to the next page in bookmark-creation order.
#[tokio::test]
async fn list_bookmarks_paginates_with_its_own_cursor() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();

    let mut bookmarked_status_ids = Vec::new();
    for _ in 0..3 {
        let status = insert_target_status(&app, app.runtime.ids.next_id()).await;
        let bookmark_id = app.runtime.ids.next_id();
        add_bookmark(
            &app.pool,
            bookmark_id,
            actor_id,
            status.id,
            app.runtime.clock.now(),
        )
        .await
        .expect("add_bookmark must succeed");
        bookmarked_status_ids.push(status.id);
    }
    // Bookmarked in order [0, 1, 2]; newest-bookmarked-first means the
    // first page (limit 2) should be [2, 1], leaving [0] for the next page.
    bookmarked_status_ids.reverse();

    let first_page = list_bookmarks(
        &app.pool,
        actor_id,
        PageParams {
            limit: Some(2),
            ..Default::default()
        },
    )
    .await
    .expect("list_bookmarks must succeed");
    assert_eq!(first_page.items.len(), 2);
    let first_ids: Vec<Id> = first_page.items.iter().map(|s| s.id).collect();
    assert_eq!(first_ids, bookmarked_status_ids[0..2]);

    let next_cursor = first_page
        .next_cursor
        .clone()
        .expect("a next_cursor must exist when more items remain");

    let second_page = list_bookmarks(
        &app.pool,
        actor_id,
        PageParams {
            max_id: Some(next_cursor),
            limit: Some(2),
            ..Default::default()
        },
    )
    .await
    .expect("list_bookmarks must succeed");
    let second_ids: Vec<Id> = second_page.items.iter().map(|s| s.id).collect();
    assert_eq!(second_ids, bookmarked_status_ids[2..3]);

    app.cleanup().await;
}

/// An empty bookmark list is a successful, empty `Page`, not an error.
#[tokio::test]
async fn list_bookmarks_returns_empty_page_when_nothing_bookmarked() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();

    let page = list_bookmarks(&app.pool, actor_id, PageParams::default())
        .await
        .expect("list_bookmarks must succeed even with nothing bookmarked");
    assert!(page.items.is_empty());
    assert!(page.next_cursor.is_none());
    assert!(page.prev_cursor.is_none());

    app.cleanup().await;
}

// -- pin ----------------------------------------------------------------

/// Requirement 12.1: pinning records a new pin and is reflected by
/// `exists_pin`.
#[tokio::test]
async fn set_pin_true_records_a_new_pin() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, actor_id).await;

    let created = set_pin(
        &app.pool,
        actor_id,
        status.id,
        true,
        app.runtime.clock.now(),
    )
    .await
    .expect("set_pin(true) must succeed");
    assert!(created);
    assert!(exists_pin(&app.pool, actor_id, status.id).await.unwrap());

    app.cleanup().await;
}

/// Requirement 12.1's own "(actor_id, status_id) 一意": pinning an
/// already-pinned status is a silent no-op, not an error or a second row.
#[tokio::test]
async fn set_pin_true_suppresses_duplicate_registration() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, actor_id).await;
    let now = app.runtime.clock.now();

    let first = set_pin(&app.pool, actor_id, status.id, true, now)
        .await
        .expect("first set_pin(true) must succeed");
    assert!(first);

    let second = set_pin(&app.pool, actor_id, status.id, true, now)
        .await
        .expect("second set_pin(true) must succeed, not error");
    assert!(!second, "duplicate pin must be reported as not-new");

    app.cleanup().await;
}

/// Requirement 12.2: unpinning removes the record; unpinning again is an
/// idempotent no-op.
#[tokio::test]
async fn set_pin_false_revokes_and_is_idempotent() {
    let app = spawn_test_app().await;
    let actor_id = app.runtime.ids.next_id();
    let status = insert_target_status(&app, actor_id).await;
    set_pin(
        &app.pool,
        actor_id,
        status.id,
        true,
        app.runtime.clock.now(),
    )
    .await
    .expect("set_pin(true) must succeed");

    let removed = set_pin(
        &app.pool,
        actor_id,
        status.id,
        false,
        app.runtime.clock.now(),
    )
    .await
    .expect("set_pin(false) must succeed");
    assert!(removed);
    assert!(!exists_pin(&app.pool, actor_id, status.id).await.unwrap());

    let removed_again = set_pin(
        &app.pool,
        actor_id,
        status.id,
        false,
        app.runtime.clock.now(),
    )
    .await
    .expect("unpinning an absent pin must succeed, not error");
    assert!(!removed_again);

    app.cleanup().await;
}

// -- reblog (read-only duplicate check) ------------------------------------

/// Requirement 9.3's read-only half: `find_reblog` locates the actor's own
/// existing boost `statuses` row (a row with `reblog_of_id` set to the
/// boosted post and `actor_id` set to the booster).
#[tokio::test]
async fn find_reblog_locates_the_actors_own_boost_row() {
    let app = spawn_test_app().await;
    let booster = app.runtime.ids.next_id();
    let original_author = app.runtime.ids.next_id();
    let original = insert_target_status(&app, original_author).await;

    assert!(
        find_reblog(&app.pool, booster, original.id)
            .await
            .expect("find_reblog must succeed")
            .is_none(),
        "no boost exists yet"
    );

    let boost = sample_status(&app, booster, Some(original.id));
    insert_status(&app.pool, &boost)
        .await
        .expect("insert_status of the boost row must succeed");

    let found = find_reblog(&app.pool, booster, original.id)
        .await
        .expect("find_reblog must succeed")
        .expect("the booster's own boost row must be found");
    assert_eq!(found.id, boost.id);
    assert_eq!(found.reblog_of_id, Some(original.id));

    app.cleanup().await;
}

/// `find_reblog` does not confuse one actor's boost with another's: a
/// different actor who has not boosted the post finds nothing.
#[tokio::test]
async fn find_reblog_is_scoped_to_the_requesting_actor() {
    let app = spawn_test_app().await;
    let booster = app.runtime.ids.next_id();
    let other_actor = app.runtime.ids.next_id();
    let original_author = app.runtime.ids.next_id();
    let original = insert_target_status(&app, original_author).await;

    let boost = sample_status(&app, booster, Some(original.id));
    insert_status(&app.pool, &boost)
        .await
        .expect("insert_status of the boost row must succeed");

    assert!(
        find_reblog(&app.pool, other_actor, original.id)
            .await
            .expect("find_reblog must succeed")
            .is_none(),
        "a different actor's own (non-existent) boost must not be found"
    );

    app.cleanup().await;
}
