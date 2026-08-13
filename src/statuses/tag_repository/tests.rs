//! Integration-style, DB-backed tests for `TagRepository` (Requirement 3.6),
//! per task 2.1's observable completion condition: "タグ関連付けが永続化さ
//! れ read-only 照会できる".
//!
//! Mirrors `src/statuses/status_repository/tests.rs`'s established
//! convention: `crate::test_harness::db_fixture::spawn_test_db` for an isolated,
//! already-migrated schema and a deterministic `RuntimeContext`. Unlike
//! `statuses.actor_id`, `status_tags.status_id`/`tag_id` carry *real*
//! physical FKs (`ON DELETE CASCADE`, `migrations/0007_statuses.sql`), so
//! these tests insert genuine `statuses` rows via
//! `crate::statuses::status_repository::insert_status` before associating
//! tags with them.

use std::collections::HashMap;

use super::{
    associate_tag, find_tag_by_name, status_ids_for_tag, tags_for_status, tags_for_statuses,
    upsert_tag,
};
use crate::domain::Id;
use crate::domain::Visibility;
use crate::statuses::model::{Status, Tag};
use crate::statuses::status_repository::insert_status;
use crate::test_harness::db_fixture::{TestDb, spawn_test_db};

fn sample_status(db: &TestDb) -> Status {
    let id = db.runtime.ids.next_id();
    let now = db.runtime.clock.now();
    Status {
        id,
        actor_id: db.runtime.ids.next_id(),
        uri: format!("https://example.test/statuses/{}", id.as_i64()),
        url: None,
        content: "hello #rustlang".to_string(),
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
        created_at: now,
        edited_at: None,
    }
}

async fn insert_test_status(db: &TestDb) -> Status {
    let status = sample_status(db);
    insert_status(&db.pool, &status)
        .await
        .expect("insert_status must succeed");
    status
}

fn sample_tag(db: &TestDb, name: &str) -> Tag {
    Tag {
        id: db.runtime.ids.next_id(),
        name: name.to_string(),
        created_at: db.runtime.clock.now(),
    }
}

// -- upsert_tag / find_tag_by_name ---------------------------------------

/// A fresh tag name is inserted and immediately findable.
#[tokio::test]
async fn upsert_tag_creates_a_new_row_findable_by_name() {
    let db = spawn_test_db().await;
    let tag = sample_tag(&db, "rustlang");

    let created = upsert_tag(&db.pool, &tag)
        .await
        .expect("upsert_tag must succeed");
    assert_eq!(created, tag);

    let found = find_tag_by_name(&db.pool, "rustlang")
        .await
        .expect("find_tag_by_name must succeed")
        .expect("the just-created tag must be found");
    assert_eq!(found, tag);

    db.cleanup().await;
}

/// Requirement 3.6 / `tags_name_unique`: upserting the same name twice never
/// creates a second row — the second call returns the *original* row's id,
/// discarding the second call's own id/created_at.
#[tokio::test]
async fn upsert_tag_is_idempotent_on_name_and_keeps_the_original_id() {
    let db = spawn_test_db().await;
    let first = sample_tag(&db, "mastodon");
    let created_first = upsert_tag(&db.pool, &first)
        .await
        .expect("first upsert_tag must succeed");
    assert_eq!(created_first.id, first.id);

    // A second, later-minted Tag value for the exact same name.
    let second = sample_tag(&db, "mastodon");
    assert_ne!(
        second.id, first.id,
        "sanity: the two Tag values must carry different minted ids"
    );
    let created_second = upsert_tag(&db.pool, &second)
        .await
        .expect("second upsert_tag must succeed");

    assert_eq!(
        created_second.id, first.id,
        "the second upsert must return the original row's id, not mint a new one"
    );
    assert_eq!(created_second.created_at, first.created_at);

    db.cleanup().await;
}

