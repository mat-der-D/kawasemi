//! Behavioral tests for the Telemetry boundary (Requirements 7.1, 7.3, 7.4,
//! 7.5). These exercise `init_telemetry`'s internals directly rather than
//! through a real HTTP request (out of scope here — see module docs).
//!
//! Global-subscriber caution: `tracing` only allows one global default
//! subscriber per process, and `cargo test` runs tests in threads within one
//! process. Every test below except
//! `init_telemetry_installs_a_global_subscriber_exactly_once` uses
//! `tracing::subscriber::set_default`, which is a *thread-local*, reentrant
//! scoped override (safe to use concurrently across tests) — it never
//! touches the process-global default. Only one test is allowed to actually
//! call `init_telemetry` and observe global-install semantics; it is
//! consolidated into a single function that checks both the success case
//! and the documented non-idempotent failure case together, so no other
//! test can race with it over the one-shot global slot.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

use super::*;
use crate::config::LogLevel;

fn log_config(level: LogLevel, sql_diagnostic: bool) -> LogConfig {
    LogConfig {
        level,
        sql_diagnostic,
    }
}

/// Collects field values recorded on a span or event into a plain map,
/// formatting non-string fields via `Debug` (mirroring how `tracing`'s own
/// `record_debug` fallback works).
#[derive(Debug, Default)]
struct FieldMap(HashMap<String, String>);

impl Visit for FieldMap {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_string(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
}

#[derive(Debug, Clone)]
struct RecordedEvent {
    level: Level,
    target: String,
}

#[derive(Debug, Clone)]
struct RecordedSpan {
    name: &'static str,
    fields: HashMap<String, String>,
}

/// A minimal in-memory `Layer` that records every event and every newly
/// created span it observes, so tests can assert on `tracing`'s actual
/// filtering and span/field behavior instead of only on the string that
/// built an `EnvFilter`.
#[derive(Clone, Default)]
struct Capture {
    events: Arc<Mutex<Vec<RecordedEvent>>>,
    spans: Arc<Mutex<Vec<RecordedSpan>>>,
}

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        self.events.lock().unwrap().push(RecordedEvent {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
        });
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut fields = FieldMap::default();
        attrs.record(&mut fields);
        self.spans.lock().unwrap().push(RecordedSpan {
            name: attrs.metadata().name(),
            fields: fields.0,
        });
    }
}

/// Requirement 7.4: `LogConfig.level` must drive the effective filter
/// directive for every supported level, independent of `sql_diagnostic`.
#[test]
fn filter_directive_encodes_every_configured_level() {
    let cases = [
        (LogLevel::Trace, "trace"),
        (LogLevel::Debug, "debug"),
        (LogLevel::Info, "info"),
        (LogLevel::Warn, "warn"),
        (LogLevel::Error, "error"),
    ];
    for (level, word) in cases {
        assert_eq!(
            filter_directive(&log_config(level, true)),
            format!("{word},sqlx::query=debug")
        );
        assert_eq!(
            filter_directive(&log_config(level, false)),
            format!("{word},sqlx::query=off")
        );
    }
}

/// Requirements 7.3, 7.4: the directive built from any valid `LogConfig`
/// must actually parse into a usable `EnvFilter` (not just be a
/// syntactically-hopeful string).
#[test]
fn build_env_filter_accepts_every_configured_level_and_flag_combination() {
    for level in [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ] {
        assert!(build_env_filter(&log_config(level, false)).is_ok());
        assert!(build_env_filter(&log_config(level, true)).is_ok());
    }
}

