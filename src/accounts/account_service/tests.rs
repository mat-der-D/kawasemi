//! Service-level tests for `AccountService` (task 5.1), per this task's own
//! observable completion condition: "ローカル/リモートいずれも Account を
//! 返し、未存在で 404 を返すサービス単位テストが green".
//!
//! Mirrors `src/accounts/remote_fetcher/tests.rs`'s/
//! `src/accounts/profile_repository/tests.rs`'s established convention:
//! `spawn_test_app` for an isolated, already-migrated schema and a
//! deterministic `RuntimeContext`, a real owner + local actor row
//! (`create_test_actor`, copied from `profile_repository/tests.rs`'s own
//! helper — these are integration-style tests exercising the real
//! `ActorDirectory`/repositories against Postgres, "サービス単位" in the
//! sense of task 5.1's own boundary — `AccountService` — not in the sense of
//! "no database at all"), and `MockFederationHttpClient` so the
//! needs-fetching path is deterministic without any real network call.

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use time::OffsetDateTime;

use super::AccountService;
use crate::accounts::model::{ProfileField, RemoteAccount};
use crate::accounts::ports::AccountPortsRegistry;
use crate::accounts::remote_fetcher::{DEFAULT_REMOTE_ACCOUNT_CACHE_TTL, RemoteAccountFetcher};
use crate::accounts::remote_repository::upsert_remote;
use crate::accounts::serializer::AccountSerializer;
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::api::pagination::ForwardedOrigin;
use crate::domain::Id;
use crate::federation::signatures::MockFederationHttpClient;
use crate::media::local_fs::LocalFsStore;
use crate::oauth::model::{RequestActorContext, ScopeSet};
use crate::test_harness::{TestApp, spawn_test_app};

/// Creates a real owner + local actor row, returning the actor's `Id` — an
/// exact copy of `profile_repository/tests.rs::create_test_actor` (this
/// module's own tests need the identical real-actor shape `ActorDirectory`
/// resolves against).
async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
    let actor = LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = app
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("insert_actor must succeed");
    tx.commit().await.expect("committing must succeed");

    actor_id
}

/// A `LocalFsStore` never actually touched: `MediaStore::public_url` (the
/// only method these tests exercise, transitively via `AccountSerializer`)
/// never reads/writes the filesystem — mirrors
/// `src/accounts/serializer/tests.rs`'s identical precedent.
fn store() -> LocalFsStore {
    LocalFsStore::new(PathBuf::from(
        "/nonexistent-kawasemi-account-service-test-root",
    ))
}

fn origin() -> ForwardedOrigin {
    ForwardedOrigin::resolve("https", "kawasemi.example", None, None)
}

/// Builds an `AccountService` against `app`'s own pool/`ActorDirectory`,
/// paired with `mock` as the `FederationHttpClient` `RemoteAccountFetcher`
/// fetches through.
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
    AccountService::new(
        app.pool.clone(),
        Arc::clone(app.actor.directory()),
        fetcher,
        AccountSerializer::new("kawasemi.example"),
        AccountPortsRegistry::new(),
        store(),
    )
}

fn sample_remote_account(id: Id, actor_uri: &str, fetched_at: OffsetDateTime) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "alice".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Alice".to_string(),
        note: "Hello from remote.example.".to_string(),
        url: "https://remote.example/@alice".to_string(),
        avatar_url: None,
        header_url: None,
        fields: vec![ProfileField {
            name: "Pronouns".to_string(),
            value: "she/her".to_string(),
            verified_at: None,
        }],
        bot: false,
        locked: false,
        fetched_at,
    }
}

/// Requirement 3.1: a bare numeric id resolving to a local actor is
/// returned as an Account JSON (not a CredentialAccount — no `source`/`role`
/// keys).
#[tokio::test]
async fn show_account_returns_an_account_for_a_local_actor() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "alice").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, mock);

    let account = svc
        .show_account(&actor_id.as_i64().to_string(), None, &origin())
        .await
        .expect("a real local actor must resolve to an Account");

    assert_eq!(account["id"], actor_id.as_i64().to_string());
    assert_eq!(account["username"], "alice");
    assert_eq!(account["acct"], "alice");
    assert!(!account["avatar"].as_str().unwrap().is_empty());
    assert!(account.get("source").is_none());

    app.cleanup().await;
}

