//! Tests for `MediaService` (Requirements 1.1, 1.3, 1.4, 1.5, 1.6, 2.1, 2.2,
//! 3.1, 3.2, 7.4), per task 4.1's observable completion condition: "有効入
//! 力で processing 状態のメディアが作られジョブが投入されること、不正入力
//! が拒否されることを統合テストで確認できる".
//!
//! Split into two groups:
//! - pure unit tests (no I/O at all) for the validation helpers
//!   ([`validate_format`], [`validate_size`], [`validate_focus`],
//!   [`media_type_for_content_type`]) — these ran RED before this module's
//!   implementation existed (`cargo check` failed to resolve `super::*`'s
//!   names), then GREEN once `service.rs` was implemented;
//! - integration tests against a real, isolated-schema Postgres instance
//!   (`crate::test_harness::spawn_test_app`, mirroring
//!   `media_repository/tests.rs`'s/`job_queue/tests.rs`'s established
//!   convention) exercising `MediaService::accept_upload`/`show_media`/
//!   `update_metadata` end to end through a real `LocalFsStore` (task 2.2,
//!   already implemented, reused here rather than a fake — this task's own
//!   text asks for "the storage/DB/queue integration path" to be proven,
//!   not just the pure validation logic in isolation).

use super::*;
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::media::job_queue::claim_due;
use crate::media::local_fs::LocalFsStore;
use crate::test_harness::{TestApp, spawn_test_app};

// ---- pure unit tests: validate_format / validate_size / validate_focus /
// media_type_for_content_type ----

fn sample_config() -> MediaConfig {
    MediaConfig {
        storage_root: std::path::PathBuf::from("media_storage"),
        max_upload_size_bytes: 1024,
        thumbnail_target_width: 400,
        thumbnail_target_height: 400,
        supported_formats: vec!["image/jpeg".to_string(), "image/png".to_string()],
        worker_concurrency: 2,
        max_retry_attempts: 5,
        lease_duration: std::time::Duration::from_secs(5 * 60),
    }
}

#[test]
fn validate_format_accepts_a_configured_format() {
    let config = sample_config();
    assert!(validate_format(&config, "image/png").is_ok());
    assert!(validate_format(&config, "image/jpeg").is_ok());
}

