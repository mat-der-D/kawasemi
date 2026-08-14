//! Integration tests for timelines task 6.1 (`.kiro/specs/timelines/tasks.md`,
//! "6.1 統合テスト（home/public/local/tag・フィルタ・ページネーション・認証）
//! を整備する", `_Boundary: TimelineService, TimelineEndpoints,
//! TimelineFilter_`): observable completion — "上記シナリオおよび充填上限到
//! 達時の部分ページ・継続カーソル返却の統合テストが全てグリーンになる"
//! (Requirements 1.1-1.6, 2.1-2.6, 3.1-3.4, 4.1-4.6, 5.1-5.4, 6.1-6.4,
//! 7.1-7.4, 9.1-9.4).
//!
//! ## Why this file, given `tests/timeline_service_it.rs` and
//! `tests/timelines_endpoints_handler_it.rs` already exist
//! This task's own brief is explicit that `_Boundary:_` names
//! `TimelineEndpoints` alongside `TimelineService`/`TimelineFilter` — meaning
//! HTTP-level coverage is required, not merely more service-level unit
//! coverage (already exhaustively provided by `tests/timeline_service_it.rs`,
//! task 4.2). `tests/timelines_endpoints_handler_it.rs` (task 5.1; formerly
//! `src/timelines/endpoints/tests.rs`, moved by
//! `.kiro/specs/test-placement-migration` task 7.2) already covers
//! auth/scope/response-code/`Link`-header wiring, but deliberately against a
//! *test-only* router it builds by hand (task 5.1's own module doc comment:
//! "this task's own boundary explicitly forbids mounting them onto the real
//! production router... task 5.2's job") — task 5.2 has since landed
//! (`.kiro/specs/timelines/tasks.md`'s "5.2 モジュール配線と Matcher シーム
//! 公開を行う" is `[x]`), so this file is the first to exercise the timeline
//! endpoints through the *real, fully-wired* production router
//! (`kawasemi::server::build_router`, exactly as booted by
//! `spawn_test_app`), mirroring `tests/social_graph_visibility_query_it.rs`'s
//! own established "drive the real HTTP surface end to end" precedent. It
//! also fills concrete coverage gaps neither existing file exercises at all:
//! `private`-visibility posts reflecting a *real* follow relationship
//! (Requirements 5.1, 5.3, 5.4 — no existing timeline test ever asserts a
//! `Visibility::Private` post is *included*, only ever excluded), tag `all[]`/
//! `none[]` (only `any[]` existed before), `only_media`/`remote` at the HTTP
//! layer for public/local/tag, relationship (block/mute) exclusion for
//! public/local/tag at the HTTP layer (previously proven only for home, or
//! only at the service layer), `Link`-header `next` targets actually
//! followed across multiple pages, and the fill-loop iteration cap's
//! partial-page-plus-valid-continuation-cursor guarantee exercised through a
//! real HTTP round trip (previously proven only by calling
//! `TimelineService::timeline` directly).
//!
//! ## RED phase evidence
//! Before this file existed, `cargo test --test timelines_endpoints_it`
//! failed with `error: no test target named `timelines_endpoints_it`` (no
//! such file, no such Cargo-discovered integration test binary) — this task
//! is pure test-authoring against already-implemented, already-wired
//! behavior (tasks 1.1 through 5.2 are all `[x]` complete; see this crate's
//! `.kiro/specs/timelines/tasks.md`), so there is no separate "make it fail,
//! then make it pass" cycle beyond the file itself not existing yet.
//!
//! ## Fixture plumbing (duplicated per this crate's own documented
//! sibling-test-file convention — see `tests/timeline_service_it.rs`'s/
//! `tests/timelines_endpoints_handler_it.rs`'s own doc comments) — largely mirrors
//! those two files' established conventions, plus
//! `tests/social_graph_visibility_query_it.rs`'s `create_remote_follower`
//! convention (renamed `remote_actor_fixture` here) for a genuinely
//! resolvable remote account, needed wherever a remote author's post must
//! survive all the way to hydration (`StatusHydrator::hydrate`'s
//! `account_json` 404s for an unresolvable actor id).

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

use kawasemi::accounts::model::{ProfileField, RemoteAccount};
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::domain::{AccountRef, Id, Visibility};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::social_graph::model::{Block, Follow, Mute};
use kawasemi::social_graph::repository as sg_repository;
use kawasemi::statuses::{Status, Tag, status_repository, tag_repository};
use kawasemi::test_harness::{TestApp, spawn_test_app};
use kawasemi::timelines::endpoints::{HOME_TIMELINE_PATH, PUBLIC_TIMELINE_PATH};

