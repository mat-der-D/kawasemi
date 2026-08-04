//! Unified cross-cutting application error type and HTTP response
//! conversion scaffold (Error boundary).
//!
//! Scope: this module owns the single [`AppError`] type every downstream
//! handler uses to report failures (Requirement 6.1), and the
//! `axum::response::IntoResponse` conversion that turns one into an HTTP
//! status code plus a structured JSON body (Requirement 6.2). It
//! distinguishes user-facing errors ([`ErrorKind::Client`], 4xx) from
//! system errors ([`ErrorKind::Server`], 5xx) (Requirement 6.3): a `Client`
//! error's `public_message` is safe to return verbatim to the caller, while
//! a `Server` error's `source` is logged via `tracing` for diagnosis but
//! never reaches the response body (Requirement 6.4).
//! [`AppError::into_response_with`] is the extension point a downstream
//! spec (e.g. api-foundation) can use to render the body in a different
//! wire format, such as a Mastodon-compatible error envelope, without
//! redefining the conversion end-to-end (Requirement 6.5).
//!
//! ## Why there is a tag alongside the flat fields
//! The three fields above carry no domain vocabulary — deliberately, since
//! every module in the crate depends on this one. That flatness leaves a
//! caller who must branch on *why* something failed with only one tool:
//! comparing `public_message` against a literal, which the compiler cannot
//! check and which detaches silently the moment anyone rewords the message.
//! [`ErrorTag`] closes that hole for the narrow set of failures a caller
//! genuinely has to distinguish, without turning this type into a registry
//! of every domain's error taxonomy. It is caller-side only: no renderer
//! and no log site reads it, so tagging an error cannot change what a
//! client observes.
//!
//! ## Router-wide default is api-foundation's Mastodon-compatible renderer
//! (task 7.1, api-foundation Requirement 7.4)
//! [`AppError`]'s own [`IntoResponse`] impl renders through
//! [`crate::api::error::mastodon_error_body`] rather than [`default_response`]
//! — see that function's own doc comment ("Usage:
//! `app_error.into_response_with(mastodon_error_body)`. Wiring this in as
//! the router-wide default...is task 7.1's job") for why this is the
//! intended way to apply api-foundation's error body cross-cuttingly to
//! every endpoint without editing each handler individually: every handler
//! in this crate reports failures as a plain `AppError` (via `?`), so
//! whichever renderer this one blanket `impl IntoResponse for AppError`
//! delegates to is automatically what the *entire* router (present and
//! future) responds with. `default_response` remains available (and
//! exercised by this module's own tests via `into_response_with`) for a
//! caller that explicitly wants the bare `{"error": public_message}` shape
//! instead.
//!
//! This module does not open or manage the `request_id`-carrying span
//! itself — that is `crate::telemetry`'s `request_span`, wired into the
//! request pipeline by a later task (7.2). A 5xx log emitted from here
//! nests inside whatever request span is active by ordinary `tracing`
//! span/event inheritance once that wiring exists; no explicit correlation
//! id handling belongs in this module.

#[cfg(test)]
mod tests;

use axum::BoxError;
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Distinguishes caller-facing errors from internal system errors
/// (Requirement 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Caller-facing error, mapped to a 4xx status. `public_message` is
    /// safe to return to the caller verbatim.
    Client,
    /// Internal system error, mapped to a 5xx status. Only the generic
    /// [`GENERIC_SERVER_MESSAGE`] reaches the response body; diagnostic
    /// detail lives in `source` and is logged, never returned (Requirement
    /// 6.4).
    Server,
}

/// Generic message carried in every [`ErrorKind::Server`] (5xx) error's
/// response body, regardless of `source` (Requirement 6.4): the body never
/// varies with internal failure detail, so there is no code path by which
/// `source` could leak into it.
pub const GENERIC_SERVER_MESSAGE: &str = "internal server error";

/// Machine-checkable discriminator for "did this fail *for this specific
/// reason*?", for the narrow set of failures a caller must branch on.
///
/// Exists because [`AppError`] is deliberately flat — `kind`/`status`/
/// `public_message` carry no domain vocabulary — which previously left
/// comparing `public_message` against a literal as the only way to ask that
/// question. A literal comparison is invisible to the compiler: rewording
/// the message (a typo fix, a wording pass) silently detaches the branch
/// and the failure surfaces somewhere far away at runtime.
///
/// This is *not* a place to enumerate every domain failure. `AppError` is a
/// cross-cutting type; accumulating per-domain vocabulary here would make
/// every module depend on every other module's error taxonomy. A variant
/// earns its place only when some caller genuinely has to distinguish that
/// one failure from its siblings, and the tag never reaches the wire — it
/// is invisible to both response renderers and to logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorTag {
    /// An actor voted in a poll it had already voted in.
    ///
    /// Distinguishing this from `record_vote`'s other rejections (deadline
    /// passed, choice out of range, single-choice violation) matters
    /// because it is the only one that is *not* necessarily a client
    /// mistake: when a poll's author is local, the vote Activity for a vote
    /// just recorded locally loops back in-process to the inbound handler,
    /// where "already voted" means "already applied" and the correct
    /// outcome is idempotent success, not a 422 for the voter who just
    /// voted.
    DuplicateVote,
}

