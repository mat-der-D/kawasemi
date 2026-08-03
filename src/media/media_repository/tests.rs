//! Integration tests for `MediaRepository` (Requirements 1.1, 1.2, 2.2, 2.3,
//! 2.4, 3.1, 3.3, 4.3), per task 3.1's observable completion condition:
//! "挿入したメディアが所有アクターでのみ取得でき、状態とメタの更新が反映
//! されることを統合テストで確認できる".
//!
//! Mirrors `src/actor/repository/tests.rs`'s established convention: reuses
//! `crate::test_harness::spawn_test_app` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext`. Every test creates a real
//! owner + local actor row first (`media.actor_id` is a logical reference to
//! `local_actors.id`; nothing here enforces a hard FK, but exercising real
//! actor rows keeps these tests representative of the real call shape a
//! future `MediaService`, task 4.1, will drive).

use super::{find_by_id, find_owned, insert_media, set_failed, set_ready, update_metadata};
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::domain::Id;
use crate::media::model::{Dimensions, Focus, Media, MediaMeta, MediaState, MediaType};
use crate::test_harness::spawn_test_app;

/// Creates a real owner + local actor row, returning the actor's `Id`, so
/// tests have a genuine owning actor to bind media to.
async fn create_test_actor(app: &crate::test_harness::TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = LocalActor {
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

/// Builds a freshly-accepted, still-`processing` `Media` value ready to hand
/// to `insert_media` (mirroring `MediaService::accept_upload`'s expected
/// pre-insert shape, task 4.1, not yet implemented).
fn sample_media(id: Id, actor_id: Id, now: time::OffsetDateTime) -> Media {
    Media {
        id,
        actor_id,
        media_type: MediaType::Image,
        state: MediaState::Processing,
        description: None,
        focus: Focus::default(),
        meta: None,
        blurhash: None,
        created_at: now,
    }
}

/// Requirements 1.1, 1.2, 2.2, 2.3: an inserted media is retrievable via
/// `find_owned` by its owning actor, with the fields it was inserted with.
#[tokio::test]
async fn insert_media_persists_a_row_findable_by_its_owning_actor() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "alice").await;

    let media_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let media = sample_media(media_id, actor_id, now);

    insert_media(&app.pool, &media, "1/original", "image/png")
        .await
        .expect("insert_media must succeed for a fresh id/actor");

    let found = find_owned(&app.pool, media_id, actor_id)
        .await
        .expect("find_owned must succeed")
        .expect("the just-inserted media must be found by its owning actor");
    assert_eq!(found, media);

    app.cleanup().await;
}

/// Requirement 2.4: `find_owned` never returns another actor's media — the
/// exact postcondition design.md states for this component.
#[tokio::test]
async fn find_owned_does_not_return_another_actors_media() {
    let app = spawn_test_app().await;
    let owner_actor = create_test_actor(&app, "owner_actor").await;
    let other_actor = create_test_actor(&app, "other_actor").await;

    let media_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let media = sample_media(media_id, owner_actor, now);
    insert_media(&app.pool, &media, "2/original", "image/jpeg")
        .await
        .expect("insert_media must succeed");

    let as_other = find_owned(&app.pool, media_id, other_actor)
        .await
        .expect("find_owned must succeed even for a non-owning actor");
    assert!(
        as_other.is_none(),
        "find_owned must not return media belonging to a different actor"
    );

    let as_owner = find_owned(&app.pool, media_id, owner_actor)
        .await
        .expect("find_owned must succeed")
        .expect("the owning actor must still find its own media");
    assert_eq!(as_owner, media);

    app.cleanup().await;
}

/// `find_owned` returns `Ok(None)` — not an error — for a `media_id`
/// nothing was ever inserted under.
#[tokio::test]
async fn find_owned_returns_none_for_an_unknown_media_id() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "bob").await;

    let unknown_id = Id::from_i64(i64::MAX - 1);
    let found = find_owned(&app.pool, unknown_id, actor_id)
        .await
        .expect("find_owned must succeed even when nothing matches");
    assert!(found.is_none());

    app.cleanup().await;
}

