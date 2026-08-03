//! Integration tests for `PgSearchBackend::search_hashtags` (search spec
//! task 3.2, `Boundary: PgSearchBackend`; Requirements 5.1, 5.3, 5.5),
//! design.md's "ハッシュタグ照合と読み取りインデックス導出" flow: an
//! on-demand [`kawasemi::search::hashtag_indexer::catch_up_from_watermark`]
//! scan runs at the *front* of every `search_hashtags` call, before
//! [`kawasemi::search::hashtag_repository::match_hashtags`] is consulted —
//! never a separate, independently-scheduled background job.
//!
//! ## Scope
//! `tests/search_accounts_it.rs`/`tests/search_statuses_it.rs` (task 3.1)
//! already cover `search_accounts`/`search_statuses`; this file only
//! exercises `search_hashtags`. `src/search/hashtag_indexer/tests.rs`
//! already proves `catch_up_from_watermark` itself (backfill, idempotent
//! re-run, watermark-only-advances-on-success, upstream-tables-untouched);
//! `src/search/hashtag_repository/tests.rs` already proves `match_hashtags`
//! itself (name-prefix match, limit/offset, ordering). This file's own,
//! distinguishing job is proving the two are *wired together* inside
//! `search_hashtags`: a post tagged and inserted directly into upstream
//! `statuses`/`tags`/`status_tags` *after* the read index's watermark was
//! last advanced still shows up in a `search_hashtags` call, because that
//! call's own on-demand catch-up runs first — without any test-side manual
//! call to `catch_up_from_watermark`.
//!
//! Fixtures mirror `src/search/hashtag_indexer/tests.rs`'s own
//! `insert_status_row`/`tag_status`/`insert_tagged_status` helpers exactly
//! (upstream `statuses` via `crate::statuses::status_repository::
//! insert_status`, upstream `tags`/`status_tags` via
//! `crate::statuses::tag_repository::upsert_tag`/`associate_tag` — standing
//! in for `StatusService::create_status`'s already-completed hashtag
//! extraction), since this file needs the identical "a tagged post exists
//! upstream, un-indexed" starting state that module's own tests build.

use kawasemi::domain::{Id, Visibility};
use kawasemi::search::model::TagMatch;
use kawasemi::search::pg_backend::PgSearchBackend;
use kawasemi::search::ports::{HashtagQuery, SearchBackend};
use kawasemi::statuses::model::{Status, Tag};
use kawasemi::statuses::status_repository::insert_status;
use kawasemi::statuses::tag_repository::{associate_tag, upsert_tag};
use kawasemi::test_harness::{TestApp, spawn_test_app};

/// Builds and inserts a minimal upstream `statuses` row, returning its id
/// (mirrors `src/search/hashtag_indexer/tests.rs::insert_status_row`).
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

/// Associates upstream-already-extracted hashtag `name` with `status_id`
/// via statuses-core's own `tags`/`status_tags` tables (mirrors
/// `src/search/hashtag_indexer/tests.rs::tag_status`) — never this spec's
/// own `search_tags`/`search_status_tags`, which `search_hashtags`'s
/// on-demand catch-up is responsible for deriving into.
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

/// Inserts a status carrying `tag_names` (each an upstream-already-
/// extracted hashtag), returning the status id.
async fn insert_tagged_status(app: &TestApp, tag_names: &[&str]) -> Id {
    let status_id = insert_status_row(app).await;
    for name in tag_names {
        tag_status(app, status_id, name).await;
    }
    status_id
}

fn query(term: &str, limit: u32, offset: u32) -> HashtagQuery {
    HashtagQuery {
        term: term.to_string(),
        limit,
        offset,
    }
}

fn tag_match(name: &str) -> TagMatch {
    TagMatch {
        name: name.to_string(),
    }
}

