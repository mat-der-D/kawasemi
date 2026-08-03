//! Integration tests for `RemoteAccountFetcher` (Requirements 7.1, 7.2, 7.3,
//! 7.4, 7.5), per task 4's observable completion condition:
//! "`FederationHttpClient` モックで取得→正規化→キャッシュ保存が成立し、未知
//! プロパティ付き文書でも正規化が成功する統合テストが green".
//!
//! Mirrors `src/federation/signatures/key_resolver/tests.rs`'s established
//! convention: `spawn_test_app` for an isolated, already-migrated schema (so
//! `upsert_remote`/`find_remote_by_uri` exercise the real `remote_accounts`
//! table), paired with `MockFederationHttpClient` so every "fetches over the
//! network" assertion is deterministic without any real HTTP call.
//! `spawn_test_app`'s `RuntimeContext` uses a `FixedClock` (always returns
//! the same fixed instant), so staleness is exercised by directly seeding a
//! cached row's `fetched_at` relative to that fixed "now" -- never by
//! advancing a clock mid-test.

use serde_json::json;

use super::{DEFAULT_REMOTE_ACCOUNT_CACHE_TTL, RemoteAccountFetcher};
use crate::accounts::model::RemoteAccount;
use crate::accounts::remote_repository::{find_remote_by_uri, upsert_remote};
use crate::federation::signatures::{HttpResponse, MockFederationHttpClient};
use crate::test_harness::spawn_test_app;
use axum::http::{HeaderMap, StatusCode};

const ACTOR_URI: &str = "https://remote.example/users/alice";

fn ok_response(body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: serde_json::to_vec(&body).expect("test fixture body must serialize"),
    }
}

/// A conventional, fully-populated ActivityPub actor document for
/// `ACTOR_URI`, including one unrecognized vendor extension property
/// (Requirement 7.5's own scenario) and one unrecognized nested property
/// inside `icon`.
fn full_actor_document() -> serde_json::Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": ACTOR_URI,
        "type": "Person",
        "preferredUsername": "alice",
        "name": "Alice Example",
        "summary": "Hello from remote.example.",
        "url": "https://remote.example/@alice",
        "inbox": "https://remote.example/users/alice/inbox",
        "outbox": "https://remote.example/users/alice/outbox",
        "manuallyApprovesFollowers": true,
        "icon": {"type": "Image", "mediaType": "image/png", "url": "https://remote.example/avatars/alice.png"},
        "image": {"type": "Image", "url": "https://remote.example/headers/alice.png"},
        "attachment": [
            {"type": "PropertyValue", "name": "Pronouns", "value": "she/her"},
            {"type": "PropertyValue", "name": "Website", "value": "https://alice.example"},
        ],
        "x-vendor-specific-extension": {"anything": ["goes", "here"]},
    })
}

/// Requirement 7.1, 7.2, 7.5: a cache-miss triggers exactly one fetch, and
/// the fetched document (with an unrecognized vendor extension property)
/// normalizes successfully into every expected `RemoteAccount` field, which
/// is then persisted (visible via `find_remote_by_uri`).
#[tokio::test]
async fn cache_miss_fetches_normalizes_and_upserts_even_with_unknown_properties() {
    let app = spawn_test_app().await;
    let mock = std::sync::Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(full_actor_document()));

    let fetcher = RemoteAccountFetcher::new(
        app.pool.clone(),
        mock.clone(),
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );

    let account = fetcher
        .fetch_and_normalize(ACTOR_URI)
        .await
        .expect("a fully-populated document must normalize successfully");

    assert_eq!(account.actor_uri, ACTOR_URI);
    assert_eq!(account.username, "alice");
    assert_eq!(account.domain, "remote.example");
    assert_eq!(account.display_name, "Alice Example");
    assert_eq!(account.note, "Hello from remote.example.");
    assert_eq!(account.url, "https://remote.example/@alice");
    assert_eq!(
        account.avatar_url.as_deref(),
        Some("https://remote.example/avatars/alice.png")
    );
    assert_eq!(
        account.header_url.as_deref(),
        Some("https://remote.example/headers/alice.png")
    );
    assert!(!account.bot);
    assert!(account.locked);
    assert_eq!(account.fields.len(), 2);
    assert_eq!(account.fields[0].name, "Pronouns");
    assert_eq!(account.fields[0].value, "she/her");
    assert_eq!(account.fields[1].name, "Website");

    assert_eq!(mock.fetched_urls().len(), 1);
    assert_eq!(mock.fetched_urls()[0].0, ACTOR_URI);

    let persisted = find_remote_by_uri(&app.pool, ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed")
        .expect("the normalized account must have been upserted into the cache");
    assert_eq!(persisted.id, account.id);
    assert_eq!(persisted.display_name, "Alice Example");

    app.cleanup().await;
}

