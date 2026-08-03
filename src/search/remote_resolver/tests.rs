//! Tests for `RemoteResolver` (Requirements 6.1, 6.2, 6.3, 6.4), per task
//! 4.3's own observable completion condition: "`FederationHttpClient` モッ
//! クで `acct:`→Account・URL→Status の解決が成立し、取得失敗が `None` にな
//! り検索全体を失敗させない統合テストが通る".
//!
//! Two tiers:
//! - Pure unit tests for this module's own private helpers
//!   (`build_webfinger_url`/`find_self_actor_href`/`is_actor_type`/
//!   `check_fetched_host`) — no network, no database, always runnable.
//! - `#[tokio::test]` integration tests, mirroring
//!   `crate::accounts::remote_fetcher::tests`'/`crate::statuses::
//!   ingest_service::tests`' established `spawn_test_app` +
//!   `MockFederationHttpClient` convention: every network call this
//!   resolver itself makes (`FederationHttpClient::fetch`, twice for the
//!   URL-actor path — see this module's own doc comment, "Actor URL: a
//!   deliberate double fetch") is queued deterministically, while
//!   `RemoteAccountFetcher`/`StatusIngestService`'s own DB-backed
//!   normalization/ingestion exercises the real, migrated schema
//!   `spawn_test_app` provides. In this sandbox (no reachable Postgres)
//!   these `#[tokio::test]` cases fail at `spawn_test_app()` itself (schema
//!   creation requires a live connection) — see this task's status report,
//!   `TESTS_RUN`, for the DB-unavailable-vs-logic-failure distinction.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use serde_json::json;

use super::*;
use crate::accounts::DEFAULT_REMOTE_ACCOUNT_CACHE_TTL;
use crate::accounts::remote_repository::find_remote_by_uri;
use crate::actor::ActorDirectory;
use crate::federation::signatures::{HttpResponse, MockFederationHttpClient};
use crate::runtime::RuntimeContext;
use crate::search::model::ParsedQuery;
use crate::statuses::status_repository;
use crate::test_harness::{TestApp, spawn_test_app};

// ---- Pure unit tests (no network, no database) -----------------------------

#[test]
fn build_webfinger_url_builds_the_expected_resource_query() {
    let url = build_webfinger_url("alice", "remote.example")
        .expect("a plain user/domain pair must build a valid webfinger URL");
    assert_eq!(
        url,
        "https://remote.example/.well-known/webfinger?resource=acct%3Aalice%40remote.example"
    );
}

#[test]
fn build_webfinger_url_rejects_a_domain_with_no_interpretable_host() {
    // An empty domain cannot form a valid `https://{domain}/...` base URL —
    // `QueryParser::parse_query` already rejects an empty domain segment
    // (`split_user_domain`), but this helper stays defensive rather than
    // assuming that invariant holds forever.
    assert!(build_webfinger_url("alice", "").is_none());
}

#[test]
fn find_self_actor_href_extracts_the_matching_self_link() {
    let document: JrdDocument = serde_json::from_value(json!({
        "subject": "acct:alice@remote.example",
        "links": [
            {"rel": "self", "type": "application/activity+json", "href": "https://remote.example/users/alice"}
        ]
    }))
    .expect("a well-formed JRD must deserialize");

    assert_eq!(
        find_self_actor_href(&document),
        Some("https://remote.example/users/alice")
    );
}

#[test]
fn find_self_actor_href_ignores_non_self_and_wrong_media_type_links() {
    let document: JrdDocument = serde_json::from_value(json!({
        "subject": "acct:alice@remote.example",
        "links": [
            {"rel": "http://webfinger.net/rel/profile-page", "type": "text/html", "href": "https://remote.example/@alice"},
            {"rel": "self", "type": "application/json", "href": "https://remote.example/wrong-type"}
        ]
    }))
    .expect("a well-formed JRD must deserialize");

    assert_eq!(find_self_actor_href(&document), None);
}

#[test]
fn find_self_actor_href_returns_none_for_no_links_at_all() {
    let document: JrdDocument =
        serde_json::from_value(json!({"subject": "acct:alice@remote.example"}))
            .expect("a JRD with no links key must still deserialize (Requirement 6.4)");
    assert_eq!(find_self_actor_href(&document), None);
}

