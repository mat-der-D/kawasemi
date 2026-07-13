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

pub mod error;
