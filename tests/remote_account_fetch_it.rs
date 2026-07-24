//! Integration test proving task 7.2's own observable completion condition
//! (`.kiro/specs/accounts-and-instance/tasks.md`, "7.2 instance v2 /
//! custom_emojis とリモート取得の統合テストを通す"): "`FederationHttpClient`
//! モックでのリモート取得→正規化→Account→キャッシュ再利用→取得失敗の互換応答
//! を検証" (Requirements 7.1, 7.2, 7.3, 7.4).
//!
//! design.md's File Structure Plan names this exact filename
//! (`remote_account_fetch_it.rs`: "リモート取得→正規化→Account・キャッシュ・
//! 取得失敗（統合, FederationHttpClient モック）") and `account_show_it.rs`'s
//! own doc comment explicitly defers "known-remote"/needs-fetching coverage
//! for `accounts/:id` to this file ("Requirement 3.2, the known-remote half,
//! is not in task 7.1's own `_Requirements_` list — that is
//! `remote_account_fetch_it.rs`'s concern, task 7.2").
//!
//! ## Why this drives `AccountService::show_account` directly, not the
//! mounted HTTP router
//! `crate::test_harness::spawn_test_app`'s real, mounted
//! `AccountsEndpointsState`/`AccountsModule` are monomorphized over the one
//! concrete production `FederationHttpClient`
//! (`crate::federation::signatures::ReqwestFederationHttpClient`,
//! `src/accounts.rs::AccountsModule`'s own field type) — there is no seam in
//! the composition root to substitute a mock `FederationHttpClient` for a
//! request that reaches the real, running listener, and adding one is out of
//! this task's boundary (tests-only, per the assignment). Reaching a real,
//! unmocked remote host over the network to exercise this path would violate
//! this task's own determinism/no-real-network-calls constraint. This file
//! therefore builds its own `AccountService<LocalFsStore,
//! MockFederationHttpClient>` directly (every constructor it needs —
//! `AccountService::new`/`RemoteAccountFetcher::new`/`AccountSerializer::new`/
//! `AccountPortsRegistry::new`/`MediaService::new`/`LocalFsStore::new` — is
//! `pub`), reusing `spawn_test_app`'s real, migrated Postgres schema and
//! deterministic `RuntimeContext` for the `RemoteAccountRepository`/
//! `RemoteAccountFetcher` calls underneath `show_account`. This mirrors the
//! exact same already-reviewed convention
//! `src/accounts/account_service/tests.rs` (task 5.1/5.4) and
//! `src/accounts/remote_fetcher/tests.rs` (task 4) already established for
//! this identical class of test — this file adds the specific scenarios task
//! 7.2 names (a genuinely fresh successful fetch producing a
//! contract-correct Account, a same-`actor_uri` cache-reuse call count
//! assertion, and a fetch-failure's Mastodon-compatible-error-body
//! assertion) that those two files do not already cover, rather than
//! duplicating their existing coverage.

use std::path::PathBuf;

use axum::body::to_bytes;
use axum::http::StatusCode;
use serde_json::Value;

use kawasemi::accounts::model::RemoteAccount;
use kawasemi::accounts::remote_repository::find_remote_by_uri;
use kawasemi::accounts::{
    AccountPortsRegistry, AccountSerializer, AccountService, DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    RemoteAccountFetcher,
};
use kawasemi::api::pagination::ForwardedOrigin;
use kawasemi::config::MediaConfig;
use kawasemi::error::AppError;
use kawasemi::federation::signatures::{HttpResponse, MockFederationHttpClient};
use kawasemi::media::local_fs::LocalFsStore;
use kawasemi::media::service::MediaService;
use kawasemi::test_harness::{TestApp, spawn_test_app};
use std::sync::Arc;

const ACTOR_URI: &str = "https://remote.example/users/alice";

fn origin() -> ForwardedOrigin {
    ForwardedOrigin::resolve("https", "kawasemi.example", None, None)
}

