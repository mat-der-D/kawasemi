//! Integration-style tests for `PublicKeyResolver`/`DbFederationPublicKeyResolver`
//! (Requirements 2.3, 2.4), per task 2.1's observable completion condition:
//! "初回取得後はキャッシュから返り、force 再取得でネットワーク取得が走り、
//! TTL 超過後の解決要求では再度ネットワーク取得が走る統合テストが通る".
//!
//! Mirrors `src/actor/keys/repository/tests.rs`'s established convention:
//! `spawn_test_app` for an isolated, already-migrated schema (so this test
//! exercises the real `remote_public_keys` table, not a stand-in), paired
//! with `MockFederationHttpClient` (task 1.4) so the "fetches over the
//! network" assertions are deterministic without any real HTTP call.
//!
//! Cache-validity across elapsed time is exercised by constructing two
//! separate `DbFederationPublicKeyResolver`s that share the same pool/mock
//! but are built with different `FixedClock` values (`FixedClock` itself
//! cannot be advanced mid-instance, mirroring `runtime::clock`'s own
//! documented "always returns the fixed time it was constructed with"
//! contract) — this simulates "time has passed" between two resolution
//! requests without depending on wall-clock time anywhere in this file.

use std::sync::Arc;

use serde_json::json;
use time::Duration;
use time::macros::datetime;

use super::*;
use crate::federation::signatures::http_client::{HttpResponse, MockFederationHttpClient};
use crate::runtime::FixedClock;
use crate::test_harness::spawn_test_app;

const TEST_KEY_ID: &str = "https://remote.example/users/alice#main-key";
const TEST_ACTOR_URI: &str = "https://remote.example/users/alice";

/// Builds a canned actor-document HTTP response carrying `pem` as
/// `publicKey.publicKeyPem`, owned by `TEST_ACTOR_URI` (the shape
/// `parse_public_key_document` reads — see this module's own doc comment).
fn actor_document_response(pem: &str) -> HttpResponse {
    let body = json!({
        "id": TEST_ACTOR_URI,
        "type": "Person",
        "publicKey": {
            "id": TEST_KEY_ID,
            "owner": TEST_ACTOR_URI,
            "publicKeyPem": pem,
        }
    })
    .to_string()
    .into_bytes();

    HttpResponse {
        status: StatusCode::OK,
        headers: axum::http::HeaderMap::new(),
        body,
    }
}

fn fixed_clock_at(offset_seconds: i64) -> Arc<dyn Clock> {
    let base = datetime!(2026-07-16 00:00:00 UTC);
    Arc::new(FixedClock::new(base + Duration::seconds(offset_seconds)))
}

// --- 1: first resolve with no cache row fetches over HTTP and caches ---

#[tokio::test]
async fn first_resolve_with_no_cache_fetches_over_http_and_caches() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(actor_document_response("PEM-ONE"));
    let resolver = DbFederationPublicKeyResolver::new(
        app.pool.clone(),
        mock.clone(),
        fixed_clock_at(0),
        Duration::hours(24),
    );

    let resolved = resolver
        .resolve_public_key(TEST_KEY_ID, false)
        .await
        .expect("first resolution must succeed via network fetch");

    assert_eq!(resolved.key_id, TEST_KEY_ID);
    assert_eq!(resolved.actor_uri, TEST_ACTOR_URI);
    assert_eq!(resolved.public_key_pem, "PEM-ONE");
    assert_eq!(
        mock.fetched_urls().len(),
        1,
        "first resolution for an uncached keyId must fetch over the network exactly once"
    );

    let (cached, _fetched_at) = find_cached(&app.pool, TEST_KEY_ID)
        .await
        .expect("reading the cache must succeed")
        .expect("the resolved key must have been cached");
    assert_eq!(cached, resolved);

    app.cleanup().await;
}

// --- 2: second resolve within TTL, force=false -> cache hit, no fetch ---

#[tokio::test]
async fn resolve_within_ttl_without_force_returns_cached_value_without_fetching() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(actor_document_response("PEM-ONE"));
    let clock = fixed_clock_at(0);
    let resolver = DbFederationPublicKeyResolver::new(
        app.pool.clone(),
        mock.clone(),
        clock,
        Duration::hours(24),
    );
    let first = resolver
        .resolve_public_key(TEST_KEY_ID, false)
        .await
        .expect("first resolution must succeed");

    // No second outcome queued: if this resolution attempted a network
    // fetch, `MockFederationHttpClient::fetch` would return an error
    // ("no queued fetch() outcome"), so `.expect` failing would prove a
    // fetch was wrongly attempted.
    let second = resolver
        .resolve_public_key(TEST_KEY_ID, false)
        .await
        .expect("resolution within TTL must be served from cache without fetching");

    assert_eq!(second, first);
    assert_eq!(
        mock.fetched_urls().len(),
        1,
        "a within-TTL, non-forced resolution must not perform a second network fetch"
    );

    app.cleanup().await;
}

// --- 3: force=true always re-fetches, even within TTL, and updates the cache ---

