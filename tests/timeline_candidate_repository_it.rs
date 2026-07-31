//! Integration tests for timelines task 2.1 (`.kiro/specs/timelines/tasks.md`,
//! "2.1 候補リポジトリを read-only で実装する", `_Boundary:
//! CandidateRepository_`): observable completion — "各種別で種別条件を満た
//! す候補が id 降順で取得でき、`only_media`/`local`/`remote`/タグ条件が反映
//! され、上流テーブルを一切書き換えない" (Requirements 1.1, 2.1, 2.3, 2.4,
//! 2.5, 3.1, 3.4, 4.1, 4.4, 4.5, 7.1, 7.4).
//!
//! `CandidateRepository::fetch_candidates` is exercised directly as a Rust
//! function against a real, migrated database (`spawn_test_app`'s pool) —
//! not through any HTTP endpoint (none exists yet; `TimelineEndpoints` is a
//! later task). Fixtures are inserted directly via
//! `kawasemi::statuses::status_repository::insert_status`/`attach_media` and
//! `kawasemi::statuses::tag_repository::upsert_tag`/`associate_tag`, mirroring
//! `tests/status_contract_it.rs`'/`tests/polls_it.rs`'s own established
//! "insert fixtures directly, bypass the creating service" convention.
//!
//! `statuses.actor_id` is a logical-only reference (no FK,
//! `migrations/0007_statuses.sql`), so author ids here are plain
//! `RuntimeContext::ids`-minted [`Id`] values with no corresponding `actors`
//! row — this repository never joins against an actor table (author
//! locality is read straight off `statuses.local`), so no real actor fixture
//! is needed to exercise it.
//!
//! This file is named `tests/timeline_candidate_repository_it.rs` (not
//! `timeline_pagination_it.rs` etc.) so it does not collide with
//! design.md's own planned `tests/timeline_*.rs` names, which belong to
//! later tasks' broader endpoint/service coverage.

use kawasemi::api::pagination::PageParams;
use kawasemi::domain::{Id, Visibility};
use kawasemi::statuses::{Status, Tag, status_repository, tag_repository};
use kawasemi::test_harness::{TestApp, spawn_test_app};
use kawasemi::timelines::candidate_repository::fetch_candidates;
use kawasemi::timelines::model::{TagFilter, TimelineKind, TimelineParams, TimelineQuerySpec};

// ---- Fixture plumbing (duplicated per sibling test-file convention — see
// `tests/status_contract_it.rs`'s own doc comment). ----

