//! Integration-level contract test for `MediaAttachmentSerializer` (task
//! 6.3, `.kiro/specs/media-pipeline/tasks.md`, "6.3 (P) MediaAttachment 契約
//! テストを実装する", `_Boundary: MediaAttachmentSerializer_`, `_Depends: 5.2,
//! 4.2_`), Requirements 6.2, 6.3, 6.4, 7.1, 7.2, 7.3, 8.1, 8.2, 8.3, 8.4,
//! 10.1, 10.3. design.md's File Structure Plan names this exact file
//! (`tests/media_attachment_contract_it.rs`, "MediaAttachment ゴールデン
//! （決定的・null 規律）（契約）").
//!
//! ## Relationship to task 4.2's own `src/media/serializer/tests.rs` goldens
//! Task 4.2 already registered two goldens
//! (`tests/golden/media/media_attachment_{processing,ready}.json`) from its
//! own `#[cfg(test)] mod tests` unit tests, built from literal,
//! hand-constructed `Media` fixtures (a synthetic id, a hand-picked BlurHash
//! string, hand-picked dimensions) — see `serializer.rs`'s own doc comment,
//! "Golden fixtures", which explicitly defers *this* file's job to "the full
//! `spawn_test_app`-backed contract test... that exercises this same
//! serializer end to end through a real `MediaService`/`MediaStore`". Those
//! two existing goldens are consumed exclusively by that unit-level suite
//! and are *not* reused here: a real upload driven through
//! `MediaService::accept_upload` and a real `ProcessingWorker::run_once`
//! produces a genuinely-computed id (from `RuntimeContext.ids`), a
//! genuinely-decoded/-resized thumbnail, and a genuinely-computed BlurHash
//! from actual pixel data — none of which can be made to literally equal
//! task 4.2's hand-picked stand-in values. This file therefore registers its
//! own, separate pair of goldens
//! (`tests/golden/media/media_attachment_contract_it_{processing,ready}.json`)
//! that *are* the reproducible output of the real pipeline, per
//! `crate::contract::assert_golden`'s own documented convention that
//! golden-file paths are caller-owned, not something this harness invents a
//! shared layout for.
//!
//! ## What "real pipeline" means here
//! Every scenario below drives the *actual* production seam end to end:
//! `MediaService::accept_upload` (task 4.1: format/size validation, real
//! `MediaStore::put`, real `MediaRepository::insert_media`, real
//! `ProcessingJobQueue::enqueue`) to reach the processing-state contract,
//! then a real `ProcessingWorker::run_once` (task 4.3: real
//! `PureRustImageProcessor` decode/resize/BlurHash, real derivative
//! `MediaStore::put`, real `MediaRepository::set_ready`) to reach the
//! ready-state contract, then `MediaService::show_media` (task 4.1's
//! owner-scoped read) to fetch the persisted row back, then
//! `crate::media::serializer::to_json` (task 4.2, unchanged by this task) to
//! produce the actual JSON compared against each golden. No step is stubbed,
//! mocked, or bypassed.
//!
//! ## Determinism (steering `tech.md`'s "決定性の強制"; Requirement 8.4/10.3)
//! Every non-deterministic seam this pipeline touches is drawn from
//! `spawn_test_app`'s fixed `RuntimeContext::deterministic` boundary (id
//! generator via `MediaService`/`ProcessingWorker`'s injected
//! `RuntimeContext`, clock likewise) — this file never reads the OS clock or
//! generates its own ids. The one remaining input this file itself must
//! keep fixed for reproducibility is the uploaded image's own bytes:
//! [`sample_png`] is a pure function of `(width, height)` (a deterministic
//! gradient, not a random fill), so `PureRustImageProcessor::process_image`
//! (itself a pure, algorithmic decode/resize/BlurHash with no RNG or clock
//! read — task 2.3's own documented determinism guarantee) always derives
//! the same thumbnail bytes, dimensions, and BlurHash string from it.
//! Together this means every field of the resulting `MediaAttachmentJson` —
//! including `id`, which two independently-`spawn_test_app`-booted instances
//! reach via the exact same sequence of `RuntimeContext.ids.next_id()` calls
//! (owner, actor, then the upload itself) — is reproducible across runs and
//! across processes.
//!
//! ## The `KAWASEMI_UPDATE_GOLDEN` baseline was recorded, not embedded
//! Following task 4.2's own established convention (`tasks.md`'s
//! Implementation Notes, "4.2"; `tests/contract_harness_it.rs`'s doc
//! comment): this file's two committed goldens
//! (`tests/golden/media/media_attachment_contract_it_{processing,ready}.json`)
//! were produced by a one-off manual run of exactly
//! `processing_and_ready_media_attachment_json_match_the_registered_goldens`
//! with `KAWASEMI_UPDATE_GOLDEN=1` set, then re-run without it to confirm a
//! clean comparison. No test in this file ever sets that environment
//! variable itself — doing so from more than one `#[tokio::test]` sharing
//! this process would race, and doing so from the drift-detection test below
//! would silently "fix" the deliberately-wrong value into the golden instead
//! of proving drift is caught.
//!
//! ## Proving drift is actually caught (Requirement 8.3's "ドリフトを検出可
//! 能にする"; this task's own acceptance text, "契約のドリフトが検出される
//! ことを確認できる")
//! Mirrors `tests/contract_harness_it.rs`'s own established pattern exactly:
//! a deliberately mutated *in-memory* JSON value (never the on-disk golden
//! file itself) is compared against the untouched, already-correct golden,
//! and the resulting panic is caught via `std::panic::catch_unwind` and
//! asserted to name the exact JSON-pointer location that diverged.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration as StdDuration;

