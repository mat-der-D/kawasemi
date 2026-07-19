//! Media domain module (media-pipeline spec, `src/media.rs` + `src/media/`,
//! mirroring the module-with-submodule convention established by
//! `src/federation.rs`/`src/federation/` and `src/oauth.rs`/`src/oauth/`).
//!
//! Scope so far:
//! - Task 2.1 (`Boundary: model`): the domain value types for a media
//!   attachment and its asynchronous processing job — [`Media`],
//!   [`MediaType`], [`MediaState`], [`Focus`] (a validated focal point
//!   constrained to `-1.0..=1.0` on both axes, defaulting to the center),
//!   [`Dimensions`], [`MediaMeta`], [`ProcessingJob`], and [`JobState`] —
//!   see [`model`].
//! - Task 2.2 (`Boundary: MediaStore, LocalFsStore`): the storage
//!   abstraction boundary — the [`MediaStore`] port (put/get/delete/
//!   public_url) and [`ObjectKey`]/[`ObjectVariant`] — see [`store`], plus
//!   its local-filesystem adapter [`LocalFsStore`] — see [`local_fs`].
//! - Task 2.3 (`Boundary: MediaProcessor, PureRustImageProcessor`): the
//!   image-processing abstraction boundary (the native-dependency gate) —
//!   the [`MediaProcessor`] port (`process_image`) and its
//!   [`ThumbnailSpec`]/[`ProcessedImage`] value types — see [`processor`],
//!   plus its pure-Rust adapter [`PureRustImageProcessor`] (decode/resize/
//!   encode via the `image` crate, BlurHash via the `blurhash` crate,
//!   neither pulling in any native/C dependency) — see [`image_processor`].
//! - Task 3.1 (`Boundary: MediaRepository`): the media attachment's own
//!   persistence — insertion (owning actor required), owner-scoped lookup
//!   (never returns another actor's media), description/focus update, and
//!   state+derived-metadata reflection (`set_ready`/`set_failed`) — see
//!   [`media_repository`].
//! - Task 3.2 (`Boundary: ProcessingJobQueue`): the asynchronous processing
//!   job queue's own persistence — job enqueue, exclusive `FOR UPDATE SKIP
//!   LOCKED` claim (covering both a fresh queued job and a lease-expired
//!   `processing` job reclaimed from a crashed worker), completion, and the
//!   temporary-failure retry/backoff/permanent-failure transition — see
//!   [`job_queue`].
//! - Task 4.1 (`Boundary: MediaService`): the media business-service layer —
//!   upload acceptance (format/size/focus validation -> original storage via
//!   [`MediaStore::put`] -> [`media_repository::insert_media`] in
//!   [`MediaState::Processing`] -> [`job_queue::enqueue`]), owner-scoped
//!   status lookup, and description/focus metadata update (accepted while
//!   still `processing`, out-of-range focus rejected the same way at both
//!   upload and update time) — see [`service`] and its
//!   [`service::MediaService`]. `MediaService<S: MediaStore>` takes its
//!   store as a generic type parameter rather than `Arc<dyn MediaStore>`
//!   (`MediaStore` is not `dyn`-object-safe, mirroring
//!   `src/federation/`'s established precedent for other non-object-safe
//!   async ports — see `service.rs`'s own doc comment). design.md's
//!   `UploadInput`/`MetadataPatch` are named but never field-defined in the
//!   excerpted Service Interface; this task defines both minimally (see
//!   `service.rs`'s doc comment, "`UploadInput`/`MetadataPatch` shapes",
//!   for the exact shape chosen and why `focus` is a raw `(f32, f32)`
//!   coordinate pair on both, validated internally via [`Focus::new`]
//!   rather than pre-validated by the caller).
//!   No HTTP surface (`MediaEndpoints`, task 5.1) exists yet, and this
//!   module is not wired into `crate::state::AppState`/`crate::bootstrap`/
//!   `crate::server` (task 5.2's job) — see design.md's File Structure Plan
//!   for the full planned module set.
//! - Task 4.3 (`Boundary: ProcessingWorker`): the resident DB-queue-
//!   consuming worker — [`worker::ProcessingWorker`], generic over `S:
//!   MediaStore, P: MediaProcessor` (mirroring [`service::MediaService`]'s
//!   own generic-over-store precedent) — see [`worker`]. Its
//!   [`worker::ProcessingWorker::run_once`] claims and fully resolves at
//!   most one due job (claim -> load original -> `process_image` -> store
//!   thumbnail -> `set_ready` -> `complete`), and
//!   [`worker::ProcessingWorker::run`] is the actual resident poll loop
//!   built on top of it, accepting an injectable shutdown signal shaped
//!   like [`crate::server::serve_with_shutdown_and_signal`]'s (task 5.2 can
//!   wire this in without modification). Classifies every failure
//!   `attempt` can produce into design.md's flowchart's two distinct edges:
//!   a storage-boundary I/O failure is `-->|transient fail| Retry` (goes
//!   through `job_queue::fail_or_retry`'s normal attempts-budget/backoff
//!   accounting), while `MediaProcessor::process_image` returning `Err`
//!   (Requirement 6.5's decode/generation failure) is
//!   `-->|decode fail| Failed` — forced immediately terminal by calling
//!   `fail_or_retry` with `max_attempts = 0`, bypassing the retry budget
//!   entirely rather than merely reaching it faster. Closes the
//!   `last_error` diagnostic gap tasks.md's "3.2 レビュー所見" flagged
//!   (Requirement 4.5): `job_queue::fail_or_retry` (task 3.2's module) is
//!   extended here with an `error_message: &str` parameter it now persists
//!   into `media_processing_jobs.last_error` on both its branches, built by
//!   this module's `diagnostic_message` helper — paired with a
//!   `tracing::warn!`/`tracing::error!` event on every failure path.
//!   Idempotent re-runs (Requirement 4.6) short-circuit on `Media::state`
//!   (the truth source) rather than re-deriving/re-storing anything once a
//!   job's media already left `Processing`. Also adds
//!   [`media_repository::find_by_id`] (an unscoped-by-actor lookup
//!   `media_repository.rs`'s task 3.1 implementation did not previously
//!   expose — a worker has only a bare `media_id` from its claimed job,
//!   never a requesting actor). Does not wire itself into `AppState`/
//!   `bootstrap.rs`/`server.rs` (task 5.2) and does not implement
//!   `MediaEndpoints` (task 5.1).
//! - Task 4.2 (`Boundary: MediaAttachmentSerializer`): the pure
//!   Mastodon-compatible MediaAttachment JSON serializer —
//!   [`serializer::to_media_attachment`]/[`serializer::to_json`], consuming
//!   an already-persisted [`Media`] plus a [`MediaStore`] (for proxy-aware
//!   `url`/`preview_url` resolution via [`MediaStore::public_url`]) and a
//!   [`crate::api::pagination::ForwardedOrigin`] — see [`serializer`]. `url`
//!   is `null` unless `Media::state == MediaState::Ready`; `preview_url` is
//!   `null` until thumbnail dimensions are actually confirmed (a narrower,
//!   data-driven gate — see `serializer.rs`'s own doc comment for why this
//!   is not simply the same `state == Ready` check repeated); `meta.original`/
//!   `meta.small` are omitted (not `null`) until confirmed; `meta.focus`
//!   defaults to center via [`Focus::default`]; `remote_url` is always
//!   `null` (no remote-media-cache concept in this MVP). Both the
//!   `Processing` and `Ready` variants of this contract are registered as
//!   goldens with api-foundation's [`crate::contract`] harness under
//!   `tests/golden/media/` (Requirements 8.3, 8.4). Does not implement
//!   `ProcessingWorker` (task 4.3) or any HTTP surface/runtime wiring
//!   (tasks 5.1/5.2) — this module has no `axum`/router/`AppState` code.
//! - Task 5.1 (`Boundary: MediaEndpoints`): the HTTP surface — `POST
//!   /api/v2/media` ([`upload_media`]), `GET /api/v1/media/:id`
//!   ([`show_media`]), `PUT /api/v1/media/:id` ([`update_media`]), all
//!   requiring `write:media` (reusing api-foundation's
//!   `oauth::middleware::RequiredActor`/`require_scope`, never
//!   reimplementing auth/scope), returning `202`/`206`/`200`/`404`/`422`
//!   per design.md's System Flows, with every failure rendered through
//!   `AppError`'s already-wired Mastodon-compatible error body — see
//!   [`endpoints`] and [`MediaEndpointsState`]. Not yet mounted on the live
//!   application router (task 5.2's job); see `endpoints`'s own doc comment
//!   for the test-local-router precedent this task's own integration
//!   coverage (`tests/media_endpoints_it.rs`) follows, and for a documented
//!   CONCERN task 5.2 must address (axum's built-in 2MB body-limit default
//!   vs. `MediaConfig::max_upload_size_bytes`).

