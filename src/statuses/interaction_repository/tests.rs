//! Integration-style, DB-backed tests for `InteractionRepository`
//! (Requirements 9.1, 9.3, 9.4, 10.1, 10.3, 10.4, 11.1, 11.2, 11.3, 12.1,
//! 12.2), per task 2.2's observable completion condition: "同一 (actor,
//! status) への二重登録が抑止され、ブックマーク一覧がブックマーク固有カー
//! ソルで取得できる（リポジトリ単体テストがグリーン）".
//!
//! Mirrors `status_repository/tests.rs`'s established convention: reuses
//! `crate::test_harness::db_fixture::spawn_test_db` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext`, and inserts real `statuses`
//! rows via `status_repository::insert_status` (this module's own writes
//! carry a real FK to `statuses(id)`, unlike `statuses.actor_id`'s
//! logical-only reference, so a real target status row is required — actor
//! ids stay plain synthetic `Id`s, same as `status_repository/tests.rs`,
//! since nothing here depends on a real `local_actors` row existing).

use std::collections::HashSet;

use crate::api::pagination::PageParams;
use crate::domain::{Id, Visibility};
use crate::statuses::model::Status;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::db_fixture::{TestDb, spawn_test_db};

use super::{
    add_bookmark, add_favourite, bookmarked_status_ids, exists_bookmark, exists_favourite,
    exists_pin, favourited_status_ids, find_reblog, list_bookmarks, pinned_status_ids,
    reblogged_status_ids, remove_bookmark, remove_favourite, set_pin,
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
fn sample_status(db: &TestDb, actor_id: Id, reblog_of_id: Option<Id>) -> Status {
    let id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();
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
async fn insert_target_status(db: &TestDb, actor_id: Id) -> Status {
    let status = sample_status(db, actor_id, None);
    insert_status(&db.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    status
}

// -- favourite ------------------------------------------------------------

/// Requirement 10.1: a fresh favourite is recorded and reported as new.
#[tokio::test]
async fn add_favourite_records_a_new_favourite() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;

    let created = add_favourite(&db.pool, actor_id, status.id, db.runtime.clock.now())
        .await
        .expect("add_favourite must succeed");
    assert!(created, "first favourite of a status must be reported new");

    let exists = exists_favourite(&db.pool, actor_id, status.id)
        .await
        .expect("exists_favourite must succeed");
    assert!(exists);

    db.cleanup().await;
}

/// Requirement 10.4: a duplicate favourite by the same actor for the same
/// status is silently suppressed (not a second row, not an error).
#[tokio::test]
async fn add_favourite_suppresses_duplicate_registration() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;
    let now = db.runtime.clock.now();

    let first = add_favourite(&db.pool, actor_id, status.id, now)
        .await
        .expect("first add_favourite must succeed");
    assert!(first);

    let second = add_favourite(&db.pool, actor_id, status.id, now)
        .await
        .expect("second add_favourite must succeed, not error");
    assert!(
        !second,
        "duplicate favourite registration must be reported as not-new"
    );

    db.cleanup().await;
}

/// Requirement 10.3: unfavouriting removes the record and is reflected by
/// `exists_favourite`; unfavouriting again is an idempotent no-op.
#[tokio::test]
async fn remove_favourite_revokes_and_is_idempotent() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;
    add_favourite(&db.pool, actor_id, status.id, db.runtime.clock.now())
        .await
        .expect("add_favourite must succeed");

    let removed = remove_favourite(&db.pool, actor_id, status.id)
        .await
        .expect("remove_favourite must succeed");
    assert!(removed);
    assert!(
        !exists_favourite(&db.pool, actor_id, status.id)
            .await
            .expect("exists_favourite must succeed")
    );

    let removed_again = remove_favourite(&db.pool, actor_id, status.id)
        .await
        .expect("removing an absent favourite must succeed, not error");
    assert!(!removed_again, "second removal must report no change made");

    db.cleanup().await;
}