/// Inserts a `statuses` row directly, returning the [`Status`] fixture.
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
        "https://timeline-candidate-repository-it.example/statuses/{}",
        id.as_i64()
    );
    let status = Status {
        id,
        actor_id,
        uri: uri.clone(),
        url: Some(uri),
        content: "a fixture post inserted directly by timeline_candidate_repository_it".to_string(),
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

/// Associates `tag_name` (as-given, not pre-normalized by the caller) with
/// `status_id`, upserting the `tags` row first.
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

fn base_params() -> TimelineParams {
    TimelineParams {
        local: false,
        remote: false,
        only_media: false,
        tag: None,
        page: PageParams::default(),
    }
}

fn spec(
    kind: TimelineKind,
    params: TimelineParams,
    max_id: Option<Id>,
    since_id: Option<Id>,
    min_id: Option<Id>,
) -> TimelineQuerySpec {
    TimelineQuerySpec {
        kind,
        params,
        max_id,
        since_id,
        min_id,
    }
}

fn ids_of(statuses: &[Status]) -> Vec<Id> {
    statuses.iter().map(|s| s.id).collect()
}

async fn table_row_counts(app: &TestApp) -> (i64, i64, i64, i64) {
    let statuses: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM statuses")
        .fetch_one(&app.pool)
        .await
        .expect("count statuses");
    let status_media: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM status_media")
        .fetch_one(&app.pool)
        .await
        .expect("count status_media");
    let tags: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tags")
        .fetch_one(&app.pool)
        .await
        .expect("count tags");
    let status_tags: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM status_tags")
        .fetch_one(&app.pool)
        .await
        .expect("count status_tags");
    (statuses.0, status_media.0, tags.0, status_tags.0)
}

// -- Home: following ∪ self, direct excluded, boosts included --------------

#[tokio::test]
async fn home_candidates_are_limited_to_following_and_self_excluding_direct_including_boosts() {
    let app = spawn_test_app().await;

    let viewer = app.runtime.ids.next_id();
    let followed = app.runtime.ids.next_id();
    let stranger = app.runtime.ids.next_id();

    let own_post = insert_status_fixture(&app, viewer, Visibility::Public, true, None).await;
    let followed_post = insert_status_fixture(&app, followed, Visibility::Public, true, None).await;
    let stranger_post = insert_status_fixture(&app, stranger, Visibility::Public, true, None).await;
    let followed_direct =
        insert_status_fixture(&app, followed, Visibility::Direct, true, None).await;
    let original_for_boost =
        insert_status_fixture(&app, stranger, Visibility::Public, true, None).await;
    let followed_boost = insert_status_fixture(
        &app,
        followed,
        Visibility::Public,
        true,
        Some(original_for_boost.id),
    )
    .await;

    let following_and_self = [viewer, followed];
    let query = spec(TimelineKind::Home, base_params(), None, None, None);
    let result = fetch_candidates(&app.pool, &query, &following_and_self, 50)
        .await
        .expect("fetch_candidates(Home) must succeed");
    let result_ids = ids_of(&result);

    assert!(
        result_ids.contains(&own_post.id),
        "own post must be included"
    );
    assert!(
        result_ids.contains(&followed_post.id),
        "followed author's post must be included"
    );
    assert!(
        result_ids.contains(&followed_boost.id),
        "followed author's boost must be included (boosts are structurally included for home)"
    );
    assert!(
        !result_ids.contains(&stranger_post.id),
        "non-followed, non-self author's post must be excluded"
    );
    assert!(
        !result_ids.contains(&followed_direct.id),
        "direct-visibility post must be excluded from home even from a followed author"
    );
    assert!(
        !result_ids.contains(&original_for_boost.id),
        "the boosted-of original (authored by a non-followed stranger) must itself be excluded"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn home_candidates_are_ordered_by_id_descending() {
    let app = spawn_test_app().await;
    let viewer = app.runtime.ids.next_id();

    let first = insert_status_fixture(&app, viewer, Visibility::Public, true, None).await;
    let second = insert_status_fixture(&app, viewer, Visibility::Public, true, None).await;
    let third = insert_status_fixture(&app, viewer, Visibility::Public, true, None).await;

    let following_and_self = [viewer];
    let query = spec(TimelineKind::Home, base_params(), None, None, None);
    let result = fetch_candidates(&app.pool, &query, &following_and_self, 50)
        .await
        .expect("fetch_candidates(Home) must succeed");

    assert_eq!(ids_of(&result), vec![third.id, second.id, first.id]);

    app.cleanup().await;
}

// -- Public: public-only, boosts excluded -----------------------------------

#[tokio::test]
async fn public_candidates_are_public_visibility_only_and_exclude_boosts() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let public_post = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let unlisted_post = insert_status_fixture(&app, author, Visibility::Unlisted, true, None).await;
    let private_post = insert_status_fixture(&app, author, Visibility::Private, true, None).await;
    let public_boost =
        insert_status_fixture(&app, author, Visibility::Public, true, Some(public_post.id)).await;

    let query = spec(TimelineKind::Public, base_params(), None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Public) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&public_post.id));
    assert!(!result_ids.contains(&unlisted_post.id));
    assert!(!result_ids.contains(&private_post.id));
    assert!(
        !result_ids.contains(&public_boost.id),
        "a public boost must be excluded from public — original posts only"
    );

    app.cleanup().await;
}