#[test]
fn is_actor_type_recognizes_every_standard_actor_type_and_rejects_note() {
    for actor_type in ["Person", "Service", "Application", "Group", "Organization"] {
        assert!(
            is_actor_type(actor_type),
            "{actor_type} must be an actor type"
        );
    }
    assert!(!is_actor_type("Note"));
    assert!(!is_actor_type("Tombstone"));
    assert!(!is_actor_type("Question"));
}

/// The origin/authority anchor's happy path: a document whose own `id`
/// shares a host with the URL this resolver actually fetched must pass.
#[test]
fn check_fetched_host_accepts_a_matching_id_host() {
    let document = json!({
        "id": "https://remote.example/notes/1",
        "type": "Note",
        "attributedTo": "https://remote.example/users/alice",
    });
    assert_eq!(
        check_fetched_host("https://remote.example/notes/1", &document),
        Ok(())
    );
}

/// The vulnerability this helper closes (see this module's doc comment on
/// `check_fetched_host`): a document that is internally self-consistent
/// (`id`/`attributedTo` agree with each other) but whose `id` names a host
/// other than the URL actually dereferenced must be rejected.
#[test]
fn check_fetched_host_rejects_a_cross_host_id() {
    let document = json!({
        "id": "https://victim.example/notes/1",
        "type": "Note",
        "attributedTo": "https://victim.example/users/alice",
    });
    assert_eq!(
        check_fetched_host("https://remote.example/notes/1", &document),
        Err(("remote.example", "victim.example"))
    );
}

/// Host comparison is ASCII-case-insensitive, matching every other host
/// comparison in this crate (`check_fetched_host`'s own doc comment).
#[test]
fn check_fetched_host_is_case_insensitive() {
    let document = json!({
        "id": "https://REMOTE.example/notes/1",
        "type": "Note",
        "attributedTo": "https://remote.example/users/alice",
    });
    assert_eq!(
        check_fetched_host("https://remote.example/notes/1", &document),
        Ok(())
    );
}

/// Mirrors `crate::statuses::ingest_service::check_fetched_host`'s own "a
/// missing `id` is not checked here" precedent: a document with no `id` at
/// all is not rejected by this check (left to downstream `id`
/// presence/shape validation instead).
#[test]
fn check_fetched_host_accepts_a_document_with_no_id() {
    let document = json!({
        "type": "Note",
        "attributedTo": "https://remote.example/users/alice",
    });
    assert_eq!(
        check_fetched_host("https://remote.example/notes/1", &document),
        Ok(())
    );
}

// ---- Integration tests (spawn_test_app + MockFederationHttpClient) --------

/// An in-memory [`RemoteActorResolver`] double — mirrors
/// `crate::statuses::ingest_service::tests::FakeRemoteActors`' identical
/// per-test-module-owned precedent (this module's own copy, not a
/// cross-module import).
struct FakeRemoteActors {
    runtime: RuntimeContext,
    by_uri: Mutex<HashMap<String, Id>>,
}

impl FakeRemoteActors {
    fn new(runtime: RuntimeContext) -> Self {
        Self {
            runtime,
            by_uri: Mutex::new(HashMap::new()),
        }
    }
}

impl RemoteActorResolver for FakeRemoteActors {
    async fn resolve_remote_actor(&self, actor_uri: &str) -> Result<Id, AppError> {
        let mut map = self.by_uri.lock().unwrap();
        if let Some(id) = map.get(actor_uri) {
            return Ok(*id);
        }
        let id = self.runtime.ids.next_id();
        map.insert(actor_uri.to_string(), id);
        Ok(id)
    }
}

const ALICE_ACTOR_URI: &str = "https://remote.example/users/alice";
const NOTE_URI: &str = "https://remote.example/notes/1";
const TEST_DOMAIN: &str = "kawasemi.example";

fn ok_response(body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: serde_json::to_vec(&body).expect("test fixture body must serialize"),
    }
}

fn jrd_document(actor_uri: &str) -> serde_json::Value {
    json!({
        "subject": format!("acct:alice@remote.example"),
        "links": [
            {"rel": "self", "type": "application/activity+json", "href": actor_uri}
        ]
    })
}

fn actor_document(actor_uri: &str) -> serde_json::Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": actor_uri,
        "type": "Person",
        "preferredUsername": "alice",
        "name": "Alice Example",
    })
}

fn note_document(uri: &str, attributed_to: &str) -> serde_json::Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": uri,
        "type": "Note",
        "attributedTo": attributed_to,
        "content": "hello from remote.example",
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
    })
}

