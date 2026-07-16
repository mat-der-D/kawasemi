//! Safe interpretation of received ActivityPub JSON-LD documents
//! (design.md "jsonld/parse.rs": "受信 JSON-LD の安全展開・未知プロパティ
//! 処理・必須プロパティ検証"; Requirements 9.2, 9.3).
//!
//! Owns exactly [`parse_activity`] and its [`ParsedActivity`] result type.
//! Deliberately does *not* deserialize into a strict, field-enumerating
//! struct: parsing into a generic [`serde_json::Value`] and reading only the
//! two properties this codec actually needs (`type`/`id`) means any
//! property this codec does not recognize is simply never inspected, so it
//! can never cause a parse failure (Requirement 9.2) — it is preserved
//! as-is on [`ParsedActivity::raw`] for downstream business processing to
//! read. Only `type`/`id` absence is treated as a validation error
//! (Requirement 9.3); every other shape question is left to downstream
//! Activity-specific processing, which this spec does not own (see
//! requirements.md's Boundary Context: "具体 Activity 種別の業務処理…は
//! 本 spec の範囲外").

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use serde_json::Value;

use crate::error::AppError;

/// A safely-interpreted inbound ActivityPub document (design.md's exact
/// `JsonLdCodec` interface type): the two properties this codec validates
/// as required (`id`, `type` — Requirement 9.3), plus the complete original
/// document (`raw`) so unknown properties (Requirement 9.2) and any other
/// Activity-specific field remain available to downstream business
/// processing this spec does not own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedActivity {
    pub id: String,
    pub activity_type: String,
    pub raw: Value,
}

/// Parses `body` as a JSON-LD ActivityPub document, returning a
/// [`ParsedActivity`].
///
/// Unknown properties never cause a failure (Requirement 9.2): `body` is
/// parsed into a generic [`serde_json::Value`] and every property beyond
/// `type`/`id` is carried through unexamined on [`ParsedActivity::raw`].
///
/// Fails with a client [`AppError`] when:
/// - `body` is not syntactically valid JSON (`400 Bad Request`).
/// - the top-level JSON value is not an object, so it cannot carry `type`/
///   `id` members at all (`422 Unprocessable Entity`).
/// - the required `type` or `id` property (Requirement 9.3) is missing, not
///   a string, or an empty string (`422 Unprocessable Entity`).
pub fn parse_activity(body: &[u8]) -> Result<ParsedActivity, AppError> {
    let raw: Value = serde_json::from_slice(body).map_err(|source| {
        AppError::client(
            StatusCode::BAD_REQUEST,
            format!("malformed JSON-LD body: {source}"),
        )
    })?;

    let Value::Object(map) = &raw else {
        return Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ActivityPub document must be a JSON object",
        ));
    };

    let id = required_string_property(map, "id")?;
    let activity_type = required_string_property(map, "type")?;

    Ok(ParsedActivity {
        id,
        activity_type,
        raw,
    })
}

/// Reads `property` off `map` as a non-empty string, or returns a
/// `422 Unprocessable Entity` [`AppError`] naming the missing property
/// (Requirement 9.3). Shared by `id`/`type` extraction in [`parse_activity`]
/// so both required properties are validated identically.
fn required_string_property(
    map: &serde_json::Map<String, Value>,
    property: &str,
) -> Result<String, AppError> {
    map.get(property)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("ActivityPub document missing required '{property}' property"),
            )
        })
}
