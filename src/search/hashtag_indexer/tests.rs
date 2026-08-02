//! Integration-style, DB-backed tests for `HashtagIndexer::catch_up_from_watermark`
//! (Requirement 5.3), per task 2.2's own observable completion condition:
//! "既存投稿のハッシュタグがインデックスへ導出され、再実行で重複生成されず
//! watermark 以降の新規投稿のみが処理され、upstream テーブルを変更しない統合
//! テストが通る".
//!
//! Mirrors `src/search/hashtag_repository/tests.rs`'s established convention
//! (`crate::test_harness::spawn_test_app` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext`), but — unlike that module's
//! own tests, which never touch `statuses`/`tags`/`status_tags` — this
//! module's fixtures *do* insert real upstream rows (`statuses` via
//! `crate::statuses::status_repository::insert_status`, `tags`/`status_tags`
//! via `crate::statuses::tag_repository::upsert_tag`/`associate_tag`) to
//! stand in for "posts statuses-core has already extracted hashtags for"
//! (this task's own upstream-context note: hashtag *extraction* is
//! `StatusService::create_status`'s job, already done upstream by the time
//! `catch_up_from_watermark` runs) — `catch_up_from_watermark` itself must
//! never write to any of those three tables, only read them (asserted
//! directly in `catch_up_from_watermark_never_writes_upstream_tables`
//! below).
//!
//! The harness's own `RuntimeContext` uses a `FixedClock`
//! (`app.runtime.clock.now()` always returns the same instant within one
//! `TestApp`), so within a single test every inserted status shares the same
//! `created_at` — this is precisely why the watermark's tie-break on `id`
//! (monotonically increasing via `app.runtime.ids.next_id()`,
//! `SeqIdGenerator`) matters, and these tests deliberately rely on `id`
//! ordering rather than `created_at` ordering to distinguish "older" from
//! "newer" statuses.

use super::catch_up_from_watermark;
use crate::domain::{Id, Visibility};
use crate::search::hashtag_repository::{load_watermark, match_hashtags};
use crate::statuses::model::{Status, Tag};
use crate::statuses::status_repository::insert_status;
use crate::statuses::tag_repository::{associate_tag, upsert_tag};
use crate::test_harness::{TestApp, spawn_test_app};

/// Builds and inserts a minimal `statuses` row (mirrors
/// `src/statuses/status_repository/tests.rs::sample_status`'s "synthetic
/// actor_id, logical-only reference" convention), returning its id.
async fn insert_status_row(app: &TestApp) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let status = Status {
        id,
        actor_id: app.runtime.ids.next_id(),
        uri: format!("https://example.test/statuses/{}", id.as_i64()),
        url: Some(format!("https://example.test/@actor/{}", id.as_i64())),
        content: "hello world".to_string(),
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
    };
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    id
}

/// Associates upstream-already-extracted hashtag `name` with `status_id` via
/// statuses-core's own `tags`/`status_tags` tables (never this spec's
/// `search_tags`/`search_status_tags`) — standing in for
/// `StatusService::create_status`'s hashtag extraction, which already
/// happened by the time `catch_up_from_watermark` runs.
async fn tag_status(app: &TestApp, status_id: Id, name: &str) {
    let tag = Tag {
        id: app.runtime.ids.next_id(),
        name: name.to_string(),
        created_at: app.runtime.clock.now(),
    };
    let tag = upsert_tag(&app.pool, &tag)
        .await
        .expect("upsert_tag must succeed");
    associate_tag(&app.pool, status_id, tag.id)
        .await
        .expect("associate_tag must succeed");
}

/// Inserts a status carrying `tag_names` (each an upstream-already-extracted
/// hashtag), returning the status id.
async fn insert_tagged_status(app: &TestApp, tag_names: &[&str]) -> Id {
    let status_id = insert_status_row(app).await;
    for name in tag_names {
        tag_status(app, status_id, name).await;
    }
    status_id
}

// -- backfill (no prior watermark) -----------------------------------------