// ---- Fixture plumbing ------------------------------------------------------

async fn actor_fixture(app: &TestApp, handle_str: &str) -> Id {
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
            display_name: format!("Timelines Endpoints IT {handle_str}"),
            summary: "an actor used by the timelines_endpoints_it integration test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    actor.id
}

/// A real, resolvable remote account row — mirrors
/// `tests/social_graph_visibility_query_it.rs::create_remote_follower`, so a
/// remote author's post can survive `StatusHydrator::hydrate`'s
/// `AccountService::show_account` lookup and actually appear in a returned
/// page rather than 404-ing hydration.
async fn remote_actor_fixture(app: &TestApp, handle_str: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let actor_uri = format!("https://remote.timelines-endpoints-it.example/users/{handle_str}");
    upsert_remote(
        &app.pool,
        &RemoteAccount {
            id,
            actor_uri: actor_uri.clone(),
            username: handle_str.to_string(),
            domain: "remote.timelines-endpoints-it.example".to_string(),
            display_name: format!("Timelines Endpoints IT Remote {handle_str}"),
            note: String::new(),
            url: actor_uri,
            avatar_url: None,
            header_url: None,
            fields: Vec::<ProfileField>::new(),
            bot: false,
            locked: false,
            fetched_at: now,
        },
    )
    .await
    .expect("upsert_remote fixture must succeed");
    id
}

async fn insert_status_fixture(
    app: &TestApp,
    actor_id: Id,
    visibility: Visibility,
    local: bool,
    reblog_of_id: Option<Id>,
) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let uri = format!(
        "https://timelines-endpoints-it.example/statuses/{}",
        id.as_i64()
    );
    let status = Status {
        id,
        actor_id,
        uri: uri.clone(),
        url: Some(uri),
        content: "a fixture post inserted directly by timelines_endpoints_it".to_string(),
        visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local,
        created_at: now,
        edited_at: None,
    };
    status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert status fixture must succeed");
    status
}

/// Attaches a media row reference to `status_id` — mirrors
/// `tests/timeline_candidate_repository_it.rs::only_media_narrows_to_posts_with_a_media_attachment`'s
/// own "a bare, logical-only media id is enough" precedent (`status_media`'s
/// own `media_id` is a logical reference like `statuses.actor_id`;
/// `StatusHydrator::media_json` gracefully skips an unresolvable media row
/// rather than erroring, per `src/timelines/hydrator.rs`'s own documented
/// "missing referenced row is not a hydration failure" precedent).
async fn attach_media_fixture(app: &TestApp, status_id: Id) {
    let media_id = app.runtime.ids.next_id();
    status_repository::attach_media(&app.pool, status_id, &[media_id])
        .await
        .expect("attach media fixture must succeed");
}

async fn tag_status(app: &TestApp, status_id: Id, tag_name: &str) {
    let tag_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let tag = tag_repository::upsert_tag(
        &app.pool,
        &Tag {
            id: tag_id,
            name: tag_name.to_string(),
            created_at: now,
        },
    )
    .await
    .expect("upsert tag fixture must succeed");
    tag_repository::associate_tag(&app.pool, status_id, tag.id)
        .await
        .expect("associate tag fixture must succeed");
}