//! - Task 5.2 (`Boundary: MediaModule wiring`): the Composition-Root wiring
//!   itself — [`MediaModule`] (the `AppState`-shared handle:
//!   `Arc<MediaService<LocalFsStore>>` + `LocalFsStore`) and
//!   [`build_media_module`], which constructs every media component with one
//!   concrete production store/processor pair
//!   ([`LocalFsStore`]/[`PureRustImageProcessor`]) and returns the
//!   not-yet-started [`MediaBackgroundWorkers`] pool (one `ProcessingWorker`
//!   per `MediaConfig::worker_concurrency`) — mirroring
//!   `crate::federation::module`'s identical `FederationModule`/
//!   `build_federation_module`/`FederationBackgroundTasks` shape. `src/state.rs`
//!   stores the resulting [`MediaModule`] on `AppState`; `src/bootstrap.rs`
//!   (production) and `src/test_harness.rs` (`spawn_test_app`) both call
//!   [`build_media_module`] and then [`MediaBackgroundWorkers::spawn`] to
//!   actually start the worker pool; `src/server.rs` mounts
//!   [`endpoints::upload_media`]/[`endpoints::show_media`]/
//!   [`endpoints::update_media`] (monomorphized over [`LocalFsStore`]) behind
//!   the same cross-cutting layers (Bearer auth, error conversion, rate
//!   limiting) every other endpoint on the real router sits behind, sized
//!   with a `DefaultBodyLimit` layer from `MediaConfig::max_upload_size_bytes`
//!   (resolving the CONCERN task 5.1 documented — see `endpoints.rs`'s own
//!   doc comment).

