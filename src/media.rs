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
//!   see [`model`]. No persistence (`MediaRepository`/`ProcessingJobQueue`,
//!   tasks 3.1/3.2), storage (`MediaStore`, task 2.2), image processing
//!   (`MediaProcessor`, task 2.3), business logic (`MediaService`, task
//!   4.1), or HTTP surface (`MediaEndpoints`, task 5.1) exist yet, and this
//!   module is not wired into `crate::state::AppState`/`crate::bootstrap`/
//!   `crate::server` (task 5.2's job) — see design.md's File Structure
//!   Plan for the full planned module set.

pub mod model;

pub use model::{
    Dimensions, FOCUS_MAX, FOCUS_MIN, Focus, FocusAxis, FocusRangeError, JobState, Media,
    MediaMeta, MediaState, MediaType, ProcessingJob,
};