/// This task's own distinguishing, observable completion condition: a post
/// tagged and inserted directly into upstream `statuses`/`tags`/
/// `status_tags` *after* the read index's watermark was last advanced (by
/// an earlier `search_hashtags` call) is still returned by a later
/// `search_hashtags` call — because that call's own on-demand
/// `catch_up_from_watermark` runs first, every call, not merely on the
/// first-ever request (Requirements 5.1, 5.3). No test code here ever
/// calls `catch_up_from_watermark` directly.
#[tokio::test]
async fn search_hashtags_catches_up_posts_inserted_after_watermark_before_matching() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());

    // A tagged post exists upstream before any search_hashtags call has
    // ever run (watermark unset -> None -> backfill scope).
    insert_tagged_status(&app, &["zwatermarkalpha"]).await;

    // This first call's own on-demand catch-up derives "zwatermarkalpha"
    // into the read index and advances the watermark past it.
    let first = backend
        .search_hashtags(&query("zwatermark", 50, 0))
        .await
        .expect("search_hashtags must succeed");
    assert_eq!(
        first,
        vec![tag_match("zwatermarkalpha")],
        "first call must find the pre-existing tagged post via backfill"
    );

    // A second tagged post is inserted directly into upstream tables
    // *after* the watermark was advanced above -- never manually indexed.
    insert_tagged_status(&app, &["zwatermarkbeta"]).await;

    // The second search_hashtags call must catch this new post up on its
    // own, on demand, before matching -- proving the catch-up runs at the
    // front of every call, not just the first.
    let second = backend
        .search_hashtags(&query("zwatermark", 50, 0))
        .await
        .expect("search_hashtags must succeed");
    assert_eq!(
        second,
        vec![tag_match("zwatermarkalpha"), tag_match("zwatermarkbeta")],
        "second call must have caught up the post inserted after the first call's watermark \
         advance, without any manual catch_up_from_watermark call in this test"
    );

    app.cleanup().await;
}

/// `limit`/`offset` are respected (Requirement 5.5), applied over the
/// name-`ASC`-ordered match set `match_hashtags` returns.
#[tokio::test]
async fn search_hashtags_applies_limit_and_offset() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());

    insert_tagged_status(&app, &["pagealpha"]).await;
    insert_tagged_status(&app, &["pagebeta"]).await;
    insert_tagged_status(&app, &["pagegamma"]).await;

    let page1 = backend
        .search_hashtags(&query("page", 1, 0))
        .await
        .expect("search_hashtags page 1 must succeed");
    assert_eq!(page1, vec![tag_match("pagealpha")]);

    let page2 = backend
        .search_hashtags(&query("page", 1, 1))
        .await
        .expect("search_hashtags page 2 must succeed");
    assert_eq!(page2, vec![tag_match("pagebeta")]);

    let page3 = backend
        .search_hashtags(&query("page", 1, 2))
        .await
        .expect("search_hashtags page 3 must succeed");
    assert_eq!(page3, vec![tag_match("pagegamma")]);

    let beyond = backend
        .search_hashtags(&query("page", 50, 100))
        .await
        .expect("search_hashtags offset-beyond-end must succeed");
    assert!(beyond.is_empty());

    app.cleanup().await;
}

/// `search_hashtags` returns bare `Vec<TagMatch>` (identifiers only,
/// Requirement 7.2) -- this both type-checks against the `SearchBackend`
/// trait signature and, at runtime, proves the `TagView -> TagMatch`
/// mapping this task performs preserves the `name` value exactly.
#[tokio::test]
async fn search_hashtags_returns_tag_match_identifiers_only() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    insert_tagged_status(&app, &["identifieronly"]).await;

    let matches: Vec<TagMatch> = backend
        .search_hashtags(&query("identifieronly", 50, 0))
        .await
        .expect("search_hashtags must succeed");

    let TagMatch { name } = matches
        .into_iter()
        .next()
        .expect("exactly one hashtag must match");
    assert_eq!(name, "identifieronly");

    app.cleanup().await;
}

/// A term matching no hashtag returns an empty `Vec`, not an error, even
/// after the on-demand catch-up runs.
#[tokio::test]
async fn search_hashtags_returns_empty_for_no_match() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    insert_tagged_status(&app, &["somehashtag"]).await;

    let matches = backend
        .search_hashtags(&query("nonexistentterm", 50, 0))
        .await
        .expect("search_hashtags must succeed even with no matches");
    assert!(matches.is_empty());

    app.cleanup().await;
}