/// A `LocalFsStore` never actually touched — this file's `AccountService`
/// never exercises avatar/header upload ingestion (`update_credentials` is
/// out of this task's own scope), mirroring
/// `src/accounts/account_service/tests.rs::store`'s identical precedent.
fn store() -> LocalFsStore {
    LocalFsStore::new(PathBuf::from(
        "/nonexistent-kawasemi-remote-fetch-it-test-root",
    ))
}

fn media_config() -> MediaConfig {
    MediaConfig {
        storage_root: PathBuf::from("/nonexistent-kawasemi-remote-fetch-it-test-root/media"),
        max_upload_size_bytes: 1024 * 1024,
        thumbnail_target_width: 400,
        thumbnail_target_height: 400,
        supported_formats: vec!["image/png".to_string(), "image/jpeg".to_string()],
        worker_concurrency: 1,
        max_retry_attempts: 5,
        lease_duration: std::time::Duration::from_secs(5 * 60),
    }
}

/// Builds an `AccountService` against `app`'s own real pool/`ActorDirectory`/
/// `RuntimeContext`, paired with `mock` as the `FederationHttpClient`
/// `RemoteAccountFetcher` fetches through — mirrors
/// `src/accounts/account_service/tests.rs::service`'s identical helper.
fn service(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
) -> AccountService<LocalFsStore, MockFederationHttpClient> {
    let fetcher = RemoteAccountFetcher::new(
        app.pool.clone(),
        mock,
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    );
    let media = Arc::new(MediaService::new(
        app.pool.clone(),
        app.runtime.clone(),
        media_config(),
        store(),
    ));
    AccountService::new(
        app.pool.clone(),
        Arc::clone(app.actor.directory()),
        fetcher,
        AccountSerializer::new("kawasemi.example"),
        AccountPortsRegistry::new(),
        store(),
        media,
        app.runtime.clone(),
    )
}

/// A conventional, fully-populated ActivityPub actor document for
/// `ACTOR_URI` — mirrors `src/accounts/remote_fetcher/tests.rs::
/// full_actor_document`'s own fixture shape.
fn full_actor_document() -> Value {
    serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": ACTOR_URI,
        "type": "Person",
        "preferredUsername": "alice",
        "name": "Alice Example",
        "summary": "Hello from remote.example.",
        "url": "https://remote.example/@alice",
        "inbox": "https://remote.example/users/alice/inbox",
        "outbox": "https://remote.example/users/alice/outbox",
        "manuallyApprovesFollowers": false,
        "icon": {"type": "Image", "url": "https://remote.example/avatars/alice.png"},
        "image": {"type": "Image", "url": "https://remote.example/headers/alice.png"},
        "attachment": [
            {"type": "PropertyValue", "name": "Pronouns", "value": "she/her"},
        ],
    })
}

fn ok_response(body: Value) -> HttpResponse {
    HttpResponse {
        status: StatusCode::OK,
        headers: axum::http::HeaderMap::new(),
        body: serde_json::to_vec(&body).expect("test fixture body must serialize"),
    }
}

/// Reads an `AppError`'s Mastodon-compatible `{"error": ...}` response body
/// (Requirement 10.3, via `AppError`'s own `IntoResponse` -> `mastodon_error_body`
/// conversion, `src/error.rs`) — mirrors `src/error/tests.rs`'s own
/// `to_bytes`-based body-reading convention.
async fn error_body_json(error: AppError) -> Value {
    let response = axum::response::IntoResponse::into_response(error);
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "expected the response status to already be 404"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("reading the response body must succeed");
    serde_json::from_slice(&bytes).expect("the error body must be valid JSON")
}

// ==========================================================================
// (1) Successful fetch -> normalize -> contract-correct Account JSON
// (Requirements 7.1, 7.2)
// ==========================================================================

