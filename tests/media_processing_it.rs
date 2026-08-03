//! Integration test proving task 6.2's own observable completion condition
//! (`.kiro/specs/media-pipeline/tasks.md`, "6.2 処理・キュー・ストレージの統
//! 合テストを実装する", `_Boundary: ProcessingWorker, ProcessingJobQueue,
//! MediaStore_`, `_Depends: 5.2_`): "ワーカーによる派生物生成と ready 化、一
//! 時失敗時の再試行とバックオフ、上限到達時の failed 化、再実行で重複派生物
//! を生まない冪等性、ロックされたままリース期間を超過したジョブが別ワーカー
//! に reclaim（試行回数加算込み）されることを検証する" (Requirements 4.2,
//! 4.3, 4.4, 4.5, 4.6, 6.1, 6.5).
//!
//! ## Relationship to `src/media/worker/tests.rs` and `src/media/job_queue/tests.rs`
//! Tasks 3.2 and 4.3 already added their own `#[cfg(test)] mod tests`
//! integration coverage directly inside `src/media/job_queue.rs`/
//! `src/media/worker.rs` (real Postgres via `spawn_test_app`, a real
//! `LocalFsStore` under a throwaway temp directory) — those tests already
//! prove almost this exact acceptance matrix. This file is task 6.2's own,
//! separate top-level file (design.md's File Structure Plan names it
//! `media_processing_it.rs`), mirroring the precedent task 6.1 already
//! established for `tests/media_upload_it.rs`/`media_poll_it.rs`/
//! `media_update_it.rs` relative to task 5.1's own `tests/media_endpoints_it.rs`:
//! phase 6 gets its own verification-level integration file per component,
//! addressed only through this crate's `pub` surface (`kawasemi::media::*`),
//! never `crate::`-internal items. It also closes one scenario this task's
//! own text calls for that the inline `worker/tests.rs` coverage does not
//! exercise: a transient failure that is retried and *then succeeds*, not
//! only retried-until-exhausted (see [`FlakyStore`] below).
//!
//! See this file's own doc comment on `CONCERNS` in the implementer status
//! report for why this substantial overlap exists and what a future
//! consolidation task might do about it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration as StdDuration;

use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba, RgbaImage};

use kawasemi::actor::model::{ActorState, ActorType, Handle, LocalActor};
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::repository::insert_actor;
use kawasemi::config::MediaConfig;
use kawasemi::domain::Id;
use kawasemi::error::AppError;
use kawasemi::media::{
    self, Focus, JobOutcome, LocalFsStore, Media, MediaState, MediaStore, MediaType, ObjectKey,
    ProcessingWorker, PureRustImageProcessor, WorkerOutcome,
};
use kawasemi::runtime::{Clock, RuntimeContext};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ==========================================================================
// Fixtures (each `tests/*.rs` file is its own compiled binary, so these
// deliberately duplicate `src/media/job_queue/tests.rs`'s/`src/media/
// worker/tests.rs`'s own identical conventions rather than sharing code).
// ==========================================================================

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

fn unique_temp_root(label: &str) -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "kawasemi_media_processing_it_{label}_{nanos}_{seq}"
    ))
}