fn resolver_for(
    app: &TestApp,
    mock: Arc<MockFederationHttpClient>,
) -> RemoteResolver<MockFederationHttpClient, FakeRemoteActors, ActorDirectory> {
    let account_fetcher = Arc::new(RemoteAccountFetcher::new(
        app.pool.clone(),
        Arc::clone(&mock),
        app.runtime.clone(),
        DEFAULT_REMOTE_ACCOUNT_CACHE_TTL,
    ));
    let status_ingest = Arc::new(StatusIngestService::new(
        app.pool.clone(),
        Arc::clone(&mock),
        app.runtime.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
        TEST_DOMAIN,
        ActorDirectory::new(app.pool.clone()),
    ));
    RemoteResolver::new(mock, account_fetcher, status_ingest)
}

/// Requirement 6.1: an `acct:user@domain` query resolves via outbound
/// WebFinger (self link -> actor_uri) then `RemoteAccountFetcher::
/// fetch_and_normalize`, to a `Resolved::Account`.
#[tokio::test]
async fn resolve_remote_acct_success_returns_normalized_account() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(jrd_document(ALICE_ACTOR_URI)));
    mock.queue_fetch_response(ok_response(actor_document(ALICE_ACTOR_URI)));
    let resolver = resolver_for(&app, mock);

    let parsed = ParsedQuery::Acct {
        user: "alice".to_string(),
        domain: "remote.example".to_string(),
    };
    let resolved = resolver
        .resolve_remote(&parsed, app.runtime.ids.next_id())
        .await
        .expect("resolve_remote itself never returns Err (Requirement 6.4)");

    let persisted = find_remote_by_uri(&app.pool, ALICE_ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed")
        .expect("fetch_and_normalize must have upserted the remote account");
    assert_eq!(
        resolved,
        Resolved::Account(AccountRef::Remote(persisted.id))
    );
}

/// Requirement 6.4: a WebFinger fetch failure normalizes to `Resolved::None`
/// rather than propagating an error.
#[tokio::test]
async fn resolve_remote_acct_webfinger_fetch_failure_normalizes_to_none() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_error(StatusCode::BAD_GATEWAY, "network unreachable");
    let resolver = resolver_for(&app, mock);

    let parsed = ParsedQuery::Acct {
        user: "alice".to_string(),
        domain: "remote.example".to_string(),
    };
    let resolved = resolver
        .resolve_remote(&parsed, app.runtime.ids.next_id())
        .await
        .expect("resolve_remote itself never returns Err (Requirement 6.4)");
    assert_eq!(resolved, Resolved::None);
}

/// Requirement 6.4: a JRD with no matching `self` link normalizes to
/// `Resolved::None`.
#[tokio::test]
async fn resolve_remote_acct_missing_self_link_normalizes_to_none() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(json!({"subject": "acct:alice@remote.example"})));
    let resolver = resolver_for(&app, mock);

    let parsed = ParsedQuery::Acct {
        user: "alice".to_string(),
        domain: "remote.example".to_string(),
    };
    let resolved = resolver
        .resolve_remote(&parsed, app.runtime.ids.next_id())
        .await
        .expect("resolve_remote itself never returns Err (Requirement 6.4)");
    assert_eq!(resolved, Resolved::None);
}

/// Requirement 6.2: a URL resolving to a `Note` document ingests via
/// `StatusIngestService::ingest_document`, to a `Resolved::Status`.
#[tokio::test]
async fn resolve_remote_url_note_ingests_and_returns_status() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(note_document(NOTE_URI, ALICE_ACTOR_URI)));
    let resolver = resolver_for(&app, mock);

    let parsed = ParsedQuery::Url(NOTE_URI.to_string());
    let resolved = resolver
        .resolve_remote(&parsed, app.runtime.ids.next_id())
        .await
        .expect("resolve_remote itself never returns Err (Requirement 6.4)");

    let persisted = status_repository::find_by_uri(&app.pool, NOTE_URI)
        .await
        .expect("find_by_uri must succeed")
        .expect("ingest_document must have persisted the Note as a Status");
    assert_eq!(resolved, Resolved::Status(persisted.id));
}

