//! Service-level tests for `AccountService` (tasks 5.1/5.4), per those tasks'
//! own observable completion conditions: task 5.1's "ローカル/リモートいずれ
//! も Account を返し、未存在で 404 を返すサービス単位テストが green" and
//! task 5.4's "部分更新が verify_credentials/accounts/:id に反映され、検証
//! 違反で 422 を返すテストが green".
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
//!
//! ## Task 5.4's own media fixture
//! [`service`]/[`service_with_ports`] now also build a real
//! `MediaService<LocalFsStore>` (task 5.4's new `AccountService` field) —
//! backed by [`store`]'s same never-actually-touched nonexistent path, which
//! is fine for every test that never exercises `update_credentials`'
//! avatar/header ingestion path (`MediaService::accept_upload` is simply
//! never called by those tests). The one test that *does* exercise avatar
//! ingestion ([`update_credentials_ingests_an_avatar_upload_via_media_service`])
//! instead uses [`writable_media_fixture`], a real temp-directory-backed
//! `LocalFsStore` (mirroring `src/media/local_fs.rs`'s own test module's
//! "counter + nanos" unique-temp-root convention, copied here rather than
//! reused since that helper is private to `local_fs.rs`'s own test module).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use time::OffsetDateTime;

use super::{
    AccountService, MAX_PROFILE_FIELDS, MediaUploadInput, ProfileFieldInput, StatusesQueryInput,
    UpdateCredentialsInput,
};
use crate::accounts::model::{ProfileField, RelationshipView, RemoteAccount};
use crate::accounts::ports::{
    AccountPortsRegistry, AccountStatusesProvider, RelationshipStateProvider, StatusesQuery,
};
use crate::accounts::profile_repository::find_profile;
use crate::accounts::remote_fetcher::{DEFAULT_REMOTE_ACCOUNT_CACHE_TTL, RemoteAccountFetcher};
use crate::accounts::remote_repository::upsert_remote;
use crate::accounts::serializer::AccountSerializer;
use crate::actor::model::{ActorState, ActorType, Handle, LocalActor};
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::api::pagination::{ForwardedOrigin, Page, PageParams};
use crate::config::MediaConfig;
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::federation::signatures::MockFederationHttpClient;
use crate::media::local_fs::LocalFsStore;
use crate::media::media_repository::find_owned;
use crate::media::service::MediaService;
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

/// A `MediaConfig` accepting `image/png`/`image/jpeg` uploads up to 1 MiB —
/// mirrors `src/media/service/tests.rs::sample_config`'s own shape, reused
/// here rather than redefined from scratch (same field set, different
/// values chosen only where this module's own tests need them, e.g. a
/// generous `max_upload_size_bytes` for the small fixture payloads these
/// tests upload).
fn media_config(storage_root: PathBuf) -> MediaConfig {
    MediaConfig {
        storage_root,
        max_upload_size_bytes: 1024 * 1024,
        thumbnail_target_width: 400,
        thumbnail_target_height: 400,
        supported_formats: vec!["image/png".to_string(), "image/jpeg".to_string()],
        worker_concurrency: 1,
        max_retry_attempts: 5,
        lease_duration: std::time::Duration::from_secs(5 * 60),
    }
}

/// Builds a `MediaService<LocalFsStore>` against `app`'s own pool/runtime,
/// backed by `store`. `store` is never actually written to unless a test
/// calls `AccountService::update_credentials` with an avatar/header upload —
/// see this module's doc comment ("Task 5.4's own media fixture").
fn media_service(app: &TestApp, store: LocalFsStore) -> Arc<MediaService<LocalFsStore>> {
    Arc::new(MediaService::new(
        app.pool.clone(),
        app.runtime.clone(),
        media_config(PathBuf::from(
            "/nonexistent-kawasemi-account-service-test-root/media",
        )),
        store,
    ))
}

/// Builds a process-unique temp directory path — copied from
/// `src/media/local_fs.rs`'s own test module's identical helper (private to
/// that module, so not reusable directly from here) — so concurrently
/// running tests never collide on the same on-disk root.
fn unique_temp_root(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "kawasemi_account_service_test_{label}_{nanos}_{seq}"
    ))
}