/// Requirement 7.5: `type: "Service"` normalizes to `bot: true`, matching
/// this crate's own established `ActorType::Service` == bot convention
/// (`src/accounts/serializer.rs`'s doc comment).
#[tokio::test]
async fn service_actor_type_normalizes_to_bot_true() {
    let app = spawn_test_app().await;
    let mock = std::sync::Arc::new(MockFederationHttpClient::new());
    let mut document = full_actor_document();
    document["type"] = json!("Service");
    mock.queue_fetch_response(ok_response(document));

    let fetcher = RemoteAccountFetcher::new(
        app.pool.clone(),
        mock,
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );

    let account = fetcher
        .fetch_and_normalize(ACTOR_URI)
        .await
        .expect("a Service-typed document must still normalize successfully");
    assert!(account.bot);

    app.cleanup().await;
}

/// Requirement 7.3: a fresh (non-stale) cache entry is returned as-is, with
/// no `FederationHttpClient::fetch` call at all.
#[tokio::test]
async fn fresh_cache_entry_skips_fetch_entirely() {
    let app = spawn_test_app().await;
    let mock = std::sync::Arc::new(MockFederationHttpClient::new());
    // Deliberately no queued fetch outcome: a fetch attempt would return
    // MockFederationHttpClient's own "no queued fetch() outcome" error,
    // which this test's assertions below would surface as a failure.

    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let cached = RemoteAccount {
        id,
        actor_uri: ACTOR_URI.to_string(),
        username: "alice".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Cached Alice".to_string(),
        note: "already cached".to_string(),
        url: "https://remote.example/@alice".to_string(),
        avatar_url: None,
        header_url: None,
        fields: Vec::new(),
        bot: false,
        locked: false,
        fetched_at: now,
    };
    upsert_remote(&app.pool, &cached)
        .await
        .expect("seeding the cache must succeed");

    let fetcher = RemoteAccountFetcher::new(
        app.pool.clone(),
        mock.clone(),
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );

    let account = fetcher
        .fetch_and_normalize(ACTOR_URI)
        .await
        .expect("a fresh cache entry must be returned without fetching");

    assert_eq!(account.display_name, "Cached Alice");
    assert!(mock.fetched_urls().is_empty());

    app.cleanup().await;
}

/// Requirement 7.1, 7.3: a stale cache entry (older than the TTL) triggers a
/// real fetch, and the cache is refreshed with the newly fetched values.
#[tokio::test]
async fn stale_cache_entry_triggers_a_fetch_and_refreshes_the_cache() {
    let app = spawn_test_app().await;
    let mock = std::sync::Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(full_actor_document()));

    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let stale_fetched_at = now - time::Duration::hours(48);
    let cached = RemoteAccount {
        id,
        actor_uri: ACTOR_URI.to_string(),
        username: "alice".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Stale Alice".to_string(),
        note: "stale".to_string(),
        url: "https://remote.example/@alice".to_string(),
        avatar_url: None,
        header_url: None,
        fields: Vec::new(),
        bot: false,
        locked: false,
        fetched_at: stale_fetched_at,
    };
    upsert_remote(&app.pool, &cached)
        .await
        .expect("seeding the stale cache row must succeed");

    let fetcher = RemoteAccountFetcher::new(
        app.pool.clone(),
        mock.clone(),
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );

    let account = fetcher
        .fetch_and_normalize(ACTOR_URI)
        .await
        .expect("a stale cache entry must be refreshed via a real fetch");

    assert_eq!(mock.fetched_urls().len(), 1);
    assert_eq!(account.display_name, "Alice Example");
    // `upsert_remote`'s own established `id`-stability contract (task 2.2):
    // a re-upsert for the same `actor_uri` keeps the original row `id`.
    assert_eq!(account.id, id);

    let persisted = find_remote_by_uri(&app.pool, ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed")
        .expect("the refreshed account must remain cached");
    assert_eq!(persisted.display_name, "Alice Example");
    assert_eq!(persisted.id, id);

    app.cleanup().await;
}