/// Requirement 3.2: an id resolving to an already-cached (`remote_accounts`)
/// remote account is returned as an Account JSON, without any network fetch
/// (Requirement 7.3's "有効に保持されている間...再取得を行わない", exercised
/// here through `show_account`'s own numeric-id path rather than
/// `RemoteAccountFetcher` directly).
#[tokio::test]
async fn show_account_returns_an_account_for_a_known_cached_remote_account() {
    let app = spawn_test_app().await;
    let remote_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let remote = sample_remote_account(remote_id, "https://remote.example/users/alice", now);
    upsert_remote(&app.pool, &remote)
        .await
        .expect("seeding the cached remote account must succeed");

    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, Arc::clone(&mock));

    let account = svc
        .show_account(&remote_id.as_i64().to_string(), None, &origin())
        .await
        .expect("a cached remote account must resolve to an Account");

    assert_eq!(account["acct"], "alice@remote.example");
    assert_eq!(account["username"], "alice");
    assert!(mock.fetched_urls().is_empty(), "a cache hit must not fetch");

    app.cleanup().await;
}

/// Requirement 3.3: an id matching neither a local actor nor a cached
/// remote account is a Mastodon-compatible 404.
#[tokio::test]
async fn show_account_returns_404_for_an_unknown_numeric_id() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, mock);

    let err = svc
        .show_account("999999999999", None, &origin())
        .await
        .expect_err("an id matching nothing must fail");
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirement 3.3 (needs-fetching path): a non-numeric id is treated as a
/// remote `actor_uri` reference and handed to `RemoteAccountFetcher`
/// (Requirement 7.1's "必要時フェッチ"); a fetch failure (queued 404 here)
/// still surfaces as this same 404, and the fetch was actually attempted
/// (proving this is the "needs fetching", not merely "unknown", branch).
#[tokio::test]
async fn show_account_attempts_a_fetch_for_a_non_numeric_id_and_maps_failure_to_404() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_error(StatusCode::NOT_FOUND, "gone");
    let svc = service(&app, Arc::clone(&mock));

    let uri = "https://remote.example/users/ghost";
    let err = svc
        .show_account(uri, None, &origin())
        .await
        .expect_err("a fetch failure must surface as an AppError");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    assert_eq!(mock.fetched_urls(), vec![(uri.to_string(), None)]);

    app.cleanup().await;
}

/// Requirement 2.1, 2.2: `verify_credentials` resolves the token-bound
/// actor into a CredentialAccount — every Account field plus `source`/
/// `role`.
#[tokio::test]
async fn verify_credentials_returns_a_credential_account_with_source_and_role() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "bob").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, mock);

    let ctx = RequestActorContext {
        actor_id,
        scopes: ScopeSet::new(["read:accounts"]),
    };

    let credential = svc
        .verify_credentials(&ctx, &origin())
        .await
        .expect("a real token-bound actor must resolve to a CredentialAccount");

    assert_eq!(credential["username"], "bob");
    assert_eq!(credential["acct"], "bob");
    assert!(credential.get("source").is_some());
    assert!(credential.get("role").is_some());
    assert_eq!(credential["source"]["follow_requests_count"], 0);

    app.cleanup().await;
}

/// Defensive path: a `RequestActorContext` naming an actor id that does not
/// exist (e.g. a stale token) fails rather than panicking.
#[tokio::test]
async fn verify_credentials_fails_for_an_actor_that_no_longer_exists() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, mock);

    let ctx = RequestActorContext {
        actor_id: Id::from_i64(123_456_789),
        scopes: ScopeSet::new(["read:accounts"]),
    };

    let err = svc
        .verify_credentials(&ctx, &origin())
        .await
        .expect_err("a nonexistent actor id must fail, not panic");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}
