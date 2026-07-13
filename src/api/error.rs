//! Mastodon-compatible error response body (api-foundation `MastodonError`
//! boundary, task 6.1).
//!
//! Scope: this module owns exactly one thing — turning a core-runtime
//! [`AppError`] into a Mastodon-compatible JSON error body via
//! [`mastodon_error_body`], a genuine alternative renderer for
//! [`AppError::into_response_with`]'s extension point (Requirement 7.4). It
//! does not define a new error type (the task text's "新たなエラー型は作らない");
//! [`AppError`]'s existing `kind`/`status`/`public_message`/`source` fields
//! are the only inputs.
//!
//! Wiring `mastodon_error_body` into the live router — so every API
//! response actually renders through it, not just `AppError`'s default
//! `{"error": public_message}` shape — is task 7.1's job
//! (`_Boundary: ApiModule wiring`, `_Depends: ..., 6.1, ...` per
//! `tasks.md`). This module deliberately stops at the renderer function and
//! does not touch `src/server.rs`/`src/bootstrap.rs`/`src/state.rs`.
//!
//! ## Design note: reconciling design.md's sketch with core-runtime's actual
//! `AppError`
//!
//! design.md's illustrative Service Interface for this component sketches
//! `mastodon_status_for(kind: &AppErrorKind) -> StatusCode`, implying a
//! status is *derived* from an error-kind enum. core-runtime's actual
//! `AppError` (task 1.x, reviewed) has no such `AppErrorKind` — call sites
//! choose a concrete `StatusCode` directly via `AppError::client`/`server`,
//! and `AppError.status` already holds it. So the "対応表"
//! (status-correspondence table) this task provides instead runs the other
//! direction: [`mastodon_error_label`] maps an already-chosen `StatusCode`
//! to the canonical Mastodon-compatible `error` label for the categories
//! Requirement 7.3 enumerates (422/401/403/404/429), which is the shape
//! that composes with the extension point that actually exists.
//!
//! ## `error` / `error_description` split (Requirements 7.1, 7.2, 7.3)
//!
//! `AppError` carries a single caller-authored `public_message` string, not
//! separate short-code and long-description fields. This module reconciles
//! that with Mastodon's `error` (+ optional `error_description`) shape as
//! follows:
//! - For the five statuses Requirement 7.3 enumerates (422/401/403/404/429),
//!   `error` is the canonical Mastodon-compatible label from
//!   [`mastodon_error_label`], and `public_message` — the call site's
//!   specific detail — becomes `error_description` (Requirement 7.2's
//!   "追加説明を伴う場合": the call site's message *is* that additional
//!   explanation, layered under the canonical label).
//! - For any other 4xx status this task's requirement doesn't enumerate,
//!   there is no canonical label to defer to, so `public_message` itself
//!   becomes `error` (still satisfying Requirement 7.1: every response
//!   includes `error`), and `error_description` is omitted.
//! - For `ErrorKind::Server` (5xx), `error` is always
//!   [`GENERIC_SERVER_MESSAGE`] (exactly what `public_message` already
//!   holds by construction) and `error_description` is always omitted —
//!   there is no second, more-detailed string to surface, and `source` is
//!   never read here (Requirement 7.5).

#[cfg(test)]
mod tests;

use crate::error::{AppError, ErrorKind, GENERIC_SERVER_MESSAGE};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Mastodon-compatible error JSON body shape: `{"error": ...}`, with
/// `error_description` present only when there is additional explanation
/// beyond the top-level `error` (Requirements 7.1, 7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MastodonError {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
}

/// Status → canonical Mastodon-compatible `error` label table (Requirement
/// 7.3): input validation (422), authentication failure (401), insufficient
/// permission (403), not-found (404), and rate-limit exceeded (429).
///
/// Returns `None` for any status outside this curated set, so callers can
/// fall back to a call-site-authored message instead of inventing a label
/// this requirement doesn't specify. Rate-limit's actual `429` response
/// shape (headers, retry timing) belongs to the `RateLimit` boundary (task
/// 6.3); this table only supplies the label a `RateLimit`-produced
/// `AppError` would render through this module's renderer.
pub fn mastodon_error_label(status: StatusCode) -> Option<&'static str> {
    match status {
        StatusCode::UNPROCESSABLE_ENTITY => Some("Validation failed"),
        StatusCode::UNAUTHORIZED => Some("The access token is invalid"),
        StatusCode::FORBIDDEN => Some("This action is outside the authorized scopes"),
        StatusCode::NOT_FOUND => Some("Record not found"),
        StatusCode::TOO_MANY_REQUESTS => Some("Too many requests"),
        _ => None,
    }
}

/// Returns `message` as `Some(String)` unless it is empty/whitespace-only,
/// in which case there is no additional explanation to surface
/// (Requirement 7.2's "追加説明を伴う場合" is conditional — an empty message
/// carries none).
fn as_description(message: &str) -> Option<String> {
    if message.trim().is_empty() {
        None
    } else {
        Some(message.to_string())
    }
}

/// Builds the [`MastodonError`] body for `error`. Pure function (no I/O),
/// kept separate from [`mastodon_error_body`] so the body shape itself is
/// directly unit-testable without going through HTTP response plumbing.
fn mastodon_error_for(error: &AppError) -> MastodonError {
    if error.kind == ErrorKind::Server {
        // Requirement 7.5 (core-runtime's own guarantee, preserved here):
        // never read `source`, and `public_message` for a `Server` error is
        // always `GENERIC_SERVER_MESSAGE` by construction — never anything
        // that could vary with internal detail.
        return MastodonError {
            error: GENERIC_SERVER_MESSAGE.to_string(),
            error_description: None,
        };
    }

    match mastodon_error_label(error.status) {
        Some(label) => MastodonError {
            error: label.to_string(),
            error_description: as_description(&error.public_message),
        },
        None => MastodonError {
            error: error.public_message.clone(),
            error_description: None,
        },
    }
}

/// Mastodon-compatible body renderer (Requirement 7.4): a genuine
/// alternative to `crate::error::default_response` for
/// [`AppError::into_response_with`]'s extension point.
///
/// Usage: `app_error.into_response_with(mastodon_error_body)`. Wiring this
/// in as the router-wide default (so plain `?`-propagated `AppError`s from
/// every handler render through it) is task 7.1's job, not this module's.
pub fn mastodon_error_body(error: &AppError) -> Response {
    let body = mastodon_error_for(error);
    (error.status, Json(body)).into_response()
}