#[tokio::test]
async fn show_account_fetches_normalizes_and_returns_a_contract_correct_account_for_a_non_numeric_id()
 {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(full_actor_document()));
    let svc = service(&app, Arc::clone(&mock));

    let account = svc
        .show_account(ACTOR_URI, None, &origin())
        .await
        .expect("a non-numeric id (actor_uri) with a queued successful fetch must resolve");

    // Requirements 1.1-1.3: the unified Account contract, remote acct
    // discipline (`username@domain`).
    assert_eq!(account["username"], "alice");
    assert_eq!(account["acct"], "alice@remote.example");
    assert_eq!(account["display_name"], "Alice Example");
    assert_eq!(account["note"], "Hello from remote.example.");
    assert_eq!(account["url"], "https://remote.example/@alice");
    assert_eq!(account["uri"], ACTOR_URI);
    assert_eq!(account["locked"], false);
    assert_eq!(account["bot"], false);
    assert!(account["id"].as_str().is_some());
    assert!(account["created_at"].as_str().is_some());

    // Requirement 1.5: avatar/header are never null, and reflect the
    // normalized remote URLs (not the default placeholder) since this
    // document supplied both.
    assert_eq!(
        account["avatar"].as_str(),
        Some("https://remote.example/avatars/alice.png")
    );
    assert_eq!(account["avatar_static"], account["avatar"]);
    assert_eq!(
        account["header"].as_str(),
        Some("https://remote.example/headers/alice.png")
    );
    assert_eq!(account["header_static"], account["header"]);

    // Requirement 7.2: `fields` normalized from the document's `attachment`.
    let fields = account["fields"]
        .as_array()
        .expect("fields must be an array");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0]["name"], "Pronouns");
    assert_eq!(fields[0]["value"], "she/her");

    // A plain Account, never a CredentialAccount (remote accounts have no
    // `source`/`role`).
    assert!(account.get("source").is_none());
    assert!(account.get("role").is_none());

    // Exactly one network fetch happened (Requirement 7.1).
    assert_eq!(mock.fetched_urls().len(), 1);
    assert_eq!(mock.fetched_urls()[0].0, ACTOR_URI);

    // Requirement 7.2: the normalized result was cached.
    let persisted = find_remote_by_uri(&app.pool, ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed")
        .expect("a successful fetch must upsert the normalized account into the cache");
    assert_eq!(persisted.username, "alice");

    app.cleanup().await;
}

// ==========================================================================
// (2) Cache reuse: a second `show_account` call for the same actor_uri
// within the TTL must not re-fetch (Requirement 7.3)
// ==========================================================================

#[tokio::test]
async fn show_account_reuses_the_cache_on_a_second_call_and_fetches_only_once() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(full_actor_document()));
    let svc = service(&app, Arc::clone(&mock));

    let first = svc
        .show_account(ACTOR_URI, None, &origin())
        .await
        .expect("the first call (cache miss) must fetch and succeed");
    assert_eq!(mock.fetched_urls().len(), 1, "the first call must fetch");

    // Deliberately no second queued fetch outcome: if `show_account` fetched
    // again here, `MockFederationHttpClient::fetch` would return its own
    // "no queued fetch() outcome" error, which would surface as a failure
    // below instead of a second success.
    let second = svc
        .show_account(ACTOR_URI, None, &origin())
        .await
        .expect("the second call within the TTL must reuse the cache, not fail");

    assert_eq!(
        mock.fetched_urls().len(),
        1,
        "a second show_account call for the same actor_uri within the TTL must not fetch again"
    );
    assert_eq!(first["acct"], second["acct"]);
    assert_eq!(
        first["id"], second["id"],
        "the cached row's id must be stable across re-reads"
    );

    app.cleanup().await;
}

// ==========================================================================
// (3) Fetch failure -> Mastodon-compatible error response (Requirement 7.4)
// ==========================================================================

