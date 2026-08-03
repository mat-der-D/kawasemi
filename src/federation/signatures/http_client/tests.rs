use std::net::TcpListener;

use axum::http::{HeaderName, HeaderValue, Method, StatusCode};

use super::*;

fn handle(raw: &str) -> Handle {
    Handle::new(raw).expect("test handle must be valid")
}

fn sample_request() -> OutboundRequest {
    let mut req = OutboundRequest::new(Method::POST, "https://remote.example/inbox")
        .with_body(b"{\"type\":\"Create\"}".to_vec());
    req.headers.insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("application/activity+json"),
    );
    req
}

fn canned_response(status: StatusCode, body: &[u8]) -> HttpResponse {
    HttpResponse {
        status,
        headers: HeaderMap::new(),
        body: body.to_vec(),
    }
}

// --- OutboundRequest builder ---

#[test]
fn outbound_request_new_starts_bodyless_with_no_headers() {
    let req = OutboundRequest::new(Method::GET, "https://remote.example/actor");

    assert_eq!(req.method, Method::GET);
    assert_eq!(req.url, "https://remote.example/actor");
    assert!(req.headers.is_empty());
    assert!(req.body.is_none());
}

#[test]
fn outbound_request_with_body_attaches_the_given_bytes() {
    let req = OutboundRequest::new(Method::POST, "https://remote.example/inbox")
        .with_body(b"payload".to_vec());

    assert_eq!(req.body.as_deref(), Some(&b"payload"[..]));
}

// --- MockFederationHttpClient: swappability (task 1.4's observable
// completion condition, "モック HTTP クライアントで送信/取得を差し替えられる") ---

/// Generic over `FederationHttpClient`, exactly as a real caller (e.g. the
/// future `DeliveryWorker`) would be -- proves a production implementation
/// and this mock are interchangeable behind the trait, not merely that the
/// mock happens to compile standalone.
async fn perform_send<C: FederationHttpClient>(
    client: &C,
    req: OutboundRequest,
) -> Result<HttpResponse, AppError> {
    client.send(req).await
}

async fn perform_fetch<C: FederationHttpClient>(
    client: &C,
    url: &str,
    signed_as: Option<&Handle>,
) -> Result<HttpResponse, AppError> {
    client.fetch(url, signed_as).await
}

#[tokio::test]
async fn mock_send_returns_the_queued_response() {
    let mock = MockFederationHttpClient::new();
    mock.queue_send_response(canned_response(StatusCode::ACCEPTED, b"ok"));

    let response = perform_send(&mock, sample_request())
        .await
        .expect("queued response must be returned");

    assert_eq!(response.status, StatusCode::ACCEPTED);
    assert_eq!(response.body, b"ok");
}

#[tokio::test]
async fn mock_send_returns_the_queued_error() {
    let mock = MockFederationHttpClient::new();
    mock.queue_send_error(StatusCode::BAD_GATEWAY, "simulated network failure");

    let result = perform_send(&mock, sample_request()).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn mock_send_records_the_exact_request_it_was_called_with() {
    let mock = MockFederationHttpClient::new();
    mock.queue_send_response(canned_response(StatusCode::OK, b""));

    let req = sample_request();
    let expected_url = req.url.clone();
    let expected_body = req.body.clone();
    perform_send(&mock, req).await.expect("must succeed");

    let recorded = mock.sent_requests();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].url, expected_url);
    assert_eq!(recorded[0].body, expected_body);
    assert_eq!(recorded[0].method, Method::POST);
}

#[tokio::test]
async fn mock_send_outcomes_are_consumed_in_fifo_order() {
    let mock = MockFederationHttpClient::new();
    mock.queue_send_response(canned_response(StatusCode::OK, b"first"));
    mock.queue_send_response(canned_response(StatusCode::CREATED, b"second"));

    let first = perform_send(&mock, sample_request())
        .await
        .expect("first queued response");
    let second = perform_send(&mock, sample_request())
        .await
        .expect("second queued response");

    assert_eq!(first.body, b"first");
    assert_eq!(second.body, b"second");
}

