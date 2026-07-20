//! Integration tests for `AccountProfileRepository` (Requirements 1.4, 2.2,
//! 6.1, 6.5), per task 2.1's observable completion condition: "upsert が
//! patch 外の項目を変更しないことを検証する統合テストが green".
//!
//! Mirrors `src/media/media_repository/tests.rs`'s established convention:
//! reuses `crate::test_harness::spawn_test_app` for an isolated,
//! already-migrated schema and a deterministic `RuntimeContext`, and creates
//! a real owner + local actor row first (`account_profiles.actor_id` is a
//! logical reference to `local_actors.id`; nothing enforces a hard FK, but
//! exercising real actor rows keeps these tests representative of the real
//! call shape a future `AccountService`, task 5.x, will drive).

use super::{find_profile, upsert_profile};
use crate::accounts::model::{AccountProfile, ProfileField, ProfilePatch};
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::domain::{Id, Visibility};
use crate::test_harness::spawn_test_app;

/// Creates a real owner + local actor row, returning the actor's `Id`, so
/// tests have a genuine local actor to bind a profile to.
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

/// Requirement 1.4/2.2: an actor with no `account_profiles` row yet is
/// reported as `None`, not a substituted default — the "safe default" is a
/// separate opt-in constructor (`AccountProfile::default_for`), not
/// something `find_profile` performs itself.
#[tokio::test]
async fn find_profile_returns_none_for_an_actor_with_no_profile_row_yet() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "alice").await;

    let found = find_profile(&app.pool, actor_id)
        .await
        .expect("find_profile must succeed even when no row exists");
    assert!(found.is_none());

    app.cleanup().await;
}

/// `AccountProfile::default_for` (this task's reconciliation of the task
/// text's "safe default" requirement) produces the same safe-default shape
/// `account_profiles`' own column `DEFAULT`s describe: empty display_name/
/// note, no avatar/header, empty fields, locked/bot/discoverable false,
/// public/non-sensitive source with no language and no fields.
#[test]
fn account_profile_default_for_is_the_safe_default_shape() {
    let actor_id = Id::from_i64(4242);
    let profile = AccountProfile::default_for(actor_id);

    assert_eq!(profile.actor_id, actor_id);
    assert_eq!(profile.display_name, "");
    assert_eq!(profile.note, "");
    assert!(profile.avatar_media.is_none());
    assert!(profile.header_media.is_none());
    assert!(profile.fields.is_empty());
    assert!(!profile.locked);
    assert!(!profile.bot);
    assert!(!profile.discoverable);
    assert_eq!(profile.source.privacy, Visibility::Public);
    assert!(!profile.source.sensitive);
    assert!(profile.source.language.is_none());
    assert!(profile.source.fields.is_empty());
    assert_eq!(profile.source.follow_requests_count, 0);
}

/// Requirement 6.1/6.5: `upsert_profile` against an actor with no row yet
/// creates one, applying only the patched items and leaving every other
/// item at its safe default (mirroring `account_profiles`' own column
/// `DEFAULT`s).
#[tokio::test]
async fn upsert_profile_creates_a_row_with_only_patched_items_set() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "bob").await;
    let now = app.runtime.clock.now();

    let patch = ProfilePatch {
        display_name: Some("Bob".to_string()),
        note: Some("Hello, I'm Bob.".to_string()),
        ..ProfilePatch::default()
    };

    let profile = upsert_profile(&app.pool, actor_id, patch, now)
        .await
        .expect("upsert_profile must succeed for a fresh actor");

    assert_eq!(profile.actor_id, actor_id);
    assert_eq!(profile.display_name, "Bob");
    assert_eq!(profile.note, "Hello, I'm Bob.");
    // Everything not in the patch keeps its safe default.
    assert!(profile.avatar_media.is_none());
    assert!(profile.header_media.is_none());
    assert!(profile.fields.is_empty());
    assert!(!profile.locked);
    assert!(!profile.bot);
    assert!(!profile.discoverable);
    assert_eq!(profile.source.privacy, Visibility::Public);
    assert!(!profile.source.sensitive);
    assert!(profile.source.language.is_none());

    let reloaded = find_profile(&app.pool, actor_id)
        .await
        .expect("find_profile must succeed")
        .expect("the row created by upsert_profile must now be findable");
    assert_eq!(reloaded, profile);

    app.cleanup().await;
}