struct TempDirGuard(std::path::PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_store(label: &str) -> (LocalFsStore, TempDirGuard) {
    let root = unique_temp_root(label);
    let guard = TempDirGuard(root.clone());
    (LocalFsStore::new(root), guard)
}

/// A small but genuine deterministic-content PNG (a gradient, not a solid
/// color), duplicating `src/media/worker/tests.rs::sample_png`'s own
/// fixture shape.
fn sample_png(width: u32, height: u32) -> Vec<u8> {
    let rgba: RgbaImage = ImageBuffer::from_fn(width, height, |x, y| {
        let r = (x * 255 / width.max(1)) as u8;
        let g = (y * 255 / height.max(1)) as u8;
        Rgba([r, g, 128, 255])
    });
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("encoding the in-memory fixture PNG must succeed");
    bytes
}

fn default_test_config() -> MediaConfig {
    MediaConfig {
        storage_root: std::path::PathBuf::from("media_storage"),
        max_upload_size_bytes: 10 * 1024 * 1024,
        thumbnail_target_width: 64,
        thumbnail_target_height: 64,
        supported_formats: vec!["image/png".to_string()],
        worker_concurrency: 2,
        max_retry_attempts: 5,
        lease_duration: StdDuration::from_secs(5 * 60),
    }
}

async fn create_test_media(
    app: &TestApp,
    store: &LocalFsStore,
    actor_id: Id,
    now: time::OffsetDateTime,
    original_bytes: &[u8],
) -> Id {
    let media_id = app.runtime.ids.next_id();
    let media = Media {
        id: media_id,
        actor_id,
        media_type: MediaType::Image,
        state: MediaState::Processing,
        description: None,
        focus: Focus::default(),
        meta: None,
        blurhash: None,
        created_at: now,
    };
    let object_key = ObjectKey::original(media_id);
    media::insert_media(&app.pool, &media, object_key.as_str(), "image/png")
        .await
        .expect("insert_media must succeed");
    store
        .put(&object_key, original_bytes, "image/png")
        .await
        .expect("storing the original object must succeed");

    media_id
}

/// A scripted `Clock` that advances to the next preset instant on every
/// call, staying on the last one once exhausted (needed to drive the
/// multi-call backoff/lease scenarios below; `spawn_test_app`'s default
/// `FixedClock` always returns the same instant).
struct SteppingClock {
    times: Vec<time::OffsetDateTime>,
    index: AtomicUsize,
}

impl SteppingClock {
    fn new(times: Vec<time::OffsetDateTime>) -> Self {
        assert!(
            !times.is_empty(),
            "SteppingClock needs at least one instant"
        );
        Self {
            times,
            index: AtomicUsize::new(0),
        }
    }
}

impl Clock for SteppingClock {
    fn now(&self) -> time::OffsetDateTime {
        let i = self.index.fetch_add(1, Ordering::SeqCst);
        self.times[i.min(self.times.len() - 1)]
    }
}

fn runtime_with_clock(app: &TestApp, clock: impl Clock + 'static) -> RuntimeContext {
    RuntimeContext {
        clock: Arc::new(clock),
        ids: app.runtime.ids.clone(),
        rng: app.runtime.rng.clone(),
        keys: app.runtime.keys.clone(),
    }
}

/// A `MediaStore` wrapper that fails [`MediaStore::get`] a configurable
/// number of times before delegating to the wrapped store, simulating a
/// transient storage hiccup that clears up on a later retry — unlike
/// `src/media/worker/tests.rs`'s own transient-failure test (which never
/// stores the original at all, so it can only ever exhaust the retry
/// budget), this lets a test observe the "retried, then succeeds" half of
/// Requirement 4.4/4.6 the inline coverage does not exercise. `put`/
/// `delete`/`public_url` are passed straight through unmodified.
#[derive(Clone)]
struct FlakyStore {
    inner: LocalFsStore,
    get_failures_remaining: Arc<AtomicUsize>,
}

impl FlakyStore {
    fn new(inner: LocalFsStore, fail_times: usize) -> Self {
        Self {
            inner,
            get_failures_remaining: Arc::new(AtomicUsize::new(fail_times)),
        }
    }
}

impl MediaStore for FlakyStore {
    async fn put(&self, key: &ObjectKey, bytes: &[u8], content_type: &str) -> Result<(), AppError> {
        self.inner.put(key, bytes, content_type).await
    }

    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, AppError> {
        let previous = self.get_failures_remaining.fetch_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |remaining| {
                if remaining > 0 {
                    Some(remaining - 1)
                } else {
                    None
                }
            },
        );
        if previous.is_ok() {
            return Err(AppError::server(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                std::io::Error::other("simulated transient storage failure"),
            ));
        }
        self.inner.get(key).await
    }

    async fn delete(&self, key: &ObjectKey) -> Result<(), AppError> {
        self.inner.delete(key).await
    }

    fn public_url(
        &self,
        key: &ObjectKey,
        origin: &kawasemi::api::pagination::ForwardedOrigin,
    ) -> String {
        self.inner.public_url(key, origin)
    }
}

// ---- (1) happy path: queued job -> ready media + derivatives stored ----

