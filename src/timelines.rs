//! Timelines domain module (timelines spec, `src/timelines.rs` +
//! `src/timelines/`, mirroring the module-with-submodule convention
//! established by `src/statuses.rs`/`src/statuses/` and
//! `src/social_graph.rs`/`src/social_graph/`).
//!
//! Scope so far:
//! - Task 1.1 (`Boundary: model`): the domain value types design.md's
//!   "model / TimelineKindRules" component names for this task —
//!   [`model::TimelineKind`] (Home/Public/Local/Tag),
//!   [`model::TimelineParams`] (local/remote/only_media/tag/page),
//!   [`model::TagFilter`] (primary/any/all/none hashtag conditions),
//!   [`model::TimelineQuerySpec`] (kind + params + cursor range for
//!   candidate retrieval), and [`model::FilterContext`] (viewer + the five
//!   relationship sets + `now`) — see [`model`]. Built on core-runtime's
//!   [`crate::domain::Id`]/`time::OffsetDateTime` and api-foundation's
//!   [`crate::api::pagination::PageParams`], per this task's own explicit
//!   instruction; none of these types are redefined here.
//!
//!   No `TimelineKindRules` (task 1.2), no `CandidateRepository`/
//!   `TimelineFilter`/`TimelineMatcher`/`StatusHydrator`/`TimelineService`/
//!   `TimelineEndpoints` (later tasks), and no wiring into
//!   `crate::state`/`crate::bootstrap`/`crate::server` (task 5.2) live here —
//!   this module is not yet mounted anywhere.

pub mod model;