/// Best-effort cleanup of a temp root a test created, regardless of whether
/// the test body panicked — copied from `src/media/local_fs.rs`'s own test
/// module's identical `TempDirGuard`.
struct TempDirGuard(PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A real, writable temp-directory-backed `LocalFsStore` + the
/// `MediaService<LocalFsStore>` built on top of it, for the one test that
/// actually exercises `update_credentials`' avatar/header ingestion path
/// (unlike [`media_service`]'s own nonexistent-path store, which is never
/// touched). The returned guard removes the temp directory on drop.
fn writable_media_fixture(
    app: &TestApp,
    label: &str,
) -> (Arc<MediaService<LocalFsStore>>, TempDirGuard) {
    let root = unique_temp_root(label);
    let store = LocalFsStore::new(root.clone());
    let media = Arc::new(MediaService::new(
        app.pool.clone(),
        app.runtime.clone(),
        media_config(root.clone()),
        store,
    ));
    (media, TempDirGuard(root))
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
        media_service(app, store()),
        app.runtime.clone(),
    )
}

/// Builds an `AccountService` sharing the caller-supplied `ports` registry
/// (rather than a fresh `AccountPortsRegistry::new()`) — for tests that
/// register a test-double provider and need the service under test to
/// consult the exact same registry, exploiting `AccountPortsRegistry`'s
/// "cheap `Arc` clone, same interior `RwLock` slots" contract (its own doc
/// comment, "Registry shape").
fn service_with_ports(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
    ports: AccountPortsRegistry,
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
        ports,
        store(),
        media_service(app, store()),
        app.runtime.clone(),
    )
}

/// Builds an `AccountService` whose `media` field is backed by a real,
/// writable temp directory (`fixture`'s own `MediaService`) rather than
/// [`service`]'s never-touched nonexistent path — for the one test that
/// actually exercises avatar/header ingestion.
fn service_with_media(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
    media: Arc<MediaService<LocalFsStore>>,
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
        media,
        app.runtime.clone(),
    )
}

/// A capturing test-double `AccountStatusesProvider`: records the exact
/// `StatusesQuery` it is called with, so a test can assert filter/
/// pagination values were threaded through unchanged (Requirement 4.4:
/// "その絞り込み条件を委譲境界へ伝達し"). Always returns an empty page itself
/// — this double's job is only to observe the query, not to exercise a real
/// Status page shape.
struct CapturingStatusesProvider {
    captured: Mutex<Option<StatusesQuery>>,
}

impl CapturingStatusesProvider {
    fn new() -> Self {
        CapturingStatusesProvider {
            captured: Mutex::new(None),
        }
    }
}

impl AccountStatusesProvider for CapturingStatusesProvider {
    fn list_statuses<'a>(
        &'a self,
        query: &'a StatusesQuery,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Page<serde_json::Value>, AppError>> + Send + 'a,
        >,
    > {
        let captured = query.clone();
        Box::pin(async move {
            *self
                .captured
                .lock()
                .expect("CapturingStatusesProvider lock must not be poisoned") = Some(captured);
            Ok(Page {
                items: Vec::new(),
                prev_cursor: None,
                next_cursor: None,
            })
        })
    }
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

/// Requirement 4.3: while no real `AccountStatusesProvider` is registered
/// (the default `EmptyStatusesProvider`), `list_statuses` still responds
/// normally — an empty page, not an error — for a real account.
#[tokio::test]
async fn list_statuses_returns_an_empty_page_when_no_provider_is_registered() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "carol").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, mock);

    let page = svc
        .list_statuses(
            &actor_id.as_i64().to_string(),
            StatusesQueryInput::default(),
            None,
        )
        .await
        .expect("a real account must still respond normally with the default provider");

    assert!(page.items.is_empty());
    assert!(page.prev_cursor.is_none());
    assert!(page.next_cursor.is_none());

    app.cleanup().await;
}