/// Requirements 4.2, 4.3, 6.1: a queued job is claimed by `ProcessingWorker`
/// and drives the owning `Media` to `Ready` with real, decodable derivatives
/// actually persisted in the `MediaStore` (not stubbed).
#[tokio::test]
async fn worker_processes_a_queued_job_to_ready_media_with_derivatives_stored() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "alice").await;
    let (store, _guard) = test_store("happy_path");
    let now = app.runtime.clock.now();
    let original = sample_png(80, 40);
    let media_id = create_test_media(&app, &store, actor_id, now, &original).await;

    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");

    let worker = ProcessingWorker::new(
        app.pool.clone(),
        app.runtime.clone(),
        default_test_config(),
        store.clone(),
        PureRustImageProcessor::new(),
    );

    let outcome = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("a due job must be claimed and resolved");
    assert_eq!(outcome, WorkerOutcome::Completed);

    let media = media::find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(media.state, MediaState::Ready);
    let meta = media.meta.expect("ready media must have meta populated");
    assert_eq!(meta.original.width, 80);
    assert_eq!(meta.original.height, 40);
    let small = meta
        .small
        .expect("ready media must have small dims populated");
    assert!(small.width <= 64 && small.height <= 64);
    assert!(media.blurhash.as_deref().is_some_and(|h| !h.is_empty()));

    let thumb_bytes = store
        .get(&ObjectKey::small(media_id))
        .await
        .expect("the thumbnail derivative must actually be stored");
    let decoded = image::load_from_memory(&thumb_bytes)
        .expect("stored thumbnail bytes must themselves decode as a valid image");
    use image::GenericImageView;
    assert_eq!(decoded.dimensions(), (small.width, small.height));

    app.cleanup().await;
}

// ---- (2a) transient failure retried with backoff, then succeeds ----

/// Requirements 4.4, 4.6: a job whose original object fetch fails exactly
/// once (a `FlakyStore`-simulated transient hiccup) is retried with backoff
/// (`attempts` incremented, `run_at` pushed forward, `last_error`
/// populated) and, once its backoff window elapses, the *same* job
/// succeeds on the next attempt — proving the retry path can actually
/// recover, not only exhaust.
#[tokio::test]
async fn transient_failure_is_retried_with_backoff_and_then_succeeds() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "bob").await;
    let (real_store, _guard) = test_store("retry_then_succeed");
    let store = FlakyStore::new(real_store.clone(), 1);
    let t0 = app.runtime.clock.now();
    let original = sample_png(30, 30);
    let media_id = create_test_media(&app, &real_store, actor_id, t0, &original).await;

    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, t0)
        .await
        .expect("enqueue must succeed");

    // DEFAULT_MEDIA_BASE_DELAY is 15s; the second attempt must run past it.
    let t1 = t0 + time::Duration::seconds(20);
    let clock = SteppingClock::new(vec![t0, t1]);
    let runtime = runtime_with_clock(&app, clock);

    let worker = ProcessingWorker::new(
        app.pool.clone(),
        runtime,
        default_test_config(),
        store,
        PureRustImageProcessor::new(),
    );

    let outcome1 = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the enqueued job must be claimed");
    assert_eq!(outcome1, WorkerOutcome::Retried);

    let (state1, attempts1, run_at1, last_error1): (
        String,
        i32,
        time::OffsetDateTime,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT state, attempts, run_at, last_error FROM media_processing_jobs WHERE media_id = $1",
    )
    .bind(media_id.as_i64())
    .fetch_one(&app.pool)
    .await
    .expect("job row must still exist after a retry");
    assert_eq!(state1, "queued");
    assert_eq!(attempts1, 1);
    assert!(run_at1 > t0, "run_at must be pushed later by the backoff");
    assert!(
        last_error1.is_some_and(|msg| !msg.is_empty()),
        "a retried job must have a diagnostic persisted (Requirement 4.5)"
    );

    let media_after_retry = media::find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(
        media_after_retry.state,
        MediaState::Processing,
        "a merely-retried job must not touch the media state yet"
    );

    let outcome2 = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the retried job must be due again once t1 arrives");
    assert_eq!(
        outcome2,
        WorkerOutcome::Completed,
        "the second attempt (no more injected failures) must succeed, not retry/fail again"
    );

    let media_after_success = media::find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(media_after_success.state, MediaState::Ready);

    app.cleanup().await;
}