pub mod endpoints;
pub mod image_processor;
pub mod job_queue;
pub mod local_fs;
pub mod media_repository;
pub mod model;
pub mod processor;
pub mod serializer;
pub mod service;
pub mod store;
pub mod worker;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use sqlx::PgPool;

use crate::config::MediaConfig;
use crate::runtime::RuntimeContext;

pub use endpoints::{
    MediaEndpointsState, ResolvedOrigin, UpdateMediaRequest, show_media, update_media, upload_media,
};
pub use image_processor::PureRustImageProcessor;
pub use job_queue::{JobOutcome, backoff_delay, claim_due, complete, enqueue, fail_or_retry};
pub use local_fs::LocalFsStore;
pub use media_repository::{
    find_by_id, find_owned, insert_media, set_failed, set_ready, update_metadata,
};
pub use model::{
    Dimensions, FOCUS_MAX, FOCUS_MIN, Focus, FocusAxis, FocusRangeError, JobState, Media,
    MediaMeta, MediaState, MediaType, ProcessingJob,
};
pub use processor::{MediaProcessor, ProcessedImage, ThumbnailSpec};
pub use serializer::{
    DimensionsJson, FocusJson, MediaAttachmentJson, MediaMetaJson, to_json, to_media_attachment,
};
pub use service::{MediaService, MetadataPatch, UploadInput};
pub use store::{MediaStore, ObjectKey, ObjectVariant};
pub use worker::{DEFAULT_POLL_INTERVAL, ProcessingWorker, WorkerOutcome};

/// The media-pipeline module bundle `AppState` shares across concurrent
/// request handlers (design.md's `MediaModule(bootstrap wiring)` component;
/// task 5.2, Requirements 1.1, 4.1, 9.5). Bundles exactly what
/// `src/server.rs`'s mounted endpoints need: the shared, `Arc`-wrapped
/// [`MediaService`] business layer and the shared [`LocalFsStore`] (needed
/// again directly by [`serializer::to_media_attachment`] for proxy-aware
/// URL resolution — see `endpoints.rs`'s own `MediaEndpointsState<S>` doc
/// comment for why both are needed, not just the service). `LocalFsStore` is
/// the one concrete [`MediaStore`] this instance mounts every media-generic
/// component with (mirroring `crate::federation::module`'s "one concrete
/// type per non-`dyn`-safe trait" convention, task 4.1's/4.3's own
/// established precedent for `MediaService<S: MediaStore>`/
/// `ProcessingWorker<S, P>`).
///
/// This type does not construct its own dependencies — [`build_media_module`]
/// does that (mirrors `AppState::new`'s own "bundle, don't build" contract,
/// and `FederationModule::new`'s identical precedent).
pub struct MediaModule {
    service: Arc<MediaService<LocalFsStore>>,
    store: LocalFsStore,
}

impl MediaModule {
    /// The shared `MediaService` handle `src/server.rs`'s
    /// `FromRef<AppState> for MediaEndpointsState<LocalFsStore>` bridge
    /// clones into every mounted media endpoint's own state — an `Arc`
    /// clone (cheap, one atomic increment), not a freshly constructed
    /// service.
    pub fn service(&self) -> Arc<MediaService<LocalFsStore>> {
        Arc::clone(&self.service)
    }

    /// The shared `LocalFsStore` handle the same bridge clones for
    /// `MediaEndpointsState::store` (needed directly by
    /// `MediaAttachmentSerializer::to_media_attachment`, not routed through
    /// `MediaService`).
    pub fn store(&self) -> &LocalFsStore {
        &self.store
    }
}

