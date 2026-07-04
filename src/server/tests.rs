//! Integration tests for the Server boundary's foundation router + TraceLayer
//! wiring (task 7.2, Requirements 1.1, 7.2).
//!
//! These bind a real ephemeral-port `TcpListener` and speak raw HTTP/1.1
//! over a `TcpStream` (no extra HTTP client dependency needed) so the
//! assertions exercise the same `axum::serve` path a real deployment uses,
//! not just the router as a bare `tower::Service`.
//!
//! The request/response-log-correlation test reuses
//! `crate::telemetry::tests`'s technique (a custom in-memory `tracing`
//! `Layer` installed via `tracing::subscriber::set_default`, a thread-local,
//! reentrant scoped override — see that module's doc comment on why this is
//! safe across `cargo test`'s shared process) but additionally tracks each
//! span's recorded fields so an event can be correlated back to the
//! `request_id` of the span it was recorded in, which is what this task
//! must prove and `telemetry::tests` (written before 7.2 existed) does not.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id as SpanId};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use super::*;
use crate::config::{AppConfig, DatabaseConfig, LogConfig, LogLevel, Secret, ServerConfig};
use crate::runtime::{DeterministicSeed, RuntimeContext};
use crate::telemetry::{REQUEST_ID_FIELD, REQUEST_SPAN_NAME};

const LAZY_TEST_DB_URL: &str = "postgres://lazy-user:lazy-pw@127.0.0.1:5432/lazy-test-db";

/// Builds an `AppState` that never touches a real database: `connect_lazy`
/// only parses the URL and configures the pool without dialing out (mirrors
/// `src/state/tests.rs`'s technique). Requirement 1.1's health confirmation
/// is about listener readiness, not database liveness, so this task's tests
/// have no need for a live PostgreSQL connection.
fn test_state(seed: u64) -> AppState {
    let config = AppConfig {
        server: ServerConfig {
            domain: "server-test.example.test".to_string(),
            bind_addr: "127.0.0.1:0".parse::<SocketAddr>().expect("valid addr"),
            shutdown_grace: Duration::from_secs(30),
        },
        database: DatabaseConfig {
            url: Secret::new(LAZY_TEST_DB_URL.to_string()),
            max_connections: 5,
            acquire_timeout: Duration::from_secs(5),
        },
        log: LogConfig {
            level: LogLevel::Info,
            sql_diagnostic: false,
        },
    };
    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect_lazy(LAZY_TEST_DB_URL)
        .expect("connect_lazy only parses the URL; it never opens a connection");
    let runtime = RuntimeContext::deterministic(DeterministicSeed::new(seed));
    AppState::new(pool, runtime, config)
}

/// Speaks a minimal raw HTTP/1.1 GET request over a fresh `TcpStream` and
/// returns the full response text. `Connection: close` tells the server to
/// close the socket once the response is fully written, so `read_to_end`
/// terminates instead of waiting on a keep-alive connection.
async fn raw_http_get(addr: SocketAddr, path: &str) -> String {
    let mut stream =
        tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
            .await
            .expect("connecting to the test listener must not time out")
            .expect("connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .expect("reading the response must not time out")
        .expect("read response");
    String::from_utf8_lossy(&buf).to_string()
}

/// Requirement 1.1: the foundation router's health-check route responds
/// successfully once the router+server is actually running and accepting
/// real socket connections (not merely reachable as a bare `tower::Service`).
#[tokio::test]
async fn health_endpoint_responds_ok_over_a_real_listener() {
    // Installs a permissive (unfiltered) thread-local default subscriber for
    // the duration of this test, mirroring every behavioral test in
    // `telemetry::tests`. Without *some* subscriber active the first time a
    // given `tracing` callsite (e.g. this task's `on_request`/`on_response`
    // `tracing::info!` call sites) fires, `tracing`'s process-wide callsite
    // interest cache can permanently record "nobody is interested" for that
    // callsite — which would then silently suppress those same events for
    // *other* tests later in this binary that install a real capturing
    // subscriber (see the correlation test below), depending on which test
    // happens to execute the callsite first under `cargo test`'s parallel
    // scheduling. Setting a permissive default here avoids that hazard
    // regardless of test execution order.
    let _guard = tracing::subscriber::set_default(tracing_subscriber::registry());

    let state = test_state(1);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(serve(listener, state));

    let response = raw_http_get(addr, HEALTH_PATH).await;

    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected a 200 response from {HEALTH_PATH}, got: {response}"
    );
}

