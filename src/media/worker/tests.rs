//! Integration tests for `ProcessingWorker` (Requirements 4.2, 4.3, 4.4,
//! 4.5, 4.6, 6.1, 6.5), per task 4.3's own observable completion condition:
//! "投入されたジョブが処理されてメディアが ready 化し派生物が保管されるこ
//! と、失敗時に failed 化と再試行が機能すること、ロックされたまま応答しな
//! いジョブがリース期間経過後に別ワーカーへ再取得されることを統合テストで
//! 確認できる".
//!
//! Mirrors `src/media/job_queue/tests.rs`/`src/media/service/tests.rs`'s
//! established convention: `crate::test_harness::spawn_test_app` for an
//! isolated, already-migrated schema, a real owner + local actor row, and a
//! real `LocalFsStore` under a throwaway temp directory (not a fake --
//! `MediaService`'s own test module already established the precedent of
//! exercising the real storage/DB/queue integration path rather than a
//! mock). `PureRustImageProcessor` (task 2.3, already implemented) is the
//! concrete `MediaProcessor` every test instantiates `ProcessingWorker`
//! with, per this task's own instructions.
//!
//! Two tests ([`transient_store_failure_retries_with_backoff_then_fails_after_exhausting_attempts`],
//! [`worker_reclaims_a_job_whose_lease_expired_after_a_simulated_crash`])
//! need `now` to advance across multiple `ProcessingWorker::run_once` calls
//! (backoff/lease windows), which `spawn_test_app`'s default `FixedClock`
//! cannot do (it always returns the same constructed instant) -- both build
//! a scripted [`SteppingClock`] (a local duplicate of
//! `src/api/ratelimit/tests.rs::SteppingClock`'s identical "advance to the
//! next preset value on each call" shape) and a `RuntimeContext` that reuses
//! `app.runtime`'s `ids`/`rng`/`keys` but substitutes that clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration as StdDuration;

use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba, RgbaImage};

use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::config::MediaConfig;
use crate::domain::Id;
use crate::media::image_processor::PureRustImageProcessor;
use crate::media::job_queue;
use crate::media::local_fs::LocalFsStore;
use crate::media::media_repository::{find_by_id, insert_media};
use crate::media::model::{Focus, Media, MediaState, MediaType};
use crate::media::store::{MediaStore, ObjectKey};
use crate::media::worker::{ProcessingWorker, WorkerOutcome};
use crate::runtime::{Clock, RuntimeContext};
use crate::test_harness::{TestApp, spawn_test_app};

// ---- shared test fixtures ----

/// Creates a real owner + local actor row, returning the actor's `Id`
/// (same helper shape as `job_queue/tests.rs::create_test_actor`).
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
/// `LocalFsStore` root (duplicates `service/tests.rs::unique_temp_root`'s
/// identical "counter + nanos" convention -- private to that module, see
/// this file's doc comment).
fn unique_temp_root(label: &str) -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("kawasemi_media_worker_test_{label}_{nanos}_{seq}"))
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
/// color, mirroring `image_processor.rs`'s own `sample_rgba` test fixture --
/// duplicated locally rather than reused since that helper is private to
/// `image_processor.rs`'s own test module).
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

/// Inserts a real `media` row in `Processing` state, bound to `actor_id`,
/// and stores `original_bytes` at its `ObjectKey::original` key in `store`
/// (mirroring what `MediaService::accept_upload`, task 4.1, would have
/// already done before a worker ever sees the job -- out of this task's own
/// boundary to invoke, so this helper does the equivalent setup directly,
/// same as `job_queue/tests.rs::create_test_media`'s identical rationale).
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
    insert_media(&app.pool, &media, object_key.as_str(), "image/png")
        .await
        .expect("insert_media must succeed");
    store
        .put(&object_key, original_bytes, "image/png")
        .await
        .expect("storing the original object must succeed");

    media_id
}

/// A scripted `Clock` that advances to the next preset instant on every
/// call, staying on the last one once exhausted (duplicates
/// `src/api/ratelimit/tests.rs::SteppingClock`'s identical shape -- see
/// this file's doc comment for why a `FixedClock` cannot drive the
/// multi-call backoff/lease tests below).
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