// -- Local: public + local-author only ---------------------------------------

#[tokio::test]
async fn local_candidates_are_public_and_local_author_only() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let remote_author = app.runtime.ids.next_id();

    let local_public =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    let remote_public =
        insert_status_fixture(&app, remote_author, Visibility::Public, false, None).await;
    let local_boost = insert_status_fixture(
        &app,
        local_author,
        Visibility::Public,
        true,
        Some(local_public.id),
    )
    .await;

    let query = spec(TimelineKind::Local, base_params(), None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Local) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&local_public.id));
    assert!(
        !result_ids.contains(&remote_public.id),
        "a remote author's public post must be excluded from local"
    );
    assert!(
        !result_ids.contains(&local_boost.id),
        "a boost from a local author must still be excluded (non-boost rule applies)"
    );

    app.cleanup().await;
}

// -- Tag: normalized match, any/all/none -------------------------------------

#[tokio::test]
async fn tag_candidates_match_the_primary_tag_case_insensitively_and_normalized() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let tagged = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, tagged.id, "rust").await;
    let untagged = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let params = TimelineParams {
        tag: Some(TagFilter {
            primary: "  RuST  ".to_string(),
            any: Vec::new(),
            all: Vec::new(),
            none: Vec::new(),
        }),
        ..base_params()
    };
    let query = spec(TimelineKind::Tag, params, None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Tag) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&tagged.id));
    assert!(!result_ids.contains(&untagged.id));

    app.cleanup().await;
}

#[tokio::test]
async fn tag_candidates_with_no_tag_filter_attached_match_nothing() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();
    let post = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, post.id, "rust").await;

    let query = spec(TimelineKind::Tag, base_params(), None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Tag) must succeed");

    assert!(result.is_empty());

    app.cleanup().await;
}

#[tokio::test]
async fn tag_candidates_any_condition_requires_at_least_one_additional_tag() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let matches_any = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, matches_any.id, "rust").await;
    tag_status(&app, matches_any.id, "programming").await;

    let misses_any = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, misses_any.id, "rust").await;

    let params = TimelineParams {
        tag: Some(TagFilter {
            primary: "rust".to_string(),
            any: vec!["rustlang".to_string(), "programming".to_string()],
            all: Vec::new(),
            none: Vec::new(),
        }),
        ..base_params()
    };
    let query = spec(TimelineKind::Tag, params, None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Tag) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&matches_any.id));
    assert!(!result_ids.contains(&misses_any.id));

    app.cleanup().await;
}

#[tokio::test]
async fn tag_candidates_all_condition_requires_every_additional_tag() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let matches_all = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, matches_all.id, "rust").await;
    tag_status(&app, matches_all.id, "async").await;
    tag_status(&app, matches_all.id, "tokio").await;

    let partial = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, partial.id, "rust").await;
    tag_status(&app, partial.id, "async").await;

    let params = TimelineParams {
        tag: Some(TagFilter {
            primary: "rust".to_string(),
            any: Vec::new(),
            all: vec!["async".to_string(), "tokio".to_string()],
            none: Vec::new(),
        }),
        ..base_params()
    };
    let query = spec(TimelineKind::Tag, params, None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Tag) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&matches_all.id));
    assert!(!result_ids.contains(&partial.id));

    app.cleanup().await;
}

#[tokio::test]
async fn tag_candidates_none_condition_excludes_when_an_excluded_tag_is_present() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let clean = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, clean.id, "rust").await;

    let spammy = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, spammy.id, "rust").await;
    tag_status(&app, spammy.id, "spam").await;

    let params = TimelineParams {
        tag: Some(TagFilter {
            primary: "rust".to_string(),
            any: Vec::new(),
            all: Vec::new(),
            none: vec!["spam".to_string()],
        }),
        ..base_params()
    };
    let query = spec(TimelineKind::Tag, params, None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Tag) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&clean.id));
    assert!(!result_ids.contains(&spammy.id));

    app.cleanup().await;
}

