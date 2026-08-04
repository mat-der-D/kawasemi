//! The standard `sqlx::Error` -> [`AppError`] conversion.
//!
//! Twenty-three repository modules each defined this same function, under
//! four different names (`map_server_error`, `map_query_error`,
//! `map_insert_error`, `map_tx_error`) that suggested a distinction none of
//! them actually made. The names are gone; the ones that genuinely differ
//! stay where they are.
//!
//! ## What deliberately does not live here
//! Three mappers inspect the failure before classifying it, turning a
//! specific unique-violation into a caller-facing 4xx rather than a 5xx:
//! `actor::repository`'s handle collision, `federation::outbound::queue`'s
//! delivery-job dedup, and `statuses::status_repository`'s status-URI
//! collision. Each knows a constraint name that belongs to its own schema,
//! so each stays private to its module. Folding them in here would either
//! drag three schemas' constraint names into a cross-cutting module or —
//! worse — quietly reclassify a documented 4xx as a 500.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;

use crate::error::AppError;

/// Converts a database failure into a 500 whose diagnostic detail is
/// logged, never returned.
///
/// This is the right mapper whenever the query has no failure mode the
/// caller could have caused or could act on. When a *specific* database
/// error does map to a caller-facing 4xx (a unique violation the caller
/// could avoid), inspect the error in the owning repository first and fall
/// back to this only for the rest — see this module's doc comment.
pub fn map_server_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}
