//! Tests for [`super::SearchHydrator`] (task 4.2 completion definition:
//! "重複アカウントが一意化され、閲覧者不可視の投稿が必ず除外され、ハッシュ
//! タグが Tag JSON 化される統合テストが通る"; Requirements 1.2, 3.2, 3.3,
//! 3.5, 4.2, 5.2).
//!
//! One pure-logic unit test needs no database
//! ([`account_ref_id_recovers_the_id_regardless_of_local_remote`]). Every
//! other test is a real, executable DB-backed integration test against
//! `crate::test_harness::spawn_test_app` — mirroring
//! `notifications/service/tests.rs`'s/`hashtag_repository/tests.rs`'s
//! established convention (`create_test_actor` is an exact copy of
//! `notifications/service/tests.rs`'s own helper of the same name;
//! `sample_status`/`create_test_status` are a parameterized variant of that
//! same module's identical helpers, since these tests need to vary
//! `visibility`/`content` per case). See this task's own status report for
//! this sandbox's confirmed lack of a reachable Postgres — every test below
//! is expected to fail with the same `PoolTimedOut`-style connection error
//! `notifications`/`hashtag_repository`'s own DB-backed tests already fail
//! with here, not a logic/compile error.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use time::OffsetDateTime;

use super::*;
use crate::accounts::model::RelationshipView;
use crate::accounts::ports::RelationshipStateProvider;
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{ActorState, ActorType, Handle};
use crate::domain::Visibility;
use crate::search::hashtag_repository::upsert_tag_usage;
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

// -- pure-logic (no DB) -------------------------------------------------

/// [`account_ref_id`] recovers the same [`Id`] regardless of local/remote-
/// ness — the helper [`SearchHydrator::hydrate_accounts`]'s dedup/
/// `following`-filter logic is built on.
#[test]
fn account_ref_id_recovers_the_id_regardless_of_local_remote() {
    assert_eq!(
        account_ref_id(&AccountRef::Local(Id::from_i64(5))),
        Id::from_i64(5)
    );
    assert_eq!(
        account_ref_id(&AccountRef::Remote(Id::from_i64(9))),
        Id::from_i64(9)
    );
}

// -- DB-backed fixtures ---------------------------------------------------

/// Creates a real owner + local actor row, returning the actor's `Id` — an
/// exact copy of `notifications/service/tests.rs::create_test_actor`.
async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = crate::actor::model::LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("insert_actor must succeed");
    tx.commit().await.expect("committing must succeed");

    actor_id
}

fn sample_status(
    id: Id,
    actor_id: Id,
    visibility: Visibility,
    content: &str,
    created_at: OffsetDateTime,
) -> Status {
    Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: content.to_string(),
        visibility,
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
        created_at,
        edited_at: None,
    }
}

async fn create_test_status(
    app: &TestApp,
    actor_id: Id,
    visibility: Visibility,
    content: &str,
) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    insert_status(
        &app.pool,
        &sample_status(id, actor_id, visibility, content, now),
    )
    .await
    .expect("insert_status must succeed");
    id
}

fn build_hydrator(app: &TestApp) -> SearchHydrator {
    SearchHydrator::new(
        app.pool.clone(),
        app.state.accounts().service(),
        app.state.accounts().ports(),
        app.state.media().store().clone(),
        app.state.statuses().relationship_query_registry(),
        app.runtime.clone(),
        app.state.config().server.domain.clone(),
    )
}

// -- hydrate_accounts (Requirements 1.2, 3.2, 3.5) -------------------------

