//! Integration-style, DB-backed tests for `HashtagIndexRepository`
//! (Requirements 5.1, 5.2, 5.5, 8.2), per task 2.1's own observable
//! completion condition: "名前一致で `TagView` が返り、`limit`/`offset` が
//! 反映され、(tag_id, status_id) 重複が一意化され、`save_watermark` 後に
//! `load_watermark` が同じ値を返す統合テストが通る".
//!
//! Mirrors `src/statuses/tag_repository/tests.rs`'s established convention:
//! `crate::test_harness::spawn_test_app` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext` (`app.runtime.ids.next_id()`/
//! `app.runtime.clock.now()` for id minting / `Clock`-sourced timestamps,
//! never `OffsetDateTime::now_utc()`/ad-hoc ids). This module never touches
//! `statuses`/`tags`/`status_tags` (statuses-core's own tables) — every
//! fixture here is a `search_tags`/`search_status_tags` row built purely
//! through this module's own [`upsert_tag_usage`], with `status_id` values
//! that are bare minted [`Id`]s standing in for statuses-core post ids (this
//! task's own boundary: `search_status_tags.status_id` is a logical-only
//! reference, `migrations/0013_search.sql`, never a physical FK, so no real
//! `statuses` row is required to exercise this repository).

use super::{load_watermark, match_hashtags, save_watermark, upsert_tag_usage};
use crate::domain::Id;
use crate::search::model::TagView;
use crate::test_harness::{TestApp, spawn_test_app};

/// Records one use of `name` by a freshly minted status id, returning the
/// status id used (callers that need a stable, distinguishable status id
/// across multiple calls get one back rather than having to mint it
/// themselves twice).
async fn record_usage(app: &TestApp, name: &str) -> Id {
    let status_id = app.runtime.ids.next_id();
    upsert_tag_usage(
        &app.pool,
        name,
        app.runtime.ids.next_id(),
        status_id,
        app.runtime.clock.now(),
    )
    .await
    .expect("upsert_tag_usage must succeed");
    status_id
}

// -- match_hashtags --------------------------------------------------------

/// A tag recorded via `upsert_tag_usage` is findable by an exact-name query,
/// returned as a `TagView` carrying that name (Requirement 5.1, 5.2).
#[tokio::test]
async fn match_hashtags_finds_a_recorded_tag_by_exact_name() {
    let app = spawn_test_app().await;
    record_usage(&app, "rustlang").await;

    let matches = match_hashtags(&app.pool, "rustlang", 20, 0)
        .await
        .expect("match_hashtags must succeed");

    assert_eq!(
        matches,
        vec![TagView {
            name: "rustlang".to_string(),
            url: "/tags/rustlang".to_string(),
            history: Vec::new(),
        }]
    );

    app.cleanup().await;
}

/// `match_hashtags` matches by name *prefix* (Requirement 5.1's "前方...一
/// 致"): a shorter query term matches every tag name starting with it, case-
/// insensitively, but not a name that merely contains the term elsewhere.
#[tokio::test]
async fn match_hashtags_matches_by_case_insensitive_prefix() {
    let app = spawn_test_app().await;
    record_usage(&app, "rustlang").await;
    record_usage(&app, "rustacean").await;
    record_usage(&app, "mastodon").await;
    // "oldrust" contains "rust" but does not *start* with it -- must not
    // match a "rust"-prefix query.
    record_usage(&app, "oldrust").await;

    let matches = match_hashtags(&app.pool, "RuSt", 20, 0)
        .await
        .expect("match_hashtags must succeed");

    let names: Vec<&str> = matches.iter().map(|tag| tag.name.as_str()).collect();
    assert_eq!(names, vec!["rustacean", "rustlang"], "name ASC order");

    app.cleanup().await;
}

/// `limit`/`offset` are applied by `match_hashtags` (Requirement 5.5).
#[tokio::test]
async fn match_hashtags_applies_limit_and_offset() {
    let app = spawn_test_app().await;
    record_usage(&app, "tagalpha").await;
    record_usage(&app, "tagbeta").await;
    record_usage(&app, "taggamma").await;

    let page1 = match_hashtags(&app.pool, "tag", 1, 0)
        .await
        .expect("match_hashtags page 1 must succeed");
    assert_eq!(
        page1.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["tagalpha"]
    );

    let page2 = match_hashtags(&app.pool, "tag", 1, 1)
        .await
        .expect("match_hashtags page 2 must succeed");
    assert_eq!(
        page2.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["tagbeta"]
    );

    let beyond = match_hashtags(&app.pool, "tag", 1, 100)
        .await
        .expect("match_hashtags offset-beyond-end must succeed");
    assert!(beyond.is_empty());

    app.cleanup().await;
}

/// A query term matching no tag returns an empty Vec, not an error.
#[tokio::test]
async fn match_hashtags_returns_empty_for_no_match() {
    let app = spawn_test_app().await;
    record_usage(&app, "rustlang").await;

    let matches = match_hashtags(&app.pool, "nonexistent", 20, 0)
        .await
        .expect("match_hashtags must succeed even with no matches");
    assert!(matches.is_empty());

    app.cleanup().await;
}

// -- upsert_tag_usage: id stability + (tag_id, status_id) dedup -----------