#[tokio::test]
async fn show_account_maps_a_non_success_fetch_status_to_a_mastodon_compatible_404() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(HttpResponse {
        status: StatusCode::NOT_FOUND,
        headers: axum::http::HeaderMap::new(),
        body: Vec::new(),
    });
    let svc = service(&app, Arc::clone(&mock));

    let ghost_uri = "https://remote.example/users/ghost";
    let error = svc
        .show_account(ghost_uri, None, &origin())
        .await
        .expect_err("a non-success upstream fetch status must fail show_account");
    assert_eq!(error.status, StatusCode::NOT_FOUND);

    let body = error_body_json(error).await;
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );

    // No cache row was created for the failed fetch.
    let persisted = find_remote_by_uri(&app.pool, ghost_uri)
        .await
        .expect("find_remote_by_uri must succeed");
    assert!(
        persisted.is_none(),
        "a fetch failure must not upsert anything into the cache"
    );

    app.cleanup().await;
}

/// Requirement 7.4: a transport-level failure (the `FederationHttpClient`
/// call itself returning `Err`, e.g. a connection failure) also surfaces as
/// a Mastodon-compatible error response through `show_account`, not merely
/// through `RemoteAccountFetcher` directly (already covered at that lower
/// layer by `src/accounts/remote_fetcher/tests.rs`).
#[tokio::test]
async fn show_account_maps_a_transport_level_fetch_failure_to_a_mastodon_compatible_error() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_error(StatusCode::BAD_GATEWAY, "connection refused");
    let svc = service(&app, Arc::clone(&mock));

    let unreachable_uri = "https://remote.example/users/unreachable";
    let error = svc
        .show_account(unreachable_uri, None, &origin())
        .await
        .expect_err("a transport-level fetch failure must fail show_account");
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);

    // Render through the same `AppError` -> Mastodon-compatible conversion
    // path (Requirement 10.3), without asserting a fixed status this time.
    let response = axum::response::IntoResponse::into_response(error);
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("reading the response body must succeed");
    let body: Value = serde_json::from_slice(&bytes).expect("the error body must be valid JSON");
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );

    let persisted = find_remote_by_uri(&app.pool, unreachable_uri)
        .await
        .expect("find_remote_by_uri must succeed");
    assert!(persisted.is_none());

    app.cleanup().await;
}

/// Sanity check that this file's helper (`service`) really does force a
/// `MockFederationHttpClient` under `show_account`'s needs-fetching path
/// (rather than accidentally reusing a cached row from an earlier test in
/// this file, which would make the "successful fetch" assertions above
/// vacuous): seeding a cache row directly and calling `show_account` for its
/// exact `actor_uri` returns the cached data without ever calling
/// `MockFederationHttpClient::fetch` at all — the same "no queued outcome
/// needed" cache-hit shape `src/accounts/remote_fetcher/tests.rs::
/// fresh_cache_entry_skips_fetch_entirely` already established one layer
/// down, reproduced here through `show_account` itself.
#[tokio::test]
async fn show_account_returns_a_pre_seeded_cache_entry_without_fetching() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, Arc::clone(&mock));

    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let seeded = RemoteAccount {
        id,
        actor_uri: ACTOR_URI.to_string(),
        username: "alice".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Already Cached Alice".to_string(),
        note: "pre-seeded".to_string(),
        url: "https://remote.example/@alice".to_string(),
        avatar_url: None,
        header_url: None,
        fields: Vec::new(),
        bot: false,
        locked: false,
        fetched_at: now,
    };
    kawasemi::accounts::remote_repository::upsert_remote(&app.pool, &seeded)
        .await
        .expect("seeding the cache must succeed");

    let account = svc
        .show_account(ACTOR_URI, None, &origin())
        .await
        .expect("a pre-seeded cache entry must resolve without fetching");
    assert_eq!(account["display_name"], "Already Cached Alice");
    assert!(mock.fetched_urls().is_empty(), "a cache hit must not fetch");

    app.cleanup().await;
}
