//! DB-backed tests for `MuteService` (Requirements 4.1-4.5), per task 3.3's
//! own observable completion condition: "ミュートで muting が真、通知ミュー
//! ト指定で muting_notifications が真、期限指定が記録され、連合配送が発生
//! しない状態".
//!
//! Mirrors `follow_service/tests.rs`'s established convention
//! (`crate::test_harness::spawn_test_app`, `create_test_actor`/
//! `create_test_remote`/`sample_remote_account` fixtures) — this service
//! needs *real* `actor`/`remote_accounts` rows, not mocks, since target
//! existence resolution is real business logic under test here.
//!
//! ## On Requirement 4.5 ("no federation delivery")
//! Unlike `follow_service/tests.rs`, this module has no `RecordingSink`/
//! `DeliverySink` double and asserts no call count against one. That is a
//! deliberate omission, not a gap: [`MuteService`] (see `mute_service.rs`'s
//! own doc comment, "No `ActivityBuilder`/`DeliveryService`/`Transitions`
//! generic parameters") carries no `ActivityBuilder`/`DeliveryService`/
//! `DeliverySink` field or generic parameter at all — there is no code path
//! through which a `mute`/`unmute` call could dispatch any Activity, proven
//! structurally by this module's own type signature rather than by a
//! runtime assertion against a mock that this service never even holds a
//! handle to.
//!
//! ## On expiry enforcement
//! `mute_records_an_expiry_and_repository_read_reflects_it_as_expired`
//! below reads `RelationshipState` directly via
//! `repository::load_states` with a caller-supplied `now` (rather than
//! through `MuteService::mute`'s own return value, whose `now` is always
//! `runtime.clock.now()` — a `FixedClock` under `spawn_test_app`, so it
//! never itself advances) to prove `expires_at` was persisted correctly and
//! that `RelationshipMapper`/`load_states`'s already-implemented (task 2.4)
//! expiry filtering correctly treats a past `expires_at` as unmuted. This
//! service's own responsibility under test is only "did it persist the
//! right absolute timestamp", not the filtering itself.

use time::{Duration, OffsetDateTime};

use super::*;
use crate::accounts::model::{ProfileField, RemoteAccount};
use crate::accounts::remote_repository::upsert_remote;
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{ActorDirectory, ActorState, ActorType, Handle};
use crate::domain::Id;
use crate::error::ErrorKind;
use crate::test_harness::{TestApp, spawn_test_app};

// --- Test fixtures ----------------------------------------------------------

/// Creates a real owner + local actor row, returning the actor's `Id` — an
/// exact copy of `follow_service/tests.rs::create_test_actor`.
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

fn sample_remote_account(id: Id, actor_uri: &str, fetched_at: OffsetDateTime) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "alice".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Alice".to_string(),
        note: String::new(),
        url: actor_uri.to_string(),
        avatar_url: None,
        header_url: None,
        fields: Vec::<ProfileField>::new(),
        bot: false,
        locked: false,
        fetched_at,
    }
}

/// Creates a real `remote_accounts` row, returning its `Id`.
async fn create_test_remote(app: &TestApp, actor_uri: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(&app.pool, &sample_remote_account(id, actor_uri, now))
        .await
        .expect("upsert_remote must succeed");
    id
}

type TestService = MuteService<ActorDirectory>;

fn build_service(app: &TestApp) -> TestService {
    MuteService::new(
        app.pool.clone(),
        app.runtime.clone(),
        ActorDirectory::new(app.pool.clone()),
    )
}

fn default_opts() -> MuteOptions {
    MuteOptions {
        notifications: false,
        duration: None,
    }
}

fn as_bool(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .unwrap_or_else(|| panic!("relationship JSON missing '{key}'"))
        .as_bool()
        .unwrap_or_else(|| panic!("relationship JSON '{key}' was not a bool"))
}

// --- mute: local target -------------------------------------------------------

#[tokio::test]
async fn mute_sets_muting_true_for_a_local_target() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter1").await;
    let target = create_test_actor(&app, "muted1").await;

    let relationship = service
        .mute(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("mute must succeed");

    assert!(as_bool(&relationship, "muting"));
    assert!(!as_bool(&relationship, "muting_notifications"));
}

#[tokio::test]
async fn mute_with_notifications_sets_muting_notifications_true() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter2").await;
    let target = create_test_actor(&app, "muted2").await;

    let opts = MuteOptions {
        notifications: true,
        duration: None,
    };
    let relationship = service
        .mute(viewer, &target.as_i64().to_string(), opts)
        .await
        .expect("mute must succeed");

    assert!(as_bool(&relationship, "muting"));
    assert!(as_bool(&relationship, "muting_notifications"));
}

#[tokio::test]
async fn mute_without_notifications_leaves_muting_notifications_false() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter3").await;
    let target = create_test_actor(&app, "muted3").await;

    let relationship = service
        .mute(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("mute must succeed");

    assert!(as_bool(&relationship, "muting"));
    assert!(!as_bool(&relationship, "muting_notifications"));
}