/// Requirements 3.1, 3.4: `update_metadata` updates description and focus
/// together and reflects the change, including while the media is still
/// `Processing`.
#[tokio::test]
async fn update_metadata_updates_description_and_focus_and_reflects_it() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "carol").await;

    let media_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let media = sample_media(media_id, actor_id, now);
    insert_media(&app.pool, &media, "3/original", "image/png")
        .await
        .expect("insert_media must succeed");
    assert_eq!(
        media.state,
        MediaState::Processing,
        "sanity: this media starts in Processing"
    );

    let later = now + time::Duration::seconds(30);
    let new_focus = Focus::new(0.5, -0.25).expect("valid focus");
    let updated = update_metadata(
        &app.pool,
        media_id,
        actor_id,
        Some("a red panda"),
        Some(new_focus),
        later,
    )
    .await
    .expect("update_metadata must succeed for an owned, existing media")
    .expect("update_metadata must return Some for an owned, existing media");

    assert_eq!(updated.description.as_deref(), Some("a red panda"));
    assert_eq!(updated.focus, new_focus);
    assert_eq!(
        updated.state,
        MediaState::Processing,
        "update_metadata must not change processing state"
    );

    let refetched = find_owned(&app.pool, media_id, actor_id)
        .await
        .expect("find_owned must succeed")
        .expect("the media must still be found");
    assert_eq!(refetched, updated);

    app.cleanup().await;
}

/// `update_metadata`'s patch semantics: an unset field (`None`) is left
/// unchanged, not blanked out.
#[tokio::test]
async fn update_metadata_leaves_unset_fields_unchanged() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "dave").await;

    let media_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let mut media = sample_media(media_id, actor_id, now);
    media.description = Some("original description".to_string());
    insert_media(&app.pool, &media, "4/original", "image/png")
        .await
        .expect("insert_media must succeed");

    // Update only the focus; description must remain untouched.
    let later = now + time::Duration::seconds(10);
    let new_focus = Focus::new(-1.0, 1.0).expect("valid focus");
    let updated = update_metadata(&app.pool, media_id, actor_id, None, Some(new_focus), later)
        .await
        .expect("update_metadata must succeed")
        .expect("media must be found");
    assert_eq!(
        updated.description.as_deref(),
        Some("original description"),
        "an unset description patch field must not blank out the existing value"
    );
    assert_eq!(updated.focus, new_focus);

    // Now update only the description; focus must remain untouched.
    let updated2 = update_metadata(
        &app.pool,
        media_id,
        actor_id,
        Some("new description"),
        None,
        later,
    )
    .await
    .expect("update_metadata must succeed")
    .expect("media must be found");
    assert_eq!(updated2.description.as_deref(), Some("new description"));
    assert_eq!(
        updated2.focus, new_focus,
        "an unset focus patch field must not reset the existing value"
    );

    app.cleanup().await;
}

/// Requirement 3.3: updating a media that is not owned by the requesting
/// actor is rejected (returns `None`, no row changed).
#[tokio::test]
async fn update_metadata_returns_none_for_a_non_owning_actor_and_does_not_modify_the_row() {
    let app = spawn_test_app().await;
    let owner_actor = create_test_actor(&app, "erin").await;
    let intruder_actor = create_test_actor(&app, "frank").await;

    let media_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let media = sample_media(media_id, owner_actor, now);
    insert_media(&app.pool, &media, "5/original", "image/png")
        .await
        .expect("insert_media must succeed");

    let later = now + time::Duration::seconds(5);
    let result = update_metadata(
        &app.pool,
        media_id,
        intruder_actor,
        Some("hijacked description"),
        None,
        later,
    )
    .await
    .expect("update_metadata must succeed (as a no-op) for a non-owning actor");
    assert!(
        result.is_none(),
        "update_metadata must return None when the actor does not own the media"
    );

    // The original row must be untouched.
    let refetched = find_owned(&app.pool, media_id, owner_actor)
        .await
        .expect("find_owned must succeed")
        .expect("media must still exist");
    assert_eq!(refetched, media);

    app.cleanup().await;
}