// ---- (2c) decode failure: immediate terminal failure, not retried ----

/// Requirement 6.5: a corrupt/undecodable original fails
/// `MediaProcessor::process_image` immediately — design.md's flowchart's
/// distinct `-->|decode fail| Failed` edge, bypassing the retry budget
/// entirely (unlike the transient-storage-failure path above, a single
/// `run_once` call already reaches the terminal `failed` state despite a
/// generous retry budget).
#[tokio::test]
async fn decode_failure_fails_the_job_and_media_immediately_without_retrying() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "ivan").await;
    let (store, _guard) = test_store("decode_failure");
    let now = app.runtime.clock.now();
    let garbage = vec![0xDEu8, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let media_id = create_test_media(&app, &store, actor_id, now, &garbage).await;

    let mut config = default_test_config();
    config.max_retry_attempts = 10;

    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let worker = ProcessingWorker::new(
        app.pool.clone(),
        app.runtime.clone(),
        config,
        store,
        PureRustImageProcessor::new(),
    );

    let outcome = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the enqueued job must be claimed");
    assert_eq!(
        outcome,
        WorkerOutcome::Failed,
        "a decode failure must fail immediately, not retry, despite a large retry budget"
    );

    let (state, attempts, last_error): (String, i32, Option<String>) = sqlx::query_as(
        "SELECT state, attempts, last_error FROM media_processing_jobs WHERE media_id = $1",
    )
    .bind(media_id.as_i64())
    .fetch_one(&app.pool)
    .await
    .expect("job row must still exist");
    assert_eq!(state, "failed");
    assert_eq!(
        attempts, 1,
        "a decode failure must terminate on the first attempt, not consume the retry budget"
    );
    assert!(last_error.is_some_and(|msg| !msg.is_empty()));

    let media = media::find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(media.state, MediaState::Failed);

    app.cleanup().await;
}

// ---- (2b) transient failure exhausting attempts -> failed ----

/// Requirement 4.5: a job whose original object is permanently missing
/// (the "always transiently fails" case) exhausts `max_retry_attempts` and
/// transitions both the job row and its owning media to a terminal failed
/// state, with a diagnostic persisted to `last_error`.
#[tokio::test]
async fn transient_failure_exhausting_retries_marks_job_and_media_failed() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "carol").await;
    let (store, _guard) = test_store("exhausts");
    let t0 = app.runtime.clock.now();

    // Insert the media row but never `put` its original object -- every
    // `store.get` therefore fails reliably, reproducing the same transient
    // failure on every attempt.
    let media_id = app.runtime.ids.next_id();
    let media = Media {
        id: media_id,
        actor_id,
        media_type: MediaType::Image,
        state: MediaState::Processing,
        description: None,
        focus: Focus::default(),
        meta: None,
        blurhash: None,
        created_at: t0,
    };
    media::insert_media(
        &app.pool,
        &media,
        ObjectKey::original(media_id).as_str(),
        "image/png",
    )
    .await
    .expect("insert_media must succeed");
    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, t0)
        .await
        .expect("enqueue must succeed");

    let t1 = t0 + time::Duration::seconds(20);
    let clock = SteppingClock::new(vec![t0, t1]);
    let runtime = runtime_with_clock(&app, clock);

    let mut config = default_test_config();
    config.max_retry_attempts = 2;
    let worker = ProcessingWorker::new(
        app.pool.clone(),
        runtime,
        config,
        store,
        PureRustImageProcessor::new(),
    );

    let outcome1 = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the enqueued job must be claimed");
    assert_eq!(outcome1, WorkerOutcome::Retried);

    let outcome2 = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the retried job must be due again");
    assert_eq!(outcome2, WorkerOutcome::Failed);

    let (state, attempts, last_error): (String, i32, Option<String>) = sqlx::query_as(
        "SELECT state, attempts, last_error FROM media_processing_jobs WHERE media_id = $1",
    )
    .bind(media_id.as_i64())
    .fetch_one(&app.pool)
    .await
    .expect("job row must still exist");
    assert_eq!(state, "failed");
    assert_eq!(attempts, 2);
    assert!(last_error.is_some_and(|msg| !msg.is_empty()));

    let media_after_failure = media::find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(media_after_failure.state, MediaState::Failed);

    app.cleanup().await;
}

