//! API cross-cutting module (api-foundation spec).
//!
//! Scope so far:
//! - Task 6.1 (`Boundary: MastodonError`): a Mastodon-compatible error
//!   response body renderer that plugs into core-runtime's
//!   [`crate::error::AppError::into_response_with`] extension point — see
//!   [`error`]. This module only builds and unit-tests the renderer itself;
//!   wiring it into the live router (so every API response actually uses
//!   it) is task 7.1's job (`_Boundary: ApiModule wiring`), which this task
//!   does not reach into (`src/server.rs`, `src/bootstrap.rs`,
//!   `src/state.rs` are out of scope here).
//! - Task 6.2 (`Boundary: Pagination`): a standalone cursor-pagination
//!   toolkit — `max_id`/`since_id`/`min_id`/`limit` interpretation, a
//!   category-swappable `Cursor` abstraction, a `Page<T>` representation,
//!   and forwarded-host/scheme-aware `Link` header generation — see
//!   [`pagination`]. No endpoint or router in this spec consumes it (this
//!   spec has no list endpoints); downstream feature specs call it
//!   directly from their own list endpoints.

pub mod error;
pub mod pagination;