/// The crux of task 2.1's own observable completion condition: applying a
/// second, narrower patch must not change any item the second patch left
/// `None` — even though the first patch set several distinct items across
/// several different columns.
#[tokio::test]
async fn upsert_profile_does_not_change_items_outside_the_patch() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "carol").await;
    let now = app.runtime.clock.now();
    let avatar_id = Id::from_i64(9001);
    let header_id = Id::from_i64(9002);

    let initial_patch = ProfilePatch {
        display_name: Some("Carol".to_string()),
        note: Some("Initial bio.".to_string()),
        avatar_media: Some(Some(avatar_id)),
        header_media: Some(Some(header_id)),
        fields: Some(vec![ProfileField {
            name: "Pronouns".to_string(),
            value: "she/her".to_string(),
            verified_at: None,
        }]),
        locked: Some(true),
        bot: Some(false),
        discoverable: Some(true),
        source_privacy: Some(Visibility::Unlisted),
        source_sensitive: Some(true),
        source_language: Some(Some("en".to_string())),
    };
    let after_initial = upsert_profile(&app.pool, actor_id, initial_patch, now)
        .await
        .expect("initial upsert_profile must succeed");
    assert_eq!(after_initial.display_name, "Carol");
    assert_eq!(after_initial.note, "Initial bio.");
    assert_eq!(after_initial.avatar_media, Some(avatar_id));
    assert_eq!(after_initial.header_media, Some(header_id));
    assert!(after_initial.locked);
    assert!(after_initial.discoverable);
    assert_eq!(after_initial.source.privacy, Visibility::Unlisted);
    assert!(after_initial.source.sensitive);
    assert_eq!(after_initial.source.language.as_deref(), Some("en"));

    // Second patch touches only `note` — everything else must stay exactly
    // as the first upsert left it.
    let later = app.runtime.clock.now();
    let narrow_patch = ProfilePatch {
        note: Some("Updated bio.".to_string()),
        ..ProfilePatch::default()
    };
    let after_narrow = upsert_profile(&app.pool, actor_id, narrow_patch, later)
        .await
        .expect("narrow upsert_profile must succeed");

    assert_eq!(after_narrow.note, "Updated bio.");
    // Every item outside the second patch is untouched.
    assert_eq!(after_narrow.display_name, "Carol");
    assert_eq!(after_narrow.avatar_media, Some(avatar_id));
    assert_eq!(after_narrow.header_media, Some(header_id));
    assert_eq!(after_narrow.fields, after_initial.fields);
    assert!(after_narrow.locked);
    assert!(!after_narrow.bot);
    assert!(after_narrow.discoverable);
    assert_eq!(after_narrow.source.privacy, Visibility::Unlisted);
    assert!(after_narrow.source.sensitive);
    assert_eq!(after_narrow.source.language.as_deref(), Some("en"));

    let reloaded = find_profile(&app.pool, actor_id)
        .await
        .expect("find_profile must succeed")
        .expect("row must still exist");
    assert_eq!(reloaded, after_narrow);

    app.cleanup().await;
}

