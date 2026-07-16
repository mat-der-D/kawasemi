//! ActivityPub JSON-LD `@context` constant and injection (design.md
//! "jsonld/context.rs": "ActivityPub @context 定数・付与"; Requirement 9.1).
//!
//! Owns exactly the ActivityPub JSON-LD context value and the pure mutation
//! that stamps it onto an outgoing document's top level. [`super::serialize`]
//! is this module's only intended caller — this module does not itself
//! encode anything to bytes.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use serde_json::{Map, Value};

use crate::error::AppError;

/// The ActivityPub JSON-LD context URI (Requirement 9.1), applied to every
/// document [`super::serialize::serialize`] emits. ActivityPub's own spec —
/// and every existing implementation — uses this single-string form rather
/// than an array/object context, so this codec follows that convention
/// rather than inventing a richer shape nothing in this codebase yet needs.
pub const ACTIVITYSTREAMS_CONTEXT: &str = "https://www.w3.org/ns/activitystreams";

/// Returns a copy of `doc` with its top-level `@context` member set to
/// [`ACTIVITYSTREAMS_CONTEXT`] (Requirement 9.1), overwriting any
/// `@context` the caller-supplied `doc` already carried — every document
/// this codec serializes ends up carrying the ActivityPub context,
/// regardless of what came in.
///
/// `doc` must be a JSON object at its top level: `@context` is, by
/// definition, a top-level object member of a JSON-LD document, so a
/// non-object `doc` cannot be given one and is treated as a caller error
/// rather than something to silently coerce.
pub fn with_activitystreams_context(doc: &Value) -> Result<Value, AppError> {
    let Value::Object(map) = doc else {
        return Err(AppError::client(
            StatusCode::BAD_REQUEST,
            "ActivityPub document must be a JSON object to attach @context to",
        ));
    };

    let mut with_context: Map<String, Value> = map.clone();
    with_context.insert(
        "@context".to_string(),
        Value::String(ACTIVITYSTREAMS_CONTEXT.to_string()),
    );

    Ok(Value::Object(with_context))
}