/// Requirement 4.3: `set_ready` transitions state to Ready and reflects the
/// derived dimensions/BlurHash.
#[tokio::test]
async fn set_ready_transitions_state_and_reflects_derived_metadata() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "grace").await;

    let media_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let media = sample_media(media_id, actor_id, now);
    insert_media(&app.pool, &media, "6/original", "image/png")
        .await
        .expect("insert_media must succeed");

    let meta = MediaMeta {
        original: Dimensions {
            width: 1920,
            height: 1080,
            aspect: 1920.0 / 1080.0,
        },
        small: Some(Dimensions {
            width: 400,
            height: 225,
            aspect: 400.0 / 225.0,
        }),
    };
    let later = now + time::Duration::seconds(15);
    set_ready(
        &app.pool,
        media_id,
        &meta,
        "LKO2?U%2Tw=w]~RBVZRi};RPxuwH",
        "6/small",
        later,
    )
    .await
    .expect("set_ready must succeed");

    let ready = find_owned(&app.pool, media_id, actor_id)
        .await
        .expect("find_owned must succeed")
        .expect("media must be found");
    assert_eq!(ready.state, MediaState::Ready);
    assert_eq!(
        ready.blurhash.as_deref(),
        Some("LKO2?U%2Tw=w]~RBVZRi};RPxuwH")
    );
    let ready_meta = ready.meta.expect("meta must be populated after set_ready");
    assert_eq!(ready_meta.original.width, 1920);
    assert_eq!(ready_meta.original.height, 1080);
    let small = ready_meta.small.expect("small dims must be populated");
    assert_eq!(small.width, 400);
    assert_eq!(small.height, 225);

    app.cleanup().await;
}

/// Requirement 4.5's resulting media-state transition: `set_failed`
/// transitions state to Failed.
#[tokio::test]
async fn set_failed_transitions_state_to_failed() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "heidi").await;

    let media_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let media = sample_media(media_id, actor_id, now);
    insert_media(&app.pool, &media, "7/original", "image/png")
        .await
        .expect("insert_media must succeed");

    let later = now + time::Duration::seconds(20);
    set_failed(&app.pool, media_id, later)
        .await
        .expect("set_failed must succeed");

    let failed = find_owned(&app.pool, media_id, actor_id)
        .await
        .expect("find_owned must succeed")
        .expect("media must be found");
    assert_eq!(failed.state, MediaState::Failed);

    app.cleanup().await;
}

/// `insert_media` requires a real `Media` value whose `actor_id` field is
/// non-optional (task 2.1's own type-level guarantee) — this test just
/// demonstrates round-tripping a description set at insert time, to cover
/// the `Some(description)` insert path (the other tests all use `None`).
#[tokio::test]
async fn insert_media_round_trips_an_initial_description() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "ivan").await;

    let media_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let mut media = sample_media(media_id, actor_id, now);
    media.description = Some("a photo of a cat".to_string());
    insert_media(&app.pool, &media, "8/original", "image/webp")
        .await
        .expect("insert_media must succeed");

    let found = find_owned(&app.pool, media_id, actor_id)
        .await
        .expect("find_owned must succeed")
        .expect("media must be found");
    assert_eq!(found.description.as_deref(), Some("a photo of a cat"));

    app.cleanup().await;
}

/// Task 4.3 addition: `find_by_id` returns the row regardless of which
/// actor owns it (unlike `find_owned`, no ownership scoping is applied at
/// all) — a `ProcessingWorker` has only a bare `media_id` from its claimed
/// job, never a requesting actor.
#[tokio::test]
async fn find_by_id_returns_the_media_row_without_any_owner_scoping() {
    let app = spawn_test_app().await;
    let owning_actor = create_test_actor(&app, "judy").await;
    let unrelated_actor = create_test_actor(&app, "kevin").await;

    let media_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let media = sample_media(media_id, owning_actor, now);
    insert_media(&app.pool, &media, "9/original", "image/png")
        .await
        .expect("insert_media must succeed");

    let found = find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must be found even though the caller supplies no actor at all");
    assert_eq!(found.id, media_id);
    assert_eq!(found.actor_id, owning_actor);

    // Sanity check that this really is unscoped, not accidentally still
    // filtering by some ambient actor: an unrelated actor querying via
    // `find_owned` must NOT see this row, while `find_by_id` does not even
    // accept an actor argument to filter by.
    let not_owned = find_owned(&app.pool, media_id, unrelated_actor)
        .await
        .expect("find_owned must succeed");
    assert!(not_owned.is_none());

    app.cleanup().await;
}

/// `find_by_id` returns `Ok(None)` for a `media_id` that was never
/// inserted, the same "not found" contract `find_owned` has for that case.
#[tokio::test]
async fn find_by_id_returns_none_for_a_nonexistent_media_id() {
    let app = spawn_test_app().await;
    let missing = crate::domain::Id::from_i64(999_999_999);

    let found = find_by_id(&app.pool, missing)
        .await
        .expect("find_by_id must succeed even for a nonexistent id");
    assert!(found.is_none());

    app.cleanup().await;
}