#[tokio::test]
async fn mute_records_an_expiry_and_repository_read_reflects_it_as_expired() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter4").await;
    let target = create_test_actor(&app, "muted4").await;

    let opts = MuteOptions {
        notifications: false,
        duration: Some(3600), // 1 hour
    };
    service
        .mute(viewer, &target.as_i64().to_string(), opts)
        .await
        .expect("mute must succeed");

    let viewer_ref = AccountRef::Local(viewer);
    let target_ref = AccountRef::Local(target);
    let mute_time = app.runtime.clock.now();

    // Immediately (before the duration elapses): still muted.
    let still_muted = repository::load_states(
        &app.pool,
        &viewer_ref,
        std::slice::from_ref(&target_ref),
        mute_time + Duration::seconds(1),
    )
    .await
    .expect("load_states must succeed");
    assert!(
        still_muted[0].mute.is_some(),
        "a mute with an unexpired duration must still read as muted"
    );

    // 1 hour and 1 second later: expired.
    let after_expiry = repository::load_states(
        &app.pool,
        &viewer_ref,
        std::slice::from_ref(&target_ref),
        mute_time + Duration::seconds(3601),
    )
    .await
    .expect("load_states must succeed");
    assert!(
        after_expiry[0].mute.is_none(),
        "an expired mute must read as unmuted (Requirement 4.3)"
    );
}

#[tokio::test]
async fn mute_without_duration_never_expires() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter5").await;
    let target = create_test_actor(&app, "muted5").await;

    service
        .mute(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("mute must succeed");

    let viewer_ref = AccountRef::Local(viewer);
    let target_ref = AccountRef::Local(target);
    let far_future = app.runtime.clock.now() + Duration::days(3650);

    let state = repository::load_states(
        &app.pool,
        &viewer_ref,
        std::slice::from_ref(&target_ref),
        far_future,
    )
    .await
    .expect("load_states must succeed");
    assert!(
        state[0].mute.is_some(),
        "an unbounded (no-duration) mute must never expire"
    );
}

#[tokio::test]
async fn mute_is_idempotent_and_repeat_call_updates_flags_and_expiry() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter6").await;
    let target = create_test_actor(&app, "muted6").await;

    let first = service
        .mute(
            viewer,
            &target.as_i64().to_string(),
            MuteOptions {
                notifications: false,
                duration: None,
            },
        )
        .await
        .expect("first mute must succeed");
    assert!(as_bool(&first, "muting"));
    assert!(!as_bool(&first, "muting_notifications"));

    let second = service
        .mute(
            viewer,
            &target.as_i64().to_string(),
            MuteOptions {
                notifications: true,
                duration: Some(60),
            },
        )
        .await
        .expect("repeat mute must succeed idempotently");
    assert!(as_bool(&second, "muting"));
    assert!(
        as_bool(&second, "muting_notifications"),
        "a repeat mute call must refresh notifications/expiry rather than \
         ignoring the new options (Requirement 1.6's idempotency principle \
         applied to mutes, per repository.rs::upsert_mute's own doc comment)"
    );
}

#[tokio::test]
async fn mute_returns_not_found_for_a_nonexistent_target() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter7").await;

    let err = service
        .mute(viewer, "999999999", default_opts())
        .await
        .expect_err("a nonexistent target must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
}

// --- mute: remote target -------------------------------------------------------

#[tokio::test]
async fn mute_sets_muting_true_for_a_remote_target() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter8").await;
    let target = create_test_remote(&app, "https://remote.example/users/eve").await;

    let relationship = service
        .mute(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("mute must succeed");

    assert!(as_bool(&relationship, "muting"));
}

// --- unmute --------------------------------------------------------------------

#[tokio::test]
async fn unmute_clears_muting_and_returns_updated_relationship() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter9").await;
    let target = create_test_actor(&app, "muted9").await;

    service
        .mute(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("mute must succeed");

    let relationship = service
        .unmute(viewer, &target.as_i64().to_string())
        .await
        .expect("unmute must succeed");

    assert!(!as_bool(&relationship, "muting"));
    assert!(!as_bool(&relationship, "muting_notifications"));
}

#[tokio::test]
async fn unmute_is_a_noop_when_no_mute_exists() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter10").await;
    let target = create_test_actor(&app, "muted10").await;

    let relationship = service
        .unmute(viewer, &target.as_i64().to_string())
        .await
        .expect("unmute must succeed even with no prior mute");

    assert!(!as_bool(&relationship, "muting"));
}

#[tokio::test]
async fn unmute_returns_not_found_for_a_nonexistent_target() {
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter11").await;

    let err = service
        .unmute(viewer, "999999999")
        .await
        .expect_err("a nonexistent target must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
}

// --- mute leaves other relationship flags untouched ----------------------------

#[tokio::test]
async fn mute_does_not_affect_an_existing_follow_relationship() {
    // Requirement 4's mute/unmute operate purely on the `mutes` table --
    // muting an already-followed target must not disturb `following`.
    let app = spawn_test_app().await;
    let service = build_service(&app);

    let viewer = create_test_actor(&app, "muter12").await;
    let target = create_test_actor(&app, "muted12").await;

    // Establish a follow directly via the repository (no need to go through
    // `FollowService` -- this test only cares that `MuteService` leaves an
    // existing `follows` row untouched).
    let viewer_ref = AccountRef::Local(viewer);
    let target_ref = AccountRef::Local(target);
    let now = app.runtime.clock.now();
    let follow = crate::social_graph::model::Follow {
        follower: viewer_ref,
        followee: target_ref,
        reblogs: true,
        notify: false,
        languages: Vec::new(),
        activity_id: "test-activity".to_string(),
        created_at: now,
    };
    repository::upsert_follow(&app.pool, app.runtime.ids.next_id(), &follow)
        .await
        .expect("upsert_follow must succeed");

    let relationship = service
        .mute(viewer, &target.as_i64().to_string(), default_opts())
        .await
        .expect("mute must succeed");

    assert!(as_bool(&relationship, "following"));
    assert!(as_bool(&relationship, "muting"));
}
