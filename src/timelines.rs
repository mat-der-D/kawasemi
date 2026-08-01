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
//! - Task 5.2 (`Boundary: TimelinesModule, server, bootstrap, state`,
//!   `_Depends: 5.1_`): this module's own wiring/assembly point — design.md's
//!   "Runtime / 配線層" -> "`TimelinesModule`（wiring）" (design.md line
//!   ~420) and "File Structure Plan"'s explicit assignment of "TimelinesModule
//!   組み立て・公開・ルータ装着点・TimelineMatcher シーム公開" to this very
//!   file — mirrors `crate::statuses::StatusesModule`/
//!   `crate::social_graph::SocialGraphModule`'s identical "bundle, don't
//!   build; accessors return `Arc::clone`" shape (`src/statuses.rs`/
//!   `src/social_graph.rs`'s own doc comments).
//!
//!   [`build_timelines_module`] constructs exactly one [`hydrator::StatusHydrator`]
//!   (task 4.1, already reviewed) from `pool`/`accounts`/`media_store`, wraps
//!   it in exactly one [`service::TimelineService`] (task 4.2, already
//!   reviewed) alongside `pool`/`runtime`, and bundles that together with a
//!   [`matcher::TimelineMatcher`] value (task 3.2, already reviewed — a
//!   stateless `Copy` unit struct, so "storing" it costs nothing and needs no
//!   async construction) into a [`TimelinesModule`]. This is the "結線"
//!   (wiring) design.md's own Responsibilities & Constraints for this
//!   component names explicitly: `VisibilityPolicy`(statuses-core)/
//!   `FilterQuery`(social-graph)/`StatusSerializer`(statuses-core)/
//!   Pagination(api-foundation) are each already consumed, unmodified, by
//!   `TimelineFilter`/`TimelineService`/`StatusHydrator`'s own already-
//!   reviewed bodies (`filter.rs`'s `VisibilityPolicy::is_visible` call,
//!   `service.rs`'s `FilterQuery::new(...)` call, `hydrator.rs`'s
//!   `serializer::status_to_json` call, `service.rs`'s `paginate` call) —
//!   none of those four upstream dependencies need a registry/port
//!   indirection the way `crate::statuses::visibility::RelationshipQueryRegistry`
//!   does, so this module's own "結線" is exactly and only constructing the
//!   already-reviewed collaborator chain once, with real production
//!   `Arc<AccountService<...>>`/`LocalFsStore` handles sourced the same way
//!   every sibling module already sources them (`accounts_module.service()`/
//!   `media_module.store()`, per `crate::statuses::build_statuses_module`'s
//!   own identical precedent for `StatusHydrator`'s own account/media
//!   dependencies).
//!
//!   `src/state.rs` gains a `timelines: TimelinesModule` field/accessor
//!   (mirroring `statuses`/`social_graph`'s own identical addition);
//!   `src/bootstrap.rs` (and every other Composition-Root call site that
//!   builds an `AppState` — `src/test_harness.rs`, `src/federation/
//!   test_harness.rs`, `src/server/tests.rs`, `src/state/tests.rs` — calls
//!   [`build_timelines_module`] after `accounts_module`/`media_module` already
//!   exist and threads the result into `AppState::new`'s new final argument;
//!   `src/server.rs` builds `timelines_router()` (mirroring
//!   `social_graph_router()`/`statuses_router()`'s own precedent — see task
//!   5.1's own Implementation Note, "no `pub fn router(...)` in this module")
//!   mounting [`endpoints::HOME_TIMELINE_PATH`]/[`endpoints::PUBLIC_TIMELINE_PATH`]/
//!   [`endpoints::TAG_TIMELINE_PATH`] to [`endpoints::home_timeline`]/
//!   [`endpoints::public_timeline`]/[`endpoints::tag_timeline`], and `.merge()`s
//!   it onto the foundation router the same way every other route group is.
//!   `TimelineMatcher` becomes reachable from `AppState` via
//!   `state.timelines().matcher()` — the public seam Requirement 8.3 and this
//!   task's own "観測可能な完了" both name, ready for a future `streaming`
//!   spec to reuse without this module's own API needing to change shape.