async fn upsert_follow(app: &TestApp, follower: AccountRef, followee: AccountRef, reblogs: bool) {
    sg_repository::upsert_follow(
        &app.pool,
        app.runtime.ids.next_id(),
        &Follow {
            follower,
            followee,
            reblogs,
            notify: false,
            languages: Vec::new(),
            activity_id: format!(
                "https://timelines-endpoints-it.example/activities/follow-{}",
                app.runtime.ids.next_id().as_i64()
            ),
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_follow fixture must succeed");
}

async fn upsert_block(app: &TestApp, blocker: AccountRef, blocked: AccountRef) {
    sg_repository::upsert_block(
        &app.pool,
        app.runtime.ids.next_id(),
        &Block {
            blocker,
            blocked,
            activity_id: format!(
                "https://timelines-endpoints-it.example/activities/block-{}",
                app.runtime.ids.next_id().as_i64()
            ),
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_block fixture must succeed");
}

async fn upsert_mute(app: &TestApp, muter: AccountRef, muted: AccountRef) {
    sg_repository::upsert_mute(
        &app.pool,
        app.runtime.ids.next_id(),
        &Mute {
            muter,
            muted,
            notifications: false,
            expires_at: None,
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_mute fixture must succeed");
}

/// The real, fully-wired production router (`server::build_router`, exactly
/// as `spawn_test_app` itself serves) — the whole point of this file over
/// `tests/timelines_endpoints_handler_it.rs`'s own hand-built test-only router (see
/// this file's module doc comment).
fn real_router(app: &TestApp) -> Router {
    server::build_router(app.state.clone())
}

async fn register_test_app(app: &TestApp) -> Id {
    let key = app.state.config().oauth.token_hash_key.clone();
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
        now,
        NewApp {
            name: "Timelines Endpoints IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn issue_test_token(app: &TestApp, app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let key = app.state.config().oauth.token_hash_key.clone();
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        &key,
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

fn get_req(path: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder
        .body(Body::empty())
        .expect("building the test request must succeed")
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("dispatching the test request must succeed");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("reading the response body must succeed");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, headers, value)
}

fn ids_of(body: &Value) -> Vec<String> {
    body.as_array()
        .expect("response body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

/// Parses a `Link` response header's `rel="next"`/`rel="prev"` target URLs —
/// mirrors `tests/pagination_it.rs::RawResponse::link_targets`'s own
/// established parsing, adapted from a raw header string map to a real
/// `HeaderMap` (this file dispatches through `tower::ServiceExt::oneshot`,
/// not raw sockets, so headers are already structured).
fn link_targets(headers: &HeaderMap) -> (Option<String>, Option<String>) {
    let Some(raw) = headers.get(header::LINK) else {
        return (None, None);
    };
    let raw = raw.to_str().expect("Link header must be valid UTF-8");
    let mut next = None;
    let mut prev = None;
    for part in raw.split(',') {
        let part = part.trim();
        let Some(url_end) = part.find('>') else {
            continue;
        };
        let url = part[1..url_end].to_string();
        if part.contains("rel=\"next\"") {
            next = Some(url);
        } else if part.contains("rel=\"prev\"") {
            prev = Some(url);
        }
    }
    (next, prev)
}

/// Extracts the path+query portion of an absolute `Link` target URL —
/// mirrors `tests/pagination_it.rs::path_and_query`, so a follow-up request
/// can be dispatched through the same in-process router without needing the
/// `Link` URL's own scheme/host to be independently resolvable (`oneshot`
/// requests carry no real socket at all).
fn path_and_query(url: &str) -> String {
    let (_scheme, after_scheme) = url
        .split_once("://")
        .expect("Link target must be an absolute URL");
    let slash = after_scheme
        .find('/')
        .expect("Link target must carry a path after the origin");
    after_scheme[slash..].to_string()
}

// =============================================================================
// Home (Requirements 1.1-1.6, 5.1, 5.3, 5.4, 6.1-6.4, 9.1, 9.3)
// =============================================================================

#[tokio::test]
async fn home_timeline_requires_authentication_and_sufficient_scope_via_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    // No bearer token at all -> 401 (Requirements 1.6, 9.1).
    let (status, _headers, body) = send(&router, get_req(HOME_TIMELINE_PATH, None)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "unauthenticated home must be rejected: {body:?}"
    );
    // Requirement 9.4: failure responses use api-foundation's
    // Mastodon-compatible error body shape (`{"error": "..."}`) — mirrors
    // `tests/status_crud_it.rs::assert_error_shape`'s own identical check.
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body:?}"
    );

    // Authenticated, but the token is missing `read:statuses` -> 403
    // (Requirement 9.3).
    let app_id = register_test_app(&app).await;
    let viewer = actor_fixture(&app, "home_scope_viewer").await;
    let token = issue_test_token(&app, app_id, viewer, &["read:accounts"]).await;
    let (status, _headers, body) = send(&router, get_req(HOME_TIMELINE_PATH, Some(&token))).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "insufficient scope must be rejected: {body:?}"
    );
    // Requirement 9.4: same Mastodon-compatible error body shape on the
    // 403 path.
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn home_timeline_includes_self_and_followed_excludes_stranger_and_direct_via_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "home_basic_viewer").await;
    let followed = actor_fixture(&app, "home_basic_followed").await;
    let stranger = actor_fixture(&app, "home_basic_stranger").await;
    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(followed),
        true,
    )
    .await;

    let own_post = insert_status_fixture(&app, viewer, Visibility::Public, true, None).await;
    let followed_post = insert_status_fixture(&app, followed, Visibility::Public, true, None).await;
    let stranger_post = insert_status_fixture(&app, stranger, Visibility::Public, true, None).await;
    let followed_direct =
        insert_status_fixture(&app, followed, Visibility::Direct, true, None).await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, headers, body) = send(&router, get_req(HOME_TIMELINE_PATH, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(
        headers.get(header::LINK).is_some(),
        "a non-empty page must carry a Link header (Requirement 7.2)"
    );

    let ids = ids_of(&body);
    assert!(
        ids.contains(&own_post.id.as_i64().to_string()),
        "self posts must appear (Requirement 1.1): {ids:?}"
    );
    assert!(
        ids.contains(&followed_post.id.as_i64().to_string()),
        "followed posts must appear (Requirement 1.1): {ids:?}"
    );
    assert!(
        !ids.contains(&stranger_post.id.as_i64().to_string()),
        "a non-followed stranger's post must be excluded (Requirement 1.1): {ids:?}"
    );
    assert!(
        !ids.contains(&followed_direct.id.as_i64().to_string()),
        "a direct-visibility post must never appear in home (Requirement 1.3): {ids:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn home_timeline_excludes_reblogs_hidden_boosts_and_boosts_of_a_blocked_original_author_via_real_router()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "home_boost_rules_viewer").await;

    // Scenario A: `show_reblogs` disabled on the follow (Requirement 1.4).
    let hidden_booster = app.runtime.ids.next_id();
    let hidden_original_author = app.runtime.ids.next_id();
    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(hidden_booster),
        false,
    )
    .await;
    let hidden_original =
        insert_status_fixture(&app, hidden_original_author, Visibility::Public, true, None).await;
    let hidden_boost = insert_status_fixture(
        &app,
        hidden_booster,
        Visibility::Public,
        true,
        Some(hidden_original.id),
    )
    .await;

    // Scenario B: the boosted-original post's author is blocked by the
    // viewer, even though the booster itself is followed normally
    // (Requirement 6.3).
    let blocked_booster = app.runtime.ids.next_id();
    let blocked_original_author = app.runtime.ids.next_id();
    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_booster),
        true,
    )
    .await;
    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_original_author),
    )
    .await;
    let blocked_original = insert_status_fixture(
        &app,
        blocked_original_author,
        Visibility::Public,
        true,
        None,
    )
    .await;
    let blocked_boost = insert_status_fixture(
        &app,
        blocked_booster,
        Visibility::Public,
        true,
        Some(blocked_original.id),
    )
    .await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, _headers, body) = send(&router, get_req(HOME_TIMELINE_PATH, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(
        !ids.contains(&hidden_boost.id.as_i64().to_string()),
        "a boost from a show_reblogs=false follow must be excluded (Requirement 1.4): {ids:?}"
    );
    assert!(
        !ids.contains(&blocked_boost.id.as_i64().to_string()),
        "a boost of a blocked original author's post must be excluded (Requirement 6.3): {ids:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn home_timeline_excludes_blocked_and_muted_authors_via_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "home_relations_viewer").await;
    let blocked_author = app.runtime.ids.next_id();
    let muted_author = app.runtime.ids.next_id();

    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
        true,
    )
    .await;
    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(muted_author),
        true,
    )
    .await;
    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
    )
    .await;
    upsert_mute(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(muted_author),
    )
    .await;

    let blocked_post =
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    let muted_post =
        insert_status_fixture(&app, muted_author, Visibility::Public, true, None).await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, _headers, body) = send(&router, get_req(HOME_TIMELINE_PATH, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(
        !ids.contains(&blocked_post.id.as_i64().to_string()),
        "a blocked author's post must be excluded (Requirements 1.5, 6.2): {ids:?}"
    );
    assert!(
        !ids.contains(&muted_post.id.as_i64().to_string()),
        "a muted author's post must be excluded (Requirements 1.5, 6.2): {ids:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn home_timeline_private_post_visibility_follows_real_follow_state_local_and_remote_alike() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "home_priv_viewer").await;
    let local_author = actor_fixture(&app, "home_priv_local_author").await;
    let remote_author = remote_actor_fixture(&app, "home_priv_remote_author").await;

    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(local_author),
        true,
    )
    .await;
    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Remote(remote_author),
        true,
    )
    .await;

    let local_private =
        insert_status_fixture(&app, local_author, Visibility::Private, true, None).await;
    let remote_private =
        insert_status_fixture(&app, remote_author, Visibility::Private, false, None).await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, _headers, body) = send(&router, get_req(HOME_TIMELINE_PATH, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let ids = ids_of(&body);
    assert!(
        ids.contains(&local_private.id.as_i64().to_string()),
        "a private post from a followed local author must be visible via real follow state \
         (Requirements 5.1, 5.4): {ids:?}"
    );
    assert!(
        ids.contains(&remote_private.id.as_i64().to_string()),
        "a private post from a followed remote author must be visible identically to a local \
         one — no local/remote visibility judgment gap (Requirement 5.3): {ids:?}"
    );

    app.cleanup().await;
}

// =============================================================================
// Public / Local (Requirements 2.1-2.6, 3.1-3.4, 5.2, 9.2, 9.4)
// =============================================================================

#[tokio::test]
async fn public_timeline_unauthenticated_includes_public_excludes_non_public_and_boosts_via_real_router()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let author = actor_fixture(&app, "public_unauth_author").await;
    let public_post = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let unlisted_post = insert_status_fixture(&app, author, Visibility::Unlisted, true, None).await;
    let private_post = insert_status_fixture(&app, author, Visibility::Private, true, None).await;
    let boost =
        insert_status_fixture(&app, author, Visibility::Public, true, Some(public_post.id)).await;

    let (status, headers, body) = send(&router, get_req(PUBLIC_TIMELINE_PATH, None)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unauthenticated public timeline requests must be allowed (Requirement 9.2): {body:?}"
    );
    assert!(headers.get(header::LINK).is_some());

    let ids = ids_of(&body);
    assert!(ids.contains(&public_post.id.as_i64().to_string()));
    assert!(
        !ids.contains(&unlisted_post.id.as_i64().to_string()),
        "only public visibility is included (Requirement 2.1): {ids:?}"
    );
    assert!(
        !ids.contains(&private_post.id.as_i64().to_string()),
        "an unauthenticated viewer must see only public posts (Requirements 5.2, 9.2): {ids:?}"
    );
    assert!(
        !ids.contains(&boost.id.as_i64().to_string()),
        "boosts are excluded from the public timeline (Requirement 2.2): {ids:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn public_timeline_local_remote_only_media_filters_via_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let local_author = actor_fixture(&app, "public_filters_local_author").await;
    let remote_author = remote_actor_fixture(&app, "public_filters_remote_author").await;

    let local_post =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    let remote_post =
        insert_status_fixture(&app, remote_author, Visibility::Public, false, None).await;
    let media_post =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    attach_media_fixture(&app, media_post.id).await;

    // `local=true` narrows to local authors only (Requirement 2.3).
    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/public?local=true", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(ids.contains(&local_post.id.as_i64().to_string()));
    assert!(!ids.contains(&remote_post.id.as_i64().to_string()));

    // `remote=true` narrows to remote authors only (Requirement 2.4).
    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/public?remote=true", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(ids.contains(&remote_post.id.as_i64().to_string()));
    assert!(!ids.contains(&local_post.id.as_i64().to_string()));

    // `only_media=true` narrows to posts carrying a media attachment
    // (Requirement 2.5).
    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/public?only_media=true", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(ids.contains(&media_post.id.as_i64().to_string()));
    assert!(!ids.contains(&local_post.id.as_i64().to_string()));
    assert!(!ids.contains(&remote_post.id.as_i64().to_string()));

    app.cleanup().await;
}

#[tokio::test]
async fn public_timeline_excludes_blocked_and_muted_authors_for_an_authenticated_viewer_via_real_router()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "public_relations_viewer").await;
    let blocked_author = actor_fixture(&app, "public_relations_blocked").await;
    let muted_author = actor_fixture(&app, "public_relations_muted").await;
    let stranger = actor_fixture(&app, "public_relations_stranger").await;

    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
    )
    .await;
    upsert_mute(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(muted_author),
    )
    .await;

    let blocked_post =
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    let muted_post =
        insert_status_fixture(&app, muted_author, Visibility::Public, true, None).await;
    let stranger_post = insert_status_fixture(&app, stranger, Visibility::Public, true, None).await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, _h, body) = send(&router, get_req(PUBLIC_TIMELINE_PATH, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(
        !ids.contains(&blocked_post.id.as_i64().to_string()),
        "Requirement 2.6: {ids:?}"
    );
    assert!(
        !ids.contains(&muted_post.id.as_i64().to_string()),
        "Requirement 2.6: {ids:?}"
    );
    assert!(ids.contains(&stranger_post.id.as_i64().to_string()));

    app.cleanup().await;
}

#[tokio::test]
async fn local_timeline_excludes_remote_authors_and_boosts_supports_only_media_via_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let local_author = actor_fixture(&app, "local_tl_author").await;
    let remote_author = remote_actor_fixture(&app, "local_tl_remote_author").await;

    let local_post =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    let remote_post =
        insert_status_fixture(&app, remote_author, Visibility::Public, false, None).await;
    let boost = insert_status_fixture(
        &app,
        local_author,
        Visibility::Public,
        true,
        Some(local_post.id),
    )
    .await;
    let media_post =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    attach_media_fixture(&app, media_post.id).await;

    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/public?local=true", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(ids.contains(&local_post.id.as_i64().to_string()));
    assert!(
        !ids.contains(&remote_post.id.as_i64().to_string()),
        "Requirement 3.1: {ids:?}"
    );
    assert!(
        !ids.contains(&boost.id.as_i64().to_string()),
        "Requirement 3.2: {ids:?}"
    );

    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/public?local=true&only_media=true", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(
        ids.contains(&media_post.id.as_i64().to_string()),
        "Requirement 3.4: {ids:?}"
    );
    assert!(!ids.contains(&local_post.id.as_i64().to_string()));

    app.cleanup().await;
}

