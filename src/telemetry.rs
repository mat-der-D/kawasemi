//! Structured logging / observability foundation (Telemetry boundary).
//!
//! Scope: this module owns process-wide `tracing` subscriber initialization
//! (Requirement 7.1), driving the effective log level from
//! [`crate::config::LogConfig`] (Requirement 7.4), gating `sqlx::query`
//! diagnostic output behind `LogConfig::sql_diagnostic` (Requirement 7.3),
//! and defining the request-correlation span policy (Requirement 7.5) that
//! a later task (7.2, the axum `TraceLayer` wiring) will call into.
//!
//! Requirement 7.2 (HTTP request/response diagnostic logging via a real
//! `TraceLayer`) is out of scope here — this module only establishes the
//! span-naming/field convention and the subscriber/filter machinery it
//! rides on; nothing in this file talks to axum or HTTP.
//!
//! `init_telemetry` is documented as callable exactly once per process (it
//! installs a *global* default `tracing` subscriber, which `tracing` itself
//! only allows to be set once): a second call returns a [`TelemetryError`]
//! rather than panicking or silently no-oping.

#[cfg(test)]
mod tests;

use std::fmt;

use tracing_subscriber::EnvFilter;

use crate::config::LogConfig;

/// Canonical name of the span every request-handling task should open to
/// carry request-scoped correlation data (Requirement 7.5). `sqlx::query`
/// diagnostic events (Requirement 7.3) emitted while this span is entered
/// inherit its fields automatically via `tracing`'s span/event nesting, so
/// they never need to carry `request_id` themselves.
pub const REQUEST_SPAN_NAME: &str = "request";

/// Canonical field name carrying the per-request correlation identifier on
/// the [`REQUEST_SPAN_NAME`] span (Requirement 7.5).
pub const REQUEST_ID_FIELD: &str = "request_id";

/// `tracing` target under which `sqlx` emits its executed-SQL diagnostic
/// events. [`build_env_filter`] maps [`LogConfig::sql_diagnostic`] onto an
/// `EnvFilter` directive scoped to this target (Requirement 7.3).
const SQLX_QUERY_TARGET: &str = "sqlx::query";

/// Failure initializing the global structured-logging subscriber.
///
/// The two failure modes this wraps are: (1) the configured level produced
/// an unparsable filter directive (should not happen for any [`LogConfig`]
/// built by [`crate::config::load_config`], since [`crate::config::LogLevel`]
/// only ever produces one of five known-good lowercase words, but a
/// hand-built `LogConfig` — e.g. in a future caller — could in principle
/// misuse this API), and (2) a global `tracing` subscriber was already
/// installed in this process (`init_telemetry` is documented as callable
/// exactly once per process; a second call surfaces as this variant instead
/// of panicking).
#[derive(Debug)]
pub enum TelemetryError {
    /// The `EnvFilter` directive string built from `cfg` failed to parse.
    InvalidFilter(String),
    /// A global `tracing` subscriber was already installed in this process.
    AlreadyInitialized(String),
}

impl fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TelemetryError::InvalidFilter(reason) => {
                write!(f, "invalid telemetry filter directive: {reason}")
            }
            TelemetryError::AlreadyInitialized(reason) => {
                write!(
                    f,
                    "telemetry already initialized (init_telemetry must be called at most once per process): {reason}"
                )
            }
        }
    }
}

impl std::error::Error for TelemetryError {}

/// Builds the `EnvFilter` directive string driven by `cfg`: the base level
/// (Requirement 7.4, via [`crate::config::LogLevel`]'s lowercase `Display`)
/// plus a directive scoped to [`SQLX_QUERY_TARGET`] that either opens it at
/// `debug` (when `cfg.sql_diagnostic` is set) or suppresses it above the
/// base level (Requirement 7.3).
fn filter_directive(cfg: &LogConfig) -> String {
    let base = cfg.level;
    let sql = if cfg.sql_diagnostic {
        format!("{SQLX_QUERY_TARGET}=debug")
    } else {
        format!("{SQLX_QUERY_TARGET}=off")
    };
    format!("{base},{sql}")
}

/// Builds the [`EnvFilter`] that [`init_telemetry`] installs, applying
/// [`LogConfig::level`] (Requirement 7.4) and [`LogConfig::sql_diagnostic`]
/// (Requirement 7.3). Exposed separately from `init_telemetry` so the
/// filtering policy can be proven behaviorally without installing a process
/// global subscriber (see `tests.rs`).
pub fn build_env_filter(cfg: &LogConfig) -> Result<EnvFilter, TelemetryError> {
    let directive = filter_directive(cfg);
    EnvFilter::try_new(&directive)
        .map_err(|e| TelemetryError::InvalidFilter(format!("'{directive}': {e}")))
}

/// Opens the [`REQUEST_SPAN_NAME`] span carrying `request_id` (Requirement
/// 7.5). Task 7.2's `TraceLayer` wiring calls this once per HTTP request;
/// any `sqlx::query` diagnostic events (Requirement 7.3) emitted while
/// handling that request nest inside it and inherit the field automatically
/// through `tracing`'s span/event context, without needing to repeat
/// `request_id` on every event themselves.
pub fn request_span(request_id: &str) -> tracing::Span {
    tracing::info_span!(REQUEST_SPAN_NAME, { REQUEST_ID_FIELD } = %request_id)
}

/// Initializes the process-wide structured-logging foundation (Requirement
/// 7.1): builds an [`EnvFilter`] from `cfg` (Requirements 7.3, 7.4) and
/// installs a `tracing-subscriber` `fmt` subscriber as the global default.
///
/// Not idempotent: `tracing` only allows a global default subscriber to be
/// installed once per process, so calling this a second time returns
/// [`TelemetryError::AlreadyInitialized`] rather than replacing the first
/// subscriber or panicking. Callers (task 7.4's `bootstrap()`) must call
/// this exactly once, before any other component logs.
pub fn init_telemetry(cfg: &LogConfig) -> Result<(), TelemetryError> {
    let filter = build_env_filter(cfg)?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .map_err(|e| TelemetryError::AlreadyInitialized(e.to_string()))
}