use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};

use kawasemi::actor::model::{ActorState, ActorType, Handle, LocalActor};
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::repository::insert_actor;
use kawasemi::api::pagination::ForwardedOrigin;
use kawasemi::config::MediaConfig;
use kawasemi::contract::assert_golden;
use kawasemi::domain::Id;
use kawasemi::media::{
    LocalFsStore, MediaService, MediaState, ProcessingWorker, PureRustImageProcessor, UploadInput,
    WorkerOutcome, to_json,
};
use kawasemi::test_harness::{TestApp, spawn_test_app};

const PROCESSING_GOLDEN: &str = "tests/golden/media/media_attachment_contract_it_processing.json";
const READY_GOLDEN: &str = "tests/golden/media/media_attachment_contract_it_ready.json";

// ==========================================================================
// Fixtures (each `tests/*.rs` file is its own compiled binary, so this
// deliberately duplicates `tests/media_processing_it.rs`'s/`tests/
// media_upload_it.rs`'s own identical conventions rather than sharing code).
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
        "kawasemi_media_attachment_contract_it_{label}_{nanos}_{seq}"
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
/// color or random fill), duplicating `tests/media_processing_it.rs::
/// sample_png`'s own fixture shape: a pure function of `(width, height)`, so
/// every run of this file's pipeline decodes/resizes/BlurHashes the exact
/// same bytes (Requirement 6.4).
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

fn contract_test_config() -> MediaConfig {
    MediaConfig {
        storage_root: std::path::PathBuf::from("media_storage"),
        max_upload_size_bytes: 10 * 1024 * 1024,
        thumbnail_target_width: 64,
        thumbnail_target_height: 64,
        supported_formats: vec!["image/png".to_string()],
        worker_concurrency: 1,
        max_retry_attempts: 5,
        lease_duration: StdDuration::from_secs(5 * 60),
    }
}

/// A fixed proxy-resolved origin (Requirement 5.4) every scenario below
/// resolves `url`/`preview_url` against.
fn origin() -> ForwardedOrigin {
    ForwardedOrigin::resolve(
        "http",
        "127.0.0.1:8080",
        Some("https"),
        Some("contract.example.social"),
    )
}