// ---- (3) idempotent reprocessing: no duplicate derivatives ----

/// Requirement 4.6: re-running processing for a media that is already
/// `Ready` (a redundant re-enqueue, as a reclaim race could produce) must
/// not create a duplicate/different derivative file and must not touch
/// `media.updated_at` a second time.
#[tokio::test]
async fn rerunning_a_job_for_already_ready_media_does_not_duplicate_derivatives() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "dave").await;
    let (store, _guard) = test_store("idempotent");
    let now = app.runtime.clock.now();
    let original = sample_png(30, 30);
    let media_id = create_test_media(&app, &store, actor_id, now, &original).await;

    let worker = ProcessingWorker::new(
        app.pool.clone(),
        app.runtime.clone(),
        default_test_config(),
        store.clone(),
        PureRustImageProcessor::new(),
    );

    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let first = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the first job must be claimed");
    assert_eq!(first, WorkerOutcome::Completed);

    let (updated_at_after_first,): (time::OffsetDateTime,) =
        sqlx::query_as("SELECT updated_at FROM media WHERE id = $1")
            .bind(media_id.as_i64())
            .fetch_one(&app.pool)
            .await
            .expect("query must succeed");
    let thumb_after_first = store
        .get(&ObjectKey::small(media_id))
        .await
        .expect("thumbnail must exist after the first run");

    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let second = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the second job must be claimed");
    assert_eq!(second, WorkerOutcome::Completed);

    let (updated_at_after_second,): (time::OffsetDateTime,) =
        sqlx::query_as("SELECT updated_at FROM media WHERE id = $1")
            .bind(media_id.as_i64())
            .fetch_one(&app.pool)
            .await
            .expect("query must succeed");
    assert_eq!(
        updated_at_after_first, updated_at_after_second,
        "an idempotent re-run must not call set_ready again"
    );
    let thumb_after_second = store
        .get(&ObjectKey::small(media_id))
        .await
        .expect("thumbnail must still exist");
    assert_eq!(
        thumb_after_first, thumb_after_second,
        "an idempotent re-run must not overwrite the derivative with different bytes"
    );

    let media_final = media::find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must exist");
    assert_eq!(media_final.state, MediaState::Ready);

    app.cleanup().await;
}

// ---- (4) reclaim: lease-expired job picked up by another worker ----

/// Requirements 4.2, 4.4: a job claimed (simulating a worker that then
/// crashed, via a direct `claim_due` call standing in for that crashed
/// worker's own claim) and never resolved gets reclaimed once its lease
/// expires, by a *second* `ProcessingWorker`'s own `run_once`, with
/// `attempts` reflecting the reclaim, and is fully processed to completion.
#[tokio::test]
async fn worker_reclaims_a_lease_expired_job_and_completes_it() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "erin").await;
    let (store, _guard) = test_store("reclaim");
    let t0 = app.runtime.clock.now();
    let original = sample_png(50, 50);
    let media_id = create_test_media(&app, &store, actor_id, t0, &original).await;

    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, t0)
        .await
        .expect("enqueue must succeed");

    let lease = time::Duration::seconds(2);
    let stale_claim = media::claim_due(&app.pool, t0, lease)
        .await
        .expect("claim_due must succeed")
        .expect("the freshly-enqueued job must be claimable");
    assert_eq!(
        stale_claim.attempts, 0,
        "the initial (non-reclaim) claim must not touch attempts"
    );

    let t1 = t0 + lease + time::Duration::seconds(1);
    let clock = SteppingClock::new(vec![t1]);
    let runtime = runtime_with_clock(&app, clock);

    let mut config = default_test_config();
    config.lease_duration = StdDuration::from_secs(2);
    let worker = ProcessingWorker::new(
        app.pool.clone(),
        runtime,
        config,
        store,
        PureRustImageProcessor::new(),
    );

    let outcome = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the lease-expired job must be reclaimed and resolved");
    assert_eq!(outcome, WorkerOutcome::Completed);

    let media = media::find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(media.state, MediaState::Ready);

    let remaining: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM media_processing_jobs WHERE media_id = $1")
            .bind(media_id.as_i64())
            .fetch_optional(&app.pool)
            .await
            .expect("query must succeed");
    assert!(remaining.is_none(), "the completed job must be retired");

    app.cleanup().await;
}

