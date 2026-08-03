//! Integration tests for timelines task 4.2 (`.kiro/specs/timelines/tasks.md`,
//! "4.2 タイムラインサービスを実装する", `_Boundary: TimelineService_`):
//! observable completion — "各種別で閲覧者にとって membership を満たす可視投
//! 稿が id 降順・`limit` 件以内・安定カーソルで返り、フィルタ後充填で件数が
//! 満たされる。反復回数上限に達した場合も無限ループ・エラー化せず、部分ペ
//! ージと有効な継続カーソルが返る" (Requirements 1.1, 2.1, 3.1, 4.1, 5.1,
//! 6.1, 7.1, 7.2, 7.3, 7.4, 10.1).
//!
//! `TimelineService::timeline` is exercised directly as a Rust function
//! against a real, migrated database (`spawn_test_app`'s pool) — not through
//! any HTTP endpoint (none exists yet; `TimelineEndpoints` is a later task).
//! Fixtures are inserted directly via
//! `kawasemi::statuses::status_repository::insert_status` and
//! `kawasemi::social_graph::repository::{upsert_follow, upsert_block,
//! upsert_mute}`, mirroring `tests/timeline_candidate_repository_it.rs`'s/
//! `tests/timeline_hydrator_it.rs`'s own established "insert fixtures
//! directly, bypass the creating service" convention. Unlike
//! `tests/timeline_candidate_repository_it.rs` (which never touches
//! `AccountService`), any author whose post is expected to *survive*
//! filtering here needs a real, resolvable actor row (via
//! `tests/timeline_hydrator_it.rs`'s own `insert_actor_fixture` convention,
//! reused as `actor_fixture` below) — `TimelineService::timeline` calls all
//! the way through `StatusHydrator::hydrate`, whose `account_json` delegates
//! to `AccountService::show_account`, which 404s for an unresolvable actor
//! id. Authors whose posts are expected to be filtered out before hydration
//! (never appear in a returned page) do not need a real actor row — plain
//! `RuntimeContext::ids`-minted `Id` values are used for those, mirroring
//! `tests/timeline_candidate_repository_it.rs`'s own rationale.

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::api::pagination::{ForwardedOrigin, PageParams};
use kawasemi::domain::{AccountRef, Id, Visibility};
use kawasemi::social_graph::model::{Block, Follow, Mute};
use kawasemi::social_graph::repository as sg_repository;
use kawasemi::statuses::{Status, status_repository};
use kawasemi::test_harness::{TestApp, spawn_test_app};
use kawasemi::timelines::hydrator::StatusHydrator;
use kawasemi::timelines::model::{TagFilter, TimelineKind, TimelineParams};
use kawasemi::timelines::service::TimelineService;

// ---- Fixture plumbing (duplicated per sibling test-file convention — see
// `tests/timeline_candidate_repository_it.rs`'s own doc comment). ----