/// Drives the real pipeline end to end (see this file's own doc comment,
/// "What 'real pipeline' means here") and returns the actual
/// `MediaAttachmentJson` values observed immediately after upload
/// acceptance (`state == Processing`, Requirement 8.2's null discipline) and
/// again after the worker completes (`state == Ready`, Requirements 6.1-6.4,
/// 8.1).
async fn build_media_attachment_at_processing_and_ready(app: &TestApp) -> (Value, Value) {
    let actor_id = create_test_actor(app, "media_contract_actor").await;
    let (store, _guard) = test_store("contract");

    let service = MediaService::new(
        app.pool.clone(),
        app.runtime.clone(),
        contract_test_config(),
        store.clone(),
    );

    let original = sample_png(320, 240);
    let media = service
        .accept_upload(
            actor_id,
            UploadInput {
                bytes: original,
                content_type: "image/png".to_string(),
                description: Some("a deterministic media-pipeline contract fixture".to_string()),
                focus: None,
            },
        )
        .await
        .expect("accept_upload must succeed for a valid fixture image");
    assert_eq!(
        media.state,
        MediaState::Processing,
        "a freshly accepted upload must start out processing (url=null contract, Requirement 8.2)"
    );

    let origin = origin();
    let processing_json = to_json(&media, &store, &origin);

    let worker = ProcessingWorker::new(
        app.pool.clone(),
        app.runtime.clone(),
        contract_test_config(),
        store.clone(),
        PureRustImageProcessor::new(),
    );
    let outcome = worker
        .run_once()
        .await
        .expect("run_once must succeed")
        .expect("the upload's enqueued processing job must be claimed and resolved");
    assert_eq!(outcome, WorkerOutcome::Completed);

    let ready_media = service
        .show_media(actor_id, media.id)
        .await
        .expect("show_media must succeed")
        .expect("the media must still exist and be owned by the uploading actor");
    assert_eq!(
        ready_media.state,
        MediaState::Ready,
        "the worker must have driven the media to ready (Requirement 4.3)"
    );
    let ready_json = to_json(&ready_media, &store, &origin);

    (processing_json, ready_json)
}

/// Downcasts a `catch_unwind` panic payload to a displayable message,
/// mirroring `tests/contract_harness_it.rs`'s own identical helper.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        String::from("<panic payload was not a string>")
    }
}

// ---- (1) real processing-state and ready-state JSON match fixed goldens ----

/// Requirements 6.2, 6.3, 7.1, 7.2, 8.1, 8.2, 8.3: the real pipeline's
/// processing-state contract (`url=null`, no `meta.original`/`meta.small`
/// yet, `meta.focus` defaulting to center) and ready-state contract
/// (`url`/`preview_url` populated, `meta.original`/`meta.small`/`blurhash`
/// populated with genuinely-computed values) both reproduce their checked-in
/// goldens exactly.
#[tokio::test]
async fn processing_and_ready_media_attachment_json_match_the_registered_goldens() {
    let app = spawn_test_app().await;
    let (processing_json, ready_json) = build_media_attachment_at_processing_and_ready(&app).await;

    // Requirement 8.2: url/preview_url/meta discipline while still processing.
    assert_eq!(processing_json["url"], Value::Null);
    assert_eq!(processing_json["preview_url"], Value::Null);
    assert!(processing_json["meta"].get("original").is_none());
    assert!(processing_json["meta"].get("small").is_none());
    // Requirement 7.2: unspecified focus defaults to center.
    assert_eq!(
        processing_json["meta"]["focus"],
        json!({"x": 0.0, "y": 0.0})
    );

    // Requirement 8.1: full contract once ready.
    assert!(ready_json["url"].is_string());
    assert!(ready_json["preview_url"].is_string());
    assert!(ready_json["meta"]["original"].is_object());
    assert!(ready_json["meta"]["small"].is_object());
    assert!(
        ready_json["blurhash"]
            .as_str()
            .is_some_and(|h| !h.is_empty())
    );

    assert_golden(PROCESSING_GOLDEN, &processing_json);
    assert_golden(READY_GOLDEN, &ready_json);

    app.cleanup().await;
}