/// Requirement 3.5: a duplicated [`AccountRef`] collapses to one rendered
/// Account JSON, and every distinct ref still renders (Requirement 1.2/3.2).
#[tokio::test]
async fn hydrate_accounts_dedups_duplicate_refs_and_renders_every_distinct_account() {
    let app = spawn_test_app().await;
    let hydrator = build_hydrator(&app);

    let alice = create_test_actor(&app, "alice").await;
    let bob = create_test_actor(&app, "bob").await;

    let refs = [
        AccountRef::Local(alice),
        AccountRef::Local(alice),
        AccountRef::Local(bob),
    ];

    let rendered = hydrator
        .hydrate_accounts(&refs, alice, false)
        .await
        .expect("hydrate_accounts must succeed");

    assert_eq!(
        rendered.len(),
        2,
        "duplicate alice ref must collapse to one"
    );
    let ids: Vec<String> = rendered
        .iter()
        .map(|account| account["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        vec![alice.as_i64().to_string(), bob.as_i64().to_string()]
    );

    app.cleanup().await;
}

/// A stub `RelationshipStateProvider` that reports `following: true` for
/// exactly the ids in `followed`, `false` for everything else — mirrors
/// `crate::accounts::ports::tests::FixedRelationshipProvider`'s identical
/// boxed-future shape, generalized to a selectable follow set so this test
/// can prove `hydrate_accounts`'s `following_only` filter actually narrows
/// the result, not just passes every candidate through.
struct SelectiveFollowingProvider {
    followed: Vec<Id>,
}

impl RelationshipStateProvider for SelectiveFollowingProvider {
    fn relationships<'a>(
        &'a self,
        _viewer: Id,
        targets: &'a [AccountRef],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RelationshipView>, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(targets
                .iter()
                .map(|target| {
                    let id = account_ref_id(target);
                    RelationshipView {
                        id,
                        following: self.followed.contains(&id),
                        showing_reblogs: false,
                        notifying: false,
                        languages: Vec::new(),
                        followed_by: false,
                        blocking: false,
                        blocked_by: false,
                        muting: false,
                        muting_notifications: false,
                        requested: false,
                        requested_by: false,
                        domain_blocking: false,
                        endorsed: false,
                        note: String::new(),
                    }
                })
                .collect())
        })
    }
}

/// Requirement 3.3: `following_only: true` narrows the result to accounts
/// the delegated relationship state reports `viewer` as following, applied
/// as the final filter (after dedup).
#[tokio::test]
async fn hydrate_accounts_following_only_narrows_to_followed_accounts() {
    let app = spawn_test_app().await;

    let viewer = create_test_actor(&app, "viewer").await;
    let followed = create_test_actor(&app, "followed").await;
    let not_followed = create_test_actor(&app, "not_followed").await;

    app.state
        .accounts()
        .ports()
        .set_relationship_provider(Arc::new(SelectiveFollowingProvider {
            followed: vec![followed],
        }));

    let hydrator = build_hydrator(&app);
    let refs = [AccountRef::Local(followed), AccountRef::Local(not_followed)];

    let rendered = hydrator
        .hydrate_accounts(&refs, viewer, true)
        .await
        .expect("hydrate_accounts must succeed");

    assert_eq!(rendered.len(), 1);
    assert_eq!(
        rendered[0]["id"].as_str().unwrap(),
        followed.as_i64().to_string()
    );

    app.cleanup().await;
}

// -- hydrate_statuses (Requirements 1.2, 4.2, 4.6) -------------------------

/// Requirement 4.2: a `private` post authored by someone `viewer` does not
/// follow is excluded, even though it was handed to the hydrator as a
/// candidate id (mirroring `PgSearchBackend::search_statuses`'s own
/// documented "candidates may include invisible posts" contract) — the
/// hydrator's own `VisibilityPolicy` application must never leak it.
#[tokio::test]
async fn hydrate_statuses_excludes_a_post_invisible_to_the_viewer() {
    let app = spawn_test_app().await;
    let hydrator = build_hydrator(&app);

    let author = create_test_actor(&app, "author").await;
    let viewer = create_test_actor(&app, "viewer").await;

    let public_id = create_test_status(&app, author, Visibility::Public, "hello rustlang").await;
    let private_id = create_test_status(&app, author, Visibility::Private, "secret rustlang").await;

    let rendered = hydrator
        .hydrate_statuses(&[public_id, private_id], viewer, 20)
        .await
        .expect("hydrate_statuses must succeed");

    assert_eq!(rendered.len(), 1, "the private post must be excluded");
    assert_eq!(
        rendered[0]["id"].as_str().unwrap(),
        public_id.as_i64().to_string()
    );
    assert_eq!(rendered[0]["content"], "hello rustlang");

    app.cleanup().await;
}

/// A post's own author always sees it, regardless of visibility (mirrors
/// `crate::statuses::visibility::is_visible`'s own author-always-visible
/// rule) — proving this hydrator does not add any *extra* restriction
/// beyond that shared policy.
#[tokio::test]
async fn hydrate_statuses_includes_a_private_post_visible_to_its_own_author() {
    let app = spawn_test_app().await;
    let hydrator = build_hydrator(&app);

    let author = create_test_actor(&app, "author2").await;
    let private_id =
        create_test_status(&app, author, Visibility::Private, "author's own secret").await;

    let rendered = hydrator
        .hydrate_statuses(&[private_id], author, 20)
        .await
        .expect("hydrate_statuses must succeed");

    assert_eq!(rendered.len(), 1);
    assert_eq!(
        rendered[0]["id"].as_str().unwrap(),
        private_id.as_i64().to_string()
    );

    app.cleanup().await;
}