/// Two different actors favouriting the same status each get their own,
/// independent record (the unique key is `(actor_id, status_id)`, not
/// `status_id` alone).
#[tokio::test]
async fn favourites_are_independent_per_actor() {
    let db = spawn_test_db().await;
    let actor_a = db.runtime.ids.next_id();
    let actor_b = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;
    let now = db.runtime.clock.now();

    assert!(
        add_favourite(&db.pool, actor_a, status.id, now)
            .await
            .expect("add_favourite must succeed")
    );
    assert!(
        add_favourite(&db.pool, actor_b, status.id, now)
            .await
            .expect("add_favourite must succeed")
    );

    assert!(
        exists_favourite(&db.pool, actor_a, status.id)
            .await
            .unwrap()
    );
    assert!(
        exists_favourite(&db.pool, actor_b, status.id)
            .await
            .unwrap()
    );

    remove_favourite(&db.pool, actor_a, status.id)
        .await
        .expect("remove_favourite must succeed");
    assert!(
        !exists_favourite(&db.pool, actor_a, status.id)
            .await
            .unwrap()
    );
    assert!(
        exists_favourite(&db.pool, actor_b, status.id)
            .await
            .unwrap(),
        "actor_b's favourite must be unaffected by actor_a's removal"
    );

    db.cleanup().await;
}

// -- bookmark ---------------------------------------------------------------

/// Requirement 11.1: a fresh bookmark is recorded and reported as new.
#[tokio::test]
async fn add_bookmark_records_a_new_bookmark() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;
    let bookmark_id = db.runtime.ids.next_id();

    let created = add_bookmark(
        &db.pool,
        bookmark_id,
        actor_id,
        status.id,
        db.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");
    assert!(created);
    assert!(
        exists_bookmark(&db.pool, actor_id, status.id)
            .await
            .unwrap()
    );

    db.cleanup().await;
}

/// Requirement 11.1's own "(actor_id, status_id) 一意": a duplicate
/// bookmark registration is silently suppressed, not a second row or error.
#[tokio::test]
async fn add_bookmark_suppresses_duplicate_registration() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;
    let now = db.runtime.clock.now();

    let first_id = db.runtime.ids.next_id();
    let first = add_bookmark(&db.pool, first_id, actor_id, status.id, now)
        .await
        .expect("first add_bookmark must succeed");
    assert!(first);

    // A second attempt using a *different* caller-minted bookmark id must
    // still be suppressed by the (actor_id, status_id) unique constraint,
    // not merely by reusing the same id.
    let second_id = db.runtime.ids.next_id();
    let second = add_bookmark(&db.pool, second_id, actor_id, status.id, now)
        .await
        .expect("second add_bookmark must succeed, not error");
    assert!(
        !second,
        "duplicate bookmark registration must be reported as not-new"
    );

    db.cleanup().await;
}

/// Requirement 11.2: unbookmarking removes the record; repeating it is an
/// idempotent no-op.
#[tokio::test]
async fn remove_bookmark_revokes_and_is_idempotent() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;
    let bookmark_id = db.runtime.ids.next_id();
    add_bookmark(
        &db.pool,
        bookmark_id,
        actor_id,
        status.id,
        db.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");

    let removed = remove_bookmark(&db.pool, actor_id, status.id)
        .await
        .expect("remove_bookmark must succeed");
    assert!(removed);
    assert!(
        !exists_bookmark(&db.pool, actor_id, status.id)
            .await
            .unwrap()
    );

    let removed_again = remove_bookmark(&db.pool, actor_id, status.id)
        .await
        .expect("removing an absent bookmark must succeed, not error");
    assert!(!removed_again);

    db.cleanup().await;
}

