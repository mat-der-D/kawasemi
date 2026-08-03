//! Integration tests for `PgSearchBackend::search_statuses` (search spec
//! task 3.1, `Boundary: PgSearchBackend`; Requirements 4.1, 4.3, 4.4, 4.5,
//! 4.6, 7.2), design.md's `search_statuses_it.rs` ("投稿検索（可視性に閉じる・
//! account_id 絞り・不可視除外・limit/offset・オーバーフェッチ後の limit
//! 切り詰め）（統合）").
//!
//! ## Scope: visibility is NOT enforced by this backend
//! `src/search/ports.rs`'s own `StatusQuery` doc comment is explicit that
//! `viewer` is carried through only as an optional optimization hint —
//! "`viewer` is carried through so a backend can prefilter to visibility
//! *candidates*... final visibility is still re-applied by `SearchHydrator`
//! downstream of this port... a backend is free to treat `viewer` as an
//! optimization hint rather than a hard filter" — and design.md's own
//! `PgSearchBackend::search_statuses` Responsibilities note says the same
//! ("最終可視性は Hydrator が `VisibilityPolicy` で再適用"). This test file
//! therefore does not assert that `search_statuses` excludes any particular
//! `visibility` value; it only proves content matching, `account_id`
//! scoping, and `limit`/`offset`/overfetch behavior — visibility
//! enforcement is a later task's (`SearchHydrator`) job, strictly outside
//! task 3.1's boundary.
//!
//! Fixtures are inserted via the real, production
//! `crate::statuses::status_repository::insert_status` (statuses-core's own
//! repository function), mirroring `src/statuses/tag_repository/tests.rs`'s
//! established convention for building genuine `statuses` rows in a search-
//! adjacent spec's own test file.

use kawasemi::domain::{Id, Visibility};
use kawasemi::search::pg_backend::PgSearchBackend;
use kawasemi::search::ports::{SearchBackend, StatusQuery};
use kawasemi::statuses::model::Status;
use kawasemi::statuses::status_repository::insert_status;
use kawasemi::test_harness::{TestApp, spawn_test_app};

/// Builds and inserts a status authored by `actor_id` with `content`,
/// `created_at` seconds after `app`'s deterministic base clock time
/// (distinguishing insertion order deterministically without relying on
/// `id` alone), returning the minted `Id`.
async fn insert_test_status(
    app: &TestApp,
    actor_id: Id,
    content: &str,
    created_offset_secs: i64,
) -> Id {
    let id = app.runtime.ids.next_id();
    let status = Status {
        id,
        actor_id,
        uri: format!("https://example.test/statuses/{}", id.as_i64()),
        url: None,
        content: content.to_string(),
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
        created_at: app.runtime.clock.now() + time::Duration::seconds(created_offset_secs),
        edited_at: None,
    };
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");
    id
}

fn query(term: &str, viewer: Id, account_id: Option<Id>, limit: u32, offset: u32) -> StatusQuery {
    StatusQuery {
        term: term.to_string(),
        viewer,
        account_id,
        limit,
        offset,
    }
}

/// Posts are matched by a partial, case-insensitive `content` substring
/// (Requirement 4.1), returning bare `Id`s (Requirement 7.2).
#[tokio::test]
async fn search_statuses_matches_content_substring() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let author = app.runtime.ids.next_id();
    let viewer = app.runtime.ids.next_id();
    let matching = insert_test_status(&app, author, "hello rustlang world", 0).await;
    insert_test_status(&app, author, "totally unrelated content", 1).await;

    let matches = backend
        .search_statuses(&query("rustlang", viewer, None, 50, 0))
        .await
        .expect("search_statuses must succeed");

    assert_eq!(matches, vec![matching]);

    app.cleanup().await;
}

/// `account_id` scopes matching to only the given author's posts
/// (Requirement 4.3).
#[tokio::test]
async fn search_statuses_scopes_to_account_id() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let viewer = app.runtime.ids.next_id();
    let author_a = app.runtime.ids.next_id();
    let author_b = app.runtime.ids.next_id();
    let a_post = insert_test_status(&app, author_a, "hello from author a", 0).await;
    insert_test_status(&app, author_b, "hello from author b", 1).await;

    let unscoped = backend
        .search_statuses(&query("hello", viewer, None, 50, 0))
        .await
        .expect("unscoped search_statuses must succeed");
    assert_eq!(unscoped.len(), 2, "both posts match the unscoped term");

    let scoped = backend
        .search_statuses(&query("hello", viewer, Some(author_a), 50, 0))
        .await
        .expect("account_id-scoped search_statuses must succeed");
    assert_eq!(scoped, vec![a_post]);

    app.cleanup().await;
}

