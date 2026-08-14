//! Search domain module (search spec, made up of `src/search.rs` and
//! `src/search/`, mirroring the module-with-submodule convention
//! established by `src/media.rs`/`src/media/`, `src/accounts.rs`/
//! `src/accounts/`, `src/statuses.rs`/`src/statuses/`,
//! `src/social_graph.rs`/`src/social_graph/`, and
//! `src/notifications.rs`/`src/notifications/`).
//!
//! Scope so far:
//! - Task 1.1 (`Boundary: Migration`, no Rust code):
//!   `migrations/0013_search.sql` — `search_tags`, `search_status_tags`,
//!   and the `search_index_watermark` singleton table (see that spec's
//!   `tasks.md` Implementation Notes for the `0010` -> `0013` migration
//!   number substitution).
//! - Task 1.2 (`Boundary: model`): the domain value types design.md's
//!   "model" component names — [`model::SearchType`],
//!   [`model::SearchParams`], [`model::ParsedQuery`],
//!   [`model::SearchMatches`] (identifiers only, Requirement 7.2),
//!   [`model::TagMatch`], [`model::TagView`], and
//!   [`model::TagHistoryEntry`]. `Id`/`AccountRef` are not redefined here:
//!   both are imported from `crate::domain` (core-runtime's canonical
//!   shared primitives module, mirroring `src/notifications/model.rs`'s
//!   and `src/statuses/model.rs`'s identical precedent) — see [`model`]'s
//!   own doc comment.
//! - Task 1.3 (`Boundary: QueryParser`): the raw-query discrimination and
//!   normalization boundary — [`query_parser::parse_query`] classifies a
//!   trimmed query string into a [`model::ParsedQuery`], in order:
//!   `acct:user@domain` (WebFinger acct URI syntax), `@user@domain`
//!   (Mastodon mention shorthand), an absolute `http`/`https` URL with a
//!   present host, or else a plain-word query, rejecting empty/whitespace-
//!   only input with a `422 Unprocessable Entity` `AppError` (Requirements
//!   2.3, 6.1, 6.2) — see [`query_parser`]'s own doc comment for the full
//!   normalization rules and edge-case resolutions.
//!
//! - Task 1.4 (`Boundary: SearchBackend`): the search backend abstraction
//!   boundary — [`ports::SearchBackend`] (`search_accounts` /
//!   `search_statuses` / `search_hashtags`, identifiers only, Requirement
//!   7.2), its three request types ([`ports::AccountQuery`],
//!   [`ports::StatusQuery`], [`ports::HashtagQuery`]), and the
//!   swap-in test double [`ports::StubSearchBackend`] (Requirement 7.5).
//!   The concrete standard-PostgreSQL implementation (`PgSearchBackend`)
//!   is implemented separately by task 3.1 below.
//!
//! - Task 2.1 (`Boundary: HashtagIndexRepository`): read/upsert access to
//!   this spec's own hashtag read index —
//!   [`hashtag_repository::match_hashtags`] (name prefix match ->
//!   `Vec<`[`TagView`]`>`, `limit`/`offset` applied), and
//!   [`hashtag_repository::load_watermark`]/
//!   [`hashtag_repository::save_watermark`] (the `HashtagIndexer`'s, task
//!   2.2, derivation-cursor read/upsert) against `search_tags` /
//!   `search_status_tags` / `search_index_watermark` only (Requirement 8.2).
//!   No derivation from upstream `statuses` happens here — that is task
//!   2.2's (`hashtag_indexer.rs`) job, strictly downstream of this module.
//!
//! - Task 2.2 (`Boundary: HashtagIndexer`): the watermark-cursor-based
//!   derivation/catch-up scan —
//!   [`hashtag_indexer::catch_up_from_watermark`] reads
//!   [`hashtag_repository::load_watermark`]'s cursor (`None` = backfill),
//!   walks `statuses` rows newer than it in ascending `(created_at, id)`
//!   batches, derives each one's already-extracted hashtags (statuses-core's
//!   own `crate::statuses::tag_repository::tags_for_status`, read-only) into
//!   `search_tags`/`search_status_tags` via
//!   [`hashtag_repository::upsert_tag_usage`], and advances the watermark
//!   via [`hashtag_repository::save_watermark`] once each batch fully
//!   completes. See [`hashtag_indexer`]'s own doc comment for the full
//!   signature/batch-shape reasoning. The wiring into `search_hashtags`/
//!   `PgSearchBackend` is implemented separately by task 3.2 below.
//!
//! - Task 3.1 (`Boundary: PgSearchBackend`): the standard-PostgreSQL default
//!   [`ports::SearchBackend`] implementation —
//!   [`pg_backend::PgSearchBackend::search_accounts`] (local
//!   `account_profiles.display_name` / known-remote `remote_accounts.
//!   username`/`domain`/`display_name`/synthesized-acct partial match,
//!   `ILIKE`) and
//!   [`pg_backend::PgSearchBackend::search_statuses`] (`statuses.content`
//!   partial match, optional `account_id` scope, design.md's overfetch
//!   convention), both `limit`/`offset`-aware and extension-free
//!   (Requirements 3.1, 3.4, 4.1, 4.3, 4.4, 4.5, 4.6, 7.2). `search_hashtags`
//!   is implemented separately by task 3.2 below. See [`pg_backend`]'s own
//!   doc comment for the full SQL query surface and its CONCERNs.
//!
//! - Task 3.2 (`Boundary: PgSearchBackend`, `search_hashtags`):
//!   [`pg_backend::PgSearchBackend::search_hashtags`] wires this spec's
//!   read-index boundary together — on every call it first runs
//!   [`hashtag_indexer::catch_up_from_watermark`] to bring `search_tags`/
//!   `search_status_tags` up to date, then matches `q.term` against the
//!   now-caught-up index via [`hashtag_repository::match_hashtags`]
//!   (Requirements 5.1, 5.3, 5.5), mapping each returned [`model::TagView`]
//!   down to the bare [`model::TagMatch`] this port's return type requires
//!   (Requirement 7.2). See [`pg_backend`]'s own doc comment (the
//!   "`search_hashtags`: on-demand catch-up then read-index match" section)
//!   for the full reasoning.
//!
//! - Task 4.1 (`Boundary: TagSerializer, SearchResultSerializer`): the
//!   Tag / SearchResults JSON rendering boundary —
//!   [`tag_serializer::TagSerializer::build_tag`] renders a [`model::TagView`]
//!   into the Tag JSON contract (`name`/`url`/`history`), making
//!   `TagView::url`'s domain-relative path absolute from a configured
//!   server `domain` (Requirement 1.3); and
//!   [`result_serializer::SearchResultSerializer::build_search_results`]
//!   assembles the SearchResults envelope (`accounts`/`statuses`/
//!   `hashtags`) from already-rendered JSON, embedding upstream
//!   Account/Status JSON verbatim (never re-serialized, Requirement 1.2)
//!   and always producing `[]` — never `null` — for an empty type
//!   (Requirement 1.4). Both register goldens with `crate::contract::
//!   assert_golden` (Requirement 1.5). See each module's own doc comment
//!   for the full reasoning. `SearchHydrator` (task 4.2, concretizing
//!   `SearchMatches` identifiers into the JSON these two modules consume)
//!   is not implemented yet.
//!
//! - Task 4.2 (`Boundary: SearchHydrator`): the identifier-to-JSON
//!   concretization boundary — [`hydrator::SearchHydrator::hydrate_accounts`]
//!   (dedup + `following`-scoped filter + accounts-and-instance Account
//!   serialization), [`hydrator::SearchHydrator::hydrate_statuses`]
//!   (statuses-core visible-status resolution via `crate::statuses::
//!   visibility::is_visible` + Status serialization, truncated to the
//!   requested `limit` after visibility filtering), and
//!   [`hydrator::SearchHydrator::hydrate_hashtags`] (`TagMatch` ->
//!   [`model::TagView`] re-resolution -> [`tag_serializer::TagSerializer::
//!   build_tag`]). See [`hydrator`]'s own doc comment for the full
//!   reasoning. `RemoteResolver` (task 4.3), `SearchService`/`SearchEndpoint`
//!   (task 5.x), and `SearchModule` wiring are not implemented yet.
//!
//! - Task 4.3 (`Boundary: RemoteResolver`): the `acct:`/URL remote-
//!   resolution orchestration boundary —
//!   [`remote_resolver::RemoteResolver::resolve_remote`] classifies a
//!   [`model::ParsedQuery::Acct`]/[`model::ParsedQuery::Url`], drives
//!   outbound WebFinger (acct) or federation fetch + JSON-LD safe expansion
//!   (URL), and delegates to accounts-and-instance's `RemoteAccountFetcher`/
//!   statuses-core's `StatusIngestService` for normalization/ingestion,
//!   normalizing every failure to [`remote_resolver::Resolved::None`]
//!   rather than an `Err` (Requirements 6.1, 6.2, 6.4). See
//!   [`remote_resolver`]'s own doc comment for the full reasoning.
//!   `SearchService`/`SearchEndpoint` (task 5.x) and `SearchModule` wiring
//!   are not implemented yet.
//!
//! - Task 5.1 (`Boundary: SearchService`): the unified-search business
//!   aggregation — [`service::SearchService::search`] wires together
//!   [`query_parser::parse_query`] (empty `q` -> 422), `type` dispatch
//!   (unrequested types return `[]`, Requirement 2.2), gated remote
//!   resolution (`resolve=true` and `Acct`/`Url` only, Requirements 6.1,
//!   6.3, 6.5), [`ports::SearchBackend`] matching (engine-agnostic —
//!   `SearchService` is generic over `B: SearchBackend`, Requirement 7.1),
//!   [`hydrator::SearchHydrator`] concretization, and
//!   [`result_serializer::SearchResultSerializer`] assembly, logging a
//!   structured diagnostic (query kind / target kind / failure point, no
//!   secrets) on any stage's failure (Requirement 9.5). See [`service`]'s
//!   own doc comment for the full reasoning. `SearchEndpoint` (task 5.2)
//!   and `SearchModule`/`AppState`/bootstrap/router wiring (task 5.3) are
//!   not implemented yet.
//!
//! - Task 5.2 (`Boundary: SearchEndpoint`): the `GET /api/v2/search` HTTP
//!   handler — [`endpoint::search`] requires Bearer + `read:search`
//!   (Requirement 9.1), extracts `q`/`type`/`resolve`/`following`/
//!   `account_id`/`limit`/`offset`/`exclude_unreviewed` into a
//!   [`model::SearchParams`] (`limit`/`offset` rounded per the
//!   api-foundation convention, Requirements 2.5, 9.3), and delegates the
//!   entire pipeline to [`service::SearchService::search`] — never
//!   reimplementing it. Every failure renders through `AppError`'s already-
//!   wired Mastodon-compatible `IntoResponse` impl (Requirement 9.2). See
//!   [`endpoint`]'s own doc comment for the full reasoning and this task's
//!   own documented CONCERNs. `SearchModule`/`AppState`/bootstrap/router
//!   wiring (task 5.3) is not implemented yet — this handler is not mounted
//!   onto the live application.
//!
//! - Task 5.3 (`Boundary: SearchModule, Bootstrap, AppState, Server`): this
//!   file's own composition point — [`build_search_module`] wires the
//!   default, extension-free [`pg_backend::PgSearchBackend`] as this
//!   instance's one [`ports::SearchBackend`] (Requirement 7.3), assembles a
//!   production [`remote_resolver::RemoteResolver`] (`ReqwestFederationHttpClient`
//!   / `crate::statuses::ProdRemoteActorResolver` / `crate::actor::ActorDirectory`),
//!   a [`hydrator::SearchHydrator`], and a [`result_serializer::SearchResultSerializer`]
//!   into one [`service::SearchService`], bundled as [`SearchModule`] —
//!   which `src/state.rs` now stores and `src/server.rs`'s `FromRef<AppState>
//!   for endpoint::SearchEndpointsState<..>` bridge derives the mounted
//!   `GET /api/v2/search` endpoint's own state from, mounted at the same
//!   Bearer/scope/error/rate-limit cross-cutting application point every
//!   other authenticated Mastodon-API endpoint in this crate already is
//!   (Requirement 9.4). See this file's own "module wiring" section below
//!   for [`SearchModule`]/[`build_search_module`]'s full doc comments, and
//!   `src/state.rs`/`src/bootstrap.rs`/`src/server.rs`'s own doc comments at
//!   each of their call sites. `HashtagIndexer` (task 2.2) needs no separate
//!   initialization step here beyond `PgSearchBackend::new`'s own `runtime`
//!   argument — see [`build_search_module`]'s own doc comment,
//!   "`HashtagIndexer`: no separate initialization step", for why.
//!
//! ## Where this module's tests live
//! There is no `search/tests.rs`. Both of this file's own tests prove
//! [`build_search_module`]'s wiring through the *real*, fully-assembled
//! [`crate::server::build_router`], which requires a running instance
//! (`crate::test_harness`'s own `spawn_test_app`), so they live in
//! `tests/search_module_it.rs` — placed there by
//! `.kiro/specs/test-placement-migration` task 3.3 so that steering
//! `structure.md`'s test layout rule ("DB込みの実起動インスタンスを要する検証
//! は `tests/` 直下の `*_it.rs` に置く") holds in fact and not only on paper.
//! A module with no `tests.rs` therefore means "no pure unit test applies
//! here", not "untested".