/// This task's own account-scoping precondition: an id matching no local
/// actor and no cached remote account is a 404, the same discipline
/// `show_account` (task 5.1) already applies — a provider can only ever be
/// asked about an account that actually exists.
#[tokio::test]
async fn list_statuses_returns_404_for_an_unknown_account() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, mock);

    let err = svc
        .list_statuses("999999999999", StatusesQueryInput::default(), None)
        .await
        .expect_err("an id matching nothing must fail");
    assert_eq!(err.status, StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// Requirement 4.4 (and 4.5's viewer context): the filter/pagination
/// parameters passed into `list_statuses` must reach the registered
/// `AccountStatusesProvider`'s `StatusesQuery` unchanged — asserted against
/// the exact captured query, not merely "no panic".
#[tokio::test]
async fn list_statuses_threads_filters_and_pagination_to_the_provider() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "dave").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let ports = AccountPortsRegistry::new();
    let provider = Arc::new(CapturingStatusesProvider::new());
    ports.set_statuses_provider(Arc::clone(&provider) as Arc<dyn AccountStatusesProvider>);
    let svc = service_with_ports(&app, mock, ports);

    let viewer_id = Id::from_i64(4242);
    let viewer_ctx = RequestActorContext {
        actor_id: viewer_id,
        scopes: ScopeSet::new(["read:statuses"]),
    };

    let query = StatusesQueryInput {
        page: PageParams {
            max_id: Some("100".to_string()),
            since_id: None,
            min_id: Some("10".to_string()),
            limit: Some(5),
        },
        pinned: true,
        only_media: true,
        exclude_replies: true,
        exclude_reblogs: true,
    };

    let page = svc
        .list_statuses(
            &actor_id.as_i64().to_string(),
            query.clone(),
            Some(&viewer_ctx),
        )
        .await
        .expect("a real account with a registered provider must succeed");
    assert!(page.items.is_empty());

    let captured = provider
        .captured
        .lock()
        .expect("lock must not be poisoned")
        .clone()
        .expect("the provider must have been called exactly once");

    assert_eq!(captured.target, AccountRef::Local(actor_id));
    assert_eq!(captured.viewer, Some(viewer_id));
    assert_eq!(captured.page, query.page);
    assert!(captured.pinned);
    assert!(captured.only_media);
    assert!(captured.exclude_replies);
    assert!(captured.exclude_reblogs);

    app.cleanup().await;
}

/// Requirement 5.4: while no real `RelationshipStateProvider` is registered
/// (the default `NoRelationshipProvider`), `relationships` for multiple real
/// target ids still responds normally — one all-default Relationship JSON
/// per target (every flag false, every count 0, `note` empty) — not an
/// error.
#[tokio::test]
async fn relationships_returns_all_default_when_no_provider_is_registered() {
    let app = spawn_test_app().await;
    let viewer_id = create_test_actor(&app, "erin").await;
    let target_a = create_test_actor(&app, "frank").await;
    let target_b = create_test_actor(&app, "grace").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, mock);

    let ctx = RequestActorContext {
        actor_id: viewer_id,
        scopes: ScopeSet::new(["read:follows"]),
    };
    let ids = vec![target_a.as_i64().to_string(), target_b.as_i64().to_string()];

    let relationships = svc
        .relationships(&ctx, &ids)
        .await
        .expect("relationships must respond normally with the default provider");

    let array = relationships
        .as_array()
        .expect("relationships must return a JSON array");
    assert_eq!(array.len(), 2);

    for (entry, expected_id) in array.iter().zip(&ids) {
        assert_eq!(entry["id"], *expected_id);
        assert_eq!(entry["following"], false);
        assert_eq!(entry["showing_reblogs"], false);
        assert_eq!(entry["notifying"], false);
        assert_eq!(entry["followed_by"], false);
        assert_eq!(entry["blocking"], false);
        assert_eq!(entry["blocked_by"], false);
        assert_eq!(entry["muting"], false);
        assert_eq!(entry["muting_notifications"], false);
        assert_eq!(entry["requested"], false);
        assert_eq!(entry["requested_by"], false);
        assert_eq!(entry["domain_blocking"], false);
        assert_eq!(entry["endorsed"], false);
        assert_eq!(entry["note"], "");
        assert_eq!(entry["languages"].as_array().unwrap().len(), 0);
    }

    app.cleanup().await;
}