#[tokio::test]
async fn force_true_always_refetches_and_updates_the_cache_even_within_ttl() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(actor_document_response("PEM-ONE"));
    mock.queue_fetch_response(actor_document_response("PEM-TWO"));
    let clock = fixed_clock_at(0);
    let resolver = DbFederationPublicKeyResolver::new(
        app.pool.clone(),
        mock.clone(),
        clock,
        Duration::hours(24),
    );
    resolver
        .resolve_public_key(TEST_KEY_ID, false)
        .await
        .expect("first resolution must succeed");

    let forced = resolver
        .resolve_public_key(TEST_KEY_ID, true)
        .await
        .expect("force=true resolution must succeed");

    assert_eq!(forced.public_key_pem, "PEM-TWO");
    assert_eq!(
        mock.fetched_urls().len(),
        2,
        "force=true must always perform a network fetch, regardless of a still-valid cache"
    );

    let (cached, _fetched_at) = find_cached(&app.pool, TEST_KEY_ID)
        .await
        .expect("reading the cache must succeed")
        .expect("a cache row must still exist");
    assert_eq!(
        cached.public_key_pem, "PEM-TWO",
        "the forced re-fetch's result must overwrite the previously cached value"
    );

    app.cleanup().await;
}

// --- 4: resolve after fetched_at + TTL has elapsed, force=false -> stale, refetches ---

#[tokio::test]
async fn resolve_after_ttl_elapsed_without_force_treats_cache_as_stale_and_refetches() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(actor_document_response("PEM-ONE"));
    let ttl = Duration::seconds(60);
    let resolver_at_t0 =
        DbFederationPublicKeyResolver::new(app.pool.clone(), mock.clone(), fixed_clock_at(0), ttl);
    resolver_at_t0
        .resolve_public_key(TEST_KEY_ID, false)
        .await
        .expect("first resolution must succeed and cache at t0");

    // A second resolver sharing the same pool/mock, but whose clock reports
    // a time strictly after `fetched_at + ttl` -- simulating that the TTL
    // has elapsed since the first resolution, without mutating any shared
    // clock mid-test (`FixedClock` cannot be advanced in place).
    mock.queue_fetch_response(actor_document_response("PEM-AFTER-TTL"));
    let resolver_after_ttl =
        DbFederationPublicKeyResolver::new(app.pool.clone(), mock.clone(), fixed_clock_at(61), ttl);

    let resolved = resolver_after_ttl
        .resolve_public_key(TEST_KEY_ID, false)
        .await
        .expect("resolution after TTL elapsed must succeed via a fresh network fetch");

    assert_eq!(resolved.public_key_pem, "PEM-AFTER-TTL");
    assert_eq!(
        mock.fetched_urls().len(),
        2,
        "a resolution requested after the cached entry's TTL has elapsed must re-fetch \
         over the network instead of returning the stale cached value"
    );

    app.cleanup().await;
}

// --- Non-success HTTP status surfaces as an error and never caches ---

#[tokio::test]
async fn fetch_returning_a_non_success_status_surfaces_as_an_error_and_does_not_cache() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(HttpResponse {
        status: StatusCode::NOT_FOUND,
        headers: axum::http::HeaderMap::new(),
        body: b"not found".to_vec(),
    });
    let resolver = DbFederationPublicKeyResolver::new(
        app.pool.clone(),
        mock.clone(),
        fixed_clock_at(0),
        Duration::hours(24),
    );

    let result = resolver.resolve_public_key(TEST_KEY_ID, false).await;

    assert!(result.is_err());
    let cached = find_cached(&app.pool, TEST_KEY_ID)
        .await
        .expect("reading the cache must succeed");
    assert!(
        cached.is_none(),
        "a failed fetch must not leave a cache row behind"
    );

    app.cleanup().await;
}

// --- parse_public_key_document: pure unit tests, no DB/network involved ---

#[test]
fn parse_public_key_document_reads_pem_and_owner() {
    let body = json!({
        "id": TEST_ACTOR_URI,
        "publicKey": { "owner": TEST_ACTOR_URI, "publicKeyPem": "PEM" }
    })
    .to_string();

    let resolved =
        parse_public_key_document(TEST_KEY_ID, body.as_bytes()).expect("must parse successfully");

    assert_eq!(resolved.key_id, TEST_KEY_ID);
    assert_eq!(resolved.actor_uri, TEST_ACTOR_URI);
    assert_eq!(resolved.public_key_pem, "PEM");
}

#[test]
fn parse_public_key_document_falls_back_to_top_level_id_when_owner_is_absent() {
    let body = json!({
        "id": TEST_ACTOR_URI,
        "publicKey": { "publicKeyPem": "PEM" }
    })
    .to_string();

    let resolved =
        parse_public_key_document(TEST_KEY_ID, body.as_bytes()).expect("must parse successfully");

    assert_eq!(resolved.actor_uri, TEST_ACTOR_URI);
}

#[test]
fn parse_public_key_document_rejects_malformed_json() {
    let result = parse_public_key_document(TEST_KEY_ID, b"not json");

    assert!(result.is_err());
}

#[test]
fn parse_public_key_document_rejects_a_document_with_no_public_key_pem() {
    let body = json!({ "id": TEST_ACTOR_URI }).to_string();

    let result = parse_public_key_document(TEST_KEY_ID, body.as_bytes());

    assert!(result.is_err());
}

#[test]
fn parse_public_key_document_rejects_a_document_with_neither_owner_nor_id() {
    let body = json!({ "publicKey": { "publicKeyPem": "PEM" } }).to_string();

    let result = parse_public_key_document(TEST_KEY_ID, body.as_bytes());

    assert!(result.is_err());
}

#[test]
fn default_public_key_cache_ttl_is_twenty_four_hours() {
    assert_eq!(DEFAULT_PUBLIC_KEY_CACHE_TTL, Duration::hours(24));
}
