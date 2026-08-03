//! Canonical ActivityPub JSON-LD serialization (design.md
//! "jsonld/serialize.rs": "正規 ActivityPub ドキュメント直列化（@context
//! 付与）"; Requirement 9.1).
//!
//! Owns exactly [`serialize`]: stamping the ActivityPub `@context`
//! (delegated to [`super::context`]) onto the document a caller hands in,
//! then encoding the result as UTF-8 JSON bytes.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use serde_json::Value;

use super::context::with_activitystreams_context;
use crate::error::AppError;

/// Serializes `doc` as a canonical ActivityPub JSON-LD document: stamps
/// [`super::context::ACTIVITYSTREAMS_CONTEXT`] onto its top level
/// (Requirement 9.1, via [`with_activitystreams_context`]) and encodes the
/// result as UTF-8 JSON bytes.
///
/// `doc` must be a JSON object at its top level — see
/// [`with_activitystreams_context`]'s own documentation for why — and that
/// same call is this function's only failure path in practice, since
/// encoding an already-built [`serde_json::Value`] to bytes cannot itself
/// fail for any value this function constructs.
pub fn serialize(doc: &Value) -> Result<Vec<u8>, AppError> {
    let with_context = with_activitystreams_context(doc)?;

    serde_json::to_vec(&with_context)
        .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))
}