/// `limit`/`offset` are applied to the SQL result (Requirement 4.6). Unlike
/// `search_accounts`, a `limit` here is *not* a hard cap on the returned
/// count — design.md's own overfetch convention deliberately lets the SQL
/// `LIMIT` exceed the requested `limit` (see
/// `search_statuses_may_overfetch_beyond_requested_limit`), so this test
/// asserts what design.md *does* guarantee: `OFFSET` genuinely skips the
/// requested number of leading rows in the query's own deterministic
/// `created_at DESC, id DESC` order, using a `limit` large enough (well
/// above every matching row plus this module's overfetch margin) that no
/// overfetch-vs-limit ambiguity affects the assertions.
///
/// Rows are ordered `created_at DESC, id DESC`, so `created_offset_secs`
/// below is chosen to give a deterministic, known descending order:
/// `beta` (offset 2) is newest, then `alpha` (offset 1), then `gamma`
/// (offset 0) is oldest.
#[tokio::test]
async fn search_statuses_applies_limit_and_offset() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let author = app.runtime.ids.next_id();
    let viewer = app.runtime.ids.next_id();
    let gamma = insert_test_status(&app, author, "paginate gamma", 0).await;
    let alpha = insert_test_status(&app, author, "paginate alpha", 1).await;
    let beta = insert_test_status(&app, author, "paginate beta", 2).await;

    let from_start = backend
        .search_statuses(&query("paginate", viewer, None, 50, 0))
        .await
        .expect("search_statuses offset 0 must succeed");
    assert_eq!(
        from_start,
        vec![beta, alpha, gamma],
        "newest (largest created_at) first"
    );

    let skip_one = backend
        .search_statuses(&query("paginate", viewer, None, 50, 1))
        .await
        .expect("search_statuses offset 1 must succeed");
    assert_eq!(skip_one, vec![alpha, gamma]);

    let skip_two = backend
        .search_statuses(&query("paginate", viewer, None, 50, 2))
        .await
        .expect("search_statuses offset 2 must succeed");
    assert_eq!(skip_two, vec![gamma]);

    let beyond = backend
        .search_statuses(&query("paginate", viewer, None, 50, 100))
        .await
        .expect("search_statuses offset-beyond-end must succeed");
    assert!(beyond.is_empty());

    app.cleanup().await;
}

/// design.md's overfetch convention: the SQL-level `LIMIT` used by
/// `search_statuses` exceeds the requested `limit` (so a downstream
/// post-visibility-filter step has spare candidates to absorb exclusions
/// from) — proven by requesting a small `limit` against more matching rows
/// than that `limit`, while never inserting more than the *overfetched*
/// bound's worth of rows, and observing more than `limit` rows come back.
#[tokio::test]
async fn search_statuses_may_overfetch_beyond_requested_limit() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let author = app.runtime.ids.next_id();
    let viewer = app.runtime.ids.next_id();
    for i in 0..5 {
        insert_test_status(&app, author, "overfetchterm common body", i).await;
    }

    let matches = backend
        .search_statuses(&query("overfetchterm", viewer, None, 1, 0))
        .await
        .expect("search_statuses must succeed");

    assert!(
        matches.len() > 1,
        "requested limit=1 but overfetch convention must return more than 1 of the 5 matches, \
         got {}",
        matches.len()
    );

    app.cleanup().await;
}

/// `OFFSET` uses the requested `offset` unchanged (not scaled by the
/// overfetch factor) — design.md: "`OFFSET` は要求 `offset` をそのまま用いる
/// （オーバーフェッチはページ境界=`offset` を動かさない）". Proven by
/// inserting exactly `offset + 1` matching rows and confirming an `offset`
/// at that boundary still returns the correct final row, not an empty page
/// (which an accidentally-scaled offset would produce).
#[tokio::test]
async fn search_statuses_offset_is_not_scaled_by_overfetch() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let author = app.runtime.ids.next_id();
    let viewer = app.runtime.ids.next_id();
    let oldest = insert_test_status(&app, author, "offsetterm row zero", 0).await;
    insert_test_status(&app, author, "offsetterm row one", 1).await;
    insert_test_status(&app, author, "offsetterm row two", 2).await;

    let last_page = backend
        .search_statuses(&query("offsetterm", viewer, None, 50, 2))
        .await
        .expect("search_statuses must succeed");

    assert_eq!(last_page, vec![oldest]);

    app.cleanup().await;
}

/// A term matching no post's content returns an empty `Vec`, not an error.
#[tokio::test]
async fn search_statuses_returns_empty_for_no_match() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone(), app.runtime.clone());
    let author = app.runtime.ids.next_id();
    let viewer = app.runtime.ids.next_id();
    insert_test_status(&app, author, "hello rustlang world", 0).await;

    let matches = backend
        .search_statuses(&query("nonexistentterm", viewer, None, 50, 0))
        .await
        .expect("search_statuses must succeed even with no matches");
    assert!(matches.is_empty());

    app.cleanup().await;
}