/// Requirement 11.3: `list_bookmarks` returns bookmarked statuses ordered
/// by the bookmark's own creation order (newest bookmark first), which is
/// deliberately the *opposite* of the statuses' own creation order here —
/// proving the cursor is bookmark-specific, not the status id/created_at.
#[tokio::test]
async fn list_bookmarks_orders_by_bookmark_creation_not_status_creation() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();

    // Create statuses in order: older, newer (older has the smaller id /
    // earlier created_at).
    let older = insert_target_status(&db, db.runtime.ids.next_id()).await;
    let newer = insert_target_status(&db, db.runtime.ids.next_id()).await;

    // Bookmark them in the *reverse* order: `newer` first, `older` second —
    // so bookmark-creation order and status-creation order disagree.
    let bookmark_of_newer = db.runtime.ids.next_id();
    add_bookmark(
        &db.pool,
        bookmark_of_newer,
        actor_id,
        newer.id,
        db.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");

    let bookmark_of_older = db.runtime.ids.next_id();
    add_bookmark(
        &db.pool,
        bookmark_of_older,
        actor_id,
        older.id,
        db.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");

    let page = list_bookmarks(&db.pool, actor_id, PageParams::default())
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

    db.cleanup().await;
}

/// `list_bookmarks` only returns the requesting actor's own bookmarks.
#[tokio::test]
async fn list_bookmarks_is_scoped_to_the_requesting_actor() {
    let db = spawn_test_db().await;
    let actor_a = db.runtime.ids.next_id();
    let actor_b = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;

    let bookmark_id = db.runtime.ids.next_id();
    add_bookmark(
        &db.pool,
        bookmark_id,
        actor_a,
        status.id,
        db.runtime.clock.now(),
    )
    .await
    .expect("add_bookmark must succeed");

    let page_a = list_bookmarks(&db.pool, actor_a, PageParams::default())
        .await
        .expect("list_bookmarks must succeed");
    assert_eq!(page_a.items.len(), 1);

    let page_b = list_bookmarks(&db.pool, actor_b, PageParams::default())
        .await
        .expect("list_bookmarks must succeed");
    assert!(page_b.items.is_empty());

    db.cleanup().await;
}

/// `list_bookmarks` respects `limit`, and its `next_cursor` can be handed
/// back as `max_id` to walk to the next page in bookmark-creation order.
#[tokio::test]
async fn list_bookmarks_paginates_with_its_own_cursor() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();

    let mut bookmarked_status_ids = Vec::new();
    for _ in 0..3 {
        let status = insert_target_status(&db, db.runtime.ids.next_id()).await;
        let bookmark_id = db.runtime.ids.next_id();
        add_bookmark(
            &db.pool,
            bookmark_id,
            actor_id,
            status.id,
            db.runtime.clock.now(),
        )
        .await
        .expect("add_bookmark must succeed");
        bookmarked_status_ids.push(status.id);
    }
    // Bookmarked in order [0, 1, 2]; newest-bookmarked-first means the
    // first page (limit 2) should be [2, 1], leaving [0] for the next page.
    bookmarked_status_ids.reverse();

    let first_page = list_bookmarks(
        &db.pool,
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
        &db.pool,
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

    db.cleanup().await;
}

/// An empty bookmark list is a successful, empty `Page`, not an error.
#[tokio::test]
async fn list_bookmarks_returns_empty_page_when_nothing_bookmarked() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();

    let page = list_bookmarks(&db.pool, actor_id, PageParams::default())
        .await
        .expect("list_bookmarks must succeed even with nothing bookmarked");
    assert!(page.items.is_empty());
    assert!(page.next_cursor.is_none());
    assert!(page.prev_cursor.is_none());

    db.cleanup().await;
}

// -- pin ----------------------------------------------------------------

/// Requirement 12.1: pinning records a new pin and is reflected by
/// `exists_pin`.
#[tokio::test]
async fn set_pin_true_records_a_new_pin() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, actor_id).await;

    let created = set_pin(&db.pool, actor_id, status.id, true, db.runtime.clock.now())
        .await
        .expect("set_pin(true) must succeed");
    assert!(created);
    assert!(exists_pin(&db.pool, actor_id, status.id).await.unwrap());

    db.cleanup().await;
}