#[test]
fn validate_format_rejects_an_unconfigured_format() {
    let config = sample_config();
    let err = validate_format(&config, "video/mp4").expect_err("unsupported format must reject");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn validate_size_accepts_a_size_at_or_under_the_limit() {
    let config = sample_config();
    assert!(validate_size(&config, 1024).is_ok());
    assert!(validate_size(&config, 0).is_ok());
}

#[test]
fn validate_size_rejects_a_size_over_the_limit() {
    let config = sample_config();
    let err = validate_size(&config, 1025).expect_err("oversized upload must reject");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn validate_focus_accepts_none_as_unspecified() {
    assert_eq!(validate_focus(None).unwrap(), None);
}

#[test]
fn validate_focus_accepts_an_in_range_coordinate_pair() {
    let focus = validate_focus(Some((0.5, -0.5))).unwrap().unwrap();
    assert_eq!(focus, Focus::new(0.5, -0.5).unwrap());
}

#[test]
fn validate_focus_rejects_an_out_of_range_coordinate_pair() {
    let err = validate_focus(Some((2.0, 0.0))).expect_err("out-of-range focus must reject");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn media_type_for_content_type_maps_image_types_to_image() {
    assert_eq!(media_type_for_content_type("image/png"), MediaType::Image);
    assert_eq!(media_type_for_content_type("image/jpeg"), MediaType::Image);
}

#[test]
fn media_type_for_content_type_maps_non_image_types_to_unknown() {
    assert_eq!(media_type_for_content_type("video/mp4"), MediaType::Unknown);
}

// ---- integration tests: accept_upload / show_media / update_metadata ----

/// Creates a real owner + local actor row, returning the actor's `Id` (same
/// helper shape as `media_repository/tests.rs::create_test_actor`).
async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
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

/// Builds a process-unique temp directory path for a throwaway
/// `LocalFsStore` root (mirrors `local_fs.rs::tests::unique_temp_root`'s own
/// "counter + nanos" convention, duplicated locally since that helper is
/// private to `local_fs.rs`'s own test module).
fn unique_temp_root(label: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kawasemi_media_service_test_{label}_{nanos}_{seq}"))
}

struct TempDirGuard(std::path::PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_media_config() -> MediaConfig {
    MediaConfig {
        storage_root: std::path::PathBuf::from("media_storage"),
        max_upload_size_bytes: 1024,
        thumbnail_target_width: 400,
        thumbnail_target_height: 400,
        supported_formats: vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "image/gif".to_string(),
            "image/webp".to_string(),
        ],
        worker_concurrency: 2,
        max_retry_attempts: 5,
        lease_duration: std::time::Duration::from_secs(5 * 60),
    }
}

fn test_service(app: &TestApp, label: &str) -> (MediaService<LocalFsStore>, TempDirGuard) {
    let root = unique_temp_root(label);
    let guard = TempDirGuard(root.clone());
    let store = LocalFsStore::new(root);
    let service = MediaService::new(
        app.pool.clone(),
        app.runtime.clone(),
        test_media_config(),
        store,
    );
    (service, guard)
}

fn sample_upload(focus: Option<(f32, f32)>) -> UploadInput {
    UploadInput {
        bytes: b"not a real image but bytes are bytes for this task's boundary".to_vec(),
        content_type: "image/png".to_string(),
        description: Some("a test image".to_string()),
        focus,
    }
}

/// Counts `media` rows owned by `actor_id`, used to prove a rejected upload
/// left no row behind (Requirements 1.3, 1.4).
async fn count_media_rows_for_actor(pool: &sqlx::PgPool, actor_id: Id) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media WHERE actor_id = $1")
        .bind(actor_id.as_i64())
        .fetch_one(pool)
        .await
        .expect("counting media rows must succeed")
}

/// Requirements 1.1, 1.2, 1.5, 1.6, 2.1: a valid upload is accepted, bound
/// to the requesting actor, created in `Processing` state with its
/// description/focus recorded, and a processing job is enqueued for it.
#[tokio::test]
async fn accept_upload_with_valid_input_creates_processing_media_and_enqueues_job() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "alice").await;
    let (service, _guard) = test_service(&app, "accept_upload_valid");

    let media = service
        .accept_upload(actor_id, sample_upload(Some((0.25, -0.25))))
        .await
        .expect("a valid upload must be accepted");

    assert_eq!(media.actor_id, actor_id);
    assert_eq!(media.state, MediaState::Processing);
    assert_eq!(media.media_type, MediaType::Image);
    assert_eq!(media.description.as_deref(), Some("a test image"));
    assert_eq!(media.focus, Focus::new(0.25, -0.25).unwrap());
    assert!(media.meta.is_none());
    assert!(media.blurhash.is_none());

    // The insert is real and owner-scoped-findable.
    let found = service
        .show_media(actor_id, media.id)
        .await
        .expect("show_media must succeed")
        .expect("just-inserted media must be findable by its owner");
    assert_eq!(found, media);

    // A processing job was enqueued for this media id (Requirement 1.6):
    // claim it and confirm it targets the just-created media.
    let now = app.runtime.clock.now();
    let job = claim_due(
        &app.pool,
        now,
        time::Duration::try_from(test_media_config().lease_duration).unwrap(),
    )
    .await
    .expect("claim_due must succeed")
    .expect("a job must have been enqueued for the accepted upload");
    assert_eq!(job.media_id, media.id);
    assert_eq!(job.attempts, 0);
}

/// Requirement 1.3: an unsupported format is rejected before any media row
/// is created.
#[tokio::test]
async fn accept_upload_rejects_an_unsupported_format_and_creates_nothing() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "bob").await;
    let (service, _guard) = test_service(&app, "accept_upload_bad_format");

    let before = count_media_rows_for_actor(&app.pool, actor_id).await;

    let mut input = sample_upload(None);
    input.content_type = "video/mp4".to_string();
    let err = service
        .accept_upload(actor_id, input)
        .await
        .expect_err("an unsupported format must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    let after = count_media_rows_for_actor(&app.pool, actor_id).await;
    assert_eq!(
        before, after,
        "a rejected upload must not create a media row"
    );
}

/// Requirement 1.4: an oversized upload is rejected before any media row is
/// created.
#[tokio::test]
async fn accept_upload_rejects_an_oversized_upload_and_creates_nothing() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "carol").await;
    let (service, _guard) = test_service(&app, "accept_upload_too_big");

    let before = count_media_rows_for_actor(&app.pool, actor_id).await;

    let mut input = sample_upload(None);
    input.bytes = vec![0u8; 2048]; // over test_media_config()'s 1024-byte limit
    let err = service
        .accept_upload(actor_id, input)
        .await
        .expect_err("an oversized upload must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    let after = count_media_rows_for_actor(&app.pool, actor_id).await;
    assert_eq!(
        before, after,
        "a rejected upload must not create a media row"
    );
}