/// Two `upsert_tag_usage` calls for the *same* tag `name` from two different
/// posts both resolve to the same `search_tags` row (no duplicate tag row
/// created for the same name) -- proven indirectly via `match_hashtags`
/// returning exactly one `TagView` for that name after both calls.
#[tokio::test]
async fn upsert_tag_usage_reuses_the_same_tag_row_for_repeated_names() {
    let app = spawn_test_app().await;
    record_usage(&app, "shared").await;
    record_usage(&app, "shared").await;

    let matches = match_hashtags(&app.pool, "shared", 20, 0)
        .await
        .expect("match_hashtags must succeed");
    assert_eq!(matches.len(), 1, "only one search_tags row for the name");

    app.cleanup().await;
}

/// Requirement 8.2 / this task's own completion condition: re-recording the
/// *same* (tag, status) pair never creates a duplicate `search_status_tags`
/// row and never double-counts `search_tags.statuses_count` -- verified
/// directly against both tables via raw SQL (this repository's own tables,
/// Requirement 8.2's boundary), since `TagView` itself does not expose
/// `statuses_count` (see this module's own doc comment on `TagView::history`
/// starting empty).
#[tokio::test]
async fn upsert_tag_usage_deduplicates_the_same_tag_status_pair() {
    let app = spawn_test_app().await;
    let status_id = app.runtime.ids.next_id();
    let new_tag_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();

    upsert_tag_usage(&app.pool, "idempotent", new_tag_id, status_id, now)
        .await
        .expect("first upsert_tag_usage must succeed");
    // A second call for the exact same (name, status_id) pair, with a
    // *different* candidate new_tag_id -- proving the first call's tag id
    // wins and this second candidate id is discarded.
    let other_candidate_id = app.runtime.ids.next_id();
    upsert_tag_usage(&app.pool, "idempotent", other_candidate_id, status_id, now)
        .await
        .expect("second (duplicate) upsert_tag_usage must succeed as a no-op association");

    let association_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM search_status_tags WHERE status_id = $1")
            .bind(status_id.as_i64())
            .fetch_one(&app.pool)
            .await
            .expect("counting search_status_tags rows must succeed");
    assert_eq!(
        association_count.0, 1,
        "the same (tag_id, status_id) pair must not be duplicated"
    );

    let tag_row: (i64, i64) =
        sqlx::query_as("SELECT id, statuses_count FROM search_tags WHERE name = $1")
            .bind("idempotent")
            .fetch_one(&app.pool)
            .await
            .expect("fetching the search_tags row must succeed");
    assert_eq!(
        tag_row.0,
        new_tag_id.as_i64(),
        "the first call's candidate id must win"
    );
    assert_eq!(
        tag_row.1, 1,
        "statuses_count must not be double-counted by the duplicate call"
    );

    app.cleanup().await;
}

/// Two *different* statuses using the same tag each create their own
/// `search_status_tags` association and both bump `statuses_count`.
#[tokio::test]
async fn upsert_tag_usage_counts_distinct_statuses_for_the_same_tag() {
    let app = spawn_test_app().await;
    record_usage(&app, "popular").await;
    record_usage(&app, "popular").await;
    record_usage(&app, "popular").await;

    let tag_row: (i64,) = sqlx::query_as("SELECT statuses_count FROM search_tags WHERE name = $1")
        .bind("popular")
        .fetch_one(&app.pool)
        .await
        .expect("fetching the search_tags row must succeed");
    assert_eq!(tag_row.0, 3);

    app.cleanup().await;
}

// -- load_watermark / save_watermark ---------------------------------------

/// `load_watermark` returns `None` when the singleton row has never been
/// written (the migration seeds no initial row).
#[tokio::test]
async fn load_watermark_returns_none_when_never_saved() {
    let app = spawn_test_app().await;

    let watermark = load_watermark(&app.pool)
        .await
        .expect("load_watermark must succeed even with no row yet");
    assert!(watermark.is_none());

    app.cleanup().await;
}

/// Requirement 5.3 / this task's own completion condition: `load_watermark`
/// after `save_watermark` returns the exact same `(created_at, status_id)`
/// pair just saved.
#[tokio::test]
async fn save_watermark_then_load_watermark_round_trips() {
    let app = spawn_test_app().await;
    let status_id = app.runtime.ids.next_id();
    let created_at = app.runtime.clock.now();

    save_watermark(&app.pool, created_at, status_id)
        .await
        .expect("save_watermark must succeed");

    let loaded = load_watermark(&app.pool)
        .await
        .expect("load_watermark must succeed")
        .expect("a watermark was just saved");

    assert_eq!(loaded.0, created_at);
    assert_eq!(loaded.1, status_id);

    app.cleanup().await;
}

/// A second `save_watermark` call overwrites the first (the singleton row is
/// upserted, not duplicated) -- `load_watermark` reflects only the latest
/// save.
#[tokio::test]
async fn save_watermark_upserts_the_singleton_row() {
    let app = spawn_test_app().await;
    let first_status = app.runtime.ids.next_id();
    let first_time = app.runtime.clock.now();
    save_watermark(&app.pool, first_time, first_status)
        .await
        .expect("first save_watermark must succeed");

    let second_status = app.runtime.ids.next_id();
    let second_time = first_time + time::Duration::seconds(60);
    save_watermark(&app.pool, second_time, second_status)
        .await
        .expect("second save_watermark must succeed");

    let loaded = load_watermark(&app.pool)
        .await
        .expect("load_watermark must succeed")
        .expect("a watermark was saved");
    assert_eq!(loaded.0, second_time);
    assert_eq!(loaded.1, second_status);

    let row_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM search_index_watermark")
        .fetch_one(&app.pool)
        .await
        .expect("counting search_index_watermark rows must succeed");
    assert_eq!(row_count.0, 1, "the watermark table must stay a singleton");

    app.cleanup().await;
}
