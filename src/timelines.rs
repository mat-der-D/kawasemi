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
//! - Task 1.2 (`Boundary: TimelineKindRules`): the per-kind structural
//!   condition design.md's "model / TimelineKindRules" component names —
//!   home = follows ∪ self, `direct` excluded, boosts included;
//!   public/local/tag = `public`-only and boost-excluded; local = +local
//!   author; tag = +hashtag any/all/none matching — see [`kind_rules`].
//!
//!   No `CandidateRepository`/`TimelineFilter`/`TimelineMatcher`/
//!   `StatusHydrator`/`TimelineService`/`TimelineEndpoints` (later tasks),
//!   and no wiring into `crate::state`/`crate::bootstrap`/`crate::server`
//!   (task 5.2) live here — this module is not yet mounted anywhere.
//! - Task 2.1 (`Boundary: CandidateRepository`): the read-only candidate
//!   fetch design.md's "Data / データ層" component names —
//!   [`candidate_repository::fetch_candidates`] — translating a
//!   [`model::TimelineQuerySpec`]'s per-kind condition (mirroring
//!   [`kind_rules::TimelineKindRules`], never re-deriving it), cursor range,
//!   and `local`/`remote`/`only_media`/tag narrowing into a read-only
//!   `statuses`/`status_media`/`tags`/`status_tags` query — see
//!   [`candidate_repository`].
//!
//!   No `TimelineFilter`/`TimelineMatcher`/`StatusHydrator`/`TimelineService`/
//!   `TimelineEndpoints` (later tasks), and no wiring into
//!   `crate::state`/`crate::bootstrap`/`crate::server` (task 5.2) live here.
//! - Task 3.1 (`Boundary: TimelineFilter`): applying, to a single already
//!   kind-matched candidate, statuses-core's `VisibilityPolicy::is_visible`
//!   (unauthenticated viewer -> public-only) and social-graph `FilterQuery`-
//!   sourced relationship exclusion (blocked/blocked_by/muted, mute-expiry
//!   already considered upstream) bundled via [`model::FilterContext`], plus
//!   the boost-specific `reblogs_hidden`/boosted-original-author exclusion
//!   rules — design.md's "Filter / フィルタ層" component names — see
//!   [`filter::TimelineFilter::keep`].
//!
//!   No `TimelineMatcher`/`StatusHydrator`/`TimelineService`/
//!   `TimelineEndpoints` (later tasks), and no wiring into
//!   `crate::state`/`crate::bootstrap`/`crate::server` (task 5.2) live here.
//! - Task 3.2 (`Boundary: TimelineMatcher`): the single generation point
//!   design.md's "Domain (single point) / 単一生成点" component names —
//!   [`matcher::TimelineMatcher::candidate_spec`] (converts a
//!   [`model::TimelineKind`] and [`model::TimelineParams`] into the
//!   [`model::TimelineQuerySpec`] `CandidateRepository` consumes, the REST
//!   candidate-query side) and [`matcher::TimelineMatcher::matches`] (single-
//!   status membership judgment combining [`kind_rules::TimelineKindRules`]
//!   with [`filter::TimelineFilter`], the Streaming reuse side) — both built
//!   on the already-established `TimelineKindRules`/`TimelineFilter` without
//!   re-deriving either (8.1, 8.2). Delivery/broadcast itself is not
//!   included (8.4) — see [`matcher`].
//!
//!   No `StatusHydrator`/`TimelineService`/`TimelineEndpoints` (later
//!   tasks), and no wiring into `crate::state`/`crate::bootstrap`/
//!   `crate::server` (task 5.2) live here.
//! - Task 4.1 (`Boundary: StatusHydrator`): hydrating a filtered candidate
//!   into Status JSON design.md's "Serialize / 具体化層" component names —
//!   [`hydrator::StatusHydrator::hydrate`] resolves the
//!   `Status -> StatusRenderInput` assembly glue (account/media/tags/
//!   emojis/interactions/poll/reblog target) and delegates the actual JSON
//!   mapping to statuses-core's `serializer::status_to_json`
//!   (Requirement 10.1), passing viewer operation state so
//!   `favourited`/`reblogged`/`bookmarked`/... reflect the authenticated
//!   context (Requirement 10.2), nesting a boost's original post under
//!   `reblog` non-recursively (Requirement 10.3), and never building its
//!   own Account/MediaAttachment representation (Requirement 10.4) — see
//!   [`hydrator`].
//!
//!   No `TimelineService`/`TimelineEndpoints` (later tasks), and no wiring
//!   into `crate::state`/`crate::bootstrap`/`crate::server` (task 5.2) live
//!   here.
//! - Task 4.2 (`Boundary: TimelineService`): the end-to-end aggregation
//!   design.md's "Service / サービス層" component names —
//!   [`service::TimelineService::timeline`] loads the viewer's relationship
//!   sets once into a [`model::FilterContext`] (`FilterQuery`, Requirement
//!   6.1), calls [`matcher::TimelineMatcher::candidate_spec`], runs a
//!   bounded fill loop ([`candidate_repository::fetch_candidates`] batches
//!   -> [`filter::TimelineFilter::keep`], capped by a small fixed
//!   `MAX_FILL_ITERATIONS` so a viewer with many blocks/mutes or a sparse
//!   tag query never approaches a full-table scan — Requirement 7.4), hands
//!   the accumulated survivors to api-foundation's `paginate` for cursor-
//!   stable windowing, and hydrates the final page via
//!   [`hydrator::StatusHydrator::hydrate`] — see [`service`].
//!
//!   No `TimelineEndpoints`/`TimelinesModule` (later tasks), and no wiring
//!   into `crate::state`/`crate::bootstrap`/`crate::server` (task 5.2) live
//!   here.
//! - Task 5.1 (`Boundary: TimelineEndpoints`): the HTTP surface design.md's
//!   "API / エンドポイント層" component names — [`endpoints::home_timeline`]
//!   (`GET /api/v1/timelines/home`, Bearer + `read:statuses`, 401 when
//!   unauthenticated), [`endpoints::public_timeline`] (`GET
//!   /api/v1/timelines/public`, optional auth, `local`/`remote`/
//!   `only_media`, unauthenticated returns public-only — also serves the
//!   local timeline via `?local=true`, no separate route), and
//!   [`endpoints::tag_timeline`] (`GET /api/v1/timelines/tag/:hashtag`,
//!   optional auth, `any[]`/`all[]`/`none[]`/`local`/`only_media`) — each
//!   applying scope validation, Mastodon-compatible errors, and `Link`
//!   header attachment (Requirements 1.1, 1.6, 2.1, 2.3, 3.1, 4.1, 7.2, 9.1,
//!   9.2, 9.3, 9.4) atop [`service::TimelineService::timeline`] (task 4.2)
//!   unchanged — see [`endpoints`].
//!
//!   No `TimelinesModule` (task 5.2), and no wiring into
//!   `crate::state`/`crate::bootstrap`/`crate::server` live here — this
//!   module's handlers are not mounted onto the live application router yet.

pub mod candidate_repository;
pub mod endpoints;
pub mod filter;
pub mod hydrator;
pub mod kind_rules;
pub mod matcher;
pub mod model;
pub mod service;