/// Unified cross-cutting application error (Requirement 6.1).
///
/// Prefer the [`AppError::client`] / [`AppError::server`] constructors over
/// building this struct directly: they keep `kind`, `public_message`, and
/// `source` consistent with each other, so a `Server` error can't
/// accidentally carry a caller-authored `public_message` that leaks
/// internal detail, and a `Client` error can't accidentally carry a hidden
/// `source` that never gets logged anywhere. The fields themselves stay
/// public (per design) for downstream code that needs to inspect an
/// `AppError` it received, e.g. a custom [`AppError::into_response_with`]
/// renderer.
#[derive(Debug)]
pub struct AppError {
    pub kind: ErrorKind,
    pub status: StatusCode,
    /// User-facing message. For `Client` errors this is caller-authored
    /// and returned verbatim. For `Server` errors this is always
    /// [`GENERIC_SERVER_MESSAGE`] (set by the [`AppError::server`]
    /// constructor), never derived from `source`.
    pub public_message: String,
    /// Internal cause. Only ever `Some` for `Server` errors (set by
    /// [`AppError::server`]); logged via `tracing`, never placed in the
    /// response body.
    pub source: Option<BoxError>,
    /// Lets a caller ask "did this fail for *this* reason?" without
    /// comparing `public_message` against a literal — see [`ErrorTag`].
    ///
    /// Purely a caller-side discriminator: no response renderer and no log
    /// site reads it, so setting one cannot change what a client observes.
    /// Only [`AppError::client_tagged`] ever sets it; `Server` errors never
    /// carry one.
    pub tag: Option<ErrorTag>,
}

impl AppError {
    /// Builds a caller-facing ([`ErrorKind::Client`]) error. `status`
    /// should be a 4xx code; `public_message` is returned to the caller
    /// verbatim in the response body (Requirement 6.3).
    pub fn client(status: StatusCode, public_message: impl Into<String>) -> Self {
        AppError {
            kind: ErrorKind::Client,
            status,
            public_message: public_message.into(),
            source: None,
            tag: None,
        }
    }

    /// Same as [`AppError::client`], plus an [`ErrorTag`] so a caller can
    /// recognize this particular failure by type rather than by comparing
    /// `public_message` against a literal.
    ///
    /// Everything a client observes — status, body, headers — is identical
    /// to the untagged constructor's; the tag exists only for in-process
    /// callers and never reaches the wire.
    pub fn client_tagged(
        status: StatusCode,
        public_message: impl Into<String>,
        tag: ErrorTag,
    ) -> Self {
        AppError {
            tag: Some(tag),
            ..AppError::client(status, public_message)
        }
    }

    /// Builds an internal system ([`ErrorKind::Server`]) error. `status`
    /// should be a 5xx code; `source` carries diagnostic detail for logging
    /// only. The response body always carries [`GENERIC_SERVER_MESSAGE`]
    /// instead of anything derived from `source` (Requirement 6.4).
    pub fn server(status: StatusCode, source: impl Into<BoxError>) -> Self {
        AppError {
            kind: ErrorKind::Server,
            status,
            public_message: GENERIC_SERVER_MESSAGE.to_string(),
            source: Some(source.into()),
            tag: None,
        }
    }

    /// Logs diagnostic detail for `Server` errors (Requirement 6.4); a
    /// no-op for `Client` errors, which carry no `source` to log.
    ///
    /// Emits via `tracing::error!` at the point the error is converted to a
    /// response. Once a later task (7.2) wires `crate::telemetry`'s
    /// request-scoped span into the request pipeline, this event nests
    /// inside it and inherits `request_id` automatically through ordinary
    /// `tracing` span inheritance — no explicit correlation id handling
    /// belongs here.
    fn log_if_server(&self) {
        if self.kind == ErrorKind::Server {
            match &self.source {
                Some(source) => {
                    tracing::error!(status = %self.status, error = %source, "internal server error");
                }
                None => {
                    tracing::error!(status = %self.status, "internal server error (no source captured)");
                }
            }
        }
    }

    /// Converts this error into an HTTP response using a caller-supplied
    /// body renderer instead of the default `{"error": public_message}`
    /// JSON shape (Requirement 6.5).
    ///
    /// This is the extension point a downstream spec (e.g. api-foundation)
    /// uses to swap in its own wire format — such as a Mastodon-compatible
    /// error envelope — while reusing this module's status/kind
    /// classification and 5xx logging behavior unchanged. `render` receives
    /// only `&AppError`: its `public_message` is already `Server`-safe by
    /// construction, and a custom renderer must likewise never place
    /// `source` in the returned body.
    pub fn into_response_with(self, render: impl FnOnce(&AppError) -> Response) -> Response {
        self.log_if_server();
        render(&self)
    }
}

/// JSON response body shape used by the default `IntoResponse` conversion:
/// `{"error": "<public_message>"}`.
#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

/// Default body renderer: status + `{"error": public_message}` JSON
/// (Requirement 6.2). Used by [`AppError`]'s `IntoResponse` impl; also
/// public so a custom [`AppError::into_response_with`] renderer can fall
/// back to it for a subset of cases instead of reimplementing it.
pub fn default_response(error: &AppError) -> Response {
    (
        error.status,
        Json(ErrorBody {
            error: &error.public_message,
        }),
    )
        .into_response()
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // See this module's doc comment ("Router-wide default is
        // api-foundation's Mastodon-compatible renderer") for why this
        // delegates to `crate::api::error::mastodon_error_body` instead of
        // this module's own `default_response` (task 7.1, api-foundation
        // Requirement 7.4).
        self.into_response_with(crate::api::error::mastodon_error_body)
    }
}