/// Creates a real owner + local actor row, returning the actor's `Id` — a
/// lighter-weight variant of `tests/timeline_hydrator_it.rs`'s own
/// `insert_actor_fixture` (that helper resolves and returns the full
/// `ResolvedActor`; this file's tests only ever need the bare `Id`).
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
            display_name: format!("Timeline Service IT {handle_str}"),
            summary: "an actor used by the timeline_service_it integration test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    actor.id
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
        "https://timeline-service-it.example/statuses/{}",
        id.as_i64()
    );
    let status = Status {
        id,
        actor_id,
        uri: uri.clone(),
        url: Some(uri),
        content: "a fixture post inserted directly by timeline_service_it".to_string(),
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
                "https://timeline-service-it.example/activities/follow-{}",
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
                "https://timeline-service-it.example/activities/block-{}",
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

fn service(app: &TestApp) -> TimelineService {
    let hydrator = StatusHydrator::new(
        app.pool.clone(),
        app.state.accounts().service(),
        app.state.media().store().clone(),
    );
    TimelineService::new(app.pool.clone(), app.runtime.clone(), hydrator)
}

fn test_origin() -> ForwardedOrigin {
    ForwardedOrigin {
        scheme: "https".to_string(),
        host: "timeline-service-it.example".to_string(),
    }
}

fn base_params() -> TimelineParams {
    TimelineParams {
        local: false,
        remote: false,
        only_media: false,
        tag: None,
        page: PageParams::default(),
    }
}

fn ids_of(items: &[serde_json::Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

// -- Home: following ∪ self, direct excluded, boosts included, relationship
// -- exclusion applied end to end ------------------------------------------

#[tokio::test]
async fn home_timeline_includes_self_and_followed_excludes_stranger_and_direct() {
    let app = spawn_test_app().await;
    let viewer = actor_fixture(&app, "viewer_home_basic").await;
    let followed = actor_fixture(&app, "followed_home_basic").await;
    let stranger = app.runtime.ids.next_id();

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

    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Home, Some(viewer), base_params(), &origin)
        .await
        .expect("home timeline must succeed");

    let ids = ids_of(&page.items);
    assert!(ids.contains(&own_post.id.as_i64().to_string()));
    assert!(ids.contains(&followed_post.id.as_i64().to_string()));
    assert!(!ids.contains(&stranger_post.id.as_i64().to_string()));
    assert!(!ids.contains(&followed_direct.id.as_i64().to_string()));

    app.cleanup().await;
}

#[tokio::test]
async fn home_timeline_excludes_blocked_and_muted_authors() {
    let app = spawn_test_app().await;
    let viewer = app.runtime.ids.next_id();
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

    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Home, Some(viewer), base_params(), &origin)
        .await
        .expect("home timeline must succeed");

    let ids = ids_of(&page.items);
    assert!(!ids.contains(&blocked_post.id.as_i64().to_string()));
    assert!(!ids.contains(&muted_post.id.as_i64().to_string()));

    app.cleanup().await;
}

// -- Boost inclusion/exclusion end to end (Requirements 1.4, 1.5, 6.3) ------

#[tokio::test]
async fn home_timeline_includes_a_followed_boosters_boost_of_a_visible_original() {
    let app = spawn_test_app().await;
    let viewer = actor_fixture(&app, "viewer_home_boost_ok").await;
    let booster = actor_fixture(&app, "booster_home_boost_ok").await;
    let original_author = actor_fixture(&app, "original_home_boost_ok").await;

    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(booster),
        true,
    )
    .await;

    let original =
        insert_status_fixture(&app, original_author, Visibility::Public, true, None).await;
    let boost =
        insert_status_fixture(&app, booster, Visibility::Public, true, Some(original.id)).await;

    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Home, Some(viewer), base_params(), &origin)
        .await
        .expect("home timeline must succeed");

    let ids = ids_of(&page.items);
    assert!(
        ids.contains(&boost.id.as_i64().to_string()),
        "a followed booster's boost of a non-excluded original must be included"
    );
    let boost_id_str = boost.id.as_i64().to_string();
    let boost_json = page
        .items
        .iter()
        .find(|item| item["id"].as_str() == Some(boost_id_str.as_str()))
        .unwrap();
    assert_eq!(boost_json["reblog"]["id"], original.id.as_i64().to_string());

    app.cleanup().await;
}

#[tokio::test]
async fn home_timeline_excludes_a_boost_whose_reblogged_author_is_blocked() {
    let app = spawn_test_app().await;
    let viewer = app.runtime.ids.next_id();
    let booster = app.runtime.ids.next_id();
    let blocked_original_author = app.runtime.ids.next_id();

    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(booster),
        true,
    )
    .await;
    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_original_author),
    )
    .await;

    let original = insert_status_fixture(
        &app,
        blocked_original_author,
        Visibility::Public,
        true,
        None,
    )
    .await;
    let boost =
        insert_status_fixture(&app, booster, Visibility::Public, true, Some(original.id)).await;

    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Home, Some(viewer), base_params(), &origin)
        .await
        .expect("home timeline must succeed");

    let ids = ids_of(&page.items);
    assert!(
        !ids.contains(&boost.id.as_i64().to_string()),
        "a boost of a blocked-author's original must be excluded (Requirement 6.3)"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn home_timeline_excludes_a_boost_from_a_reblogs_hidden_follow() {
    let app = spawn_test_app().await;
    let viewer = app.runtime.ids.next_id();
    let booster = app.runtime.ids.next_id();
    let original_author = app.runtime.ids.next_id();

    // `reblogs: false` -> reblogs_hidden.
    upsert_follow(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(booster),
        false,
    )
    .await;

    let original =
        insert_status_fixture(&app, original_author, Visibility::Public, true, None).await;
    let boost =
        insert_status_fixture(&app, booster, Visibility::Public, true, Some(original.id)).await;

    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Home, Some(viewer), base_params(), &origin)
        .await
        .expect("home timeline must succeed");

    let ids = ids_of(&page.items);
    assert!(
        !ids.contains(&boost.id.as_i64().to_string()),
        "a boost from a show_reblogs=false follow must be excluded (Requirement 1.4)"
    );

    app.cleanup().await;
}

