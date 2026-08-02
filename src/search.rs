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
//!   No concrete standard-PostgreSQL implementation (`PgSearchBackend`)
//!   exists yet — that is task 3.1's job, strictly downstream of this
//!   port definition.
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
//! This file will eventually become the `SearchModule` composition point
//! (design.md's File Structure Plan: "`src/search.rs` — SearchModule
//! 組み立て...と公開・ルータ装着点") once later tasks (2.2-5.3:
//! `hashtag_indexer`, `pg_backend`, `remote_resolver`, `hydrator`,
//! `tag_serializer`, `result_serializer`, `service`, `endpoint`, and their
//! wiring into `AppState`/bootstrap/server) land. For now it declares the
//! `model`/`query_parser`/`ports`/`hashtag_repository` submodules and
//! re-exports `model`'s/`ports`' types — no backend adapter, indexer,
//! resolver, serializer, service, endpoint, or wiring code exists yet.

pub mod hashtag_repository;
pub mod model;
pub mod ports;
pub mod query_parser;

pub use model::{
    ParsedQuery, SearchMatches, SearchParams, SearchType, TagHistoryEntry, TagMatch, TagView,
};
pub use ports::{AccountQuery, HashtagQuery, SearchBackend, StatusQuery, StubSearchBackend};
pub use query_parser::parse_query;
