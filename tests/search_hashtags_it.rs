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
//!
//! ## Filename note (search spec task 6.2, `Boundary: search_hashtags_it`)
//! This file was originally created under task 3.2 as
//! `tests/search_hashtags_pg_backend_it.rs`; design.md's own File Structure
//! Plan names this exact file `tests/search_hashtags_it.rs`. Task 6.2
//! renamed it to match design.md (via `git mv`, preserving every existing
//! test below unmodified) rather than creating a second, parallel file, per
//! this task's own dispatch brief.
//!
//! ## Task 6.2 additions: `exclude_unreviewed=true` acceptance through the
//! real, full pipeline (Requirement 5.4)
//! `tests/search_service_it.rs::
//! search_exclude_unreviewed_is_accepted_without_changing_results` already
//! proves `SearchService::search` accepts `exclude_unreviewed=true` without
//! rejecting the request or altering its (minimal-implementation) hashtag
//! results — but only against a `StubSearchBackend`, never this spec's own
//! default, production `PgSearchBackend`. The test below closes that gap at
//! the true integration level (this task's own boundary,
//! `tests/search_hashtags_it.rs`) by driving a real
//! `GET /api/v2/search?exclude_unreviewed=true&type=hashtags` request
//! through the actual HTTP router (`crate::server::build_router`, mirroring
//! `tests/search_contract_it.rs`'s established full-pipeline technique)
//! against a genuinely `HashtagIndexer`-derived tag, proving the parameter
//! is accepted end to end and the real, default backend's hashtag results
//! are unaffected by it (Requirement 5.4's "本サーバーの最小実装に整合する
//! 結果を返す").

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::{Id, Visibility};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::search::model::TagMatch;
use kawasemi::search::pg_backend::PgSearchBackend;
use kawasemi::search::ports::{HashtagQuery, SearchBackend};
use kawasemi::server;
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

// ==========================================================================
// Task 6.2 additions: `exclude_unreviewed=true` acceptance through the real,
// full pipeline (Requirement 5.4). See this file's own doc comment, "Task
// 6.2 additions", for why this drives the actual `GET /api/v2/search` HTTP
// endpoint against the real, default `PgSearchBackend` rather than a
// `StubSearchBackend`.
//
// Fixture plumbing below mirrors `tests/search_contract_it.rs`'s own
// already-reviewed helpers of the same names (each `tests/*.rs` file is its
// own compiled crate, so this deliberately duplicates rather than imports).
// ==========================================================================

async fn insert_actor_fixture(app: &TestApp, handle_str: &str) -> ResolvedActor {
    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new(handle_str).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: format!("Search Hashtags IT {handle_str}"),
            summary: "an actor used by the search_hashtags_it integration test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    app.actor
        .directory()
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolving the just-created actor must succeed")
        .expect("the just-created actor must be resolvable")
}

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Search Hashtags IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn issue_test_token(app: &TestApp, app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ModelScopeSet::new(scopes.iter().copied()),
        },
    )
    .await
    .expect("issue_token must succeed");
    issued.plaintext.expose_secret().to_string()
}

const TEST_DOMAIN: &str = "test-harness.kawasemi.internal";

fn req(method: &str, path: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-host", TEST_DOMAIN);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("build request")
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

async fn search(router: &Router, token: &str, query: &str) -> (StatusCode, Value) {
    send(
        router,
        req("GET", &format!("/api/v2/search?{query}"), Some(token)),
    )
    .await
}

fn hashtag_names(body: &Value) -> Vec<String> {
    body["hashtags"]
        .as_array()
        .expect("hashtags must be a JSON array")
        .iter()
        .map(|tag| tag["name"].as_str().unwrap().to_string())
        .collect()
}

/// Requirement 5.4: `exclude_unreviewed=true`, driven through the real
/// `GET /api/v2/search` HTTP endpoint against the real, default
/// `PgSearchBackend` (not a `StubSearchBackend`), is accepted (the request
/// still succeeds, not rejected) and does not change this minimal
/// implementation's hashtag results -- proven against a genuinely
/// `HashtagIndexer`-derived tag (`insert_tagged_status`, the same upstream-
/// only fixture every other test in this file uses).
#[tokio::test]
async fn search_exclude_unreviewed_true_is_accepted_through_the_full_pipeline_with_the_real_backend()
 {
    let app = spawn_test_app().await;
    let router = server::build_router(app.state.clone());

    insert_tagged_status(&app, &["excludeunreviewedit"]).await;

    let searcher = insert_actor_fixture(&app, "search_hash_it_searcher").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, searcher.id, &["read:search"]).await;

    let (status, without_flag) =
        search(&router, &token, "q=excludeunreviewedit&type=hashtags").await;
    assert_eq!(status, StatusCode::OK, "got: {without_flag:?}");
    assert_eq!(
        hashtag_names(&without_flag),
        vec!["excludeunreviewedit".to_string()]
    );

    let (status, with_flag) = search(
        &router,
        &token,
        "q=excludeunreviewedit&type=hashtags&exclude_unreviewed=true",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "exclude_unreviewed=true must be accepted, not rejected: {with_flag:?}"
    );
    assert_eq!(
        hashtag_names(&with_flag),
        vec!["excludeunreviewedit".to_string()],
        "exclude_unreviewed=true must not change this minimal implementation's hashtag results \
         (Requirement 5.4)"
    );

    app.cleanup().await;
}