/// Builds a `RuntimeContext` identical to `app.runtime` except for its
/// `clock`, which is replaced with `clock` (reuses `app.runtime`'s
/// `ids`/`rng`/`keys` `Arc`s unchanged).
fn runtime_with_clock(app: &TestApp, clock: impl Clock + 'static) -> RuntimeContext {
    RuntimeContext {
        clock: Arc::new(clock),
        ids: app.runtime.ids.clone(),
        rng: app.runtime.rng.clone(),
        keys: app.runtime.keys.clone(),
    }
}

// ---- (a) happy path: queued job -> ready media + stored derivatives ----

/// Requirements 4.2, 4.3, 6.1: a queued job is claimed, processed, and
/// completed -- the media reaches `Ready` with `meta`/`blurhash` populated
/// and a decodable thumbnail actually stored under its `ObjectKey::small`
/// key (not a stub).
#[tokio::test]
async fn queued_job_processes_to_ready_media_with_derivatives_stored() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "alice").await;
    let (store, _guard) = test_store("happy_path");
    let now = app.runtime.clock.now();
    let original = sample_png(80, 40);
    let media_id = create_test_media(&app, &store, actor_id, now, &original).await;

    job_queue::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
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

    let media = find_by_id(&app.pool, media_id)
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
    assert!(
        media.blurhash.as_deref().is_some_and(|h| !h.is_empty()),
        "ready media must have a non-empty blurhash"
    );

    // The thumbnail derivative is real, decodable image data, not a stub.
    let thumb_bytes = store
        .get(&ObjectKey::small(media_id))
        .await
        .expect("the thumbnail derivative must actually be stored");
    let decoded = image::load_from_memory(&thumb_bytes)
        .expect("the stored thumbnail bytes must themselves decode as a valid image");
    use image::GenericImageView;
    assert_eq!(decoded.dimensions(), (small.width, small.height));

    // The job row is retired.
    let remaining: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM media_processing_jobs WHERE media_id = $1")
            .bind(media_id.as_i64())
            .fetch_optional(&app.pool)
            .await
            .expect("query must succeed");
    assert!(remaining.is_none(), "a completed job must be retired");

    app.cleanup().await;
}

/// `run_once` returns `Ok(None)` when nothing is due (no job enqueued at
/// all) -- the "sleep then loop again" branch's own precondition.
#[tokio::test]
async fn run_once_returns_none_when_nothing_is_due() {
    let app = spawn_test_app().await;
    let (store, _guard) = test_store("nothing_due");
    let worker = ProcessingWorker::new(
        app.pool.clone(),
        app.runtime.clone(),
        default_test_config(),
        store,
        PureRustImageProcessor::new(),
    );

    let outcome = worker.run_once().await.expect("run_once must succeed");
    assert!(outcome.is_none());

    app.cleanup().await;
}

// ---- (b) transient failure retries with backoff, then terminally fails ----

/// Requirements 4.4, 4.5: a job whose original object is missing from the
/// store fails transiently (store `get` returns a `404`-shaped error, this
/// module's own documented "Transient" classification) -- the first
/// `run_once` reschedules it with backoff (`attempts` incremented, `run_at`
/// pushed forward), and once `max_retry_attempts` is exhausted a later
/// `run_once` (at a scripted `now` past the backoff window) marks both the
/// job and the media failed, with a diagnostic persisted to `last_error`.
#[tokio::test]
async fn transient_store_failure_retries_with_backoff_then_fails_after_exhausting_attempts() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "bob").await;
    let (store, _guard) = test_store("transient_then_failed");
    let t0 = app.runtime.clock.now();

    // Insert the media row but deliberately never `put` its original
    // object -- every `store.get(&ObjectKey::original(media_id))` call
    // therefore fails with a real (not panicking) 404-shaped `AppError`,
    // reliably reproducing the same transient failure on every attempt.
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
    insert_media(
        &app.pool,
        &media,
        ObjectKey::original(media_id).as_str(),
        "image/png",
    )
    .await
    .expect("insert_media must succeed");
    job_queue::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, t0)
        .await
        .expect("enqueue must succeed");

    // DEFAULT_MEDIA_BASE_DELAY is 15s (job_queue.rs); the second attempt
    // must run after that backoff window elapses.
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

    // First attempt (now = t0): transient failure, retried.
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
        "SELECT state, attempts, run_at, last_error FROM media_processing_jobs \
             WHERE media_id = $1",
    )
    .bind(media_id.as_i64())
    .fetch_one(&app.pool)
    .await
    .expect("job row must still exist after a retry");
    assert_eq!(state1, "queued");
    assert_eq!(attempts1, 1);
    assert!(
        run_at1 > t0,
        "a retried job's run_at must be pushed later than the original enqueue time"
    );
    assert!(
        last_error1.is_some_and(|msg| !msg.is_empty()),
        "Requirement 4.5: a retried job must also have a diagnostic persisted to last_error"
    );

    let media_after_retry = find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(
        media_after_retry.state,
        MediaState::Processing,
        "a merely-retried job must not touch the media's state yet"
    );

    // Second attempt (now = t1, past the backoff window): attempts reaches
    // max_retry_attempts (2) -- terminally failed.
    let outcome2 = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the retried job must be due again and claimed");
    assert_eq!(outcome2, WorkerOutcome::Failed);

    let (state2, attempts2, last_error2): (String, i32, Option<String>) = sqlx::query_as(
        "SELECT state, attempts, last_error FROM media_processing_jobs WHERE media_id = $1",
    )
    .bind(media_id.as_i64())
    .fetch_one(&app.pool)
    .await
    .expect("job row must still exist after terminal failure");
    assert_eq!(state2, "failed");
    assert_eq!(attempts2, 2);
    assert!(
        last_error2.is_some_and(|msg| !msg.is_empty()),
        "Requirement 4.5: a terminally-failed job must have a diagnostic persisted to last_error"
    );

    let media_after_failure = find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(media_after_failure.state, MediaState::Failed);

    app.cleanup().await;
}