// -- Public/Local/Tag membership end to end ---------------------------------

#[tokio::test]
async fn public_timeline_includes_public_posts_and_excludes_boosts_and_non_public() {
    let app = spawn_test_app().await;
    let author = actor_fixture(&app, "author_public_basic").await;

    let public_post = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let unlisted_post = insert_status_fixture(&app, author, Visibility::Unlisted, true, None).await;
    let boost =
        insert_status_fixture(&app, author, Visibility::Public, true, Some(public_post.id)).await;

    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Public, None, base_params(), &origin)
        .await
        .expect("public timeline must succeed");

    let ids = ids_of(&page.items);
    assert!(ids.contains(&public_post.id.as_i64().to_string()));
    assert!(!ids.contains(&unlisted_post.id.as_i64().to_string()));
    assert!(!ids.contains(&boost.id.as_i64().to_string()));

    app.cleanup().await;
}

#[tokio::test]
async fn local_timeline_excludes_remote_authors() {
    let app = spawn_test_app().await;
    let local_author = actor_fixture(&app, "local_author_local_tl").await;
    let remote_author = app.runtime.ids.next_id();

    let local_post =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    let remote_post =
        insert_status_fixture(&app, remote_author, Visibility::Public, false, None).await;

    let params = TimelineParams {
        local: true,
        ..base_params()
    };
    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Local, None, params, &origin)
        .await
        .expect("local timeline must succeed");

    let ids = ids_of(&page.items);
    assert!(ids.contains(&local_post.id.as_i64().to_string()));
    assert!(!ids.contains(&remote_post.id.as_i64().to_string()));

    app.cleanup().await;
}

#[tokio::test]
async fn tag_timeline_matches_the_normalized_primary_tag() {
    use kawasemi::statuses::Tag;
    use kawasemi::statuses::tag_repository;

    let app = spawn_test_app().await;
    let author = actor_fixture(&app, "author_tag_basic").await;

    let tagged = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
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
    tag_repository::associate_tag(&app.pool, tagged.id, tag.id)
        .await
        .expect("associate tag fixture must succeed");
    let untagged = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let params = TimelineParams {
        tag: Some(TagFilter {
            primary: "RuST".to_string(),
            any: Vec::new(),
            all: Vec::new(),
            none: Vec::new(),
        }),
        ..base_params()
    };
    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Tag, None, params, &origin)
        .await
        .expect("tag timeline must succeed");

    let ids = ids_of(&page.items);
    assert!(ids.contains(&tagged.id.as_i64().to_string()));
    assert!(!ids.contains(&untagged.id.as_i64().to_string()));

    app.cleanup().await;
}

// -- Unauthenticated access (Requirement 9.2) -------------------------------

#[tokio::test]
async fn public_timeline_unauthenticated_returns_only_public_posts() {
    let app = spawn_test_app().await;
    let author = actor_fixture(&app, "author_public_unauth").await;

    let public_post = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let private_post = insert_status_fixture(&app, author, Visibility::Private, true, None).await;

    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Public, None, base_params(), &origin)
        .await
        .expect("unauthenticated public timeline must succeed, not error");

    let ids = ids_of(&page.items);
    assert!(ids.contains(&public_post.id.as_i64().to_string()));
    assert!(!ids.contains(&private_post.id.as_i64().to_string()));

    app.cleanup().await;
}

// -- Pagination stability (max_id/since_id/min_id, Requirements 7.1-7.3) ----

#[tokio::test]
async fn max_id_returns_ids_strictly_below_it_newest_first() {
    let app = spawn_test_app().await;
    let author = actor_fixture(&app, "author_max_id").await;

    let first = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let second = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let third = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let params = TimelineParams {
        page: PageParams {
            max_id: Some(third.id.as_i64().to_string()),
            ..PageParams::default()
        },
        ..base_params()
    };
    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Public, None, params, &origin)
        .await
        .expect("public timeline with max_id must succeed");

    assert_eq!(
        ids_of(&page.items),
        vec![
            second.id.as_i64().to_string(),
            first.id.as_i64().to_string()
        ]
    );

    app.cleanup().await;
}