/// This task's own account-scoping judgment call (see
/// `AccountService::relationships`'s own doc comment, "Unresolvable ids"):
/// an id matching no local actor and no cached/fetchable remote account is
/// silently omitted from the result, rather than failing the whole batch.
#[tokio::test]
async fn relationships_omits_an_unresolvable_id_instead_of_failing_the_batch() {
    let app = spawn_test_app().await;
    let viewer_id = create_test_actor(&app, "harold").await;
    let target = create_test_actor(&app, "ivy").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_error(StatusCode::NOT_FOUND, "gone");
    let svc = service(&app, mock);

    let ctx = RequestActorContext {
        actor_id: viewer_id,
        scopes: ScopeSet::new(["read:follows"]),
    };
    let ids = vec!["999999999999".to_string(), target.as_i64().to_string()];

    let relationships = svc
        .relationships(&ctx, &ids)
        .await
        .expect("an unresolvable id must not fail the whole batch");

    let array = relationships
        .as_array()
        .expect("relationships must return a JSON array");
    assert_eq!(array.len(), 1, "only the resolvable id must be reflected");
    assert_eq!(array[0]["id"], target.as_i64().to_string());

    app.cleanup().await;
}

/// A capturing test-double `RelationshipStateProvider`: records the exact
/// `viewer`/`targets` it is called with (so a test can assert
/// `AccountService::relationships` threaded the token-bound viewer and the
/// resolved targets through unchanged, Requirement 5.1/5.3) and returns a
/// distinguishable `RelationshipView` per target (`following`/`note` derived
/// from the target's own id) so a test can prove the JSON array reflects
/// the provider's per-target values correctly, in the right order — not
/// merely that the array has the right length.
struct CapturingRelationshipProvider {
    captured: Mutex<Option<(Id, Vec<AccountRef>)>>,
}

impl CapturingRelationshipProvider {
    fn new() -> Self {
        CapturingRelationshipProvider {
            captured: Mutex::new(None),
        }
    }
}

impl RelationshipStateProvider for CapturingRelationshipProvider {
    fn relationships<'a>(
        &'a self,
        viewer: Id,
        targets: &'a [AccountRef],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<RelationshipView>, AppError>> + Send + 'a>,
    > {
        let captured_targets = targets.to_vec();
        Box::pin(async move {
            *self
                .captured
                .lock()
                .expect("CapturingRelationshipProvider lock must not be poisoned") =
                Some((viewer, captured_targets));
            Ok(targets
                .iter()
                .enumerate()
                .map(|(index, target)| {
                    let id = match *target {
                        AccountRef::Local(id) => id,
                        AccountRef::Remote(id) => id,
                    };
                    // distinguishable per target: alternate `following`,
                    // and encode the target's own position in `note`.
                    RelationshipView {
                        id,
                        following: index % 2 == 0,
                        showing_reblogs: false,
                        notifying: false,
                        languages: Vec::new(),
                        followed_by: index % 2 == 1,
                        blocking: false,
                        blocked_by: false,
                        muting: false,
                        muting_notifications: false,
                        requested: false,
                        requested_by: false,
                        domain_blocking: false,
                        endorsed: false,
                        note: format!("relationship-{index}"),
                    }
                })
                .collect())
        })
    }
}

/// This task's own "複数 id で Relationship 配列を返す" acceptance criterion
/// (Requirements 5.1, 5.2, 5.3): with a registered test-double provider that
/// returns distinguishable values per target, the resulting JSON array must
/// reflect exactly those per-target values, in the same order as the input
/// ids — and the provider must have been called with the viewer's own
/// `ctx.actor_id` and the correctly resolved `AccountRef`s (not, say, all
/// `AccountRef::Remote` or a viewer id that doesn't match the token).
#[tokio::test]
async fn relationships_returns_a_relationship_array_from_a_registered_provider() {
    let app = spawn_test_app().await;
    let viewer_id = create_test_actor(&app, "jack").await;
    let target_a = create_test_actor(&app, "karen").await;
    let target_b = create_test_actor(&app, "leo").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let ports = AccountPortsRegistry::new();
    let provider = Arc::new(CapturingRelationshipProvider::new());
    ports.set_relationship_provider(Arc::clone(&provider) as Arc<dyn RelationshipStateProvider>);
    let svc = service_with_ports(&app, mock, ports);

    let ctx = RequestActorContext {
        actor_id: viewer_id,
        scopes: ScopeSet::new(["read:follows"]),
    };
    let ids = vec![target_a.as_i64().to_string(), target_b.as_i64().to_string()];

    let relationships = svc
        .relationships(&ctx, &ids)
        .await
        .expect("relationships must succeed with a registered provider");

    let array = relationships
        .as_array()
        .expect("relationships must return a JSON array");
    assert_eq!(array.len(), 2);

    // Order preserved, and each entry reflects its own target's
    // distinguishable provider value — not just a length check.
    assert_eq!(array[0]["id"], target_a.as_i64().to_string());
    assert_eq!(array[0]["following"], true);
    assert_eq!(array[0]["followed_by"], false);
    assert_eq!(array[0]["note"], "relationship-0");

    assert_eq!(array[1]["id"], target_b.as_i64().to_string());
    assert_eq!(array[1]["following"], false);
    assert_eq!(array[1]["followed_by"], true);
    assert_eq!(array[1]["note"], "relationship-1");

    let (captured_viewer, captured_targets) = provider
        .captured
        .lock()
        .expect("lock must not be poisoned")
        .clone()
        .expect("the provider must have been called exactly once");
    assert_eq!(captured_viewer, viewer_id);
    assert_eq!(
        captured_targets,
        vec![AccountRef::Local(target_a), AccountRef::Local(target_b)]
    );

    app.cleanup().await;
}