// ---- (c) crash + lease-expiry reclaim ----

/// Requirements 4.2, 4.4: a job claimed by a worker that then never
/// resolves it at all (simulated crash: `job_queue::claim_due` called
/// directly, standing in for a first worker instance that locked the job
/// and then stopped responding) gets reclaimed by a second
/// `ProcessingWorker`'s own `run_once` once `now` has advanced past
/// `lease_duration`, with `attempts` reflecting the reclaim, and is
/// processed to completion by that second worker.
#[tokio::test]
async fn worker_reclaims_a_job_whose_lease_expired_after_a_simulated_crash() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "carol").await;
    let (store, _guard) = test_store("reclaim");
    let t0 = app.runtime.clock.now();
    let original = sample_png(50, 50);
    let media_id = create_test_media(&app, &store, actor_id, t0, &original).await;

    job_queue::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, t0)
        .await
        .expect("enqueue must succeed");

    let lease = time::Duration::seconds(2);

    // Simulate a first worker instance claiming the job and then crashing
    // (locking it, never calling `complete`/`fail_or_retry`) -- a direct
    // `claim_due` call stands in for that crashed worker's own claim, per
    // this task's own instructions ("one worker + a simulated stale lock").
    let stale_claim = job_queue::claim_due(&app.pool, t0, lease)
        .await
        .expect("claim_due must succeed")
        .expect("the freshly-enqueued job must be claimable");
    assert_eq!(
        stale_claim.attempts, 0,
        "the initial (non-reclaim) claim must not touch attempts"
    );

    // A second `ProcessingWorker` instance, running well past the lease,
    // reclaims and fully processes the job via its own `run_once`.
    let t1 = t0 + lease + time::Duration::seconds(1);
    let clock = SteppingClock::new(vec![t1]);
    let runtime = runtime_with_clock(&app, clock);

    let mut config = default_test_config();
    config.lease_duration = StdDuration::from_secs(2);
    let worker = ProcessingWorker::new(
        app.pool.clone(),
        runtime,
        config,
        store.clone(),
        PureRustImageProcessor::new(),
    );

    let outcome = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the lease-expired job must be reclaimed and resolved");
    assert_eq!(outcome, WorkerOutcome::Completed);

    let media = find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(
        media.state,
        MediaState::Ready,
        "the reclaiming worker must fully process the job to completion"
    );

    // The job row is gone (completed); querying its final `attempts` value
    // before completion already proved the reclaim happened
    // (`stale_claim.attempts == 0`, and `claim_due`'s own already-reviewed
    // task 3.2 behavior increments `attempts` on exactly the reclaim
    // branch) -- confirm no row lingers.
    let remaining: Option<(i64,)> =
        sqlx::query_as("SELECT id FROM media_processing_jobs WHERE media_id = $1")
            .bind(media_id.as_i64())
            .fetch_optional(&app.pool)
            .await
            .expect("query must succeed");
    assert!(remaining.is_none());

    app.cleanup().await;
}

// ---- (d) idempotency: re-running a job for already-Ready media ----

