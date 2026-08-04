//! Query-parameter interpretation shared by every list endpoint.
//!
//! Every endpoint module used to carry its own private copy of these three
//! functions — byte-identical copies, each with a doc comment explaining
//! that it deliberately mirrored the others. That arrangement makes the
//! wire contract a convention rather than a fact: nothing prevents the
//! sixth copy from accepting `"yes"`, or the seventh from defaulting
//! `limit` to something, and no test would notice because each copy is only
//! ever exercised through its own module's endpoints.
//!
//! These are deliberately *not* extractors or typed newtypes. Endpoints
//! take these fields as `Option<String>` off `axum::extract::Query` so a
//! malformed value becomes a Mastodon-shaped `422` instead of axum's own
//! `QueryRejection`; parsing therefore happens inside the handler, and what
//! is shared is the parse itself.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;

use crate::error::AppError;

/// Parses `limit`'s raw wire value, if present, into a `u32`.
///
/// Absent is `None` rather than a default: choosing the page size for an
/// omitted `limit` is each endpoint's own decision, not this function's.
pub fn parse_optional_limit(raw: Option<&str>) -> Result<Option<u32>, AppError> {
    match raw {
        None => Ok(None),
        Some(value) => value.parse::<u32>().map(Some).map_err(|_| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("limit must be a non-negative integer, got {value:?}"),
            )
        }),
    }
}

/// Parses a loosely-typed boolean query value, accepting the `"true"`/`"1"`
/// and `"false"`/`"0"` spellings Mastodon clients actually send.
///
/// `field_name` appears in the rejection message, so the caller learns
/// *which* parameter it got wrong rather than just that something was
/// unparseable.
pub fn parse_loose_bool(field_name: &str, raw: &str) -> Result<bool, AppError> {
    match raw {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{field_name} must be \"true\"/\"1\" or \"false\"/\"0\", got {other:?}"),
        )),
    }
}

/// Parses an optional loosely-typed boolean query field, defaulting to
/// `false` when absent — Mastodon's own convention for filter flags such as
/// `pinned`/`only_media`/`exclude_replies`: omitting the filter means "do
/// not apply it", never "apply its inverse".
pub fn parse_optional_bool_query(field_name: &str, raw: Option<&str>) -> Result<bool, AppError> {
    match raw {
        None => Ok(false),
        Some(value) => parse_loose_bool(field_name, value),
    }
}