/// Requirement 4.6: once `limit` visible results have been collected,
/// `hydrate_statuses` stops taking further candidates, even when more
/// visible candidates remain in `ids` (the post-visibility-filter
/// truncation `PgSearchBackend`'s overfetch convention hands off to this
/// method).
#[tokio::test]
async fn hydrate_statuses_truncates_to_the_requested_limit_after_visibility_filtering() {
    let app = spawn_test_app().await;
    let hydrator = build_hydrator(&app);

    let author = create_test_actor(&app, "author3").await;
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(create_test_status(&app, author, Visibility::Public, &format!("post {i}")).await);
    }

    let rendered = hydrator
        .hydrate_statuses(&ids, author, 2)
        .await
        .expect("hydrate_statuses must succeed");

    assert_eq!(rendered.len(), 2, "must truncate to the requested limit");

    app.cleanup().await;
}

/// An unknown status id among the candidates is simply skipped, not an
/// error (mirrors this crate's established "dangling reference is not a
/// hydration failure" convention).
#[tokio::test]
async fn hydrate_statuses_skips_an_unknown_id_without_erroring() {
    let app = spawn_test_app().await;
    let hydrator = build_hydrator(&app);

    let author = create_test_actor(&app, "author4").await;
    let real_id = create_test_status(&app, author, Visibility::Public, "real post").await;
    let missing_id = app.runtime.ids.next_id();

    let rendered = hydrator
        .hydrate_statuses(&[missing_id, real_id], author, 20)
        .await
        .expect("hydrate_statuses must succeed");

    assert_eq!(rendered.len(), 1);
    assert_eq!(
        rendered[0]["id"].as_str().unwrap(),
        real_id.as_i64().to_string()
    );

    app.cleanup().await;
}

// -- hydrate_hashtags (Requirement 5.2) ------------------------------------

/// Requirement 5.2: a [`TagMatch`] backed by a real `search_tags` row
/// renders into Tag JSON (`name`/`url`/`history`) via `TagSerializer`.
#[tokio::test]
async fn hydrate_hashtags_renders_a_matched_tag_to_tag_json() {
    let app = spawn_test_app().await;
    let hydrator = build_hydrator(&app);

    let status_id = app.runtime.ids.next_id();
    upsert_tag_usage(
        &app.pool,
        "rustlang",
        app.runtime.ids.next_id(),
        status_id,
        app.runtime.clock.now(),
    )
    .await
    .expect("upsert_tag_usage must succeed");

    let matches = [TagMatch {
        name: "rustlang".to_string(),
    }];
    let rendered = hydrator
        .hydrate_hashtags(&matches)
        .await
        .expect("hydrate_hashtags must succeed");

    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0]["name"], "rustlang");
    assert_eq!(
        rendered[0]["url"],
        format!("https://{}/tags/rustlang", app.state.config().server.domain)
    );
    assert!(rendered[0]["history"].is_array());

    app.cleanup().await;
}

/// A [`TagMatch`] whose name no longer resolves to any `search_tags` row
/// (e.g. a race with a concurrent delete) is silently skipped, not an
/// error.
#[tokio::test]
async fn hydrate_hashtags_skips_a_tag_that_no_longer_resolves() {
    let app = spawn_test_app().await;
    let hydrator = build_hydrator(&app);

    let matches = [TagMatch {
        name: "doesnotexist".to_string(),
    }];
    let rendered = hydrator
        .hydrate_hashtags(&matches)
        .await
        .expect("hydrate_hashtags must succeed");

    assert!(rendered.is_empty());

    app.cleanup().await;
}

/// Multiple distinct tag names each render their own Tag JSON, in the same
/// order `tags` was given.
#[tokio::test]
async fn hydrate_hashtags_renders_multiple_tags_in_order() {
    let app = spawn_test_app().await;
    let hydrator = build_hydrator(&app);

    for name in ["mastodon", "fediverse"] {
        upsert_tag_usage(
            &app.pool,
            name,
            app.runtime.ids.next_id(),
            app.runtime.ids.next_id(),
            app.runtime.clock.now(),
        )
        .await
        .expect("upsert_tag_usage must succeed");
    }

    let matches = [
        TagMatch {
            name: "mastodon".to_string(),
        },
        TagMatch {
            name: "fediverse".to_string(),
        },
    ];
    let rendered = hydrator
        .hydrate_hashtags(&matches)
        .await
        .expect("hydrate_hashtags must succeed");

    let names: Vec<&str> = rendered
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["mastodon", "fediverse"]);

    app.cleanup().await;
}