pub mod endpoint;
pub mod hashtag_indexer;
pub mod hashtag_repository;
pub mod hydrator;
pub mod model;
pub mod pg_backend;
pub mod ports;
pub mod query_parser;
pub mod remote_resolver;
pub mod result_serializer;
pub mod service;
pub mod tag_serializer;

pub use endpoint::{SEARCH_PATH, SearchEndpointsState, search};
pub use hydrator::SearchHydrator;
pub use model::{
    ParsedQuery, SearchMatches, SearchParams, SearchType, TagHistoryEntry, TagMatch, TagView,
};
pub use pg_backend::PgSearchBackend;
pub use ports::{AccountQuery, HashtagQuery, SearchBackend, StatusQuery, StubSearchBackend};
pub use query_parser::parse_query;
pub use remote_resolver::{RemoteResolver, Resolved};
pub use result_serializer::SearchResultSerializer;
pub use service::SearchService;
pub use tag_serializer::TagSerializer;

// ---- Task 5.3 (Boundary: SearchModule, Bootstrap, AppState, Server):
// module wiring -----------------------------------------------------------

use std::sync::Arc;

use sqlx::PgPool;

use crate::accounts::account_service::AccountService;
use crate::accounts::ports::AccountPortsRegistry;
use crate::accounts::{DEFAULT_REMOTE_ACCOUNT_CACHE_TTL, RemoteAccountFetcher};
use crate::actor::ActorDirectory;
use crate::federation::signatures::ReqwestFederationHttpClient;
use crate::media::LocalFsStore;
use crate::runtime::RuntimeContext;
use crate::statuses::ProdRemoteActorResolver;
use crate::statuses::ingest_service::StatusIngestService;
use crate::statuses::visibility::RelationshipQueryRegistry;