/// find_tag_by_name returns None (not an error) for a name nothing was ever
/// created under.
#[tokio::test]
async fn find_tag_by_name_returns_none_for_an_unknown_name() {
    let db = spawn_test_db().await;
    let found = find_tag_by_name(&db.pool, "neverexisted")
        .await
        .expect("find_tag_by_name must succeed even when nothing matches");
    assert!(found.is_none());
    db.cleanup().await;
}

// -- associate_tag / tags_for_status / status_ids_for_tag -----------------

/// Requirement 3.6's observable completion: a tag associated with a status
/// is persisted and queryable read-only from both directions.
#[tokio::test]
async fn associate_tag_is_queryable_from_both_status_and_tag_directions() {
    let db = spawn_test_db().await;
    let status = insert_test_status(&db).await;
    let tag = sample_tag(&db, "rustlang");
    upsert_tag(&db.pool, &tag)
        .await
        .expect("upsert_tag must succeed");

    associate_tag(&db.pool, status.id, tag.id)
        .await
        .expect("associate_tag must succeed");

    let tags = tags_for_status(&db.pool, status.id)
        .await
        .expect("tags_for_status must succeed");
    assert_eq!(tags, vec![tag.clone()]);

    let status_ids = status_ids_for_tag(&db.pool, tag.id)
        .await
        .expect("status_ids_for_tag must succeed");
    assert_eq!(status_ids, vec![status.id]);

    db.cleanup().await;
}

/// Associating the same (status, tag) pair twice is a silent no-op — the
/// `status_tags` PK's dedup guarantee, not a duplicate association.
#[tokio::test]
async fn associate_tag_is_idempotent_for_the_same_pair() {
    let db = spawn_test_db().await;
    let status = insert_test_status(&db).await;
    let tag = sample_tag(&db, "idempotent");
    upsert_tag(&db.pool, &tag)
        .await
        .expect("upsert_tag must succeed");

    associate_tag(&db.pool, status.id, tag.id)
        .await
        .expect("first associate_tag must succeed");
    associate_tag(&db.pool, status.id, tag.id)
        .await
        .expect("second associate_tag (duplicate) must succeed as a no-op");

    let tags = tags_for_status(&db.pool, status.id).await.unwrap();
    assert_eq!(tags.len(), 1, "no duplicate association must be created");

    db.cleanup().await;
}

/// tags_for_status only returns tags belonging to the queried status, not
/// another status' tags.
#[tokio::test]
async fn tags_for_status_returns_only_that_statuss_tags() {
    let db = spawn_test_db().await;
    let status_a = insert_test_status(&db).await;
    let status_b = insert_test_status(&db).await;
    let tag_a = sample_tag(&db, "taga");
    let tag_b = sample_tag(&db, "tagb");
    upsert_tag(&db.pool, &tag_a).await.unwrap();
    upsert_tag(&db.pool, &tag_b).await.unwrap();
    associate_tag(&db.pool, status_a.id, tag_a.id)
        .await
        .unwrap();
    associate_tag(&db.pool, status_b.id, tag_b.id)
        .await
        .unwrap();

    let tags_a = tags_for_status(&db.pool, status_a.id).await.unwrap();
    assert_eq!(tags_a, vec![tag_a]);

    let tags_b = tags_for_status(&db.pool, status_b.id).await.unwrap();
    assert_eq!(tags_b, vec![tag_b]);

    db.cleanup().await;
}

/// status_ids_for_tag only returns statuses associated with the queried tag,
/// newest first.
#[tokio::test]
async fn status_ids_for_tag_returns_only_that_tags_statuses_newest_first() {
    let db = spawn_test_db().await;
    let tag = sample_tag(&db, "shared");
    upsert_tag(&db.pool, &tag).await.unwrap();
    let other_tag = sample_tag(&db, "unrelated");
    upsert_tag(&db.pool, &other_tag).await.unwrap();

    let first = insert_test_status(&db).await;
    let second = insert_test_status(&db).await;
    let unrelated = insert_test_status(&db).await;
    associate_tag(&db.pool, first.id, tag.id).await.unwrap();
    associate_tag(&db.pool, second.id, tag.id).await.unwrap();
    associate_tag(&db.pool, unrelated.id, other_tag.id)
        .await
        .unwrap();

    let ids = status_ids_for_tag(&db.pool, tag.id).await.unwrap();
    assert_eq!(ids, vec![second.id, first.id], "newest (largest id) first");

    db.cleanup().await;
}