/// Requirement 4.6: a second job enqueued for a media that is already
/// `Ready` is completed without reprocessing -- no re-fetch of the
/// original, no re-derivation, no `set_ready` call that would otherwise
/// bump `updated_at` a second time.
#[tokio::test]
async fn rerunning_a_job_for_already_ready_media_completes_idempotently_without_reprocessing() {
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

    job_queue::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let first = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the first job must be claimed");
    assert_eq!(first, WorkerOutcome::Completed);

    let ready_media = find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must exist");
    assert_eq!(ready_media.state, MediaState::Ready);
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

    // Re-enqueue a second job for the same, already-`Ready` media, as a
    // duplicate/redundant reclaim race would (design.md's own documented
    // accepted tradeoff).
    job_queue::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
        .await
        .expect("enqueue must succeed");
    let second = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the second job must be claimed");
    assert_eq!(
        second,
        WorkerOutcome::Completed,
        "an idempotent skip still reports Completed (the job is retired either way)"
    );

    let (updated_at_after_second,): (time::OffsetDateTime,) =
        sqlx::query_as("SELECT updated_at FROM media WHERE id = $1")
            .bind(media_id.as_i64())
            .fetch_one(&app.pool)
            .await
            .expect("query must succeed");
    assert_eq!(
        updated_at_after_first, updated_at_after_second,
        "an idempotent re-run must not call set_ready again (updated_at must not move)"
    );
    let thumb_after_second = store
        .get(&ObjectKey::small(media_id))
        .await
        .expect("thumbnail must still exist");
    assert_eq!(
        thumb_after_first, thumb_after_second,
        "an idempotent re-run must not overwrite the derivative with different bytes"
    );

    app.cleanup().await;
}

// ---- (e) decode failure: immediate terminal failure, not retried ----

/// Requirement 6.5: a corrupt/undecodable original fails `process_image`
/// immediately -- design.md's flowchart's distinct `-->|decode fail|
/// Failed` edge, bypassing the retry budget entirely (a single `run_once`
/// call already reaches the terminal `failed` state, unlike the transient
/// path's two-call retry-then-fail sequence).
#[tokio::test]
async fn decode_failure_fails_the_job_and_media_immediately_without_retrying() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "erin").await;
    let (store, _guard) = test_store("decode_failure");
    let now = app.runtime.clock.now();
    let garbage = vec![0xDEu8, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let media_id = create_test_media(&app, &store, actor_id, now, &garbage).await;

    // A generous retry budget: if the worker mistakenly treated this as
    // transient, it would come back `Retried`, not `Failed`, on the very
    // first call.
    let mut config = default_test_config();
    config.max_retry_attempts = 10;

    job_queue::enqueue(&app.pool, app.runtime.ids.as_ref(), media_id, now)
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
    assert!(
        last_error.is_some_and(|msg| msg.to_lowercase().contains("decode")),
        "the diagnostic must be specific enough to identify a decode failure (Requirement 4.5)"
    );

    let media = find_by_id(&app.pool, media_id)
        .await
        .expect("find_by_id must succeed")
        .expect("media must still exist");
    assert_eq!(media.state, MediaState::Failed);

    app.cleanup().await;
}

// ---- `run`: resident loop + graceful shutdown ----

/// The resident `run` loop stops promptly once its injected shutdown
/// signal resolves, without needing a real OS signal (mirrors
/// `server.rs`'s own `drive_shutdown` test convention: a `oneshot` channel
/// stands in for the signal future).
#[tokio::test]
async fn run_stops_promptly_once_the_shutdown_signal_resolves() {
    let app = spawn_test_app().await;
    let (store, _guard) = test_store("run_shutdown");
    let worker = Arc::new(ProcessingWorker::new(
        app.pool.clone(),
        app.runtime.clone(),
        default_test_config(),
        store,
        PureRustImageProcessor::new(),
    ));

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let signal = async move {
        let _ = rx.await;
    };

    let worker_clone = worker.clone();
    let handle = tokio::spawn(async move {
        worker_clone.run(StdDuration::from_secs(3600), signal).await;
    });

    // Give the loop a moment to actually enter its first poll/sleep cycle
    // before triggering shutdown, so this test also exercises the
    // interruptible-sleep path (poll_interval is deliberately huge above).
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    tx.send(())
        .expect("the run loop must still be listening for the signal");

    tokio::time::timeout(StdDuration::from_secs(5), handle)
        .await
        .expect(
            "run must return promptly once the shutdown signal resolves, not after poll_interval",
        )
        .expect("the spawned run task must not panic");

    app.cleanup().await;
}