#[tokio::test]
async fn local_timeline_excludes_blocked_and_muted_authors_for_an_authenticated_viewer_via_real_router()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "local_relations_viewer").await;
    let blocked_author = actor_fixture(&app, "local_relations_blocked").await;
    let muted_author = actor_fixture(&app, "local_relations_muted").await;
    let stranger = actor_fixture(&app, "local_relations_stranger").await;

    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
    )
    .await;
    upsert_mute(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(muted_author),
    )
    .await;

    let blocked_post =
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    let muted_post =
        insert_status_fixture(&app, muted_author, Visibility::Public, true, None).await;
    let stranger_post = insert_status_fixture(&app, stranger, Visibility::Public, true, None).await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/public?local=true", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(
        !ids.contains(&blocked_post.id.as_i64().to_string()),
        "Requirement 3.3: {ids:?}"
    );
    assert!(
        !ids.contains(&muted_post.id.as_i64().to_string()),
        "Requirement 3.3: {ids:?}"
    );
    assert!(ids.contains(&stranger_post.id.as_i64().to_string()));

    app.cleanup().await;
}

// =============================================================================
// Tag (Requirements 4.1-4.6)
// =============================================================================

#[tokio::test]
async fn tag_timeline_matches_normalized_tag_case_insensitively_and_excludes_boosts_via_real_router()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let author = actor_fixture(&app, "tag_basic_author").await;
    let tagged = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, tagged.id, "rust").await;
    let boost =
        insert_status_fixture(&app, author, Visibility::Public, true, Some(tagged.id)).await;
    tag_status(&app, boost.id, "rust").await;
    let untagged = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let (status, headers, body) = send(&router, get_req("/api/v1/timelines/tag/RuSt", None)).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(headers.get(header::LINK).is_some());
    let ids = ids_of(&body);
    assert!(
        ids.contains(&tagged.id.as_i64().to_string()),
        "case-insensitive normalized matching (Requirements 4.1, 4.2): {ids:?}"
    );
    assert!(!ids.contains(&untagged.id.as_i64().to_string()));
    assert!(
        !ids.contains(&boost.id.as_i64().to_string()),
        "boosts are excluded from tag timelines (Requirement 4.3): {ids:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn tag_timeline_any_all_none_combination_via_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let author = actor_fixture(&app, "tag_combo_author").await;

    // `any`: at least one of the extra tags must be present.
    let matches_any = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, matches_any.id, "rust").await;
    tag_status(&app, matches_any.id, "rustlang").await;
    let misses_any = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, misses_any.id, "rust").await;

    let (status, _h, body) = send(
        &router,
        get_req(
            "/api/v1/timelines/tag/rust?any[]=rustlang&any[]=programming",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(ids.contains(&matches_any.id.as_i64().to_string()));
    assert!(!ids.contains(&misses_any.id.as_i64().to_string()));

    // `all`: every extra tag must be present.
    let matches_all = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, matches_all.id, "rust").await;
    tag_status(&app, matches_all.id, "async").await;
    tag_status(&app, matches_all.id, "tokio").await;
    let misses_all = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, misses_all.id, "rust").await;
    tag_status(&app, misses_all.id, "async").await;

    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/tag/rust?all[]=async&all[]=tokio", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(ids.contains(&matches_all.id.as_i64().to_string()));
    assert!(!ids.contains(&misses_all.id.as_i64().to_string()));

    // `none`: excludes any post carrying the given tag.
    let excluded_by_none =
        insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, excluded_by_none.id, "rust").await;
    tag_status(&app, excluded_by_none.id, "spam").await;
    let kept_by_none = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, kept_by_none.id, "rust").await;

    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/tag/rust?none[]=spam", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(
        !ids.contains(&excluded_by_none.id.as_i64().to_string()),
        "Requirement 4.4 (none): {ids:?}"
    );
    assert!(ids.contains(&kept_by_none.id.as_i64().to_string()));

    app.cleanup().await;
}