/// status_ids_for_tag returns an empty Vec (not an error) for a tag with no
/// associated statuses.
#[tokio::test]
async fn status_ids_for_tag_returns_empty_for_an_unused_tag() {
    let db = spawn_test_db().await;
    let tag = sample_tag(&db, "unused");
    upsert_tag(&db.pool, &tag).await.unwrap();

    let ids = status_ids_for_tag(&db.pool, tag.id).await.unwrap();
    assert!(ids.is_empty());

    db.cleanup().await;
}

/// Deleting a status cascade-removes its `status_tags` associations via the
/// physical FK (`ON DELETE CASCADE`), so tags_for_status returns empty
/// afterward — proving this repository's read boundary reflects the same
/// physical cascade `status_repository`'s own doc comment documents relying
/// on for every other same-spec relation.
#[tokio::test]
async fn deleting_a_status_cascades_its_tag_associations() {
    let db = spawn_test_db().await;
    let status = insert_test_status(&db).await;
    let tag = sample_tag(&db, "cascaded");
    upsert_tag(&db.pool, &tag).await.unwrap();
    associate_tag(&db.pool, status.id, tag.id).await.unwrap();

    crate::statuses::status_repository::delete_status(&db.pool, status.id)
        .await
        .expect("delete_status must succeed");

    let tags = tags_for_status(&db.pool, status.id).await.unwrap();
    assert!(tags.is_empty());
    let status_ids = status_ids_for_tag(&db.pool, tag.id).await.unwrap();
    assert!(status_ids.is_empty());

    db.cleanup().await;
}

// -- tags_for_statuses -----------------------------------------------------

/// N singular calls and one batched call must agree: `tags_for_statuses`
/// returns, for every id, exactly what
/// `tags_for_status` returns for that same id on its own — same `WHERE`
/// scoping, same `ORDER BY tags.id` within each status, same treatment of a
/// status carrying no tags at all.
#[tokio::test]
async fn tags_for_statuses_matches_calling_the_singular_version_per_status() {
    let db = spawn_test_db().await;
    let with_many = insert_test_status(&db).await;
    let with_one = insert_test_status(&db).await;
    let without_tags = insert_test_status(&db).await;

    // Minted in ascending id order, but *named* in descending alphabetical
    // order relative to that id order, so neither "no `ORDER BY` at all"
    // (which would surface the association order below) nor an
    // `ORDER BY tags.name` could coincidentally agree with the singular
    // version's `ORDER BY tags.id`.
    let low = sample_tag(&db, "zulu");
    let mid = sample_tag(&db, "mike");
    let high = sample_tag(&db, "alpha");
    assert!(
        low.id < mid.id && mid.id < high.id,
        "sanity: the deterministic IdGenerator must mint ascending ids"
    );
    for tag in [&low, &mid, &high] {
        upsert_tag(&db.pool, tag).await.unwrap();
    }

    // Deliberately associated in *descending* tag-id order.
    for tag in [&high, &mid, &low] {
        associate_tag(&db.pool, with_many.id, tag.id).await.unwrap();
    }
    associate_tag(&db.pool, with_one.id, mid.id).await.unwrap();

    // A status id no tag was ever associated with *and* that no `statuses`
    // row exists for: the plural version must handle it exactly like the
    // singular one does (no entry, not an error).
    let unknown = Id::from_i64(i64::MAX - 17);
    let ids = [with_many.id, with_one.id, without_tags.id, unknown];

    let mut per_call: HashMap<Id, Vec<Tag>> = HashMap::new();
    for &status_id in &ids {
        let singular = tags_for_status(&db.pool, status_id)
            .await
            .expect("tags_for_status must succeed");
        if !singular.is_empty() {
            per_call.insert(status_id, singular);
        }
    }

    let batched = tags_for_statuses(&db.pool, &ids)
        .await
        .expect("tags_for_statuses must succeed");
    assert_eq!(
        batched, per_call,
        "one batched call must agree with N singular calls"
    );

    // Spelled out too, so the comparison above cannot pass vacuously if both
    // sides were to degrade the same way.
    assert_eq!(
        batched.get(&with_many.id),
        Some(&vec![low.clone(), mid.clone(), high]),
        "tags.id order, not association or name order, must be preserved"
    );
    assert_eq!(batched.get(&with_one.id), Some(&vec![mid]));
    assert_eq!(batched.get(&without_tags.id), None);
    assert_eq!(batched.get(&unknown), None);

    db.cleanup().await;
}