/// The doubled `Option<Option<Id>>` fields' "leave unchanged" (outer `None`)
/// vs. "explicitly clear" (outer `Some(None)`) distinction (Requirements
/// 6.1, 6.5; `ProfilePatch`'s own doc comment): an outer `None` must leave a
/// previously-set avatar untouched, while an outer `Some(None)` must clear
/// it to `None`, and the two must not be conflated.
#[tokio::test]
async fn upsert_profile_distinguishes_leaving_avatar_unchanged_from_clearing_it() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "dave").await;
    let now = app.runtime.clock.now();
    let avatar_id = Id::from_i64(7001);

    let set_patch = ProfilePatch {
        avatar_media: Some(Some(avatar_id)),
        ..ProfilePatch::default()
    };
    let after_set = upsert_profile(&app.pool, actor_id, set_patch, now)
        .await
        .expect("setting the avatar must succeed");
    assert_eq!(after_set.avatar_media, Some(avatar_id));

    // Leave unchanged: outer `None`.
    let leave_unchanged = ProfilePatch::default();
    let after_leave = upsert_profile(&app.pool, actor_id, leave_unchanged, now)
        .await
        .expect("a no-op patch must still succeed");
    assert_eq!(after_leave.avatar_media, Some(avatar_id));

    // Explicitly clear: outer `Some(None)`.
    let clear_patch = ProfilePatch {
        avatar_media: Some(None),
        ..ProfilePatch::default()
    };
    let after_clear = upsert_profile(&app.pool, actor_id, clear_patch, now)
        .await
        .expect("clearing the avatar must succeed");
    assert!(after_clear.avatar_media.is_none());

    app.cleanup().await;
}

/// Requirement 2.2: `fields` (a `ProfileField[]`, including a `verified_at`
/// timestamp) round-trips through the JSONB column exactly, across an
/// upsert -> find_profile round trip.
#[tokio::test]
async fn upsert_profile_round_trips_fields_including_verified_at() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "erin").await;
    let now = app.runtime.clock.now();

    let patch = ProfilePatch {
        fields: Some(vec![
            ProfileField {
                name: "Pronouns".to_string(),
                value: "she/her".to_string(),
                verified_at: None,
            },
            ProfileField {
                name: "Website".to_string(),
                value: "https://erin.example".to_string(),
                verified_at: Some(now),
            },
        ]),
        ..ProfilePatch::default()
    };
    let profile = upsert_profile(&app.pool, actor_id, patch, now)
        .await
        .expect("upsert_profile must succeed");

    assert_eq!(profile.fields.len(), 2);
    assert_eq!(profile.fields[0].name, "Pronouns");
    assert_eq!(profile.fields[0].value, "she/her");
    assert!(profile.fields[0].verified_at.is_none());
    assert_eq!(profile.fields[1].name, "Website");
    assert_eq!(profile.fields[1].value, "https://erin.example");
    // `verified_at` is carried as a Unix timestamp, so sub-second precision
    // is not guaranteed to round-trip; compare at second granularity.
    assert_eq!(
        profile.fields[1]
            .verified_at
            .expect("verified_at must round-trip as Some")
            .unix_timestamp(),
        now.unix_timestamp()
    );
    assert_eq!(profile.source.fields, profile.fields);

    let reloaded = find_profile(&app.pool, actor_id)
        .await
        .expect("find_profile must succeed")
        .expect("row must exist");
    assert_eq!(reloaded.fields, profile.fields);

    app.cleanup().await;
}

/// `upsert_profile`'s `ON CONFLICT` path is a genuine upsert, not an
/// accidental duplicate-row insert: two upserts against the same
/// `actor_id` still leave exactly one row behind, findable via
/// `find_profile`.
#[tokio::test]
async fn upsert_profile_does_not_create_duplicate_rows_for_the_same_actor() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "frank").await;
    let now = app.runtime.clock.now();

    upsert_profile(
        &app.pool,
        actor_id,
        ProfilePatch {
            display_name: Some("Frank".to_string()),
            ..ProfilePatch::default()
        },
        now,
    )
    .await
    .expect("first upsert must succeed");

    upsert_profile(
        &app.pool,
        actor_id,
        ProfilePatch {
            note: Some("Second write.".to_string()),
            ..ProfilePatch::default()
        },
        now,
    )
    .await
    .expect("second upsert must succeed");

    let row_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM account_profiles WHERE actor_id = $1")
            .bind(actor_id.as_i64())
            .fetch_one(&app.pool)
            .await
            .expect("counting rows must succeed");
    assert_eq!(row_count.0, 1);

    app.cleanup().await;
}
