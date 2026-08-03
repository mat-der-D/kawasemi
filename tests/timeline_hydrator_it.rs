//! Integration tests for timelines task 4.1 (`.kiro/specs/timelines/tasks.md`,
//! "4.1 (P) ステータス具体化を実装する", `_Boundary: StatusHydrator_`):
//! observable completion — "タイムライン要素が投稿 API と同一の Status
//! JSON 形で具体化され、認証文脈で操作状態が反映され、ブーストが `reblog`
//! にネストされる" (Requirements 10.1, 10.2, 10.3, 10.4).
//!
//! `StatusHydrator::hydrate` needs a real, wired
//! `AccountService<LocalFsStore, ReqwestFederationHttpClient>` (to resolve
//! `account`, Requirement 10.4's delegation) and real repository-backed
//! interaction/poll/tag state — mirroring
//! `tests/timeline_candidate_repository_it.rs`'s own established
//! "database-backed component gets a `tests/*.rs` integration test, not an
//! in-process `#[cfg(test)] mod tests`" precedent (`src/timelines/
//! candidate_repository.rs`'s identical judgment call), and
//! `tests/status_contract_it.rs`'s own `insert_actor_fixture` convention for
//! building a real, resolvable local actor `AccountService::show_account`
//! can render. Fixtures are inserted directly via
//! `kawasemi::statuses::status_repository::insert_status`/
//! `kawasemi::statuses::interaction_repository`/`kawasemi::statuses::
//! poll_repository`/`kawasemi::statuses::tag_repository`, bypassing the
//! creating services — this task's own boundary is hydration, not
//! creation.

