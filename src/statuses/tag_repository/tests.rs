//! Integration-style, DB-backed tests for `TagRepository` (Requirement 3.6),
//! per task 2.1's observable completion condition: "タグ関連付けが永続化さ
//! れ read-only 照会できる".
//!
//! Mirrors `src/statuses/status_repository/tests.rs`'s established
//! convention: `crate::test_harness::spawn_test_app` for an isolated,
//! already-migrated schema and a deterministic `RuntimeContext`. Unlike
//! `statuses.actor_id`, `status_tags.status_id`/`tag_id` carry *real*
//! physical FKs (`ON DELETE CASCADE`, `migrations/0007_statuses.sql`), so
//! these tests insert genuine `statuses` rows via
//! `crate::statuses::status_repository::insert_status` before associating
//! tags with them.

use super::{associate_tag, find_tag_by_name, status_ids_for_tag, tags_for_status, upsert_tag};
use crate::domain::Visibility;
use crate::statuses::model::{Status, Tag};
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

fn sample_status(app: &TestApp) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    Status {
        id,
        actor_id: app.runtime.ids.next_id(),
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

async fn insert_test_status(app: &TestApp) -> Status {
    let status = sample_status(app);
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");
    status
}

fn sample_tag(app: &TestApp, name: &str) -> Tag {
    Tag {
        id: app.runtime.ids.next_id(),
        name: name.to_string(),
        created_at: app.runtime.clock.now(),
    }
}

// -- upsert_tag / find_tag_by_name ---------------------------------------

/// A fresh tag name is inserted and immediately findable.
#[tokio::test]
async fn upsert_tag_creates_a_new_row_findable_by_name() {
    let app = spawn_test_app().await;
    let tag = sample_tag(&app, "rustlang");

    let created = upsert_tag(&app.pool, &tag)
        .await
        .expect("upsert_tag must succeed");
    assert_eq!(created, tag);

    let found = find_tag_by_name(&app.pool, "rustlang")
        .await
        .expect("find_tag_by_name must succeed")
        .expect("the just-created tag must be found");
    assert_eq!(found, tag);

    app.cleanup().await;
}

/// Requirement 3.6 / `tags_name_unique`: upserting the same name twice never
/// creates a second row — the second call returns the *original* row's id,
/// discarding the second call's own id/created_at.
#[tokio::test]
async fn upsert_tag_is_idempotent_on_name_and_keeps_the_original_id() {
    let app = spawn_test_app().await;
    let first = sample_tag(&app, "mastodon");
    let created_first = upsert_tag(&app.pool, &first)
        .await
        .expect("first upsert_tag must succeed");
    assert_eq!(created_first.id, first.id);

    // A second, later-minted Tag value for the exact same name.
    let second = sample_tag(&app, "mastodon");
    assert_ne!(
        second.id, first.id,
        "sanity: the two Tag values must carry different minted ids"
    );
    let created_second = upsert_tag(&app.pool, &second)
        .await
        .expect("second upsert_tag must succeed");

    assert_eq!(
        created_second.id, first.id,
        "the second upsert must return the original row's id, not mint a new one"
    );
    assert_eq!(created_second.created_at, first.created_at);

    app.cleanup().await;
}

/// find_tag_by_name returns None (not an error) for a name nothing was ever
/// created under.
#[tokio::test]
async fn find_tag_by_name_returns_none_for_an_unknown_name() {
    let app = spawn_test_app().await;
    let found = find_tag_by_name(&app.pool, "neverexisted")
        .await
        .expect("find_tag_by_name must succeed even when nothing matches");
    assert!(found.is_none());
    app.cleanup().await;
}

// -- associate_tag / tags_for_status / status_ids_for_tag -----------------

/// Requirement 3.6's observable completion: a tag associated with a status
/// is persisted and queryable read-only from both directions.
#[tokio::test]
async fn associate_tag_is_queryable_from_both_status_and_tag_directions() {
    let app = spawn_test_app().await;
    let status = insert_test_status(&app).await;
    let tag = sample_tag(&app, "rustlang");
    upsert_tag(&app.pool, &tag)
        .await
        .expect("upsert_tag must succeed");

    associate_tag(&app.pool, status.id, tag.id)
        .await
        .expect("associate_tag must succeed");

    let tags = tags_for_status(&app.pool, status.id)
        .await
        .expect("tags_for_status must succeed");
    assert_eq!(tags, vec![tag.clone()]);

    let status_ids = status_ids_for_tag(&app.pool, tag.id)
        .await
        .expect("status_ids_for_tag must succeed");
    assert_eq!(status_ids, vec![status.id]);

    app.cleanup().await;
}

/// Associating the same (status, tag) pair twice is a silent no-op — the
/// `status_tags` PK's dedup guarantee, not a duplicate association.
#[tokio::test]
async fn associate_tag_is_idempotent_for_the_same_pair() {
    let app = spawn_test_app().await;
    let status = insert_test_status(&app).await;
    let tag = sample_tag(&app, "idempotent");
    upsert_tag(&app.pool, &tag)
        .await
        .expect("upsert_tag must succeed");

    associate_tag(&app.pool, status.id, tag.id)
        .await
        .expect("first associate_tag must succeed");
    associate_tag(&app.pool, status.id, tag.id)
        .await
        .expect("second associate_tag (duplicate) must succeed as a no-op");

    let tags = tags_for_status(&app.pool, status.id).await.unwrap();
    assert_eq!(tags.len(), 1, "no duplicate association must be created");

    app.cleanup().await;
}

/// tags_for_status only returns tags belonging to the queried status, not
/// another status' tags.
#[tokio::test]
async fn tags_for_status_returns_only_that_statuss_tags() {
    let app = spawn_test_app().await;
    let status_a = insert_test_status(&app).await;
    let status_b = insert_test_status(&app).await;
    let tag_a = sample_tag(&app, "taga");
    let tag_b = sample_tag(&app, "tagb");
    upsert_tag(&app.pool, &tag_a).await.unwrap();
    upsert_tag(&app.pool, &tag_b).await.unwrap();
    associate_tag(&app.pool, status_a.id, tag_a.id)
        .await
        .unwrap();
    associate_tag(&app.pool, status_b.id, tag_b.id)
        .await
        .unwrap();

    let tags_a = tags_for_status(&app.pool, status_a.id).await.unwrap();
    assert_eq!(tags_a, vec![tag_a]);

    let tags_b = tags_for_status(&app.pool, status_b.id).await.unwrap();
    assert_eq!(tags_b, vec![tag_b]);

    app.cleanup().await;
}

/// status_ids_for_tag only returns statuses associated with the queried tag,
/// newest first.
#[tokio::test]
async fn status_ids_for_tag_returns_only_that_tags_statuses_newest_first() {
    let app = spawn_test_app().await;
    let tag = sample_tag(&app, "shared");
    upsert_tag(&app.pool, &tag).await.unwrap();
    let other_tag = sample_tag(&app, "unrelated");
    upsert_tag(&app.pool, &other_tag).await.unwrap();

    let first = insert_test_status(&app).await;
    let second = insert_test_status(&app).await;
    let unrelated = insert_test_status(&app).await;
    associate_tag(&app.pool, first.id, tag.id).await.unwrap();
    associate_tag(&app.pool, second.id, tag.id).await.unwrap();
    associate_tag(&app.pool, unrelated.id, other_tag.id)
        .await
        .unwrap();

    let ids = status_ids_for_tag(&app.pool, tag.id).await.unwrap();
    assert_eq!(ids, vec![second.id, first.id], "newest (largest id) first");

    app.cleanup().await;
}

/// status_ids_for_tag returns an empty Vec (not an error) for a tag with no
/// associated statuses.
#[tokio::test]
async fn status_ids_for_tag_returns_empty_for_an_unused_tag() {
    let app = spawn_test_app().await;
    let tag = sample_tag(&app, "unused");
    upsert_tag(&app.pool, &tag).await.unwrap();

    let ids = status_ids_for_tag(&app.pool, tag.id).await.unwrap();
    assert!(ids.is_empty());

    app.cleanup().await;
}

/// Deleting a status cascade-removes its `status_tags` associations via the
/// physical FK (`ON DELETE CASCADE`), so tags_for_status returns empty
/// afterward — proving this repository's read boundary reflects the same
/// physical cascade `status_repository`'s own doc comment documents relying
/// on for every other same-spec relation.
#[tokio::test]
async fn deleting_a_status_cascades_its_tag_associations() {
    let app = spawn_test_app().await;
    let status = insert_test_status(&app).await;
    let tag = sample_tag(&app, "cascaded");
    upsert_tag(&app.pool, &tag).await.unwrap();
    associate_tag(&app.pool, status.id, tag.id).await.unwrap();

    crate::statuses::status_repository::delete_status(&app.pool, status.id)
        .await
        .expect("delete_status must succeed");

    let tags = tags_for_status(&app.pool, status.id).await.unwrap();
    assert!(tags.is_empty());
    let status_ids = status_ids_for_tag(&app.pool, tag.id).await.unwrap();
    assert!(status_ids.is_empty());

    app.cleanup().await;
}