/// Requirements 4.2, 4.4 (`ProcessingJobQueue` boundary directly): a
/// `processing` job whose `locked_at` is older than `now - lease_duration`
/// is reclaimed by `claim_due` with `attempts` incremented, while a job
/// still within its lease is left untouched.
#[tokio::test]
async fn claim_due_reclaims_only_once_the_lease_has_actually_expired() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "frank").await;
    let now = app.runtime.clock.now();
    let (store, _guard) = test_store("lease_boundary");
    let media_id = create_test_media(&app, &store, actor_id, now, &sample_png(10, 10)).await;
    let lease = time::Duration::minutes(5);

    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let job = media::claim_due(&app.pool, now, lease)
        .await
        .expect("claim_due must succeed")
        .expect("job must be claimable");
    assert_eq!(job.attempts, 0);

    let still_within_lease = now + time::Duration::minutes(2);
    let not_reclaimed = media::claim_due(&app.pool, still_within_lease, lease)
        .await
        .expect("claim_due must succeed");
    assert!(
        not_reclaimed.is_none(),
        "a processing job still within its lease must not be reclaimed"
    );

    let past_lease = now + lease + time::Duration::seconds(1);
    let reclaimed = media::claim_due(&app.pool, past_lease, lease)
        .await
        .expect("claim_due must succeed")
        .expect("a lease-expired processing job must be reclaimed");
    assert_eq!(reclaimed.id, job.id);
    assert_eq!(
        reclaimed.attempts, 1,
        "reclaim must increment attempts versus the pre-reclaim value"
    );

    app.cleanup().await;
}

/// Requirement 4.2 (`ProcessingJobQueue` boundary directly): two concurrent
/// `claim_due` calls against the same single queued job never both return
/// it — `FOR UPDATE SKIP LOCKED` guarantees exactly one winner.
#[tokio::test]
async fn two_concurrent_claim_due_calls_never_both_claim_the_same_job() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "grace").await;
    let now = app.runtime.clock.now();
    let (store, _guard) = test_store("concurrent_claim");
    let media_id = create_test_media(&app, &store, actor_id, now, &sample_png(10, 10)).await;

    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");

    let pool_a = app.pool.clone();
    let pool_b = app.pool.clone();
    let lease = time::Duration::minutes(5);
    let (result_a, result_b) = tokio::join!(
        media::claim_due(&pool_a, now, lease),
        media::claim_due(&pool_b, now, lease)
    );
    let winners = [
        result_a.expect("claim_due (a) must succeed"),
        result_b.expect("claim_due (b) must succeed"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    assert_eq!(
        winners.len(),
        1,
        "exactly one of the two concurrent claims must win the single queued job"
    );

    app.cleanup().await;
}

/// Requirement 4.5, `JobOutcome`: `fail_or_retry` at/above `max_attempts`
/// reports `Failed` directly from the `ProcessingJobQueue` boundary
/// (complementing the worker-level assertion above with a direct check on
/// the queue's own return value).
#[tokio::test]
async fn fail_or_retry_reports_failed_once_max_attempts_is_reached() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "heidi").await;
    let now = app.runtime.clock.now();
    let (store, _guard) = test_store("fail_or_retry_direct");
    let media_id = create_test_media(&app, &store, actor_id, now, &sample_png(10, 10)).await;

    media::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let job = media::claim_due(&app.pool, now, time::Duration::minutes(5))
        .await
        .expect("claim_due must succeed")
        .expect("job must be claimable");

    let outcome = media::fail_or_retry(&app.pool, &job, 1, now, "simulated terminal failure")
        .await
        .expect("fail_or_retry must succeed");
    assert_eq!(outcome, JobOutcome::Failed);

    app.cleanup().await;
}