/// Requirement 5.3 / this task's own completion condition ("既存投稿のハッシ
/// ュタグがインデックスへ導出され"): with no watermark saved yet,
/// `catch_up_from_watermark` treats every existing status as needing
/// processing (backfill) and derives its upstream-already-extracted tags
/// into `search_tags`/`search_status_tags`, findable afterward via
/// `match_hashtags`.
#[tokio::test]
async fn catch_up_from_watermark_backfills_existing_posts_hashtags() {
    let app = spawn_test_app().await;
    insert_tagged_status(&app, &["rustlang", "mastodon"]).await;

    let processed = catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("catch_up_from_watermark must succeed");
    assert_eq!(processed, 1, "exactly one status was processed");

    let mut names: Vec<String> = match_hashtags(&app.pool, "", 20, 0)
        .await
        .expect("match_hashtags must succeed")
        .into_iter()
        .map(|tag| tag.name)
        .collect();
    names.sort();
    assert_eq!(names, vec!["mastodon".to_string(), "rustlang".to_string()]);

    app.cleanup().await;
}

/// A status backed by zero upstream hashtags is still counted as processed
/// and must not stall the watermark (this task's own explicit acceptance
/// note: "a status contributing zero hashtags doesn't stall the watermark
/// from advancing past it") — proven here by a rerun processing nothing
/// further once such a status is the newest one.
#[tokio::test]
async fn catch_up_from_watermark_advances_past_a_status_with_zero_hashtags() {
    let app = spawn_test_app().await;
    insert_tagged_status(&app, &[]).await;

    let processed = catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("catch_up_from_watermark must succeed");
    assert_eq!(processed, 1, "the zero-hashtag status is still processed");

    let rerun = catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("rerunning catch_up_from_watermark must succeed");
    assert_eq!(
        rerun, 0,
        "the watermark must have advanced past the zero-hashtag status"
    );

    app.cleanup().await;
}

// -- idempotency / re-run behavior ------------------------------------------

/// Requirement 5.3 / this task's own completion condition ("再実行で重複生
/// 成されず"): running `catch_up_from_watermark` a second time immediately
/// after a successful run, with no new statuses in between, is a safe no-op
/// — it processes zero statuses and creates no duplicate `search_status_tags`
/// rows or double-counted `search_tags.statuses_count`.
#[tokio::test]
async fn catch_up_from_watermark_rerun_with_no_new_statuses_is_a_safe_no_op() {
    let app = spawn_test_app().await;
    insert_tagged_status(&app, &["idempotent"]).await;

    let first = catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("first catch_up_from_watermark must succeed");
    assert_eq!(first, 1);

    let second = catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("second catch_up_from_watermark must succeed");
    assert_eq!(second, 0, "no new statuses to process on rerun");

    let tag_row: (i64,) = sqlx::query_as("SELECT statuses_count FROM search_tags WHERE name = $1")
        .bind("idempotent")
        .fetch_one(&app.pool)
        .await
        .expect("fetching the search_tags row must succeed");
    assert_eq!(
        tag_row.0, 1,
        "statuses_count must not be double-counted by the rerun"
    );

    let association_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM search_status_tags")
        .fetch_one(&app.pool)
        .await
        .expect("counting search_status_tags rows must succeed");
    assert_eq!(
        association_count.0, 1,
        "no duplicate (tag_id, status_id) association was created by the rerun"
    );

    app.cleanup().await;
}

// -- watermark-scoped catch-up (only newer statuses processed) -------------