/// The not-yet-started resident `ProcessingWorker` pool [`build_media_module`]
/// returns alongside [`MediaModule`] — mirrors
/// `crate::federation::module::FederationBackgroundTasks`'s identical
/// "constructing is synchronous and cheap; only [`Self::spawn`] actually
/// starts anything" contract, so constructing this never blocks
/// `bootstrap()`'s/`spawn_test_app`'s own startup. One worker per
/// `MediaConfig::worker_concurrency` — Requirement 4.2's exclusive `FOR
/// UPDATE SKIP LOCKED` claim is exactly what lets more than one of these
/// safely run concurrently against the same DB queue.
pub struct MediaBackgroundWorkers {
    workers: Vec<ProcessingWorker<LocalFsStore, PureRustImageProcessor>>,
    poll_interval: StdDuration,
}

impl MediaBackgroundWorkers {
    /// Starts every worker in the pool as its own detached `tokio::spawn`
    /// task and returns immediately (never awaits any of them) — the
    /// caller's own startup sequence is never blocked on this call,
    /// mirroring `FederationBackgroundTasks::spawn`'s identical
    /// non-blocking contract.
    ///
    /// Unlike `FederationBackgroundTasks::spawn` (whose delivery-worker loop
    /// has no shutdown hook at all), [`ProcessingWorker::run`] was built
    /// (task 4.3) specifically to accept an injectable shutdown signal
    /// mirroring `crate::server::serve_with_shutdown_and_signal`'s
    /// convention — see `worker.rs`'s own doc comment, "`run`'s shutdown
    /// signal", which explicitly names this task as the one expected to
    /// wire a real signal through rather than leaving that parameter
    /// permanently unexercised. `signal_factory` is called once *per
    /// worker* (not once total) because each [`ProcessingWorker::run`] call
    /// needs to own a distinct `Future` — `crate::server::os_shutdown_signal`
    /// (made `pub(crate)` by this task specifically for this reuse, passed
    /// directly as this factory by `src/bootstrap.rs`) supports exactly
    /// this: tokio's `ctrl_c()`/`signal(SignalKind::terminate())` both
    /// support any number of concurrent independent listeners, so calling it
    /// once per worker (plus once more, independently, inside
    /// `crate::server::serve_with_shutdown`'s own internal call) is not a
    /// race — every listener observes the same real OS signal, and an
    /// in-flight `run_once` is always allowed to finish before a worker's
    /// own loop returns (drain, not abort — see `worker.rs`'s doc comment).
    /// `src/test_harness.rs` passes a factory that never resolves
    /// (`std::future::pending`) instead, mirroring
    /// `FederationBackgroundTasks`'s own tests-never-stop-the-worker
    /// precedent (a `#[tokio::test]`'s own per-test runtime simply aborts
    /// every still-running spawned task, detached workers included, when
    /// the test function returns).
    pub fn spawn<F, Fut>(self, signal_factory: F)
    where
        F: Fn() -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        for worker in self.workers {
            let signal = signal_factory();
            let poll_interval = self.poll_interval;
            tokio::spawn(async move {
                worker.run(poll_interval, signal).await;
            });
        }
    }
}

/// Assembles every media component with one concrete production
/// store/processor pair ([`LocalFsStore`]/[`PureRustImageProcessor`]),
/// bundles them as a [`MediaModule`], and returns the not-yet-started
/// [`MediaBackgroundWorkers`] pool (task 5.2, Requirements 1.1, 4.1). Shared
/// by `src/bootstrap.rs` (production) and `src/test_harness.rs`
/// (`spawn_test_app`), mirroring
/// `crate::federation::module::build_federation_module`'s identical "one
/// composition function, two callers" precedent.
///
/// `pool`/`runtime` are already-established (this function never opens its
/// own pool or builds its own `RuntimeContext`) — threaded through from
/// whichever stage of `build_state`/`spawn_test_app` already built them,
/// mirroring `build_federation_module`'s identical parameter contract.
/// `config` is `AppConfig.media` (task 1.2): `storage_root` seeds
/// [`LocalFsStore::new`]; every worker shares one clone of `config` (and of
/// `store`/`processor` — `LocalFsStore` is cheap to clone, a `PathBuf`
/// underneath; `PureRustImageProcessor` is zero-sized and `Copy`).
pub fn build_media_module(
    pool: PgPool,
    runtime: RuntimeContext,
    config: MediaConfig,
) -> (MediaModule, MediaBackgroundWorkers) {
    let store = LocalFsStore::new(config.storage_root.clone());
    let processor = PureRustImageProcessor::new();

    let service = Arc::new(MediaService::new(
        pool.clone(),
        runtime.clone(),
        config.clone(),
        store.clone(),
    ));

    let workers = (0..config.worker_concurrency)
        .map(|_| {
            ProcessingWorker::new(
                pool.clone(),
                runtime.clone(),
                config.clone(),
                store.clone(),
                processor,
            )
        })
        .collect();

    let module = MediaModule { service, store };
    let background = MediaBackgroundWorkers {
        workers,
        poll_interval: DEFAULT_POLL_INTERVAL,
    };
    (module, background)
}