/// This instance's one concrete [`SearchService`] instantiation (design.md's
/// "engine-agnostic by construction" `SearchBackend` seam, Requirement 7.3,
/// resolved here to exactly one concrete `B`): the default, extension-free
/// [`PgSearchBackend`] plus the production `H: FederationHttpClient, R:
/// RemoteActorResolver, M: LocalMentionResolver` triple every other
/// remote-resolution consumer in this crate's own bootstrap wiring already
/// settles on (`ReqwestFederationHttpClient` /
/// [`crate::statuses::ProdRemoteActorResolver`] /
/// [`crate::actor::ActorDirectory`]) — mirrors
/// `crate::accounts::AccountsModule`'s own `Arc<AccountService<LocalFsStore,
/// ReqwestFederationHttpClient>>` field's identical "concretize every
/// generic parameter at the composition root, so the module struct itself
/// stays a plain, non-generic type" shape. See [`SearchModule`]'s own doc
/// comment ("Why a plain, non-generic struct, not `AppState<B, H, R, M>`")
/// for the full reasoning this resolves.
type ProdSearchService = SearchService<
    PgSearchBackend,
    ReqwestFederationHttpClient,
    ProdRemoteActorResolver,
    ActorDirectory,
>;

/// The search module bundle (design.md's exact `SearchModule（wiring）`
/// component; task 5.3, Requirements 7.3, 7.4, 8.1, 8.4, 9.4): the shared
/// [`SearchService`] handle `src/server.rs`'s `FromRef<AppState> for
/// endpoint::SearchEndpointsState<..>` bridge derives the mounted `GET
/// /api/v2/search` endpoint's own state from. Built by
/// [`build_search_module`].
///
/// ## Why a plain, non-generic struct, not `AppState<B, H, R, M>`
/// [`service::SearchService`] (task 5.1) is generic over `B: SearchBackend,
/// H: FederationHttpClient, R: RemoteActorResolver, M: LocalMentionResolver`
/// because it must stay engine-agnostic *at the type level* for its own
/// tests (`search::service::tests` swaps in `StubSearchBackend`/
/// `MockFederationHttpClient`/`FakeRemoteActors` without this crate's own
/// `SearchService` code ever naming any of them — see `service.rs`'s own
/// doc comment, "Engine-agnostic by construction"). That is a different
/// question from what `AppState`/`SearchModule` need: a running instance of
/// this crate only ever wires up *one* concrete choice for every one of
/// those four parameters (this file, exactly once) — Requirement 7.3's own
/// "既定として PgSearchBackend を SearchBackend として配線" already commits
/// to a single default at this composition-root layer, and neither
/// `AppState` nor `Router<AppState>` can be generic (axum's `State<S>`
/// extractor and `FromRef<AppState>` both need one concrete `AppState`
/// type). This module therefore concretizes every one of `SearchService`'s
/// four generic parameters exactly once, right here — the one place
/// task 5.1's own dispatch brief already named as `SearchModule`'s job
/// ("Constructor: `bundle, don't build`" in `service.rs`'s doc comment) —
/// via the [`ProdSearchService`] alias, mirroring
/// `crate::accounts::AccountsModule`'s/`crate::statuses::StatusesModule`'s
/// own identical "wrap an already-fully-concretized generic service type in
/// a plain, non-generic module struct" precedent (confirmed by reading both
/// modules' own struct definitions: neither `AccountsModule` nor
/// `StatusesModule` itself carries a generic parameter, even though the
/// `AccountService`/`StatusService` types they hold internally are generic
/// declarations). No `dyn`/boxing is needed anywhere in this resolution:
/// `SearchBackend` cannot be `dyn`-safe at all (its own `async fn` methods,
/// `ports.rs`'s own doc comment, "`async fn` in trait, not boxed futures"),
/// and this crate's established precedent for that exact situation is
/// "concretize at the composition root", not "erase behind `dyn`" — which
/// this module follows rather than deviating into a boxed-trait-object
/// design no sibling module uses.
pub struct SearchModule {
    search_service: Arc<ProdSearchService>,
}

