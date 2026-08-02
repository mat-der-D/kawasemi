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
//!
//! This file will eventually become the `SearchModule` composition point
//! (design.md's File Structure Plan: "`src/search.rs` — SearchModule
//! 組み立て...と公開・ルータ装着点") once later tasks (1.3-5.3:
//! `query_parser`, `ports`, `pg_backend`, `hashtag_repository`,
//! `hashtag_indexer`, `remote_resolver`, `hydrator`, `tag_serializer`,
//! `result_serializer`, `service`, `endpoint`, and their wiring into
//! `AppState`/bootstrap/server) land. For this task (`Boundary: model`), it
//! declares only the `model` submodule and re-exports its types — no
//! parser, port, backend, repository, indexer, resolver, serializer,
//! service, endpoint, or wiring code exists yet.

pub mod model;

pub use model::{
    ParsedQuery, SearchMatches, SearchParams, SearchType, TagHistoryEntry, TagMatch, TagView,
};
