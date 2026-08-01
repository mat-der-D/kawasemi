//! Integration tests for timelines task 6.2 (`.kiro/specs/timelines/tasks.md`,
//! "6.2 (P) 単一生成点の一致と契約テストを整備する", `_Boundary:
//! TimelineMatcher, StatusHydrator_`): part (a) of that task's own dispatch
//! brief — "REST タイムライン取得結果と `TimelineMatcher.matches` の単一投稿
//! 判定が同一 membership になることを検証" (Requirements 8.1, 8.2). Part (b)
//! (Status-JSON contract conformance, Requirements 10.1-10.4) lives in the
//! sibling file `tests/timeline_status_contract_it.rs` — see that file's own
//! doc comment for why the two are split.
//!
//! ## Why this file, given `tests/timeline_service_it.rs`,
//! `src/timelines/matcher.rs`'s own unit tests, and
//! `tests/timelines_endpoints_it.rs` (task 6.1) already exist
//! - `src/timelines/matcher.rs`'s own `#[cfg(test)] mod tests` (task 3.2,
//!   already reviewed) proves `TimelineMatcher::matches` agrees with a
//!   locally-reconstructed composition of `TimelineKindRules::matches` +
//!   `TimelineFilter::keep` — i.e. internal self-consistency of `matches`'s
//!   own implementation against its own two collaborators, entirely in
//!   memory, no HTTP, no `TimelineService`, no `CandidateRepository` SQL.
//! - `tests/timeline_service_it.rs` (task 4.2) and `tests/
//!   timelines_endpoints_it.rs` (task 6.1) both prove `TimelineService`/the
//!   real HTTP surface returns the *correct* set of posts for a given
//!   scenario — but neither ever calls `TimelineMatcher::matches` itself, so
//!   neither can catch a scenario where the REST candidate-query +
//!   `TimelineFilter::keep` path and the `matches` path have silently
//!   diverged (e.g. a future edit to one call site that forgets to mirror
//!   the other).
//!
//! This file closes that gap: for a real, `spawn_test_app`-booted instance,
//! it independently calls `TimelineMatcher::matches` — the exact seam a
//! downstream `streaming` spec will call per Requirement 8.3 — for every
//! fixture status in a scenario, and asserts its verdict agrees, status by
//! status, with whether the *real* REST endpoint (driven through
//! `kawasemi::server::build_router`, the same production router `tests/
//! timelines_endpoints_it.rs` already established as this crate's own
//! "real, fully-wired router" precedent) actually returned that status. Any
//! future change to `TimelineService`/`CandidateRepository`/`TimelineFilter`
//! on one side, or to `TimelineMatcher::matches` on the other, that makes the
//! two disagree fails here even if both individually still "look correct" in
//! isolation — the single-generation-point guarantee Requirement 8.1/8.2
//! exists for.
//!
//! ## A documented, deliberate scope boundary: `local`/`remote`/`only_media`
//! Reading `src/timelines/kind_rules.rs::TimelineKindRules` in full: the
//! per-kind structural condition `TimelineMatcher::matches` composes never
//! reads `TimelineParams.local`/`.remote`/`.only_media` for `Public`/`Home`/
//! `Tag` at all (only `Local`'s own `candidate.local` check, which *is* part
//! of the kind condition itself, not a narrowing knob). Those three flags are
//! applied *only* by `CandidateRepository::fetch_candidates`'s SQL (the REST
//! candidate-retrieval path) — confirmed by `tasks.md`'s own Implementation
//! Notes entry for task 2.1 ("`local`/`remote`/`only_media` 絞り込みを 4 種
//! 別すべてに一様適用した"). This is not an oversight this task's own
//! dispatch brief asks to "fix": Requirement 8.2's shared-condition text
//! names "フィルタ・可視性・タイムライン種別条件" (filter / visibility /
//! kind condition) — not per-request display-narrowing flags a client
//! happens to pass on a given page fetch, which a downstream Streaming
//! subscription would apply as its own separate subscription-level filter on
//! top of the base single-post membership judgment, not bake into `matches`
//! itself. Every scenario below therefore either leaves `local`/`remote`/
//! `only_media` at their default `false` (`Home`, `Public`, `Tag` scenarios)
//! or uses `local=true` only for the `Local`-kind-*selection* case, where
//! `TimelineKindRules::matches_local`'s own `candidate.local` check already
//! covers the identical condition on both sides (REST's redundant SQL
//! narrowing and `matches`'s own kind check agree by construction, not by
//! coincidence) — deliberately never exercising a `remote=true`/
//! `only_media=true` request, which would produce a *documented*, expected
//! REST/`matches` disagreement that is not this task's boundary to close.
//!
//! ## Fixture plumbing
//! Duplicated per this crate's own established sibling-test-file convention
//! (`tests/timelines_endpoints_it.rs`'s own doc comment: "each `tests/*.rs`
//! file is its own compiled crate") — `actor_fixture`/`remote_actor_fixture`/
//! `insert_status_fixture`/`tag_status`/`upsert_follow`/`upsert_block`/
//! `upsert_mute`/`real_router`/`register_test_app`/`issue_test_token`/
//! `get_req`/`send`/`ids_of` all mirror `tests/timelines_endpoints_it.rs`'s
//! own identical helpers verbatim.