/// Requirement 5.3 / this task's own completion condition ("watermark 以降
/// の新規投稿のみが処理され"): after an initial run processes an existing
/// status and advances the watermark, a newly inserted status is picked up
/// by the next run while the already-processed status is not reprocessed.
#[tokio::test]
async fn catch_up_from_watermark_only_processes_statuses_newer_than_watermark() {
    let app = spawn_test_app().await;
    insert_tagged_status(&app, &["oldtag"]).await;

    let first = catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("first catch_up_from_watermark must succeed");
    assert_eq!(first, 1);

    insert_tagged_status(&app, &["newtag"]).await;

    let second = catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("second catch_up_from_watermark must succeed");
    assert_eq!(second, 1, "only the newly inserted status is processed");

    let mut names: Vec<String> = match_hashtags(&app.pool, "", 20, 0)
        .await
        .expect("match_hashtags must succeed")
        .into_iter()
        .map(|tag| tag.name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["newtag".to_string(), "oldtag".to_string()],
        "both the old and new tag must be indexed, but the old one only once"
    );

    app.cleanup().await;
}

/// After a successful run, `load_watermark` reflects the newest processed
/// status's `(created_at, id)` — proven directly against the repository read
/// this indexer itself relies on for its next catch-up scan.
#[tokio::test]
async fn catch_up_from_watermark_advances_the_saved_watermark() {
    let app = spawn_test_app().await;
    let status_id = insert_tagged_status(&app, &["watermarked"]).await;

    catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("catch_up_from_watermark must succeed");

    let watermark = load_watermark(&app.pool)
        .await
        .expect("load_watermark must succeed")
        .expect("a watermark was saved");
    assert_eq!(watermark.1, status_id);

    app.cleanup().await;
}

// -- read-only against upstream tables ---------------------------------------

/// This task's own explicit constraint ("upstream テーブルを変更しない"):
/// running `catch_up_from_watermark` must not alter `statuses`/`tags`/
/// `status_tags` row counts or content in any way — only this spec's own
/// `search_tags`/`search_status_tags`/`search_index_watermark` tables change.
#[tokio::test]
async fn catch_up_from_watermark_never_writes_upstream_tables() {
    let app = spawn_test_app().await;
    insert_tagged_status(&app, &["readonly"]).await;

    let statuses_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM statuses")
        .fetch_one(&app.pool)
        .await
        .expect("counting statuses rows must succeed");
    let tags_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tags")
        .fetch_one(&app.pool)
        .await
        .expect("counting tags rows must succeed");
    let status_tags_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM status_tags")
        .fetch_one(&app.pool)
        .await
        .expect("counting status_tags rows must succeed");

    catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("catch_up_from_watermark must succeed");

    let statuses_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM statuses")
        .fetch_one(&app.pool)
        .await
        .expect("counting statuses rows must succeed");
    let tags_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tags")
        .fetch_one(&app.pool)
        .await
        .expect("counting tags rows must succeed");
    let status_tags_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM status_tags")
        .fetch_one(&app.pool)
        .await
        .expect("counting status_tags rows must succeed");

    assert_eq!(
        statuses_before, statuses_after,
        "statuses row count unchanged"
    );
    assert_eq!(tags_before, tags_after, "tags row count unchanged");
    assert_eq!(
        status_tags_before, status_tags_after,
        "status_tags row count unchanged"
    );

    app.cleanup().await;
}

// -- multiple statuses sharing a tag -----------------------------------------

/// Two different statuses using the same upstream tag both get indexed as
/// distinct `search_status_tags` associations, and `search_tags.statuses_count`
/// reflects both.
#[tokio::test]
async fn catch_up_from_watermark_indexes_distinct_statuses_sharing_a_tag() {
    let app = spawn_test_app().await;
    insert_tagged_status(&app, &["popular"]).await;
    insert_tagged_status(&app, &["popular"]).await;

    let processed = catch_up_from_watermark(&app.pool, &app.runtime)
        .await
        .expect("catch_up_from_watermark must succeed");
    assert_eq!(processed, 2);

    let tag_row: (i64,) = sqlx::query_as("SELECT statuses_count FROM search_tags WHERE name = $1")
        .bind("popular")
        .fetch_one(&app.pool)
        .await
        .expect("fetching the search_tags row must succeed");
    assert_eq!(tag_row.0, 2);

    app.cleanup().await;
}