/// An empty `status_ids` returns an empty map *without issuing a query*.
/// Closing the pool first is what makes that second half observable — every
/// statement against a closed `PgPool` fails with `sqlx::Error::PoolClosed`,
/// so an `Ok` here can only mean the function short-circuited before touching
/// the database.
#[tokio::test]
async fn tags_for_statuses_returns_empty_for_an_empty_slice_without_querying() {
    let db = spawn_test_db().await;
    db.pool.close().await;

    let batched = tags_for_statuses(&db.pool, &[])
        .await
        .expect("an empty slice must succeed even against a closed pool");
    assert!(batched.is_empty());

    db.cleanup().await;
}

// -- executor genericity ---------------------------------------------------

/// [`upsert_tag`] and [`associate_tag`] accept an open transaction, and
/// rolling that transaction back leaves neither the `tags` row nor the
/// `status_tags` association behind — the property
/// `StatusService::create_status` needs so a failed post creation cannot
/// strand tag rows.
#[tokio::test]
async fn tag_writes_accept_a_transaction_and_roll_back_together() {
    let db = spawn_test_db().await;
    let status = insert_test_status(&db).await;
    let tag = sample_tag(&db, "rollbacktag");

    let mut tx = db.pool.begin().await.expect("begin must succeed");
    let created = upsert_tag(&mut *tx, &tag)
        .await
        .expect("upsert_tag must succeed against a transaction");
    associate_tag(&mut *tx, status.id, created.id)
        .await
        .expect("associate_tag must succeed against a transaction");
    tx.rollback().await.expect("rollback must succeed");

    let found = find_tag_by_name(&db.pool, "rollbacktag")
        .await
        .expect("find_tag_by_name must succeed");
    assert!(
        found.is_none(),
        "a rolled-back upsert_tag must leave no row"
    );

    let associated = tags_for_status(&db.pool, status.id)
        .await
        .expect("tags_for_status must succeed");
    assert!(
        associated.is_empty(),
        "a rolled-back associate_tag must leave no status_tags row"
    );

    db.cleanup().await;
}

/// The commit half of
/// [`tag_writes_accept_a_transaction_and_roll_back_together`]: committed
/// through a transaction, both functions persist exactly what the
/// pool-driven path persists.
#[tokio::test]
async fn tag_writes_committed_through_a_transaction_persist_normally() {
    let db = spawn_test_db().await;
    let status = insert_test_status(&db).await;
    let tag = sample_tag(&db, "committag");

    let mut tx = db.pool.begin().await.expect("begin must succeed");
    let created = upsert_tag(&mut *tx, &tag)
        .await
        .expect("upsert_tag must succeed against a transaction");
    associate_tag(&mut *tx, status.id, created.id)
        .await
        .expect("associate_tag must succeed against a transaction");
    tx.commit().await.expect("commit must succeed");

    assert_eq!(created, tag);
    let associated = tags_for_status(&db.pool, status.id)
        .await
        .expect("tags_for_status must succeed");
    assert_eq!(associated, vec![tag]);

    db.cleanup().await;
}