// ---- task 5.4: update_credentials ----

/// Requirements 6.1, 6.5: a partial `update_credentials` (only
/// `display_name`) changes just that field and leaves another already-set
/// profile field (`note`) untouched, reflected in a subsequent
/// `verify_credentials` call.
#[tokio::test]
async fn update_credentials_partial_update_is_reflected_in_verify_credentials_and_leaves_other_fields_untouched()
 {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "mia").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, mock);

    let ctx = RequestActorContext {
        actor_id,
        scopes: ScopeSet::new(["write:accounts"]),
    };

    // Seed both fields with an initial full update.
    let seed = UpdateCredentialsInput {
        display_name: Some("Original Name".to_string()),
        note: Some("Original bio.".to_string()),
        ..UpdateCredentialsInput::default()
    };
    svc.update_credentials(&ctx, seed, &origin())
        .await
        .expect("seeding the initial profile must succeed");

    // Partial update: only display_name.
    let patch = UpdateCredentialsInput {
        display_name: Some("New Name".to_string()),
        ..UpdateCredentialsInput::default()
    };
    let updated = svc
        .update_credentials(&ctx, patch, &origin())
        .await
        .expect("a valid partial update must succeed");
    assert_eq!(updated["display_name"], "New Name");
    assert_eq!(updated["source"]["note"], "Original bio.");

    let credential = svc
        .verify_credentials(&ctx, &origin())
        .await
        .expect("verify_credentials must succeed");
    assert_eq!(credential["display_name"], "New Name");
    assert_eq!(credential["note"], "Original bio.");
    assert_eq!(credential["source"]["note"], "Original bio.");

    // Task 5.4's own completion condition names both endpoints explicitly
    // ("部分更新が verify_credentials/accounts/:id に反映され") — `show_account`
    // reads the exact same `account_profiles` row `verify_credentials` just
    // read above, so the partial update must be reflected there too, not
    // only via `verify_credentials`.
    let account = svc
        .show_account(&actor_id.as_i64().to_string(), None, &origin())
        .await
        .expect("show_account must succeed");
    assert_eq!(account["display_name"], "New Name");
    assert_eq!(account["note"], "Original bio.");

    app.cleanup().await;
}

/// Requirement 6.3: a validation violation (here, more than
/// `MAX_PROFILE_FIELDS` profile fields) is rejected with a 422 *before* any
/// write — a subsequent read proves the already-seeded `display_name` was
/// left untouched by the rejected update's own (different) value.
#[tokio::test]
async fn update_credentials_rejects_too_many_profile_fields_with_422_and_does_not_write() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "noah").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let svc = service(&app, mock);

    let ctx = RequestActorContext {
        actor_id,
        scopes: ScopeSet::new(["write:accounts"]),
    };

    let seed = UpdateCredentialsInput {
        display_name: Some("Baseline Name".to_string()),
        ..UpdateCredentialsInput::default()
    };
    svc.update_credentials(&ctx, seed, &origin())
        .await
        .expect("seeding must succeed");

    let too_many_fields = (0..=MAX_PROFILE_FIELDS)
        .map(|i| ProfileFieldInput {
            name: format!("Field {i}"),
            value: format!("Value {i}"),
        })
        .collect();
    let violating = UpdateCredentialsInput {
        display_name: Some("Should Not Apply".to_string()),
        fields_attributes: Some(too_many_fields),
        ..UpdateCredentialsInput::default()
    };

    let err = svc
        .update_credentials(&ctx, violating, &origin())
        .await
        .expect_err("too many profile fields must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Nothing was written: display_name is still the seeded baseline, not
    // the rejected update's value.
    let profile = find_profile(&app.pool, actor_id)
        .await
        .expect("find_profile must succeed")
        .expect("the seeded profile row must exist");
    assert_eq!(profile.display_name, "Baseline Name");

    let credential = svc
        .verify_credentials(&ctx, &origin())
        .await
        .expect("verify_credentials must succeed");
    assert_eq!(credential["display_name"], "Baseline Name");

    app.cleanup().await;
}