/// Requirement 12.1's own "(actor_id, status_id) 一意": pinning an
/// already-pinned status is a silent no-op, not an error or a second row.
#[tokio::test]
async fn set_pin_true_suppresses_duplicate_registration() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, actor_id).await;
    let now = db.runtime.clock.now();

    let first = set_pin(&db.pool, actor_id, status.id, true, now)
        .await
        .expect("first set_pin(true) must succeed");
    assert!(first);

    let second = set_pin(&db.pool, actor_id, status.id, true, now)
        .await
        .expect("second set_pin(true) must succeed, not error");
    assert!(!second, "duplicate pin must be reported as not-new");

    db.cleanup().await;
}

/// Requirement 12.2: unpinning removes the record; unpinning again is an
/// idempotent no-op.
#[tokio::test]
async fn set_pin_false_revokes_and_is_idempotent() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, actor_id).await;
    set_pin(&db.pool, actor_id, status.id, true, db.runtime.clock.now())
        .await
        .expect("set_pin(true) must succeed");

    let removed = set_pin(&db.pool, actor_id, status.id, false, db.runtime.clock.now())
        .await
        .expect("set_pin(false) must succeed");
    assert!(removed);
    assert!(!exists_pin(&db.pool, actor_id, status.id).await.unwrap());

    let removed_again = set_pin(&db.pool, actor_id, status.id, false, db.runtime.clock.now())
        .await
        .expect("unpinning an absent pin must succeed, not error");
    assert!(!removed_again);

    db.cleanup().await;
}

// -- reblog (read-only duplicate check) ------------------------------------

/// Requirement 9.3's read-only half: `find_reblog` locates the actor's own
/// existing boost `statuses` row (a row with `reblog_of_id` set to the
/// boosted post and `actor_id` set to the booster).
#[tokio::test]
async fn find_reblog_locates_the_actors_own_boost_row() {
    let db = spawn_test_db().await;
    let booster = db.runtime.ids.next_id();
    let original_author = db.runtime.ids.next_id();
    let original = insert_target_status(&db, original_author).await;

    assert!(
        find_reblog(&db.pool, booster, original.id)
            .await
            .expect("find_reblog must succeed")
            .is_none(),
        "no boost exists yet"
    );

    let boost = sample_status(&db, booster, Some(original.id));
    insert_status(&db.pool, &boost)
        .await
        .expect("insert_status of the boost row must succeed");

    let found = find_reblog(&db.pool, booster, original.id)
        .await
        .expect("find_reblog must succeed")
        .expect("the booster's own boost row must be found");
    assert_eq!(found.id, boost.id);
    assert_eq!(found.reblog_of_id, Some(original.id));

    db.cleanup().await;
}

/// `find_reblog` does not confuse one actor's boost with another's: a
/// different actor who has not boosted the post finds nothing.
#[tokio::test]
async fn find_reblog_is_scoped_to_the_requesting_actor() {
    let db = spawn_test_db().await;
    let booster = db.runtime.ids.next_id();
    let other_actor = db.runtime.ids.next_id();
    let original_author = db.runtime.ids.next_id();
    let original = insert_target_status(&db, original_author).await;

    let boost = sample_status(&db, booster, Some(original.id));
    insert_status(&db.pool, &boost)
        .await
        .expect("insert_status of the boost row must succeed");

    assert!(
        find_reblog(&db.pool, other_actor, original.id)
            .await
            .expect("find_reblog must succeed")
            .is_none(),
        "a different actor's own (non-existent) boost must not be found"
    );

    db.cleanup().await;
}

// -- batched interaction state ---------------------------------------------

/// The fixture shape shared by all four "singular N times == batched once"
/// comparisons. Every one of the four batched functions is
/// viewer-scoped, so each fixture deliberately contains a status that a
/// *different* actor interacted with: without it, a batched query that
/// dropped its `actor_id` predicate entirely would still agree with the
/// singular version on every id under test.
struct BatchFixture {
    viewer: Id,
    other_actor: Id,
    /// The status `viewer` interacts with — must end up in the batched set.
    mine: Status,
    /// The status only `other_actor` interacts with — must stay out of
    /// `viewer`'s batched set (the viewer-scoping probe).
    theirs: Status,
    /// A real status nobody interacted with.
    untouched: Status,
    /// An id with no `statuses` row at all.
    unknown: Id,
    /// Every id above, as handed to the batched call.
    ids: Vec<Id>,
}

