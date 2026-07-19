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

pub mod image_processor;
pub mod job_queue;
pub mod local_fs;
pub mod media_repository;
pub mod model;
pub mod processor;
pub mod service;
pub mod store;

pub use image_processor::PureRustImageProcessor;
pub use job_queue::{JobOutcome, backoff_delay, claim_due, complete, enqueue, fail_or_retry};
pub use local_fs::LocalFsStore;
pub use media_repository::{find_owned, insert_media, set_failed, set_ready, update_metadata};
pub use model::{
    Dimensions, FOCUS_MAX, FOCUS_MIN, Focus, FocusAxis, FocusRangeError, JobState, Media,
    MediaMeta, MediaState, MediaType, ProcessingJob,
};
pub use processor::{MediaProcessor, ProcessedImage, ThumbnailSpec};
pub use service::{MediaService, MetadataPatch, UploadInput};
pub use store::{MediaStore, ObjectKey, ObjectVariant};