/// Requirement 7.4 (behavioral): a filter built for `Warn` actually drops
/// `info!` events and passes `warn!`/`error!` events through a real
/// `tracing` dispatch, not merely a string comparison.
#[test]
fn level_filter_actually_blocks_events_below_the_configured_level() {
    let filter = build_env_filter(&log_config(LogLevel::Warn, false)).expect("valid filter");
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(capture.clone());

    let _guard = tracing::subscriber::set_default(subscriber);
    tracing::info!("must be filtered out: below warn");
    tracing::warn!("must pass: at warn");
    tracing::error!("must pass: above warn");
    drop(_guard);

    let events = capture.events.lock().unwrap();
    assert_eq!(
        events.len(),
        2,
        "expected only warn+error to pass through a Warn-level filter: {events:?}"
    );
    assert!(
        events.iter().all(|e| e.level <= Level::WARN),
        "an event below the configured level leaked through: {events:?}"
    );
}

/// Requirement 7.3 (behavioral): `sql_diagnostic = true` opens the
/// `sqlx::query` target at `debug` even when the base level (`info`) would
/// otherwise suppress debug-level events for every other target.
#[test]
fn sql_diagnostic_true_lets_sqlx_query_debug_events_through_despite_a_higher_base_level() {
    let filter = build_env_filter(&log_config(LogLevel::Info, true)).expect("valid filter");
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(capture.clone());

    {
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::event!(target: "sqlx::query", Level::DEBUG, "select 1 from accounts");
        tracing::debug!("must still be filtered: unrelated target, base level is info");
    }

    let events = capture.events.lock().unwrap();
    assert_eq!(
        events.len(),
        1,
        "sql_diagnostic=true should let exactly the sqlx::query debug event through: {events:?}"
    );
    assert_eq!(events[0].target, "sqlx::query");
}

/// Requirement 7.3 (behavioral): `sql_diagnostic = false` suppresses the
/// `sqlx::query` target's debug-level events, even though nothing about the
/// base level changed relative to the previous test.
#[test]
fn sql_diagnostic_false_suppresses_sqlx_query_debug_events() {
    let filter = build_env_filter(&log_config(LogLevel::Info, false)).expect("valid filter");
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(capture.clone());

    {
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::event!(target: "sqlx::query", Level::DEBUG, "select 1 from accounts");
    }

    assert_eq!(
        capture.events.lock().unwrap().len(),
        0,
        "sql_diagnostic=false should suppress sqlx::query debug events"
    );
}

/// Requirement 7.5: `request_span` opens a span named [`REQUEST_SPAN_NAME`]
/// carrying a [`REQUEST_ID_FIELD`] field set to the given correlation id —
/// the policy later task 7.2's `TraceLayer` wiring depends on.
#[test]
fn request_span_uses_the_canonical_name_and_carries_the_request_id_field() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let span = request_span("corr-abc-123");
    drop(span.enter());

    let spans = capture.spans.lock().unwrap();
    assert_eq!(spans.len(), 1, "request_span should create exactly one span");
    assert_eq!(spans[0].name, REQUEST_SPAN_NAME);
    assert_eq!(
        spans[0].fields.get(REQUEST_ID_FIELD).map(String::as_str),
        Some("corr-abc-123"),
        "request_span must record the request id under REQUEST_ID_FIELD: {:?}",
        spans[0].fields
    );
}

/// Requirement 7.1 + design constraint ("冪等でなく...一度だけ呼ぶ"): the
/// first `init_telemetry` call in this process must succeed (proving the
/// subscriber foundation actually initializes), and a second call in the
/// same process must fail rather than silently no-op or panic, proving the
/// "call exactly once" contract is enforced rather than merely documented.
/// Consolidated into one test so no other test can race over the process's
/// one-shot global-subscriber slot.
#[test]
fn init_telemetry_installs_a_global_subscriber_exactly_once() {
    let cfg = log_config(LogLevel::Info, false);

    let first = init_telemetry(&cfg);
    assert!(
        first.is_ok(),
        "the first init_telemetry call in this process must succeed: {first:?}"
    );

    let second = init_telemetry(&cfg);
    match second {
        Err(TelemetryError::AlreadyInitialized(_)) => {}
        other => panic!(
            "a second init_telemetry call must fail as AlreadyInitialized, got: {other:?}"
        ),
    }
}