pub mod candidate_repository;
pub mod endpoints;
pub mod filter;
pub mod hydrator;
pub mod kind_rules;
pub mod matcher;
pub mod model;
pub mod service;

// ---- Task 5.2 (Boundary: TimelinesModule, server, bootstrap, state):
// module wiring -------------------------------------------------------------

use std::sync::Arc;

use sqlx::PgPool;

use crate::accounts::account_service::AccountService;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::local_fs::LocalFsStore;
use crate::runtime::RuntimeContext;
use crate::timelines::hydrator::StatusHydrator;
use crate::timelines::matcher::TimelineMatcher;
use crate::timelines::service::TimelineService;

/// The timelines module bundle (design.md's exact `TimelinesModule`
/// component; task 5.2, Requirements 8.1, 8.3): the shared
/// [`service::TimelineService`] handle `src/server.rs`'s
/// `FromRef<AppState> for crate::timelines::endpoints::TimelineEndpointsState`
/// bridge derives every mounted home/public/tag timeline endpoint's own
/// state from, plus the [`matcher::TimelineMatcher`] public seam a future
/// `streaming` spec reuses via `AppState::timelines().matcher()` (Requirement
/// 8.3). Built by [`build_timelines_module`].
pub struct TimelinesModule {
    service: Arc<TimelineService>,
    matcher: TimelineMatcher,
}

impl TimelinesModule {
    /// The shared `TimelineService` handle — mirrors
    /// `crate::statuses::StatusesModule::status_service`/
    /// `crate::social_graph::SocialGraphModule::follow`'s own identical
    /// "cheap `Arc::clone` accessor" shape.
    pub fn service(&self) -> Arc<TimelineService> {
        Arc::clone(&self.service)
    }

    /// The `TimelineMatcher` public seam (Requirement 8.3): a downstream
    /// `streaming` spec reaches the single membership-judgment point this
    /// spec establishes through this accessor, exactly the way REST
    /// (`service::TimelineService::timeline`, task 4.2, already reviewed)
    /// already does internally. Returned by value (not `&`/`Arc`): the type
    /// itself is a zero-field, `Copy` unit struct (`matcher.rs`'s own doc
    /// comment — "Holds no state of its own"), so cloning/copying it is
    /// exactly as cheap as taking a reference would be, with none of a
    /// reference's borrow-lifetime friction for a caller that wants to hold
    /// its own owned value (e.g. to capture into a future `streaming`
    /// broadcast closure).
    pub fn matcher(&self) -> TimelineMatcher {
        self.matcher
    }
}

/// Assembles the [`TimelinesModule`] bundle (task 5.2, Requirements 8.1,
/// 8.3): builds one [`hydrator::StatusHydrator`] from `pool`/`accounts`/
/// `media_store`, wraps it in one [`service::TimelineService`] alongside
/// `pool`/`runtime`, and pairs that with a fresh [`matcher::TimelineMatcher`]
/// value — see this module's own doc comment ("Task 5.2") for why no
/// registry/port indirection is needed for the four upstream dependencies
/// design.md's own Responsibilities & Constraints names
/// (`VisibilityPolicy`/`FilterQuery`/`StatusSerializer`/Pagination).
///
/// `accounts`/`media_store` are the exact same production handles every
/// sibling module already sources from the same two upstream module bundles
/// (`accounts_module.service()`/`media_module.store().clone()`, mirroring
/// `crate::statuses::register_account_ports`'s own identical parameter
/// pair) — callers (`src/bootstrap.rs`, `src/test_harness.rs`, `src/federation/
/// test_harness.rs`) must call this after both `accounts::build_accounts_module`
/// and `media::build_media_module` have already run.
pub fn build_timelines_module(
    pool: PgPool,
    runtime: RuntimeContext,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    media_store: LocalFsStore,
) -> TimelinesModule {
    let hydrator = StatusHydrator::new(pool.clone(), accounts, media_store);
    let service = Arc::new(TimelineService::new(pool, runtime, hydrator));

    TimelinesModule {
        service,
        matcher: TimelineMatcher,
    }
}