#[tokio::test]
async fn since_id_returns_ids_strictly_above_it_anchored_at_the_newest() {
    let app = spawn_test_app().await;
    let author = actor_fixture(&app, "author_since_id").await;

    let first = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let second = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let third = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let params = TimelineParams {
        page: PageParams {
            since_id: Some(first.id.as_i64().to_string()),
            ..PageParams::default()
        },
        ..base_params()
    };
    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Public, None, params, &origin)
        .await
        .expect("public timeline with since_id must succeed");

    assert_eq!(
        ids_of(&page.items),
        vec![
            third.id.as_i64().to_string(),
            second.id.as_i64().to_string()
        ]
    );

    app.cleanup().await;
}

#[tokio::test]
async fn min_id_returns_ids_strictly_above_it_anchored_at_the_oldest() {
    let app = spawn_test_app().await;
    let author = actor_fixture(&app, "author_min_id").await;

    let first = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let second = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let third = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let params = TimelineParams {
        page: PageParams {
            min_id: Some(first.id.as_i64().to_string()),
            limit: Some(1),
            ..PageParams::default()
        },
        ..base_params()
    };
    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Public, None, params, &origin)
        .await
        .expect("public timeline with min_id must succeed");

    // anchor_oldest with limit=1: the one item closest to min_id (i.e. the
    // smaller of the two candidates above it), not the newest overall.
    let ids = ids_of(&page.items);
    assert_eq!(ids, vec![second.id.as_i64().to_string()]);
    assert!(
        !ids.contains(&third.id.as_i64().to_string()),
        "anchor_oldest must prefer the item closest to min_id over the newest overall"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn a_second_page_via_max_id_has_no_duplicate_and_no_gap_with_the_first() {
    let app = spawn_test_app().await;
    let author = actor_fixture(&app, "author_second_page").await;

    let mut inserted = Vec::new();
    for _ in 0..5 {
        inserted.push(insert_status_fixture(&app, author, Visibility::Public, true, None).await);
    }

    let params = TimelineParams {
        page: PageParams {
            limit: Some(2),
            ..PageParams::default()
        },
        ..base_params()
    };
    let origin = test_origin();
    let svc = service(&app);
    let first_page = svc
        .timeline(TimelineKind::Public, None, params.clone(), &origin)
        .await
        .expect("first page must succeed");
    assert_eq!(first_page.items.len(), 2);
    let next_cursor = first_page
        .next_cursor
        .clone()
        .expect("first page must carry a next cursor");

    let second_params = TimelineParams {
        page: PageParams {
            max_id: Some(next_cursor),
            limit: Some(2),
            ..PageParams::default()
        },
        ..base_params()
    };
    let second_page = svc
        .timeline(TimelineKind::Public, None, second_params, &origin)
        .await
        .expect("second page must succeed");

    let first_ids = ids_of(&first_page.items);
    let second_ids = ids_of(&second_page.items);
    for id in &second_ids {
        assert!(
            !first_ids.contains(id),
            "the second page must not repeat an id from the first page"
        );
    }
    // 5 inserted, pages of 2: first page = [5th, 4th], second page = [3rd, 2nd].
    let expected_second: Vec<String> = inserted[1..3]
        .iter()
        .rev()
        .map(|s| s.id.as_i64().to_string())
        .collect();
    assert_eq!(second_ids, expected_second);

    app.cleanup().await;
}

// -- Fill loop: a second batch is pulled when the first batch's survivors
// -- fall short of `limit` (Requirement 7.4) --------------------------------

#[tokio::test]
async fn fill_loop_pulls_a_second_batch_when_the_first_is_mostly_filtered_out() {
    let app = spawn_test_app().await;
    let viewer = app.runtime.ids.next_id();
    let blocked_author = app.runtime.ids.next_id();
    let visible_author = actor_fixture(&app, "visible_author_fill_loop").await;

    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
    )
    .await;

    // `limit=3`, `batch_limit = limit * BATCH_LIMIT_MULTIPLIER(4) = 12`.
    // Oldest first (lowest ids): the 3 visible posts, followed by 13
    // blocked-author posts (newer/higher ids) — one more than `batch_limit`.
    // Candidates are fetched newest-first, so the *first* `batch_limit`(12)
    // fetch contains only the 12 newest blocked-author posts (all filtered
    // out, accumulating zero survivors); only the *second* fetch — the
    // remaining 1 blocked post plus the 3 visible posts — reaches the
    // survivors. This mirrors `iteration_cap_returns_a_partial_page_with_a_
    // valid_continuation_cursor`'s own correct oldest-visible/newest-blocked
    // fixture ordering, so the loop's second-batch code path is genuinely
    // exercised (not satisfied by the first, id-descending batch alone).
    let mut visible_posts = Vec::new();
    for _ in 0..3 {
        visible_posts.push(
            insert_status_fixture(&app, visible_author, Visibility::Public, true, None).await,
        );
    }
    for _ in 0..13 {
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    }

    let params = TimelineParams {
        page: PageParams {
            limit: Some(3),
            ..PageParams::default()
        },
        ..base_params()
    };
    let origin = test_origin();
    let page = service(&app)
        .timeline(TimelineKind::Public, Some(viewer), params, &origin)
        .await
        .expect("public timeline must succeed");

    assert_eq!(
        page.items.len(),
        3,
        "the fill loop must pull a second batch to satisfy `limit` after the first batch was \
         entirely filtered out"
    );
    let ids = ids_of(&page.items);
    for post in &visible_posts {
        assert!(ids.contains(&post.id.as_i64().to_string()));
    }

    app.cleanup().await;
}