/// Requirement 7.4: an out-of-range focal point supplied at upload time is
/// rejected before any media row is created (not silently clamped/ignored).
#[tokio::test]
async fn accept_upload_rejects_an_out_of_range_focus_and_creates_nothing() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "dave").await;
    let (service, _guard) = test_service(&app, "accept_upload_bad_focus");

    let before = count_media_rows_for_actor(&app.pool, actor_id).await;

    let err = service
        .accept_upload(actor_id, sample_upload(Some((1.5, 0.0))))
        .await
        .expect_err("an out-of-range focus must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    let after = count_media_rows_for_actor(&app.pool, actor_id).await;
    assert_eq!(
        before, after,
        "a rejected upload must not create a media row"
    );
}

/// Requirements 2.1, 2.2, 2.4: `show_media` returns `None` for an unknown
/// media id and for a media id owned by a different actor, and `Some` for
/// the owner.
#[tokio::test]
async fn show_media_is_owner_scoped() {
    let app = spawn_test_app().await;
    let owner = create_test_actor(&app, "erin").await;
    let other = create_test_actor(&app, "frank").await;
    let (service, _guard) = test_service(&app, "show_media_scope");

    let media = service
        .accept_upload(owner, sample_upload(None))
        .await
        .expect("upload must succeed");

    assert!(
        service
            .show_media(owner, media.id)
            .await
            .expect("show_media must succeed")
            .is_some()
    );
    assert!(
        service
            .show_media(other, media.id)
            .await
            .expect("show_media must succeed")
            .is_none()
    );
    let unknown_id = Id::from_i64(media.id.as_i64() + 999_999);
    assert!(
        service
            .show_media(owner, unknown_id)
            .await
            .expect("show_media must succeed")
            .is_none()
    );
}

/// Requirement 3.4: description/focus can be updated while the media is
/// still `Processing`, and the update is reflected.
#[tokio::test]
async fn update_metadata_updates_description_and_focus_while_processing() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "grace").await;
    let (service, _guard) = test_service(&app, "update_metadata_ok");

    let media = service
        .accept_upload(actor_id, sample_upload(None))
        .await
        .expect("upload must succeed");
    assert_eq!(media.state, MediaState::Processing);

    let updated = service
        .update_metadata(
            actor_id,
            media.id,
            MetadataPatch {
                description: Some("updated description".to_string()),
                focus: Some((-0.5, 0.5)),
            },
        )
        .await
        .expect("update_metadata must succeed")
        .expect("the just-created media must be updatable");

    assert_eq!(updated.state, MediaState::Processing);
    assert_eq!(updated.description.as_deref(), Some("updated description"));
    assert_eq!(updated.focus, Focus::new(-0.5, 0.5).unwrap());
}

/// Requirement 7.4: an out-of-range focal point supplied at update time is
/// rejected and the stored metadata is left unchanged.
#[tokio::test]
async fn update_metadata_rejects_an_out_of_range_focus_without_writing() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "heidi").await;
    let (service, _guard) = test_service(&app, "update_metadata_bad_focus");

    let media = service
        .accept_upload(actor_id, sample_upload(Some((0.1, 0.1))))
        .await
        .expect("upload must succeed");

    let err = service
        .update_metadata(
            actor_id,
            media.id,
            MetadataPatch {
                description: Some("should not apply".to_string()),
                focus: Some((-2.0, 0.0)),
            },
        )
        .await
        .expect_err("an out-of-range focus must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    let unchanged = service
        .show_media(actor_id, media.id)
        .await
        .expect("show_media must succeed")
        .expect("media must still exist");
    assert_eq!(unchanged.description.as_deref(), Some("a test image"));
    assert_eq!(unchanged.focus, Focus::new(0.1, 0.1).unwrap());
}

/// Requirement 3.3: an update against media owned by a different actor is
/// rejected (returns `None`, matching `show_media`'s not-found-or-not-owned
/// contract), not applied.
#[tokio::test]
async fn update_metadata_returns_none_for_media_owned_by_another_actor() {
    let app = spawn_test_app().await;
    let owner = create_test_actor(&app, "ivan").await;
    let other = create_test_actor(&app, "judy").await;
    let (service, _guard) = test_service(&app, "update_metadata_not_owned");

    let media = service
        .accept_upload(owner, sample_upload(None))
        .await
        .expect("upload must succeed");

    let result = service
        .update_metadata(
            other,
            media.id,
            MetadataPatch {
                description: Some("attempted hijack".to_string()),
                focus: None,
            },
        )
        .await
        .expect("update_metadata must succeed even when not owned (returns None)");
    assert!(result.is_none());

    let unchanged = service
        .show_media(owner, media.id)
        .await
        .expect("show_media must succeed")
        .expect("media must still exist for its real owner");
    assert_eq!(unchanged.description.as_deref(), Some("a test image"));
}
