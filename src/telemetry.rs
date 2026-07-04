//! Structured logging / telemetry initialization (Requirement 7).
//!
//! `init_telemetry()` installs the process-wide `tracing` subscriber
//! exactly once, at startup, with its verbosity and SQL-diagnostic
//! visibility controlled by [`LogConfig`] (Requirement 7.1, 7.4). It is not
//! idempotent by design: a second call after a subscriber is already
//! installed fails, matching the "called once at process startup"
//! contract from the design (`design.md` Telemetry component).
//!
//! This module also documents and exercises the `request_id` span
//! convention (Requirement 7.5): a tracing span carrying a `request_id`
//! field, which later request-handling code (task 7.2's `TraceLayer`
//! wiring) enters for the duration of a request. Diagnostic events raised
//! inside that span -- including sqlx's executed-SQL events, which sqlx
//! emits under the `sqlx::query` tracing target (Requirement 7.3) --
//! inherit the enclosing span's fields automatically via `tracing`'s
//! event/span nesting, so they correlate with `request_id` without having
//! to carry it themselves.
//!
//! `init_telemetry()` itself is not yet wired into the startup sequence:
//! that composition-root wiring is task 7.4's job. Until then this module
//! is exercised only by its own unit tests, so it is allowed to be
//! otherwise unused.

#![allow(dead_code)]

use std::fmt;

use tracing_subscriber::EnvFilter;

use crate::config::{LogConfig, LogLevel};

/// The tracing span field name used to carry a per-request correlation
/// identifier. Task 7.2 creates a span with this field and enters it
/// around handler dispatch so every diagnostic event emitted while
/// handling a request -- including SQL logged at diagnostic level -- is
/// attributed to the same `request_id` (Requirement 7.5).
pub const REQUEST_ID_FIELD: &str = "request_id";

/// Builds the per-request correlation span described by
/// [`REQUEST_ID_FIELD`]. Callers (task 7.2's request middleware) enter the
/// returned span for the duration of handling one request.
pub fn request_span(request_id: &str) -> tracing::Span {
    tracing::info_span!("request", request_id = %request_id)
}

/// Telemetry initialization failed, most likely because a global
/// subscriber was already installed by an earlier call.
#[derive(Debug)]
pub struct TelemetryError(pub String);

impl fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to initialize telemetry: {}", self.0)
    }
}

impl std::error::Error for TelemetryError {}

/// Builds the [`EnvFilter`] implied by `cfg`: the configured log level as
/// the default directive, plus an explicit directive for the `sqlx::query`
/// target reflecting `cfg.sql_diagnostics` (Requirement 7.3, 7.4).
pub fn build_filter(cfg: &LogConfig) -> EnvFilter {
    let sql_directive = if cfg.sql_diagnostics {
        "sqlx::query=debug"
    } else {
        "sqlx::query=off"
    };
    let directives = format!("{},{sql_directive}", level_str(cfg.level));
    EnvFilter::new(directives)
}