/// A minimal in-memory `Layer` recording every span's fields (keyed by
/// `tracing::span::Id`) and every event, so events can be correlated back to
/// the `request_id` field of the span they were recorded in.
#[derive(Debug, Default)]
struct FieldMap(HashMap<String, String>);

impl Visit for FieldMap {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
}

#[derive(Debug, Clone)]
struct RecordedEvent {
    message: String,
    /// `request_id` of the span this event was recorded inside, resolved by
    /// looking up the event's current span in `Capture::spans`.
    request_id: Option<String>,
}

#[derive(Clone, Default)]
struct Capture {
    spans: Arc<Mutex<HashMap<SpanId, HashMap<String, String>>>>,
    events: Arc<Mutex<Vec<RecordedEvent>>>,
}

impl<S> Layer<S> for Capture
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &SpanId, _ctx: Context<'_, S>) {
        let mut fields = FieldMap::default();
        attrs.record(&mut fields);
        self.spans.lock().unwrap().insert(id.clone(), fields.0);
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = FieldMap::default();
        event.record(&mut fields);
        let message = fields.0.get("message").cloned().unwrap_or_default();

        let request_id = ctx.event_span(event).and_then(|span_ref| {
            self.spans
                .lock()
                .unwrap()
                .get(&span_ref.id())
                .and_then(|f| f.get(REQUEST_ID_FIELD).cloned())
        });

        self.events.lock().unwrap().push(RecordedEvent {
            message,
            request_id,
        });
    }
}

/// Requirement 7.2 (and 7.5's correlation-id policy): the request and
/// response diagnostic logs `TraceLayer` emits must both carry the same
/// non-empty `request_id`, proving the span `TraceLayer`'s `make_span_with`
/// opens (via `telemetry::request_span`) actually wraps request handling —
/// if `TraceLayer` or the `request_span` wiring were removed, the "request
/// received"/"response sent" events either would not appear at all or would
/// have no correlating `request_id`, so this test would fail rather than
/// merely checking the response status.
#[tokio::test]
async fn trace_layer_logs_request_and_response_correlated_by_the_same_request_id() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let state = test_state(2);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(serve(listener, state));

    let response = raw_http_get(addr, HEALTH_PATH).await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected a 200 response from {HEALTH_PATH}, got: {response}"
    );

    let events = capture.events.lock().unwrap();
    let request_span_created = capture
        .spans
        .lock()
        .unwrap()
        .values()
        .any(|fields| fields.contains_key(REQUEST_ID_FIELD));
    assert!(
        request_span_created,
        "expected TraceLayer to open a span (named {REQUEST_SPAN_NAME:?}) carrying {REQUEST_ID_FIELD:?}"
    );

    let received: Vec<_> = events
        .iter()
        .filter(|e| e.message.contains("request received"))
        .collect();
    let sent: Vec<_> = events
        .iter()
        .filter(|e| e.message.contains("response sent"))
        .collect();
    assert_eq!(
        received.len(),
        1,
        "expected exactly one 'request received' log event: {events:?}"
    );
    assert_eq!(
        sent.len(),
        1,
        "expected exactly one 'response sent' log event: {events:?}"
    );

    let request_id = received[0]
        .request_id
        .clone()
        .expect("the 'request received' event must carry a correlating request_id");
    assert!(!request_id.is_empty(), "request_id must not be empty");
    assert_eq!(
        sent[0].request_id,
        Some(request_id),
        "the 'request received' and 'response sent' events must carry the same request_id"
    );
}