// ---- (2) same input reproduces stable, identical JSON across independent
// instances (this task's own acceptance text: "同一入力で安定したゴールデン
// 比較が成立する") ----

/// Requirements 6.4, 8.4, 10.3: two independently-`spawn_test_app`-booted
/// instances, each driving the identical real upload -> real processing ->
/// real serialization pipeline, produce byte-for-byte identical
/// `MediaAttachmentJson` for both states — no non-determinism (clock, id,
/// image decode/resize, BlurHash) leaks through the pipeline.
#[tokio::test]
async fn the_same_input_reproduces_byte_identical_json_across_independent_instances() {
    let app_a = spawn_test_app().await;
    let app_b = spawn_test_app().await;

    let (processing_a, ready_a) = build_media_attachment_at_processing_and_ready(&app_a).await;
    let (processing_b, ready_b) = build_media_attachment_at_processing_and_ready(&app_b).await;

    assert_eq!(
        processing_a, processing_b,
        "two independently spawn_test_app-booted instances processing the identical fixture \
         image must produce byte-for-byte identical processing-state MediaAttachment JSON"
    );
    assert_eq!(
        ready_a, ready_b,
        "two independently spawn_test_app-booted instances processing the identical fixture \
         image must produce byte-for-byte identical ready-state MediaAttachment JSON"
    );

    // The stable, reproduced output also still reproduces the checked-in
    // goldens established by the sibling test above -- the harness's core
    // guarantee (Requirement 8.3/8.4) holds across yet another independent
    // run, not merely once.
    assert_golden(PROCESSING_GOLDEN, &processing_a);
    assert_golden(READY_GOLDEN, &ready_a);

    app_a.cleanup().await;
    app_b.cleanup().await;
}

// ---- (3) a deliberate drift IS detected, proving the harness actually
// catches contract drift rather than merely passing once ----

/// Requirement 8.3's "出力ドリフトを検出可能にする"; this task's own
/// acceptance text, "契約のドリフトが検出されることを確認できる". Mirrors
/// `tests/contract_harness_it.rs`'s own established pattern: mutate the
/// *actual* in-memory value (never the on-disk golden, never
/// `KAWASEMI_UPDATE_GOLDEN`) and confirm `assert_golden` panics, pinpointing
/// the exact JSON location that diverged.
#[tokio::test]
async fn a_deliberately_mutated_field_is_detected_as_a_golden_mismatch() {
    let app = spawn_test_app().await;
    let (_processing_json, ready_json) = build_media_attachment_at_processing_and_ready(&app).await;

    let mut mutated = ready_json.clone();
    mutated["blurhash"] = json!("0000000000000000000000000000");

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        assert_golden(READY_GOLDEN, &mutated);
    }));
    let err =
        result.expect_err("a deliberately mutated blurhash must cause assert_golden to panic");
    let message = panic_message(err);
    assert!(
        message.contains("$.blurhash"),
        "mismatch report did not pinpoint the mutated field's location: {message}"
    );

    // A second, independent mutation (a different field, a different
    // location) is also caught -- not just the one field happened to be
    // checked first.
    let mut mutated_focus = ready_json.clone();
    mutated_focus["meta"]["focus"]["x"] = json!(0.9999);
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        assert_golden(READY_GOLDEN, &mutated_focus);
    }));
    let err = result.expect_err("a deliberately mutated focus.x must cause assert_golden to panic");
    let message = panic_message(err);
    assert!(
        message.contains("$.meta.focus.x"),
        "mismatch report did not pinpoint the mutated field's location: {message}"
    );

    app.cleanup().await;
}
