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
//! This file will eventually become the `SearchModule` composition point
//! (design.md's File Structure Plan: "`src/search.rs` — SearchModule
//! 組み立て...と公開・ルータ装着点") once the remaining tasks (5.2-5.3:
//! `endpoint` and its wiring into `AppState`/bootstrap/server) land. For now
//! it declares the `model`/`query_parser`/`ports`/`hashtag_repository`/
//! `hashtag_indexer`/`pg_backend`/`tag_serializer`/`result_serializer`/
//! `hydrator`/`remote_resolver`/`service` submodules and re-exports each
//! one's public types — no endpoint or `AppState`/bootstrap/router wiring
//! code exists yet.

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

pub use hydrator::SearchHydrator;
pub use model::{
    ParsedQuery, SearchMatches, SearchParams, SearchType, TagHistoryEntry, TagMatch, TagView,
};
pub use ports::{AccountQuery, HashtagQuery, SearchBackend, StatusQuery, StubSearchBackend};
pub use query_parser::parse_query;
pub use remote_resolver::{RemoteResolver, Resolved};
pub use result_serializer::SearchResultSerializer;
pub use service::SearchService;
pub use tag_serializer::TagSerializer;