impl SearchModule {
    /// The shared `SearchService` handle (an `Arc` clone, cheap — mirrors
    /// `crate::accounts::AccountsModule::service`'s/
    /// `crate::timelines::TimelinesModule::service`'s identical shape):
    /// `src/server.rs`'s `FromRef<AppState> for
    /// endpoint::SearchEndpointsState<..>` bridge derives the mounted `GET
    /// /api/v2/search` endpoint's own state from this handle, rather than
    /// each request/router construction rebuilding its own `SearchService`.
    pub fn search_service(&self) -> Arc<ProdSearchService> {
        Arc::clone(&self.search_service)
    }
}

/// Assembles the [`SearchModule`] bundle (task 5.3, Requirements 7.3, 7.4,
/// 8.1, 8.4, 9.4): builds this instance's one default
/// [`pg_backend::PgSearchBackend`] (`pool`/`runtime` only — no `CREATE
/// EXTENSION`-requiring PostgreSQL extension anywhere in its own SQL, see
/// `pg_backend.rs`'s own doc comment and `migrations/0013_search.sql`'s own
/// "Extension-free default" section, Requirement 8.1), a production
/// [`remote_resolver::RemoteResolver`] (its own `H`/`R`/`M` triple, below),
/// a [`hydrator::SearchHydrator`], and a
/// [`result_serializer::SearchResultSerializer`], composing all four into
/// one [`ProdSearchService`] via [`service::SearchService::new`] ("bundle,
/// don't build" — this function never reimplements any of task 5.1's own
/// pipeline logic).
///
/// ## `H`/`R`/`M`: this module's own, separately-constructed instances (not
/// shared `Arc`s with `statuses`/`social_graph`'s own resolvers)
/// Mirrors `src/bootstrap.rs`'s own established "each spec that needs a
/// `RemoteAccountFetcher`/`ReqwestFederationHttpClient` builds its own
/// separate instance rather than sharing one `Arc` across specs" convention
/// (confirmed at that file's own `statuses_remote_actor_fetcher`/
/// `social_graph_remote_actor_fetcher`/`accounts_module`'s own client
/// construction call sites, each commented "never the same `Arc`
/// `<other module>`'s own client uses"): `http_client` (`Arc<
/// ReqwestFederationHttpClient>`) and the `account_fetcher` it feeds
/// (`Arc<RemoteAccountFetcher<ReqwestFederationHttpClient>>`, this spec's
/// own remote-account resolution cache with its own independent TTL
/// lifetime) are both freshly built here, never handles borrowed from
/// `crate::accounts::AccountsModule`/`crate::statuses::StatusesModule`. The
/// one exception, `directory: Arc<ActorDirectory>`, *is* the same shared
/// handle `src/bootstrap.rs` already threads into every other consumer
/// (`actor_module.directory()`) — `ActorDirectory` is a stateless read-only
/// query wrapper around `pool` (no per-consumer cache to keep separate the
/// way `RemoteAccountFetcher`'s TTL cache needs to be), exactly the same
/// choice `src/bootstrap.rs`'s own `ProdRemoteActorResolver::new` call sites
/// already make for their own `directory` argument.
///
/// `R = ProdRemoteActorResolver` reuses this same freshly-built
/// `account_fetcher` as its own genuinely-remote fallback (its `fetcher`
/// field is exactly `Arc<RemoteAccountFetcher<ReqwestFederationHttpClient>>`,
/// the identical concrete type `RemoteResolver`'s own `account_fetcher`
/// needs) — one cache, not two, for the one instance's worth of
/// remote-account resolution this module performs.
///
/// `M = ActorDirectory`, held by value (not `Arc`) inside
/// [`ingest_service::StatusIngestService`]'s own `mentions` field — a fresh
/// `ActorDirectory::new(pool.clone())`, mirroring
/// `crate::statuses::register_downstream_handlers`'s own identical `let
/// mentions = ActorDirectory::new(pool.clone());` precedent for the exact
/// same by-value `LocalMentionResolver` slot.
///
/// ## `HashtagIndexer`: no separate initialization step
/// This task's own text asks for "`HashtagIndexer` を初期化", but
/// [`hashtag_indexer`] (task 2.2) exposes no `HashtagIndexer` struct to
/// construct at all — only the free function
/// [`hashtag_indexer::catch_up_from_watermark`], which
/// [`pg_backend::PgSearchBackend::search_hashtags`] (task 3.2, already
/// reviewed/committed) already calls itself, on demand, at the front of
/// every hashtag-search request (design.md's own flow: catch-up happens
/// "検索直前", inside the request, never as a separate background job at
/// boot — confirmed by re-reading `hashtag_indexer.rs`'s and
/// `pg_backend.rs`'s own doc comments). `PgSearchBackend::new` already takes
/// exactly the `pool`/`runtime` pair that on-demand catch-up call needs
/// (`pg_backend.rs`: "using `runtime`... for `search_hashtags`'s on-demand
/// `catch_up_from_watermark` scan"). "Initializing `HashtagIndexer`" for
/// this task's purposes therefore reduces to building `PgSearchBackend`
/// itself with a real `pool`/`runtime` — done below — not a separate call
/// this function would otherwise be missing.
#[allow(clippy::too_many_arguments)]
pub fn build_search_module(
    pool: PgPool,
    runtime: RuntimeContext,
    domain: impl Into<String>,
    directory: Arc<ActorDirectory>,
    accounts: Arc<AccountService<LocalFsStore, ReqwestFederationHttpClient>>,
    account_ports: AccountPortsRegistry,
    media_store: LocalFsStore,
    relationship_query: RelationshipQueryRegistry,
) -> SearchModule {
    let domain = domain.into();

    let http_client = Arc::new(ReqwestFederationHttpClient::new());
    let account_fetcher = Arc::new(RemoteAccountFetcher::new(
        pool.clone(),
        Arc::clone(&http_client),
        runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    ));
    let remote_actor_resolver = Arc::new(ProdRemoteActorResolver::new(
        domain.clone(),
        directory,
        Arc::clone(&account_fetcher),
    ));
    let mentions = ActorDirectory::new(pool.clone());
    let status_ingest = Arc::new(StatusIngestService::new(
        pool.clone(),
        Arc::clone(&http_client),
        runtime.clone(),
        Arc::clone(&remote_actor_resolver),
        domain.clone(),
        mentions,
    ));
    let remote_resolver = RemoteResolver::new(http_client, account_fetcher, status_ingest);

    let hydrator = SearchHydrator::new(
        pool.clone(),
        accounts,
        account_ports,
        media_store,
        relationship_query,
        runtime.clone(),
        domain,
    );

    let backend = PgSearchBackend::new(pool, runtime);

    let search_service = Arc::new(SearchService::new(
        backend,
        remote_resolver,
        hydrator,
        SearchResultSerializer::new(),
    ));

    SearchModule { search_service }
}