#[tokio::test]
async fn tag_candidates_exclude_boosts_and_non_public_visibility() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let original = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, original.id, "rust").await;
    let boost =
        insert_status_fixture(&app, author, Visibility::Public, true, Some(original.id)).await;
    tag_status(&app, boost.id, "rust").await;
    let private_tagged = insert_status_fixture(&app, author, Visibility::Private, true, None).await;
    tag_status(&app, private_tagged.id, "rust").await;

    let params = TimelineParams {
        tag: Some(TagFilter {
            primary: "rust".to_string(),
            any: Vec::new(),
            all: Vec::new(),
            none: Vec::new(),
        }),
        ..base_params()
    };
    let query = spec(TimelineKind::Tag, params, None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Tag) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&original.id));
    assert!(!result_ids.contains(&boost.id));
    assert!(!result_ids.contains(&private_tagged.id));

    app.cleanup().await;
}

// -- only_media / local / remote request-level narrowing ---------------------

#[tokio::test]
async fn only_media_narrows_to_posts_with_a_media_attachment() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let with_media = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let media_id = app.runtime.ids.next_id();
    status_repository::attach_media(&app.pool, with_media.id, &[media_id])
        .await
        .expect("attach media fixture must succeed");
    let without_media = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let params = TimelineParams {
        only_media: true,
        ..base_params()
    };
    let query = spec(TimelineKind::Public, params, None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Public, only_media) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&with_media.id));
    assert!(!result_ids.contains(&without_media.id));

    app.cleanup().await;
}

#[tokio::test]
async fn local_param_narrows_to_local_authors_only() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let remote_author = app.runtime.ids.next_id();

    let local_post =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    let remote_post =
        insert_status_fixture(&app, remote_author, Visibility::Public, false, None).await;

    let params = TimelineParams {
        local: true,
        ..base_params()
    };
    let query = spec(TimelineKind::Public, params, None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Public, local) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&local_post.id));
    assert!(!result_ids.contains(&remote_post.id));

    app.cleanup().await;
}

#[tokio::test]
async fn remote_param_narrows_to_remote_authors_only() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let remote_author = app.runtime.ids.next_id();

    let local_post =
        insert_status_fixture(&app, local_author, Visibility::Public, true, None).await;
    let remote_post =
        insert_status_fixture(&app, remote_author, Visibility::Public, false, None).await;

    let params = TimelineParams {
        remote: true,
        ..base_params()
    };
    let query = spec(TimelineKind::Public, params, None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Public, remote) must succeed");
    let result_ids = ids_of(&result);

    assert!(result_ids.contains(&remote_post.id));
    assert!(!result_ids.contains(&local_post.id));

    app.cleanup().await;
}

// -- Cursor bounds: max_id / since_id / min_id -------------------------------

#[tokio::test]
async fn max_id_bounds_results_to_ids_strictly_below_it() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let first = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let second = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let third = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let query = spec(
        TimelineKind::Public,
        base_params(),
        Some(third.id),
        None,
        None,
    );
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Public, max_id) must succeed");

    assert_eq!(ids_of(&result), vec![second.id, first.id]);

    app.cleanup().await;
}

#[tokio::test]
async fn since_id_bounds_results_to_ids_strictly_above_it() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let first = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let second = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let third = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let query = spec(
        TimelineKind::Public,
        base_params(),
        None,
        Some(first.id),
        None,
    );
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Public, since_id) must succeed");

    assert_eq!(ids_of(&result), vec![third.id, second.id]);

    app.cleanup().await;
}