async fn batch_fixture(db: &TestDb) -> BatchFixture {
    let viewer = db.runtime.ids.next_id();
    let other_actor = db.runtime.ids.next_id();
    let author = db.runtime.ids.next_id();
    let mine = insert_target_status(db, author).await;
    let theirs = insert_target_status(db, author).await;
    let untouched = insert_target_status(db, author).await;
    let unknown = Id::from_i64(i64::MAX - 17);

    BatchFixture {
        viewer,
        other_actor,
        ids: vec![mine.id, theirs.id, untouched.id, unknown],
        mine,
        theirs,
        untouched,
        unknown,
    }
}

/// Spells out the membership every batched result must have, so a comparison
/// against the singular version cannot pass vacuously by both sides
/// degrading the same way.
fn assert_batch_membership(batched: &HashSet<Id>, fixture: &BatchFixture, what: &str) {
    assert!(
        batched.contains(&fixture.mine.id),
        "{what}: the viewer's own interaction must be present"
    );
    assert!(
        !batched.contains(&fixture.theirs.id),
        "{what}: another actor's interaction must not leak into the viewer's set"
    );
    assert!(
        !batched.contains(&fixture.untouched.id),
        "{what}: an uninteracted status must be absent"
    );
    assert!(
        !batched.contains(&fixture.unknown),
        "{what}: an id with no status row must be absent"
    );
    assert_eq!(batched.len(), 1, "{what}: nothing else may be present");
}

/// N individual existence checks and one batched call must agree:
/// `favourited_status_ids` agrees with N calls to [`exists_favourite`],
/// including its viewer scoping.
#[tokio::test]
async fn favourited_status_ids_matches_calling_the_singular_version_per_status() {
    let db = spawn_test_db().await;
    let fixture = batch_fixture(&db).await;
    let now = db.runtime.clock.now();

    add_favourite(&db.pool, fixture.viewer, fixture.mine.id, now)
        .await
        .expect("add_favourite must succeed");
    add_favourite(&db.pool, fixture.other_actor, fixture.theirs.id, now)
        .await
        .expect("add_favourite must succeed");

    let mut per_call: HashSet<Id> = HashSet::new();
    for &status_id in &fixture.ids {
        if exists_favourite(&db.pool, fixture.viewer, status_id)
            .await
            .expect("exists_favourite must succeed")
        {
            per_call.insert(status_id);
        }
    }

    let batched = favourited_status_ids(&db.pool, fixture.viewer, &fixture.ids)
        .await
        .expect("favourited_status_ids must succeed");
    assert_eq!(
        batched, per_call,
        "one batched call must agree with N singular calls"
    );
    assert_batch_membership(&batched, &fixture, "favourited_status_ids");

    db.cleanup().await;
}

/// The same agreement, for bookmarks: `bookmarked_status_ids` agrees
/// with N calls to [`exists_bookmark`], including its viewer scoping.
#[tokio::test]
async fn bookmarked_status_ids_matches_calling_the_singular_version_per_status() {
    let db = spawn_test_db().await;
    let fixture = batch_fixture(&db).await;
    let now = db.runtime.clock.now();

    let mine_bookmark = db.runtime.ids.next_id();
    add_bookmark(
        &db.pool,
        mine_bookmark,
        fixture.viewer,
        fixture.mine.id,
        now,
    )
    .await
    .expect("add_bookmark must succeed");
    let theirs_bookmark = db.runtime.ids.next_id();
    add_bookmark(
        &db.pool,
        theirs_bookmark,
        fixture.other_actor,
        fixture.theirs.id,
        now,
    )
    .await
    .expect("add_bookmark must succeed");

    let mut per_call: HashSet<Id> = HashSet::new();
    for &status_id in &fixture.ids {
        if exists_bookmark(&db.pool, fixture.viewer, status_id)
            .await
            .expect("exists_bookmark must succeed")
        {
            per_call.insert(status_id);
        }
    }

    let batched = bookmarked_status_ids(&db.pool, fixture.viewer, &fixture.ids)
        .await
        .expect("bookmarked_status_ids must succeed");
    assert_eq!(
        batched, per_call,
        "one batched call must agree with N singular calls"
    );
    assert_batch_membership(&batched, &fixture, "bookmarked_status_ids");

    db.cleanup().await;
}