use std::collections::HashSet;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
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
use kawasemi::social_graph::providers::FilterQuery;
use kawasemi::social_graph::repository as sg_repository;
use kawasemi::statuses::{Status, status_repository, tag_repository};
use kawasemi::test_harness::{TestApp, spawn_test_app};
use kawasemi::timelines::endpoints::{HOME_TIMELINE_PATH, PUBLIC_TIMELINE_PATH};
use kawasemi::timelines::matcher::TimelineMatcher;
use kawasemi::timelines::model::{FilterContext, TagFilter, TimelineKind, TimelineParams};

// ---- Fixture plumbing (mirrors tests/timelines_endpoints_it.rs verbatim) --

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
            display_name: format!("Matcher REST Equivalence IT {handle_str}"),
            summary: "an actor used by the timeline_matcher_rest_equivalence_it integration test"
                .to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    actor.id
}

/// A real, resolvable remote account row — needed wherever a remote author's
/// post must survive `StatusHydrator::hydrate`'s `AccountService::
/// show_account` lookup and actually appear in a returned page (mirrors
/// `tests/timelines_endpoints_it.rs::remote_actor_fixture`).
async fn remote_actor_fixture(app: &TestApp, handle_str: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let actor_uri = format!("https://remote.matcher-rest-equiv-it.example/users/{handle_str}");
    upsert_remote(
        &app.pool,
        &RemoteAccount {
            id,
            actor_uri: actor_uri.clone(),
            username: handle_str.to_string(),
            domain: "remote.matcher-rest-equiv-it.example".to_string(),
            display_name: format!("Matcher REST Equivalence IT Remote {handle_str}"),
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
        "https://matcher-rest-equiv-it.example/statuses/{}",
        id.as_i64()
    );
    let status = Status {
        id,
        actor_id,
        uri: uri.clone(),
        url: Some(uri),
        content: "a fixture post inserted directly by timeline_matcher_rest_equivalence_it"
            .to_string(),
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

async fn tag_status(app: &TestApp, status_id: Id, tag_name: &str) {
    let tag_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let tag = kawasemi::statuses::Tag {
        id: tag_id,
        name: tag_name.to_string(),
        created_at: now,
    };
    let tag = tag_repository::upsert_tag(&app.pool, &tag)
        .await
        .expect("upsert_tag fixture must succeed");
    tag_repository::associate_tag(&app.pool, status_id, tag.id)
        .await
        .expect("associate_tag fixture must succeed");
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
                "https://matcher-rest-equiv-it.example/activities/follow-{}",
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
                "https://matcher-rest-equiv-it.example/activities/block-{}",
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
            name: "Matcher REST Equivalence IT Client".to_string(),
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

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("dispatching the test request must succeed");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("reading the response body must succeed");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

fn ids_of(body: &Value) -> Vec<String> {
    body.as_array()
        .expect("response body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

// ---- FilterContext construction (mirrors `TimelineService::
// build_filter_context`, `src/timelines/service.rs`, exactly — deliberately
// duplicated rather than made `pub(crate)` and imported, since this file
// must independently reconstruct what a real caller does, not borrow its
// implementation) ------------------------------------------------------------

fn to_id_set(refs: Vec<AccountRef>) -> HashSet<Id> {
    refs.into_iter()
        .map(|account_ref| match account_ref {
            AccountRef::Local(id) | AccountRef::Remote(id) => id,
        })
        .collect()
}

async fn build_filter_context(app: &TestApp, viewer_id: Option<Id>) -> FilterContext {
    let now = app.runtime.clock.now();
    let Some(viewer_id) = viewer_id else {
        return FilterContext {
            viewer: None,
            blocked: HashSet::new(),
            blocked_by: HashSet::new(),
            muted: HashSet::new(),
            following: HashSet::new(),
            reblogs_hidden: HashSet::new(),
            now,
        };
    };

    let viewer_ref = AccountRef::Local(viewer_id);
    let query = FilterQuery::new(app.pool.clone(), app.runtime.clone());
    let relationship_sets = query
        .blocked_set(&viewer_ref)
        .await
        .expect("blocked_set must succeed");
    let following = query
        .following_set(&viewer_ref)
        .await
        .expect("following_set must succeed");
    let reblogs_hidden = query
        .reblogs_hidden_set(&viewer_ref)
        .await
        .expect("reblogs_hidden_set must succeed");

    FilterContext {
        viewer: Some(viewer_id),
        blocked: to_id_set(relationship_sets.blocked),
        blocked_by: to_id_set(relationship_sets.blocked_by),
        muted: to_id_set(relationship_sets.muted),
        following: to_id_set(following),
        reblogs_hidden: to_id_set(reblogs_hidden),
        now,
    }
}

/// Resolves a boost candidate's boosted-original post's author id — mirrors
/// `TimelineService::resolve_reblogged_author`'s identical lookup.
async fn resolve_reblogged_author(app: &TestApp, reblog_of_id: Option<Id>) -> Option<Id> {
    let target_id = reblog_of_id?;
    let target = status_repository::find_by_id(&app.pool, target_id)
        .await
        .expect("find_by_id must succeed");
    target.map(|status| status.actor_id)
}

async fn tags_of(app: &TestApp, status_id: Id) -> HashSet<String> {
    tag_repository::tags_for_status(&app.pool, status_id)
        .await
        .expect("tags_for_status must succeed")
        .into_iter()
        .map(|tag| tag.name)
        .collect()
}

fn empty_params() -> TimelineParams {
    TimelineParams {
        local: false,
        remote: false,
        only_media: false,
        tag: None,
        page: kawasemi::api::pagination::PageParams {
            max_id: None,
            since_id: None,
            min_id: None,
            limit: Some(40),
        },
    }
}

/// The single-generation-point assertion this whole file exists to make:
/// independently recomputes `TimelineMatcher::matches` for `status` and
/// compares it against whether `rest_ids` (the real REST response's own id
/// list) actually contains it (Requirements 8.1, 8.2).
async fn assert_matcher_agrees_with_rest(
    app: &TestApp,
    matcher: TimelineMatcher,
    kind: TimelineKind,
    params: &TimelineParams,
    ctx: &FilterContext,
    status: &Status,
    rest_ids: &[String],
) {
    let tags = tags_of(app, status.id).await;
    let reblogged_author = resolve_reblogged_author(app, status.reblog_of_id).await;
    let matcher_result = matcher.matches(status, kind, params, &tags, reblogged_author, ctx);
    let rest_result = rest_ids.contains(&status.id.as_i64().to_string());
    assert_eq!(
        matcher_result, rest_result,
        "single-generation-point violation (Requirement 8.1, 8.2): REST membership \
         ({rest_result}) disagreed with TimelineMatcher::matches ({matcher_result}) for status \
         {:?}, kind {:?}",
        status.id, kind
    );
}

// =============================================================================
// Home
// =============================================================================

/// A mixed relationship scenario exercising every exclusion reason the
/// task's own dispatch brief names — visibility (`direct`), block, mute,
/// boost-hidden (`show_reblogs=false`), and boosted-original-author block
/// (Requirement 6.3) — plus a followed-author inclusion and a self-post
/// inclusion, proving REST and `TimelineMatcher::matches` agree on every one.
#[tokio::test]
async fn home_timeline_rest_membership_matches_timeline_matcher_across_relationship_scenarios() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "equiv_home_viewer").await;
    let followed = actor_fixture(&app, "equiv_home_followed").await;
    let reblogs_hidden_author = actor_fixture(&app, "equiv_home_reblogs_hidden").await;
    let blocked_author = actor_fixture(&app, "equiv_home_blocked").await;
    let muted_author = actor_fixture(&app, "equiv_home_muted").await;
    let stranger = actor_fixture(&app, "equiv_home_stranger").await;
    let blocked_original_author = actor_fixture(&app, "equiv_home_blocked_original").await;

    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(followed),
        true,
    )
    .await;
    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(reblogs_hidden_author),
        false,
    )
    .await;
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
    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_original_author),
    )
    .await;

    let own_post = insert_status_fixture(&app, viewer, Visibility::Public, true, None).await;
    let followed_post = insert_status_fixture(&app, followed, Visibility::Public, true, None).await;
    let followed_direct =
        insert_status_fixture(&app, followed, Visibility::Direct, true, None).await;
    let hidden_original =
        insert_status_fixture(&app, stranger, Visibility::Public, true, None).await;
    let hidden_boost = insert_status_fixture(
        &app,
        reblogs_hidden_author,
        Visibility::Public,
        true,
        Some(hidden_original.id),
    )
    .await;
    let blocked_post =
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    let muted_post =
        insert_status_fixture(&app, muted_author, Visibility::Public, true, None).await;
    let stranger_post = insert_status_fixture(&app, stranger, Visibility::Public, true, None).await;
    let blocked_original = insert_status_fixture(
        &app,
        blocked_original_author,
        Visibility::Public,
        true,
        None,
    )
    .await;
    let boost_of_blocked_original = insert_status_fixture(
        &app,
        followed,
        Visibility::Public,
        true,
        Some(blocked_original.id),
    )
    .await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, body) = send(
        &router,
        get_req(&format!("{HOME_TIMELINE_PATH}?limit=40"), Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rest_ids = ids_of(&body);

    // Sanity: the scenario's own expected memberships (documents *why* each
    // status is in/out, independent of the matcher-agreement check below).
    assert!(rest_ids.contains(&own_post.id.as_i64().to_string()));
    assert!(rest_ids.contains(&followed_post.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&followed_direct.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&hidden_boost.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&blocked_post.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&muted_post.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&stranger_post.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&boost_of_blocked_original.id.as_i64().to_string()));

    let ctx = build_filter_context(&app, Some(viewer)).await;
    let params = empty_params();
    let matcher = app.state.timelines().matcher();

    for candidate in [
        &own_post,
        &followed_post,
        &followed_direct,
        &hidden_original,
        &hidden_boost,
        &blocked_post,
        &muted_post,
        &stranger_post,
        &blocked_original,
        &boost_of_blocked_original,
    ] {
        assert_matcher_agrees_with_rest(
            &app,
            matcher,
            TimelineKind::Home,
            &params,
            &ctx,
            candidate,
            &rest_ids,
        )
        .await;
    }

    app.cleanup().await;
}

// =============================================================================
// Public / Local
// =============================================================================

/// Unauthenticated `public` — visibility exclusion (`unlisted`/`private`/
/// `direct`) and boost exclusion, both kind-structural conditions
/// `TimelineMatcher::matches` and REST must agree on identically with no
/// viewer at all (`ctx.viewer = None`, Requirement 9.2).
#[tokio::test]
async fn public_timeline_rest_membership_matches_timeline_matcher_for_unauthenticated_visibility_and_boost_exclusion()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let author = actor_fixture(&app, "equiv_public_author").await;
    let remote_author = remote_actor_fixture(&app, "equiv_public_remote_author").await;

    let public_post = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let unlisted_post = insert_status_fixture(&app, author, Visibility::Unlisted, true, None).await;
    let private_post = insert_status_fixture(&app, author, Visibility::Private, true, None).await;
    let direct_post = insert_status_fixture(&app, author, Visibility::Direct, true, None).await;
    let boost =
        insert_status_fixture(&app, author, Visibility::Public, true, Some(public_post.id)).await;
    let remote_public_post =
        insert_status_fixture(&app, remote_author, Visibility::Public, false, None).await;

    let (status, body) = send(
        &router,
        get_req(&format!("{PUBLIC_TIMELINE_PATH}?limit=40"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rest_ids = ids_of(&body);

    assert!(rest_ids.contains(&public_post.id.as_i64().to_string()));
    assert!(rest_ids.contains(&remote_public_post.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&unlisted_post.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&private_post.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&direct_post.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&boost.id.as_i64().to_string()));

    let ctx = build_filter_context(&app, None).await;
    let params = empty_params();
    let matcher = app.state.timelines().matcher();

    for candidate in [
        &public_post,
        &unlisted_post,
        &private_post,
        &direct_post,
        &boost,
        &remote_public_post,
    ] {
        assert_matcher_agrees_with_rest(
            &app,
            matcher,
            TimelineKind::Public,
            &params,
            &ctx,
            candidate,
            &rest_ids,
        )
        .await;
    }

    app.cleanup().await;
}

/// `local=true` selects `TimelineKind::Local` (see this module's doc
/// comment, "Kind selection for public/local"), whose own kind condition
/// additionally requires `candidate.local` — REST and `matches` must still
/// agree, including for an authenticated viewer with a blocked local author
/// (Requirement 8.2's relationship-exclusion coverage extended to Local).
#[tokio::test]
async fn public_timeline_local_true_rest_membership_matches_timeline_matcher_for_local_kind_selection()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "equiv_local_viewer").await;
    let local_author = actor_fixture(&app, "equiv_local_author").await;
    let blocked_local_author = actor_fixture(&app, "equiv_local_blocked_author").await;
    let remote_author = remote_actor_fixture(&app, "equiv_local_remote_author").await;

    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_local_author),
    )
    .await;

    let local_post =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    let remote_post =
        insert_status_fixture(&app, remote_author, Visibility::Public, false, None).await;
    let blocked_local_post =
        insert_status_fixture(&app, blocked_local_author, Visibility::Public, true, None).await;
    let local_boost = insert_status_fixture(
        &app,
        local_author,
        Visibility::Public,
        true,
        Some(local_post.id),
    )
    .await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, body) = send(
        &router,
        get_req(
            &format!("{PUBLIC_TIMELINE_PATH}?local=true&limit=40"),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rest_ids = ids_of(&body);

    assert!(rest_ids.contains(&local_post.id.as_i64().to_string()));
    assert!(
        !rest_ids.contains(&remote_post.id.as_i64().to_string()),
        "Local excludes remote authors: {rest_ids:?}"
    );
    assert!(
        !rest_ids.contains(&blocked_local_post.id.as_i64().to_string()),
        "a blocked local author's post must be excluded: {rest_ids:?}"
    );
    assert!(!rest_ids.contains(&local_boost.id.as_i64().to_string()));

    let ctx = build_filter_context(&app, Some(viewer)).await;
    let mut params = empty_params();
    params.local = true;
    let matcher = app.state.timelines().matcher();

    for candidate in [&local_post, &remote_post, &blocked_local_post, &local_boost] {
        assert_matcher_agrees_with_rest(
            &app,
            matcher,
            TimelineKind::Local,
            &params,
            &ctx,
            candidate,
            &rest_ids,
        )
        .await;
    }

    app.cleanup().await;
}

// =============================================================================
// Tag
// =============================================================================

/// Tag mismatch, boost exclusion, and (for an authenticated viewer) block/
/// mute relationship exclusion, all on the tag timeline — the fourth and
/// final `TimelineKind`, closing out the matrix the task's own dispatch
/// brief names ("visibility・ブロック/ミュート・direct・ブースト非表示・タグ
/// 不一致").
#[tokio::test]
async fn tag_timeline_rest_membership_matches_timeline_matcher_for_tag_conditions_and_relationships()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let app_id = register_test_app(&app).await;

    let viewer = actor_fixture(&app, "equiv_tag_viewer").await;
    let author = actor_fixture(&app, "equiv_tag_author").await;
    let blocked_author = actor_fixture(&app, "equiv_tag_blocked_author").await;
    let muted_author = actor_fixture(&app, "equiv_tag_muted_author").await;

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

    let matches_primary = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, matches_primary.id, "rust").await;

    let matches_any = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, matches_any.id, "rust").await;
    tag_status(&app, matches_any.id, "rustlang").await;

    let untagged = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let mismatched_tag = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, mismatched_tag.id, "ruby").await;

    let excluded_by_none =
        insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, excluded_by_none.id, "rust").await;
    tag_status(&app, excluded_by_none.id, "spam").await;

    let boosted_tagged = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, boosted_tagged.id, "rust").await;
    let tagged_boost = insert_status_fixture(
        &app,
        author,
        Visibility::Public,
        true,
        Some(boosted_tagged.id),
    )
    .await;
    tag_status(&app, tagged_boost.id, "rust").await;

    let blocked_tagged =
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    tag_status(&app, blocked_tagged.id, "rust").await;

    let muted_tagged =
        insert_status_fixture(&app, muted_author, Visibility::Public, true, None).await;
    tag_status(&app, muted_tagged.id, "rust").await;

    let token = issue_test_token(&app, app_id, viewer, &["read:statuses"]).await;
    let (status, body) = send(
        &router,
        get_req(
            "/api/v1/timelines/tag/rust?any[]=rustlang&none[]=spam&limit=40",
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rest_ids = ids_of(&body);

    assert!(
        !rest_ids.contains(&matches_primary.id.as_i64().to_string()),
        "any[]=rustlang requires rustlang too, primary-only must not satisfy any: {rest_ids:?}"
    );
    assert!(rest_ids.contains(&matches_any.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&untagged.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&mismatched_tag.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&excluded_by_none.id.as_i64().to_string()));
    assert!(
        !rest_ids.contains(&tagged_boost.id.as_i64().to_string()),
        "boosts are excluded from tag timelines even when tagged: {rest_ids:?}"
    );
    assert!(!rest_ids.contains(&blocked_tagged.id.as_i64().to_string()));
    assert!(!rest_ids.contains(&muted_tagged.id.as_i64().to_string()));

    let ctx = build_filter_context(&app, Some(viewer)).await;
    let mut params = empty_params();
    params.tag = Some(TagFilter {
        primary: "rust".to_string(),
        any: vec!["rustlang".to_string()],
        all: Vec::new(),
        none: vec!["spam".to_string()],
    });
    let matcher = app.state.timelines().matcher();

    for candidate in [
        &matches_primary,
        &matches_any,
        &untagged,
        &mismatched_tag,
        &excluded_by_none,
        &boosted_tagged,
        &tagged_boost,
        &blocked_tagged,
        &muted_tagged,
    ] {
        assert_matcher_agrees_with_rest(
            &app,
            matcher,
            TimelineKind::Tag,
            &params,
            &ctx,
            candidate,
            &rest_ids,
        )
        .await;
    }

    app.cleanup().await;
}
