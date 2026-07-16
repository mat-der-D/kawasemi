//! Unit tests for `BootstrapError`'s aggregation behavior: it must retain
//! each wrapped stage's real error (not a stringly-typed summary), expose it
//! via both `Display` and `Error::source`, and identify which stage failed.
//!
//! These deliberately construct the wrapped `*Error` values directly rather
//! than driving them through `bootstrap()`/`build_state()` — proving
//! `BootstrapError`'s own plumbing here does not require a live database or
//! `init_telemetry` (whose global, install-once-per-process side effect this
//! module's doc comment explains is why `bootstrap()`-driving tests live in
//! separate `tests/*.rs` binaries instead of here).

use std::error::Error as _;

use axum::http::StatusCode;

use super::*;
use crate::config::ConfigIssue;
use crate::error::AppError;

#[test]
fn config_error_is_wrapped_with_source_and_display_preserved() {
    let inner = ConfigError {
        issues: vec![ConfigIssue::Missing {
            field: "server.domain".to_string(),
        }],
    };
    let inner_display = inner.to_string();
    let err = BootstrapError::from(inner);

    assert!(matches!(err, BootstrapError::Config(_)));
    assert!(
        err.to_string().contains(&inner_display),
        "BootstrapError::Config's Display must retain the wrapped ConfigError's own \
         diagnostic text: {err}"
    );
    assert!(
        err.source().is_some(),
        "BootstrapError must expose the wrapped ConfigError via Error::source"
    );
}

#[test]
fn telemetry_error_is_wrapped_with_source_and_display_preserved() {
    let inner = TelemetryError::InvalidFilter("bogus directive".to_string());
    let inner_display = inner.to_string();
    let err = BootstrapError::from(inner);

    assert!(matches!(err, BootstrapError::Telemetry(_)));
    assert!(
        err.to_string().contains(&inner_display),
        "BootstrapError::Telemetry's Display must retain the wrapped TelemetryError's own \
         diagnostic text: {err}"
    );
    assert!(
        err.source().is_some(),
        "BootstrapError must expose the wrapped TelemetryError via Error::source"
    );
}

#[test]
fn each_bootstrap_error_variant_identifies_its_own_stage_in_display() {
    // Requirement 1.2's diagnostic-output intent is only met if a reader can
    // tell *which* stage aborted startup, not just that something did.
    let config_err = BootstrapError::from(ConfigError {
        issues: vec![ConfigIssue::Missing {
            field: "database.url".to_string(),
        }],
    });
    assert!(config_err.to_string().contains("configuration"));

    let telemetry_err = BootstrapError::from(TelemetryError::InvalidFilter("bad".to_string()));
    assert!(telemetry_err.to_string().contains("telemetry"));
}

/// Requirement 6.1 (actor-model, task 6.1): a failure loading/opening
/// actor-model's persisted signing keys must aggregate into
/// `BootstrapError::KeySupply`, identify its own stage in `Display` (like
/// every other variant, see the test above), and -- unlike every other
/// variant -- report `None` from `Error::source`, since `AppError` itself
/// does not implement `std::error::Error` (see `src/error.rs`).
#[test]
fn key_supply_error_is_wrapped_and_identifies_its_own_stage_in_display_with_no_source_chain() {
    let inner = AppError::server(
        StatusCode::INTERNAL_SERVER_ERROR,
        "sealed key would not open",
    );
    let err = BootstrapError::from(inner);

    assert!(matches!(err, BootstrapError::KeySupply(_)));
    assert!(
        err.to_string().contains("actor signing keys"),
        "BootstrapError::KeySupply's Display must identify its own stage: {err}"
    );
    assert!(
        err.source().is_none(),
        "AppError does not implement std::error::Error, so KeySupply must report no source chain"
    );
}