#[tokio::test]
async fn tag_timeline_local_and_only_media_filters_via_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let local_author = actor_fixture(&app, "tag_filters_local_author").await;
    let remote_author = remote_actor_fixture(&app, "tag_filters_remote_author").await;

    let local_tagged =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    tag_status(&app, local_tagged.id, "rust").await;
    let remote_tagged =
        insert_status_fixture(&app, remote_author, Visibility::Public, false, None).await;
    tag_status(&app, remote_tagged.id, "rust").await;
    let media_tagged =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    tag_status(&app, media_tagged.id, "rust").await;
    attach_media_fixture(&app, media_tagged.id).await;

    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/tag/rust?local=true", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(ids.contains(&local_tagged.id.as_i64().to_string()));
    assert!(
        !ids.contains(&remote_tagged.id.as_i64().to_string()),
        "Requirement 4.5 (local): {ids:?}"
    );

    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/tag/rust?only_media=true", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(ids.contains(&media_tagged.id.as_i64().to_string()));
    assert!(
        !ids.contains(&local_tagged.id.as_i64().to_string()),
        "Requirement 4.5 (only_media): {ids:?}"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn tag_timeline_excludes_blocked_and_muted_authors_for_an_authenticated_viewer_via_real_router()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "tag_relations_viewer").await;
    let blocked_author = actor_fixture(&app, "tag_relations_blocked").await;
    let muted_author = actor_fixture(&app, "tag_relations_muted").await;

    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
    )
    .await;
    upsert_mute(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(muted_author),
    )
    .await;

    let blocked_post =
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    tag_status(&app, blocked_post.id, "rust").await;
    let muted_post =
        insert_status_fixture(&app, muted_author, Visibility::Public, true, None).await;
    tag_status(&app, muted_post.id, "rust").await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, _h, body) =
        send(&router, get_req("/api/v1/timelines/tag/rust", Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert!(
        !ids.contains(&blocked_post.id.as_i64().to_string()),
        "Requirement 4.6: {ids:?}"
    );
    assert!(
        !ids.contains(&muted_post.id.as_i64().to_string()),
        "Requirement 4.6: {ids:?}"
    );

    app.cleanup().await;
}

// =============================================================================
// Pagination (Requirements 7.1-7.4)
// =============================================================================

#[tokio::test]
async fn pagination_max_id_since_id_min_id_and_link_header_are_navigable_via_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let author = actor_fixture(&app, "pagination_author").await;

    let mut posts = Vec::new();
    for _ in 0..5 {
        posts.push(insert_status_fixture(&app, author, Visibility::Public, true, None).await);
    }
    // `posts[0]` is oldest, `posts[4]` is newest.

    // -- `max_id` + `Link` header navigation across all 3 pages of limit=2 -
    let (status, headers, body) =
        send(&router, get_req("/api/v1/timelines/public?limit=2", None)).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let page1_ids = ids_of(&body);
    assert_eq!(
        page1_ids,
        vec![
            posts[4].id.as_i64().to_string(),
            posts[3].id.as_i64().to_string()
        ]
    );
    let (next1, _prev1) = link_targets(&headers);
    let next1 = next1.expect("page 1 of 3 must carry a Link next target (Requirement 7.2)");

    let (status, headers, body) = send(&router, get_req(&path_and_query(&next1), None)).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let page2_ids = ids_of(&body);
    assert_eq!(
        page2_ids,
        vec![
            posts[2].id.as_i64().to_string(),
            posts[1].id.as_i64().to_string()
        ]
    );
    for id in &page2_ids {
        assert!(
            !page1_ids.contains(id),
            "page 2 must not repeat a page 1 id (Requirement 7.4): {page1_ids:?} / {page2_ids:?}"
        );
    }
    let (next2, _prev2) = link_targets(&headers);
    let next2 = next2.expect("page 2 of 3 must carry a Link next target");

    let (status, _headers, body) = send(&router, get_req(&path_and_query(&next2), None)).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let page3_ids = ids_of(&body);
    assert_eq!(page3_ids, vec![posts[0].id.as_i64().to_string()]);

    // -- `since_id`: anchored at the newest end (Requirement 7.3) ----------
    let (status, _h, body) = send(
        &router,
        get_req(
            &format!(
                "/api/v1/timelines/public?since_id={}&limit=10",
                posts[1].id.as_i64()
            ),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        ids_of(&body),
        vec![
            posts[4].id.as_i64().to_string(),
            posts[3].id.as_i64().to_string(),
            posts[2].id.as_i64().to_string()
        ]
    );

    // -- `min_id`: anchored at the oldest end (Requirement 7.3) ------------
    let (status, _h, body) = send(
        &router,
        get_req(
            &format!(
                "/api/v1/timelines/public?min_id={}&limit=1",
                posts[0].id.as_i64()
            ),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(ids_of(&body), vec![posts[1].id.as_i64().to_string()]);

    app.cleanup().await;
}

#[tokio::test]
async fn pagination_fill_after_filter_has_no_duplicate_or_gap_across_pages_via_real_router() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "pagination_fill_viewer").await;
    let blocked_author = app.runtime.ids.next_id();
    let visible_author = actor_fixture(&app, "pagination_fill_visible_author").await;

    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
    )
    .await;

    // Mirrors `tests/timeline_service_it.rs::fill_loop_pulls_a_second_batch_
    // when_the_first_is_mostly_filtered_out`'s own fixture ordering: 3
    // visible posts (oldest), then 13 blocked-author posts (newer) — one
    // more than `batch_limit` (`limit(3) * BATCH_LIMIT_MULTIPLIER(4) = 12`)
    // — so the first batch alone is entirely filtered out and a second
    // fetch is genuinely required to reach the 3 visible posts.
    let mut visible_posts = Vec::new();
    for _ in 0..3 {
        visible_posts.push(
            insert_status_fixture(&app, visible_author, Visibility::Public, true, None).await,
        );
    }
    for _ in 0..13 {
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    }

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, _h, body) = send(
        &router,
        get_req("/api/v1/timelines/public?limit=3", Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids = ids_of(&body);
    assert_eq!(
        ids.len(),
        3,
        "the fill loop must pull a second batch to satisfy `limit` after the first batch was \
         entirely filtered out (Requirement 7.4): {ids:?}"
    );
    for post in &visible_posts {
        assert!(ids.contains(&post.id.as_i64().to_string()));
    }

    app.cleanup().await;
}

#[tokio::test]
async fn pagination_iteration_cap_returns_partial_page_with_valid_continuation_cursor_via_real_router()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "pagination_cap_viewer").await;
    let blocked_author = app.runtime.ids.next_id();
    let visible_author = actor_fixture(&app, "pagination_cap_visible_author").await;

    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
    )
    .await;

    // Mirrors `tests/timeline_service_it.rs::iteration_cap_returns_a_
    // partial_page_with_a_valid_continuation_cursor`'s own fixture: one
    // visible post far below the scanned window, then 25 blocked-author
    // posts above it. `limit=1` -> `batch_limit=4`, `MAX_FILL_ITERATIONS=5`
    // -> at most 20 candidates scanned, all from the blocked author — the
    // cap is hit with zero survivors, so this exercises the "cap-hit
    // cursor override" path specifically (Requirement 7.4).
    let visible_post =
        insert_status_fixture(&app, visible_author, Visibility::Public, true, None).await;
    for _ in 0..25 {
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    }

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, headers, body) = send(
        &router,
        get_req("/api/v1/timelines/public?limit=1", Some(&token)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the iteration cap must yield a partial page, not an error (Requirement 7.4): {body:?}"
    );
    assert!(
        ids_of(&body).is_empty(),
        "every candidate scanned within the iteration cap came from the blocked author: {body:?}"
    );

    let (next, _prev) = link_targets(&headers);
    let next = next.expect(
        "a cap-hit page must still carry a valid, resumable Link next target (Requirement 7.4), \
         not none, even when zero candidates survived filtering",
    );

    let (status, _h, body) = send(&router, get_req(&path_and_query(&next), Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        ids_of(&body),
        vec![visible_post.id.as_i64().to_string()],
        "resuming from the cap-hit cursor must reach the previously-unscanned visible post"
    );

    app.cleanup().await;
}