/// Requirement 6.3: an out-of-range avatar focus is rejected with a 422
/// *before* the avatar is ever ingested via `MediaService` and before any
/// `account_profiles` row is written — proving this validation is genuinely
/// fail-fast (checked before the first side effect), not merely "eventually
/// errors after ingesting the media".
#[tokio::test]
async fn update_credentials_rejects_an_out_of_range_avatar_focus_before_any_write() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "piper").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let (media, _guard) = writable_media_fixture(&app, "focus_reject");
    let svc = service_with_media(&app, mock, media);

    let ctx = RequestActorContext {
        actor_id,
        scopes: ScopeSet::new(["write:accounts"]),
    };

    let input = UpdateCredentialsInput {
        avatar: Some(MediaUploadInput {
            bytes: b"bytes".to_vec(),
            content_type: "image/png".to_string(),
            focus: Some((2.0, 0.0)),
        }),
        ..UpdateCredentialsInput::default()
    };

    let err = svc
        .update_credentials(&ctx, input, &origin())
        .await
        .expect_err("an out-of-range focus must be rejected");
    assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);

    let profile = find_profile(&app.pool, actor_id)
        .await
        .expect("find_profile must succeed");
    assert!(
        profile.is_none(),
        "no account_profiles row should have been written for a rejected update"
    );

    app.cleanup().await;
}

/// Requirement 6.2: a valid avatar upload is ingested through the real
/// `MediaService::accept_upload` (a genuine `media` row is created, owned by
/// the actor) and the resulting media id becomes the profile's
/// `avatar_media`, changing the CredentialAccount's `avatar` URL away from
/// the default image URL (Requirement 1.5's "既定の画像URLを出力し...null に
/// しない" — this test proves the *non-default* half: a real avatar was
/// actually attached).
#[tokio::test]
async fn update_credentials_ingests_an_avatar_upload_via_media_service() {
    let app = spawn_test_app().await;
    let actor_id = create_test_actor(&app, "olive").await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let (media, _guard) = writable_media_fixture(&app, "avatar_ingest");
    let svc = service_with_media(&app, mock, media);

    let ctx = RequestActorContext {
        actor_id,
        scopes: ScopeSet::new(["write:accounts"]),
    };

    let before = svc
        .verify_credentials(&ctx, &origin())
        .await
        .expect("verify_credentials must succeed");
    let default_avatar = before["avatar"]
        .as_str()
        .expect("avatar must be a string")
        .to_string();

    let input = UpdateCredentialsInput {
        avatar: Some(MediaUploadInput {
            bytes: b"not-a-real-image-but-accept_upload-never-decodes-it".to_vec(),
            content_type: "image/png".to_string(),
            focus: None,
        }),
        ..UpdateCredentialsInput::default()
    };

    let updated = svc
        .update_credentials(&ctx, input, &origin())
        .await
        .expect("a valid avatar upload must be accepted");

    let new_avatar = updated["avatar"].as_str().expect("avatar must be a string");
    assert_ne!(
        new_avatar, default_avatar,
        "avatar must change away from the default image URL"
    );
    assert!(new_avatar.contains("/media/"), "got {new_avatar}");

    let profile = find_profile(&app.pool, actor_id)
        .await
        .expect("find_profile must succeed")
        .expect("profile row must exist after the update");
    let avatar_media_id = profile
        .avatar_media
        .expect("avatar_media must be set after ingestion");

    let media_row = find_owned(&app.pool, avatar_media_id, actor_id)
        .await
        .expect("find_owned must succeed")
        .expect("the ingested media row must exist and be owned by the actor");
    assert_eq!(media_row.actor_id, actor_id);

    app.cleanup().await;
}