// -- Iteration cap: partial page + valid continuation cursor, never an error
// -- or an infinite loop (Requirement 7.4) ----------------------------------

#[tokio::test]
async fn iteration_cap_returns_a_partial_page_with_a_valid_continuation_cursor() {
    let app = spawn_test_app().await;
    let viewer = app.runtime.ids.next_id();
    let blocked_author = app.runtime.ids.next_id();
    let visible_author = actor_fixture(&app, "visible_author_cap_resume").await;

    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
    )
    .await;

    // Oldest first: one visible post far below the scanned window, then 25
    // blocked-author posts above it (newer). `limit=1` -> `batch_limit=4`,
    // `MAX_FILL_ITERATIONS=5` -> at most 20 candidates scanned per call. The
    // first call must exhaust its cap scanning only blocked-author posts
    // (the visible post sits below the scanned range entirely), yielding an
    // empty partial page — but per Requirement 7.4 must still hand back a
    // resumable cursor, not `None` (which would misleadingly read as "no
    // more results" even though the visible post is still unscanned).
    let visible_post =
        insert_status_fixture(&app, visible_author, Visibility::Public, true, None).await;
    for _ in 0..25 {
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    }

    let params = TimelineParams {
        page: PageParams {
            limit: Some(1),
            ..PageParams::default()
        },
        ..base_params()
    };
    let origin = test_origin();
    let svc = service(&app);
    let first_page = svc
        .timeline(TimelineKind::Public, Some(viewer), params, &origin)
        .await
        .expect("the iteration cap must yield a partial page, not an error");

    assert!(
        first_page.items.is_empty(),
        "every candidate scanned within the iteration cap came from the blocked author"
    );
    let next_cursor = first_page.next_cursor.clone().expect(
        "a cap-hit page must still carry a valid, resumable continuation cursor \
         (Requirement 7.4), not `None`, even when zero candidates survived filtering",
    );

    // Resuming with that cursor must reach the visible post that was
    // structurally unreachable within the first call's iteration budget —
    // proving the cursor is genuinely valid (no gap), not merely non-`None`.
    let second_params = TimelineParams {
        page: PageParams {
            max_id: Some(next_cursor),
            limit: Some(1),
            ..PageParams::default()
        },
        ..base_params()
    };
    let second_page = svc
        .timeline(TimelineKind::Public, Some(viewer), second_params, &origin)
        .await
        .expect("resuming from the cap-hit cursor must succeed");

    assert_eq!(
        ids_of(&second_page.items),
        vec![visible_post.id.as_i64().to_string()],
        "resuming from the cap-hit cursor must reach the previously-unscanned visible post"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn iteration_cap_does_not_hang_and_terminates_within_the_fixed_batch_budget() {
    let app = spawn_test_app().await;
    let viewer = app.runtime.ids.next_id();
    let blocked_author = app.runtime.ids.next_id();

    upsert_block(
        &app,
        AccountRef::Local(viewer),
        AccountRef::Local(blocked_author),
    )
    .await;

    for _ in 0..25 {
        insert_status_fixture(&app, blocked_author, Visibility::Public, true, None).await;
    }

    let params = TimelineParams {
        page: PageParams {
            limit: Some(1),
            ..PageParams::default()
        },
        ..base_params()
    };
    let origin = test_origin();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        service(&app).timeline(TimelineKind::Public, Some(viewer), params, &origin),
    )
    .await;

    assert!(
        result.is_ok(),
        "the fill loop must terminate promptly, not hang, once the iteration cap is reached"
    );
    result
        .unwrap()
        .expect("the iteration cap must yield Ok(partial page), not an error");

    app.cleanup().await;
}