#[tokio::test]
async fn min_id_bounds_results_to_ids_strictly_above_it() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let first = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let second = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    let third = insert_status_fixture(&app, author, Visibility::Public, true, None).await;

    let query = spec(
        TimelineKind::Public,
        base_params(),
        None,
        None,
        Some(first.id),
    );
    let result = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates(Public, min_id) must succeed");

    assert_eq!(ids_of(&result), vec![third.id, second.id]);

    app.cleanup().await;
}

#[tokio::test]
async fn candidates_remain_id_descending_and_stable_across_repeated_calls() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let mut inserted = Vec::new();
    for _ in 0..5 {
        inserted.push(insert_status_fixture(&app, author, Visibility::Public, true, None).await);
    }
    let expected_ids: Vec<Id> = inserted.iter().rev().map(|s| s.id).collect();

    let query = spec(TimelineKind::Public, base_params(), None, None, None);
    let first_call = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates first call must succeed");
    let second_call = fetch_candidates(&app.pool, &query, &[], 50)
        .await
        .expect("fetch_candidates second call must succeed");

    assert_eq!(ids_of(&first_call), expected_ids);
    assert_eq!(ids_of(&second_call), expected_ids);

    app.cleanup().await;
}

// -- batch_limit is a straight SQL LIMIT, not silently capped ---------------

#[tokio::test]
async fn batch_limit_larger_than_the_pagination_toolkits_max_limit_returns_up_to_batch_limit() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    // kawasemi::api::pagination::MAX_LIMIT is 40 — insert more than that and
    // request a batch_limit above it too, proving this repository's LIMIT is
    // not silently capped to the pagination toolkit's own page-size ceiling
    // (a different concern entirely, applied later by `TimelineService`).
    const TOTAL: usize = 45;
    const REQUESTED_BATCH: u32 = 41;
    for _ in 0..TOTAL {
        insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    }

    let query = spec(TimelineKind::Public, base_params(), None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], REQUESTED_BATCH)
        .await
        .expect("fetch_candidates(Public) must succeed");

    assert_eq!(result.len(), REQUESTED_BATCH as usize);

    app.cleanup().await;
}

#[tokio::test]
async fn batch_limit_smaller_than_available_candidates_caps_the_result() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    for _ in 0..6 {
        insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    }

    let query = spec(TimelineKind::Public, base_params(), None, None, None);
    let result = fetch_candidates(&app.pool, &query, &[], 3)
        .await
        .expect("fetch_candidates(Public) must succeed");

    assert_eq!(result.len(), 3);

    app.cleanup().await;
}

// -- Read-only: no mutation of statuses/status_media/tags/status_tags -------

#[tokio::test]
async fn fetch_candidates_does_not_mutate_any_upstream_table() {
    let app = spawn_test_app().await;
    let author = app.runtime.ids.next_id();

    let post = insert_status_fixture(&app, author, Visibility::Public, true, None).await;
    tag_status(&app, post.id, "rust").await;
    let media_id = app.runtime.ids.next_id();
    status_repository::attach_media(&app.pool, post.id, &[media_id])
        .await
        .expect("attach media fixture must succeed");

    let before = table_row_counts(&app).await;

    let params = TimelineParams {
        only_media: true,
        tag: Some(TagFilter {
            primary: "rust".to_string(),
            any: Vec::new(),
            all: Vec::new(),
            none: Vec::new(),
        }),
        ..base_params()
    };
    // Run every kind once, so no code path in this repository is left
    // unexercised by this read-only assertion.
    for kind in [
        TimelineKind::Home,
        TimelineKind::Public,
        TimelineKind::Local,
        TimelineKind::Tag,
    ] {
        let query = spec(kind, params.clone(), None, None, None);
        fetch_candidates(&app.pool, &query, &[author], 50)
            .await
            .expect("fetch_candidates must succeed for every kind");
    }

    let after = table_row_counts(&app).await;
    assert_eq!(
        before, after,
        "fetch_candidates must not change row counts in statuses/status_media/tags/status_tags"
    );

    app.cleanup().await;
}