use std::collections::HashSet;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::api::pagination::ForwardedOrigin;
use kawasemi::domain::{Id, Visibility};
use kawasemi::statuses::{
    Poll, PollOption, Status, Tag, interaction_repository, poll_repository, status_repository,
    tag_repository,
};
use kawasemi::test_harness::{TestApp, spawn_test_app};
use kawasemi::timelines::hydrator::StatusHydrator;
use kawasemi::timelines::model::FilterContext;

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/status_contract_it.rs`'s own
// established `insert_actor_fixture`/status-row conventions rather than
// importing them). ----

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
            display_name: format!("Timeline Hydrator IT {handle_str}"),
            summary: "an actor used by the timeline_hydrator_it integration test".to_string(),
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

async fn insert_status_fixture(
    app: &TestApp,
    actor_id: Id,
    content: &str,
    reblog_of_id: Option<Id>,
    poll_id: Option<Id>,
) -> Status {
    insert_status_fixture_with_visibility(
        app,
        actor_id,
        content,
        Visibility::Public,
        reblog_of_id,
        poll_id,
    )
    .await
}

/// Same as [`insert_status_fixture`] but with an explicit `visibility` —
/// needed by the Finding-1 reblog-target-visibility-leak regression test,
/// which requires a `Private` post rather than [`insert_status_fixture`]'s
/// hardcoded `Public`.
async fn insert_status_fixture_with_visibility(
    app: &TestApp,
    actor_id: Id,
    content: &str,
    visibility: Visibility,
    reblog_of_id: Option<Id>,
    poll_id: Option<Id>,
) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let uri = format!(
        "https://timeline-hydrator-it.example/statuses/{}",
        id.as_i64()
    );
    let status = Status {
        id,
        actor_id,
        uri: uri.clone(),
        url: Some(uri),
        content: content.to_string(),
        visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id,
        poll_id,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    };
    status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert status fixture must succeed");
    status
}

fn empty_filter_context(app: &TestApp, viewer: Option<Id>) -> FilterContext {
    FilterContext {
        viewer,
        blocked: HashSet::new(),
        blocked_by: HashSet::new(),
        muted: HashSet::new(),
        following: HashSet::new(),
        reblogs_hidden: HashSet::new(),
        now: app.runtime.clock.now(),
    }
}

fn test_origin() -> ForwardedOrigin {
    ForwardedOrigin {
        scheme: "https".to_string(),
        host: "timeline-hydrator-it.example".to_string(),
    }
}

fn hydrator(app: &TestApp) -> StatusHydrator {
    StatusHydrator::new(
        app.pool.clone(),
        app.state.accounts().service(),
        app.state.media().store().clone(),
    )
}

// ---- Requirement 10.1 / 10.4: same Status JSON contract, Account/media
// delegated upstream, no local representation. ----

#[tokio::test]
async fn hydrate_produces_the_same_status_json_contract_as_the_posting_api() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_basic").await;
    let status = insert_status_fixture(&app, alice.id, "hello from the hydrator", None, None).await;

    let ctx = empty_filter_context(&app, None);
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&status), &ctx, &origin)
        .await
        .expect("hydrate must succeed");

    assert_eq!(out.len(), 1);
    let json = &out[0];
    assert_eq!(json["id"], status.id.as_i64().to_string());
    assert_eq!(json["content"], "hello from the hydrator");
    assert_eq!(json["account"]["id"], alice.id.as_i64().to_string());
    assert!(json["media_attachments"].as_array().unwrap().is_empty());
    assert!(json["tags"].as_array().unwrap().is_empty());
    assert!(json["reblog"].is_null());
    assert!(json["poll"].is_null());
    // Unauthenticated viewer: every operation-state field is false
    // (Requirement 10.2's "認証済みアクター文脈で" only applies once
    // authenticated).
    assert_eq!(json["favourited"], false);
    assert_eq!(json["reblogged"], false);
    assert_eq!(json["bookmarked"], false);
    assert_eq!(json["pinned"], false);
    assert_eq!(json["muted"], false);

    app.cleanup().await;
}

// ---- Requirement 10.2: authenticated viewer operation state. ----

#[tokio::test]
async fn hydrate_reflects_the_viewers_favourite_bookmark_and_pin_state() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_ops").await;
    let bob = insert_actor_fixture(&app, "bob_hydrator_ops").await;
    let status =
        insert_status_fixture(&app, alice.id, "a post bob interacts with", None, None).await;

    let now = app.runtime.clock.now();
    interaction_repository::add_favourite(&app.pool, bob.id, status.id, now)
        .await
        .expect("add favourite fixture must succeed");
    let bookmark_id = app.runtime.ids.next_id();
    interaction_repository::add_bookmark(&app.pool, bookmark_id, bob.id, status.id, now)
        .await
        .expect("add bookmark fixture must succeed");
    interaction_repository::set_pin(&app.pool, bob.id, status.id, true, now)
        .await
        .expect("set pin fixture must succeed");

    let ctx = empty_filter_context(&app, Some(bob.id));
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&status), &ctx, &origin)
        .await
        .expect("hydrate must succeed");

    let json = &out[0];
    assert_eq!(json["favourited"], true);
    assert_eq!(json["bookmarked"], true);
    assert_eq!(json["pinned"], true);
    // Bob never boosted this post himself.
    assert_eq!(json["reblogged"], false);

    app.cleanup().await;
}

#[tokio::test]
async fn hydrate_reflects_the_viewers_own_reblog_of_the_status() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_reblogged").await;
    let bob = insert_actor_fixture(&app, "bob_hydrator_reblogged").await;
    let status = insert_status_fixture(&app, alice.id, "a post bob boosts", None, None).await;
    let _boost = insert_status_fixture(&app, bob.id, "", Some(status.id), None).await;

    let ctx = empty_filter_context(&app, Some(bob.id));
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&status), &ctx, &origin)
        .await
        .expect("hydrate must succeed");

    assert_eq!(out[0]["reblogged"], true);

    app.cleanup().await;
}

// ---- Requirement 10.3: boost nested under `reblog`, non-recursively. ----

#[tokio::test]
async fn hydrate_nests_a_boosted_status_under_reblog() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_boost_origin").await;
    let bob = insert_actor_fixture(&app, "bob_hydrator_boost_actor").await;
    let original = insert_status_fixture(&app, alice.id, "alice's original post", None, None).await;
    let boost = insert_status_fixture(&app, bob.id, "", Some(original.id), None).await;

    let ctx = empty_filter_context(&app, None);
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&boost), &ctx, &origin)
        .await
        .expect("hydrate must succeed");

    let json = &out[0];
    assert_eq!(json["id"], boost.id.as_i64().to_string());
    assert_eq!(json["account"]["id"], bob.id.as_i64().to_string());
    let reblog = &json["reblog"];
    assert!(
        !reblog.is_null(),
        "a boost must nest its original under `reblog`"
    );
    assert_eq!(reblog["id"], original.id.as_i64().to_string());
    assert_eq!(reblog["content"], "alice's original post");
    assert_eq!(reblog["account"]["id"], alice.id.as_i64().to_string());
    // Non-recursive: the nested reblog target's own `reblog` is always null.
    assert!(reblog["reblog"].is_null());

    app.cleanup().await;
}

// ---- Requirement 10.2: `muted` sourced from `FilterContext`, per-status. --

#[tokio::test]
async fn hydrate_reflects_muted_from_the_filter_context_per_status_author() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_muted").await;
    let bob = insert_actor_fixture(&app, "bob_hydrator_not_muted").await;
    let alice_status = insert_status_fixture(&app, alice.id, "alice, muted", None, None).await;
    let bob_status = insert_status_fixture(&app, bob.id, "bob, not muted", None, None).await;

    let mut ctx = empty_filter_context(&app, None);
    ctx.muted.insert(alice.id);
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(&[alice_status, bob_status], &ctx, &origin)
        .await
        .expect("hydrate must succeed");

    assert_eq!(out[0]["muted"], true);
    assert_eq!(out[1]["muted"], false);

    app.cleanup().await;
}

// ---- Requirement 10.1/10.4: poll rendering delegated to statuses-core's
// own `poll_to_json`. ----

#[tokio::test]
async fn hydrate_renders_a_polls_status_poll_field() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_poll").await;
    let poll_id = app.runtime.ids.next_id();
    let status =
        insert_status_fixture(&app, alice.id, "what's your favorite?", None, Some(poll_id)).await;
    let poll = Poll {
        id: poll_id,
        status_id: status.id,
        expires_at: None,
        multiple: false,
    };
    let options = vec![
        PollOption {
            poll_id,
            idx: 0,
            title: "rust".to_string(),
            votes_count: 0,
        },
        PollOption {
            poll_id,
            idx: 1,
            title: "not rust".to_string(),
            votes_count: 0,
        },
    ];
    poll_repository::insert_poll(&app.pool, &poll, &options)
        .await
        .expect("insert poll fixture must succeed");

    let ctx = empty_filter_context(&app, None);
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&status), &ctx, &origin)
        .await
        .expect("hydrate must succeed");

    let json_poll = &out[0]["poll"];
    assert!(!json_poll.is_null());
    assert_eq!(json_poll["id"], poll_id.as_i64().to_string());
    assert_eq!(json_poll["options"].as_array().unwrap().len(), 2);
    assert_eq!(json_poll["voted"], false);

    app.cleanup().await;
}

// ---- Requirement 10.4: tags delegated upstream (statuses-core's own
// `tags_for_status`, no independent representation). ----

#[tokio::test]
async fn hydrate_renders_a_statuss_tags() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_tags").await;
    let status = insert_status_fixture(&app, alice.id, "a tagged post #rust", None, None).await;

    let tag_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let tag = tag_repository::upsert_tag(
        &app.pool,
        &Tag {
            id: tag_id,
            name: "rust".to_string(),
            created_at: now,
        },
    )
    .await
    .expect("upsert tag fixture must succeed");
    tag_repository::associate_tag(&app.pool, status.id, tag.id)
        .await
        .expect("associate tag fixture must succeed");

    let ctx = empty_filter_context(&app, None);
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&status), &ctx, &origin)
        .await
        .expect("hydrate must succeed");

    let tags = out[0]["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0]["name"], "rust");

    app.cleanup().await;
}

// ---- Reviewer round-1 remediation: reblog-target visibility leak
// (Requirement 10.3 nesting is not a visibility bypass — `TimelineFilter`
// only ever validates the boost row's own visibility snapshot, keyed to the
// booster, never the boosted-original's own author). ----

#[tokio::test]
async fn hydrate_does_not_leak_a_private_reblog_targets_content_to_a_non_follower_viewer() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_private_leak").await;
    let bob = insert_actor_fixture(&app, "bob_hydrator_private_leak_booster").await;
    let viewer = insert_actor_fixture(&app, "viewer_hydrator_private_leak").await;

    // Alice's post is `Private` (followers-only); the viewer does not
    // follow Alice.
    let private_original = insert_status_fixture_with_visibility(
        &app,
        alice.id,
        "alice's private post the viewer must not see",
        Visibility::Private,
        None,
        None,
    )
    .await;
    // Bob boosts it — the boost row's own `visibility` is the
    // boost-creation-time snapshot of the target's visibility, mirroring
    // `InteractionService::reblog`'s real row construction
    // (`visibility: target.visibility`).
    let boost = insert_status_fixture_with_visibility(
        &app,
        bob.id,
        "",
        Visibility::Private,
        Some(private_original.id),
        None,
    )
    .await;

    let mut ctx = empty_filter_context(&app, Some(viewer.id));
    // The viewer follows the booster (Bob) but not the original author
    // (Alice) — exactly the leak scenario the reviewer flagged: a boost
    // this viewer is entitled to see (via following Bob) must not smuggle in
    // Alice's private content the viewer has no independent right to see.
    ctx.following.insert(bob.id);
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&boost), &ctx, &origin)
        .await
        .expect("hydrate must succeed");

    let json = &out[0];
    assert_eq!(json["id"], boost.id.as_i64().to_string());
    assert!(
        json["reblog"].is_null(),
        "a boost's reblog target the viewer has no independent visibility \
         into must degrade to `reblog: None`, not leak the original's \
         content/account"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn hydrate_still_nests_a_private_reblog_target_when_the_viewer_is_a_follower_of_the_original_author()
 {
    // Companion to the leak regression above: the independent visibility
    // re-check must not become an unconditional `reblog: None` — a viewer
    // who *does* have a legitimate relationship to the original author (here,
    // following them directly) must still see the nested reblog.
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_private_ok").await;
    let bob = insert_actor_fixture(&app, "bob_hydrator_private_ok_booster").await;
    let viewer = insert_actor_fixture(&app, "viewer_hydrator_private_ok").await;

    let private_original = insert_status_fixture_with_visibility(
        &app,
        alice.id,
        "alice's private post the viewer may see",
        Visibility::Private,
        None,
        None,
    )
    .await;
    let boost = insert_status_fixture_with_visibility(
        &app,
        bob.id,
        "",
        Visibility::Private,
        Some(private_original.id),
        None,
    )
    .await;

    let mut ctx = empty_filter_context(&app, Some(viewer.id));
    ctx.following.insert(bob.id);
    ctx.following.insert(alice.id);
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&boost), &ctx, &origin)
        .await
        .expect("hydrate must succeed");

    let reblog = &out[0]["reblog"];
    assert!(
        !reblog.is_null(),
        "a viewer who follows the original author must still see the nested reblog"
    );
    assert_eq!(reblog["id"], private_original.id.as_i64().to_string());

    app.cleanup().await;
}

// ---- Reviewer round-1 remediation: dangling reblog target/poll degrade to
// `None`, never an error. ----

#[tokio::test]
async fn hydrate_degrades_a_dangling_reblog_target_to_none_rather_than_erroring() {
    let app = spawn_test_app().await;
    let bob = insert_actor_fixture(&app, "bob_hydrator_dangling_reblog").await;
    // `reblog_of_id` points at an id that was never inserted (deleted /
    // referentially inconsistent row) — Finding 2's "dangling reblog_of_id"
    // coverage.
    let dangling_target_id = app.runtime.ids.next_id();
    let boost = insert_status_fixture(&app, bob.id, "", Some(dangling_target_id), None).await;

    let ctx = empty_filter_context(&app, None);
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&boost), &ctx, &origin)
        .await
        .expect("hydrate must succeed, not error, on a dangling reblog target");

    assert!(out[0]["reblog"].is_null());

    app.cleanup().await;
}

#[tokio::test]
async fn hydrate_degrades_a_dangling_poll_target_to_none_rather_than_erroring() {
    let app = spawn_test_app().await;
    let alice = insert_actor_fixture(&app, "alice_hydrator_dangling_poll").await;
    // `poll_id` points at an id that was never inserted — Finding 2's
    // "dangling poll_id" coverage.
    let dangling_poll_id = app.runtime.ids.next_id();
    let status = insert_status_fixture(
        &app,
        alice.id,
        "a post whose poll row is missing",
        None,
        Some(dangling_poll_id),
    )
    .await;

    let ctx = empty_filter_context(&app, None);
    let origin = test_origin();
    let out = hydrator(&app)
        .hydrate(std::slice::from_ref(&status), &ctx, &origin)
        .await
        .expect("hydrate must succeed, not error, on a dangling poll target");

    assert!(out[0]["poll"].is_null());

    app.cleanup().await;
}