fn level_str(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

/// Initializes the process-wide structured logging subscriber
/// (Requirement 7.1), with verbosity controlled by `cfg` (Requirement 7.4)
/// and SQL diagnostics gated by `cfg.sql_diagnostics` (Requirement 7.3).
/// Not idempotent: intended to be called exactly once, during process
/// startup, before any other component emits tracing events.
pub fn init_telemetry(cfg: &LogConfig) -> Result<(), TelemetryError> {
    tracing_subscriber::fmt()
        .with_env_filter(build_filter(cfg))
        .try_init()
        .map_err(|err| TelemetryError(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LogConfig, LogLevel};
    use std::io;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for CapturedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedWriter {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn captured_text(cfg: &LogConfig, emit: impl FnOnce()) -> String {
        let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let writer = CapturedWriter(buf.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(build_filter(cfg))
            .with_writer(writer)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, emit);
        String::from_utf8(buf.lock().unwrap().clone()).expect("captured output must be utf8")
    }

    #[test]
    fn build_filter_reflects_configured_level_as_default_directive() {
        let cfg = LogConfig { level: LogLevel::Warn, sql_diagnostics: false };
        let filter = build_filter(&cfg).to_string();
        assert!(filter.contains("warn"), "expected filter {filter:?} to contain the configured level");
    }

    #[test]
    fn build_filter_enables_sqlx_query_diagnostics_when_flag_is_set() {
        let cfg = LogConfig { level: LogLevel::Info, sql_diagnostics: true };
        let filter = build_filter(&cfg).to_string();
        assert!(
            filter.contains("sqlx::query=debug"),
            "expected sqlx::query=debug directive, got {filter:?}"
        );
    }

    #[test]
    fn build_filter_suppresses_sqlx_query_diagnostics_when_flag_is_unset() {
        let cfg = LogConfig { level: LogLevel::Info, sql_diagnostics: false };
        let filter = build_filter(&cfg).to_string();
        assert!(
            !filter.contains("sqlx::query=debug"),
            "did not expect sql diagnostics directive, got {filter:?}"
        );
        assert!(
            filter.contains("sqlx::query=off"),
            "expected an explicit suppression directive, got {filter:?}"
        );
    }

    #[test]
    fn log_output_changes_based_on_configured_level() {
        let info_cfg = LogConfig { level: LogLevel::Info, sql_diagnostics: false };
        let output = captured_text(&info_cfg, || {
            tracing::debug!("debug message should be suppressed");
            tracing::info!("info message should appear");
        });
        assert!(
            !output.contains("debug message"),
            "debug should be filtered out at info level, got {output:?}"
        );
        assert!(
            output.contains("info message"),
            "info should appear at info level, got {output:?}"
        );

        let debug_cfg = LogConfig { level: LogLevel::Debug, sql_diagnostics: false };
        let output = captured_text(&debug_cfg, || {
            tracing::debug!("debug message should appear now");
        });
        assert!(
            output.contains("debug message"),
            "debug should appear at debug level, got {output:?}"
        );
    }

    #[test]
    fn sqlx_query_events_are_gated_by_sql_diagnostics_flag() {
        let cfg_off = LogConfig { level: LogLevel::Info, sql_diagnostics: false };
        let output = captured_text(&cfg_off, || {
            // Simulates what sqlx emits internally: a debug-level event
            // under the `sqlx::query` target.
            tracing::debug!(target: "sqlx::query", "select 1");
        });
        assert!(
            !output.contains("select 1"),
            "sql diagnostics should be suppressed when disabled, got {output:?}"
        );

        let cfg_on = LogConfig { level: LogLevel::Info, sql_diagnostics: true };
        let output = captured_text(&cfg_on, || {
            tracing::debug!(target: "sqlx::query", "select 1");
        });
        assert!(
            output.contains("select 1"),
            "sql diagnostics should appear when enabled, got {output:?}"
        );
    }

    #[test]
    fn request_span_carries_request_id_field_into_captured_output() {
        let cfg = LogConfig { level: LogLevel::Info, sql_diagnostics: false };
        let output = captured_text(&cfg, || {
            let span = request_span("req-123");
            let _entered = span.enter();
            tracing::info!("handled request");
        });
        assert!(
            output.contains("req-123"),
            "expected the request_id value to be captured in log output, got {output:?}"
        );
        assert!(
            output.contains(REQUEST_ID_FIELD),
            "expected the request_id field name to appear in output, got {output:?}"
        );
    }

    #[test]
    fn init_telemetry_installs_global_subscriber_exactly_once() {
        let cfg = LogConfig { level: LogLevel::Info, sql_diagnostics: false };
        let first = init_telemetry(&cfg);
        assert!(first.is_ok(), "first call should succeed: {first:?}");

        let second = init_telemetry(&cfg);
        assert!(
            second.is_err(),
            "second call should fail because a global subscriber is already installed"
        );
    }
}