#[tokio::test]
async fn mock_send_with_nothing_queued_returns_an_error_instead_of_panicking() {
    let mock = MockFederationHttpClient::new();

    let result = perform_send(&mock, sample_request()).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn mock_fetch_returns_the_queued_response() {
    let mock = MockFederationHttpClient::new();
    mock.queue_fetch_response(canned_response(StatusCode::OK, b"{\"type\":\"Person\"}"));

    let response = perform_fetch(&mock, "https://remote.example/users/alice", None)
        .await
        .expect("queued response must be returned");

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, b"{\"type\":\"Person\"}");
}

#[tokio::test]
async fn mock_fetch_returns_the_queued_error() {
    let mock = MockFederationHttpClient::new();
    mock.queue_fetch_error(StatusCode::NOT_FOUND, "simulated 404");

    let result = perform_fetch(&mock, "https://remote.example/users/missing", None).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn mock_fetch_records_the_url_and_signed_as_handle() {
    let mock = MockFederationHttpClient::new();
    mock.queue_fetch_response(canned_response(StatusCode::OK, b""));
    let signer = handle("alice");

    perform_fetch(&mock, "https://remote.example/users/bob", Some(&signer))
        .await
        .expect("must succeed");

    let recorded = mock.fetched_urls();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, "https://remote.example/users/bob");
    assert_eq!(recorded[0].1, Some(signer));
}

#[tokio::test]
async fn mock_fetch_records_none_when_unsigned() {
    let mock = MockFederationHttpClient::new();
    mock.queue_fetch_response(canned_response(StatusCode::OK, b""));

    perform_fetch(&mock, "https://remote.example/users/bob", None)
        .await
        .expect("must succeed");

    let recorded = mock.fetched_urls();
    assert_eq!(recorded[0].1, None);
}

#[tokio::test]
async fn mock_send_and_fetch_outcome_queues_are_independent() {
    let mock = MockFederationHttpClient::new();
    mock.queue_send_response(canned_response(StatusCode::OK, b"send"));

    // fetch has nothing queued -- must not accidentally consume send's queue.
    let fetch_result = perform_fetch(&mock, "https://remote.example/x", None).await;
    assert!(fetch_result.is_err());

    let send_result = perform_send(&mock, sample_request()).await;
    assert_eq!(send_result.expect("send outcome untouched").body, b"send");
}

// --- ReqwestFederationHttpClient: thin adapter over a real HTTP client.
// No live-network dependency: connects to a local port nothing is
// listening on (bound then immediately released), so the connection is
// deterministically refused rather than depending on the internet. ---

fn unused_local_port() -> u16 {
    let listener =
        TcpListener::bind("127.0.0.1:0").expect("binding an ephemeral port must succeed");
    let port = listener
        .local_addr()
        .expect("bound listener must have a local address")
        .port();
    drop(listener);
    port
}

#[tokio::test]
async fn reqwest_client_send_surfaces_a_connection_failure_as_an_app_error() {
    let client = ReqwestFederationHttpClient::new();
    let port = unused_local_port();
    let req = OutboundRequest::new(Method::POST, format!("http://127.0.0.1:{port}/inbox"))
        .with_body(b"{}".to_vec());

    let result = client.send(req).await;

    assert!(
        result.is_err(),
        "sending to a refused connection must surface as an error, not panic"
    );
}

#[tokio::test]
async fn reqwest_client_fetch_surfaces_a_connection_failure_as_an_app_error() {
    let client = ReqwestFederationHttpClient::default();
    let port = unused_local_port();

    let result = client
        .fetch(&format!("http://127.0.0.1:{port}/users/alice"), None)
        .await;

    assert!(
        result.is_err(),
        "fetching from a refused connection must surface as an error, not panic"
    );
}

// --- Interchangeability across implementations (production vs. mock) ---

#[tokio::test]
async fn production_and_mock_clients_both_satisfy_the_same_generic_send_call() {
    // Mock path (deterministic, exercised for real here).
    let mock = MockFederationHttpClient::new();
    mock.queue_send_response(canned_response(StatusCode::OK, b"mock"));
    let mock_result = perform_send(&mock, sample_request()).await;
    assert!(mock_result.is_ok());

    // Production path type-checks against the exact same generic call site
    // -- proving `perform_send` (and by extension any real caller written
    // against `FederationHttpClient`) does not need to know which
    // implementation it received.
    let production = ReqwestFederationHttpClient::new();
    let port = unused_local_port();
    let production_result = perform_send(
        &production,
        OutboundRequest::new(Method::GET, format!("http://127.0.0.1:{port}/")),
    )
    .await;
    assert!(production_result.is_err());
}