/// The same agreement, for pins: `pinned_status_ids` agrees with N
/// calls to [`exists_pin`]. A pin is conventionally an author's pin of their
/// own post, but the `pins` table is keyed `(actor_id, status_id)` exactly
/// like `favourites`/`bookmarks`, and [`exists_pin`] scopes by `actor_id`
/// with no ownership check at this layer — so the batched form is scoped the
/// same way, and a different actor's pin must not leak in.
#[tokio::test]
async fn pinned_status_ids_matches_calling_the_singular_version_per_status() {
    let db = spawn_test_db().await;
    let fixture = batch_fixture(&db).await;
    let now = db.runtime.clock.now();

    set_pin(&db.pool, fixture.viewer, fixture.mine.id, true, now)
        .await
        .expect("set_pin must succeed");
    set_pin(&db.pool, fixture.other_actor, fixture.theirs.id, true, now)
        .await
        .expect("set_pin must succeed");

    let mut per_call: HashSet<Id> = HashSet::new();
    for &status_id in &fixture.ids {
        if exists_pin(&db.pool, fixture.viewer, status_id)
            .await
            .expect("exists_pin must succeed")
        {
            per_call.insert(status_id);
        }
    }

    let batched = pinned_status_ids(&db.pool, fixture.viewer, &fixture.ids)
        .await
        .expect("pinned_status_ids must succeed");
    assert_eq!(
        batched, per_call,
        "one batched call must agree with N singular calls"
    );
    assert_batch_membership(&batched, &fixture, "pinned_status_ids");

    db.cleanup().await;
}

/// The same agreement, for boosts: `reblogged_status_ids` agrees with
/// N calls to [`find_reblog`]. The reblog relation lives in `statuses` itself
/// (a boost is a row with `reblog_of_id` set), so the batched form keys its
/// result on `reblog_of_id` — the *boosted* status's id, which is what the
/// caller asked about — not on the boost row's own id.
#[tokio::test]
async fn reblogged_status_ids_matches_calling_the_singular_version_per_status() {
    let db = spawn_test_db().await;
    let fixture = batch_fixture(&db).await;

    let my_boost = sample_status(&db, fixture.viewer, Some(fixture.mine.id));
    insert_status(&db.pool, &my_boost)
        .await
        .expect("insert_status of the boost row must succeed");
    let their_boost = sample_status(&db, fixture.other_actor, Some(fixture.theirs.id));
    insert_status(&db.pool, &their_boost)
        .await
        .expect("insert_status of the boost row must succeed");

    let mut per_call: HashSet<Id> = HashSet::new();
    for &status_id in &fixture.ids {
        if find_reblog(&db.pool, fixture.viewer, status_id)
            .await
            .expect("find_reblog must succeed")
            .is_some()
        {
            per_call.insert(status_id);
        }
    }

    let batched = reblogged_status_ids(&db.pool, fixture.viewer, &fixture.ids)
        .await
        .expect("reblogged_status_ids must succeed");
    assert_eq!(
        batched, per_call,
        "one batched call must agree with N singular calls"
    );
    assert_batch_membership(&batched, &fixture, "reblogged_status_ids");
    assert!(
        !batched.contains(&my_boost.id),
        "the result must key on the boosted status's id, not the boost row's own id"
    );

    db.cleanup().await;
}