/// Requirement 7.4: a missing required `preferredUsername` property fails
/// with an `AppError`, and no cache row is created.
#[tokio::test]
async fn missing_preferred_username_fails_and_upserts_nothing() {
    let app = spawn_test_app().await;
    let mock = std::sync::Arc::new(MockFederationHttpClient::new());
    let mut document = full_actor_document();
    document
        .as_object_mut()
        .expect("test fixture must be a JSON object")
        .remove("preferredUsername");
    mock.queue_fetch_response(ok_response(document));

    let fetcher = RemoteAccountFetcher::new(
        app.pool.clone(),
        mock,
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );

    let error = fetcher
        .fetch_and_normalize(ACTOR_URI)
        .await
        .expect_err("a document missing preferredUsername must fail normalization");
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);

    let persisted = find_remote_by_uri(&app.pool, ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed");
    assert!(
        persisted.is_none(),
        "no account should be cached when required-property validation fails"
    );

    app.cleanup().await;
}

/// Requirement 7.4: a missing required `id`/`type` property (enforced by
/// `parse_activity` itself, upstream of this module's own
/// `preferredUsername` check) also fails, and upserts nothing.
#[tokio::test]
async fn missing_type_property_fails_via_jsonld_required_property_validation() {
    let app = spawn_test_app().await;
    let mock = std::sync::Arc::new(MockFederationHttpClient::new());
    let mut document = full_actor_document();
    document
        .as_object_mut()
        .expect("test fixture must be a JSON object")
        .remove("type");
    mock.queue_fetch_response(ok_response(document));

    let fetcher = RemoteAccountFetcher::new(
        app.pool.clone(),
        mock,
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );

    let error = fetcher
        .fetch_and_normalize(ACTOR_URI)
        .await
        .expect_err("a document missing 'type' must fail via parse_activity");
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);

    let persisted = find_remote_by_uri(&app.pool, ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed");
    assert!(persisted.is_none());

    app.cleanup().await;
}

/// Requirement 7.4: a non-success HTTP status on fetch fails with a
/// caller-facing 404, and upserts nothing.
#[tokio::test]
async fn non_success_fetch_status_fails_as_not_found() {
    let app = spawn_test_app().await;
    let mock = std::sync::Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(HttpResponse {
        status: StatusCode::NOT_FOUND,
        headers: HeaderMap::new(),
        body: Vec::new(),
    });

    let fetcher = RemoteAccountFetcher::new(
        app.pool.clone(),
        mock,
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );

    let error = fetcher
        .fetch_and_normalize(ACTOR_URI)
        .await
        .expect_err("a non-success upstream status must fail fetch_and_normalize");
    assert_eq!(error.status, StatusCode::NOT_FOUND);

    let persisted = find_remote_by_uri(&app.pool, ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed");
    assert!(persisted.is_none());

    app.cleanup().await;
}

/// Requirement 7.4: a network-level fetch failure (the
/// `FederationHttpClient` call itself returning `Err`) propagates as a
/// failure, and upserts nothing.
#[tokio::test]
async fn fetch_transport_failure_propagates_and_upserts_nothing() {
    let app = spawn_test_app().await;
    let mock = std::sync::Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_error(StatusCode::BAD_GATEWAY, "connection refused");

    let fetcher = RemoteAccountFetcher::new(
        app.pool.clone(),
        mock,
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );

    let error = fetcher
        .fetch_and_normalize(ACTOR_URI)
        .await
        .expect_err("a transport-level fetch failure must propagate");
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);

    let persisted = find_remote_by_uri(&app.pool, ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed");
    assert!(persisted.is_none());

    app.cleanup().await;
}