/// Requirement 6.2: a URL resolving to an Actor document normalizes via
/// `RemoteAccountFetcher::fetch_and_normalize`, to a `Resolved::Account` —
/// see this module's own doc comment ("Actor URL: a deliberate double
/// fetch") for why two fetch responses are queued for this one call.
#[tokio::test]
async fn resolve_remote_url_actor_normalizes_and_returns_account() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(actor_document(ALICE_ACTOR_URI)));
    mock.queue_fetch_response(ok_response(actor_document(ALICE_ACTOR_URI)));
    let resolver = resolver_for(&app, mock);

    let parsed = ParsedQuery::Url(ALICE_ACTOR_URI.to_string());
    let resolved = resolver
        .resolve_remote(&parsed, app.runtime.ids.next_id())
        .await
        .expect("resolve_remote itself never returns Err (Requirement 6.4)");

    let persisted = find_remote_by_uri(&app.pool, ALICE_ACTOR_URI)
        .await
        .expect("find_remote_by_uri must succeed")
        .expect("fetch_and_normalize must have upserted the remote account");
    assert_eq!(
        resolved,
        Resolved::Account(AccountRef::Remote(persisted.id))
    );
}

/// Requirement 6.4: a non-success upstream fetch status for a URL query
/// normalizes to `Resolved::None`.
#[tokio::test]
async fn resolve_remote_url_fetch_failure_normalizes_to_none() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(HttpResponse {
        status: StatusCode::NOT_FOUND,
        headers: HeaderMap::new(),
        body: Vec::new(),
    });
    let resolver = resolver_for(&app, mock);

    let parsed = ParsedQuery::Url(NOTE_URI.to_string());
    let resolved = resolver
        .resolve_remote(&parsed, app.runtime.ids.next_id())
        .await
        .expect("resolve_remote itself never returns Err (Requirement 6.4)");
    assert_eq!(resolved, Resolved::None);
}

/// Requirement 6.4 + the origin/authority anchor (`check_fetched_host`, this
/// module's own doc comment): a `Note` document fetched from one host
/// (`remote.example`) whose own `id`/`attributedTo` are internally
/// self-consistent but name a *different* host (`victim.example`) must
/// normalize to `Resolved::None` and must never be persisted — this is the
/// exact content-spoofing/attribution-injection shape `check_fetched_host`
/// exists to reject: `ingest_document` alone (its `id`-vs-`attributedTo`
/// check) would happily accept this document, laundering it as
/// `victim.example`'s own content.
#[tokio::test]
async fn resolve_remote_url_note_cross_host_id_normalizes_to_none_and_is_not_persisted() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    const CROSS_HOST_NOTE_URI: &str = "https://victim.example/notes/1";
    const CROSS_HOST_ATTRIBUTED_TO: &str = "https://victim.example/users/alice";
    mock.queue_fetch_response(ok_response(note_document(
        CROSS_HOST_NOTE_URI,
        CROSS_HOST_ATTRIBUTED_TO,
    )));
    let resolver = resolver_for(&app, mock);

    // Fetched from `remote.example`, but the document's own `id` claims
    // `victim.example` — self-consistent with its own `attributedTo`, but
    // not with the URL actually dereferenced.
    let parsed = ParsedQuery::Url(NOTE_URI.to_string());
    let resolved = resolver
        .resolve_remote(&parsed, app.runtime.ids.next_id())
        .await
        .expect("resolve_remote itself never returns Err (Requirement 6.4)");
    assert_eq!(resolved, Resolved::None);

    let persisted = status_repository::find_by_uri(&app.pool, CROSS_HOST_NOTE_URI)
        .await
        .expect("find_by_uri must succeed");
    assert!(
        persisted.is_none(),
        "a cross-host document must never be ingested into the statuses table"
    );
}

/// A `Plain` query is never a resolution target for this resolver (design.md's
/// flow diagram only invokes `resolve_remote` for `Acct`/`Url` kinds) — this
/// resolver's own decision (see this module's doc comment) is to normalize
/// that case to `Resolved::None` without making any network call, rather than
/// panicking on an unreachable arm.
#[tokio::test]
async fn resolve_remote_plain_query_returns_none_without_any_network_call() {
    let app = spawn_test_app().await;
    let mock = Arc::new(MockFederationHttpClient::new());
    let resolver = resolver_for(&app, Arc::clone(&mock));

    let parsed = ParsedQuery::Plain("hello world".to_string());
    let resolved = resolver
        .resolve_remote(&parsed, app.runtime.ids.next_id())
        .await
        .expect("resolve_remote itself never returns Err (Requirement 6.4)");
    assert_eq!(resolved, Resolved::None);
    assert!(
        mock.fetched_urls().is_empty(),
        "a Plain query must never trigger a federation fetch"
    );
}