/// For all four functions at once: an empty `status_ids` returns an empty
/// set *without issuing a query*. Closing the
/// pool first is what makes that second half observable — every statement
/// against a closed `PgPool` fails with `sqlx::Error::PoolClosed`, so an `Ok`
/// here can only mean the function short-circuited before touching the
/// database. All four share one closed pool rather than one fixture each,
/// since the assertion is identical and fixtures are the scarce resource
/// (each holds its own connection pool).
#[tokio::test]
async fn batched_interaction_state_returns_empty_for_an_empty_slice_without_querying() {
    let db = spawn_test_db().await;
    let viewer = db.runtime.ids.next_id();
    db.pool.close().await;

    assert!(
        favourited_status_ids(&db.pool, viewer, &[])
            .await
            .expect("an empty slice must succeed even against a closed pool")
            .is_empty()
    );
    assert!(
        bookmarked_status_ids(&db.pool, viewer, &[])
            .await
            .expect("an empty slice must succeed even against a closed pool")
            .is_empty()
    );
    assert!(
        pinned_status_ids(&db.pool, viewer, &[])
            .await
            .expect("an empty slice must succeed even against a closed pool")
            .is_empty()
    );
    assert!(
        reblogged_status_ids(&db.pool, viewer, &[])
            .await
            .expect("an empty slice must succeed even against a closed pool")
            .is_empty()
    );

    db.cleanup().await;
}

// -- executor genericity ---------------------------------------------------

/// [`add_favourite`] accepts an open transaction, and rolling that
/// transaction back leaves no `favourites` row behind — the property
/// `InteractionService::favourite` needs so a favourite row and its counter
/// can never diverge.
#[tokio::test]
async fn add_favourite_accepts_a_transaction_and_rolls_back() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;

    let mut tx = db.pool.begin().await.expect("begin must succeed");
    let created = add_favourite(&mut *tx, actor_id, status.id, db.runtime.clock.now())
        .await
        .expect("add_favourite must succeed against a transaction");
    assert!(created, "first favourite of a status must be reported new");
    tx.rollback().await.expect("rollback must succeed");

    let exists = exists_favourite(&db.pool, actor_id, status.id)
        .await
        .expect("exists_favourite must succeed");
    assert!(!exists, "a rolled-back add_favourite must leave no row");

    db.cleanup().await;
}

/// The deletion half: [`remove_favourite`] accepts an open transaction, and
/// rolling that transaction back restores the row it deleted.
#[tokio::test]
async fn remove_favourite_accepts_a_transaction_and_rolls_back() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;
    add_favourite(&db.pool, actor_id, status.id, db.runtime.clock.now())
        .await
        .expect("add_favourite must succeed");

    let mut tx = db.pool.begin().await.expect("begin must succeed");
    let removed = remove_favourite(&mut *tx, actor_id, status.id)
        .await
        .expect("remove_favourite must succeed against a transaction");
    assert!(removed, "an existing favourite must be reported deleted");
    tx.rollback().await.expect("rollback must succeed");

    let exists = exists_favourite(&db.pool, actor_id, status.id)
        .await
        .expect("exists_favourite must succeed");
    assert!(
        exists,
        "a rolled-back remove_favourite must restore the row"
    );

    db.cleanup().await;
}

/// The commit half of the two tests above: driven through a committed
/// transaction, add/remove persist exactly what the pool-driven path does.
#[tokio::test]
async fn favourite_writes_committed_through_a_transaction_persist_normally() {
    let db = spawn_test_db().await;
    let actor_id = db.runtime.ids.next_id();
    let status = insert_target_status(&db, db.runtime.ids.next_id()).await;

    let mut tx = db.pool.begin().await.expect("begin must succeed");
    add_favourite(&mut *tx, actor_id, status.id, db.runtime.clock.now())
        .await
        .expect("add_favourite must succeed against a transaction");
    tx.commit().await.expect("commit must succeed");
    assert!(
        exists_favourite(&db.pool, actor_id, status.id)
            .await
            .expect("exists_favourite must succeed")
    );

    let mut tx = db.pool.begin().await.expect("begin must succeed");
    remove_favourite(&mut *tx, actor_id, status.id)
        .await
        .expect("remove_favourite must succeed against a transaction");
    tx.commit().await.expect("commit must succeed");
    assert!(
        !exists_favourite(&db.pool, actor_id, status.id)
            .await
            .expect("exists_favourite must succeed")
    );

    db.cleanup().await;
}
