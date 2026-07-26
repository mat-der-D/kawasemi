//! DB-backed tests for `StatusIngestService` (Requirements 14.1, 14.2, 14.3),
//! per task 6.2's own observable completion condition: "URL/ドキュメント指定
//! で Note が取得・正規化され Status として取り込まれる（単体/結合テストが
//! グリーン）。受信ハンドラ経路と同一結果になる".
//!
//! Mirrors `inbound_handlers/tests.rs`'s established convention
//! (`crate::test_harness::spawn_test_app` for an isolated, migrated schema
//! plus a deterministic `RuntimeContext`), paired with
//! `MockFederationHttpClient` (mirroring `remote_fetcher/tests.rs`'s
//! identical use of that same mock for a URL-fetching service) so
//! `ingest_url`'s network call is deterministic. `FakeRemoteActors` is this
//! module's own copy of `inbound_handlers/tests.rs::FakeRemoteActors`'s
//! identical in-memory-fake precedent (each test module owns its own small
//! double rather than reaching across a module boundary for it, matching
//! `poll_service/tests.rs::MockActorLookup`'s established convention).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};

use super::*;
use crate::domain::{Id, Visibility};
use crate::error::ErrorKind;
use crate::federation::VerifiedSigner;
use crate::federation::inbound::dispatcher::InboundContext;
use crate::federation::signatures::{HttpResponse, MockFederationHttpClient};
use crate::statuses::inbound_handlers::CreateNoteHandler;
use crate::statuses::status_repository;
use crate::test_harness::{TestApp, spawn_test_app};

/// An in-memory [`RemoteActorResolver`]: hands out a stable [`Id`] per
/// `actor_uri`, minting a fresh one on first sight and remembering it
/// thereafter. See this module's doc comment for why this is a per-test-
/// module copy rather than a cross-module import.
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

const REMOTE_ALICE: &str = "https://remote.example/actors/alice";
const NOTE_URI: &str = "https://remote.example/notes/1";

fn ok_response(body: Value) -> HttpResponse {
    HttpResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: serde_json::to_vec(&body).expect("test fixture body must serialize"),
    }
}

/// A conventional standalone `Note` document, the shape a `Note` object URL
/// dereferences to directly (not wrapped in a `Create`) — Requirement 14.2's
/// "リモート投稿をローカルの Status モデルへ取り込み、返信・可視性・添付・
/// メンションを反映する" applied to this out-of-dispatch entry point.
fn note_document(uri: &str) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": uri,
        "type": "Note",
        "attributedTo": REMOTE_ALICE,
        "content": "hello from an out-of-dispatch fetch",
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
    })
}

fn service_for(
    app: &TestApp,
    http_client: Arc<MockFederationHttpClient>,
    remote_actors: Arc<FakeRemoteActors>,
) -> StatusIngestService<MockFederationHttpClient, FakeRemoteActors> {
    StatusIngestService::new(
        app.pool.clone(),
        http_client,
        app.runtime.clone(),
        remote_actors,
    )
}

/// Requirements 14.1, 14.2: `ingest_document` normalizes a standalone `Note`
/// document into a persisted `Status` with the expected content/visibility.
#[tokio::test]
async fn ingest_document_ingests_a_standalone_note() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let service = service_for(
        &app,
        Arc::new(MockFederationHttpClient::new()),
        remote_actors,
    );

    let status = service
        .ingest_document(&note_document(NOTE_URI))
        .await
        .expect("a well-formed standalone Note must ingest successfully");

    assert_eq!(status.uri, NOTE_URI);
    assert_eq!(status.content, "hello from an out-of-dispatch fetch");
    assert_eq!(status.visibility, Visibility::Public);
    assert!(!status.local);

    let persisted = status_repository::find_by_uri(&app.pool, NOTE_URI)
        .await
        .expect("find_by_uri must succeed")
        .expect("the ingested Note must be persisted and readable back");
    assert_eq!(persisted.id, status.id);
    assert_eq!(persisted.content, status.content);
}

/// Requirement 14.2 (reply reflection): a Note whose `inReplyTo` resolves to
/// a known local Status has its `in_reply_to_id`/`in_reply_to_account_id`
/// populated, identical to `CreateNoteHandler`'s own reply handling.
#[tokio::test]
async fn ingest_document_reflects_a_reply_to_a_known_status() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let parent = insert_test_status(&app, local_author, Visibility::Public).await;

    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let service = service_for(
        &app,
        Arc::new(MockFederationHttpClient::new()),
        remote_actors,
    );

    let mut document = note_document(NOTE_URI);
    document["inReplyTo"] = json!(parent.uri);

    let status = service
        .ingest_document(&document)
        .await
        .expect("a reply Note must ingest successfully");

    assert_eq!(status.in_reply_to_id, Some(parent.id));
    assert_eq!(status.in_reply_to_account_id, Some(parent.actor_id));
}

/// Requirement 14.2 (idempotency, matching `CreateNoteHandler`'s own
/// re-delivery handling): ingesting the identical `uri` twice returns the
/// same persisted row rather than inserting a duplicate.
#[tokio::test]
async fn ingest_document_is_idempotent_on_repeated_uri() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let service = service_for(
        &app,
        Arc::new(MockFederationHttpClient::new()),
        remote_actors,
    );

    let first = service
        .ingest_document(&note_document(NOTE_URI))
        .await
        .expect("first ingestion must succeed");
    let second = service
        .ingest_document(&note_document(NOTE_URI))
        .await
        .expect("re-ingesting the same uri must succeed as a safe no-op");

    assert_eq!(first.id, second.id);
}

/// A non-`Note` document (e.g. a fetched actor) is rejected with a `422`,
/// not silently ingested as a Status.
#[tokio::test]
async fn ingest_document_rejects_a_non_note_type() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let service = service_for(
        &app,
        Arc::new(MockFederationHttpClient::new()),
        remote_actors,
    );

    let document = json!({
        "id": "https://remote.example/actors/alice",
        "type": "Person",
    });

    let error = service
        .ingest_document(&document)
        .await
        .expect_err("a non-Note document must be rejected");
    assert_eq!(error.kind, ErrorKind::Client);
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// A `Note` document missing `attributedTo` cannot be attributed to any
/// actor and must be rejected with a `422`.
#[tokio::test]
async fn ingest_document_rejects_a_note_missing_attributed_to() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let service = service_for(
        &app,
        Arc::new(MockFederationHttpClient::new()),
        remote_actors,
    );

    let document = json!({
        "id": NOTE_URI,
        "type": "Note",
        "content": "no author here",
    });

    let error = service
        .ingest_document(&document)
        .await
        .expect_err("a Note without attributedTo must be rejected");
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Remediation regression test (reviewer finding: origin/authority check):
/// a `Note` document served from one host (`evil.example`, both its own
/// `id` and the fetch `url`) claiming `attributedTo` on a different host
/// (`REMOTE_ALICE`, `remote.example`) must be rejected outright -- not
/// resolved to the real, unrelated `remote.example` actor and ingested as
/// if `evil.example` had genuine authority to vouch for that authorship.
#[tokio::test]
async fn ingest_document_rejects_a_note_whose_id_host_differs_from_attributed_to_host() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let service = service_for(
        &app,
        Arc::new(MockFederationHttpClient::new()),
        Arc::clone(&remote_actors),
    );

    let forged_uri = "https://evil.example/notes/forged";
    let document = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": forged_uri,
        "type": "Note",
        "attributedTo": REMOTE_ALICE,
        "content": "evil.example claims this was written by remote.example/alice",
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
    });

    let error = service
        .ingest_document(&document)
        .await
        .expect_err("a Note whose id host differs from its attributedTo host must be rejected");
    assert_eq!(error.kind, ErrorKind::Client);
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);

    let persisted = status_repository::find_by_uri(&app.pool, forged_uri)
        .await
        .expect("find_by_uri must succeed");
    assert!(
        persisted.is_none(),
        "a rejected origin-mismatched Note must not be persisted as a Status"
    );

    // The mismatch must be caught before the actor is ever resolved -- no
    // Id should have been minted for REMOTE_ALICE via this rejected attempt.
    assert!(
        remote_actors.by_uri.lock().unwrap().is_empty(),
        "attributedTo must not be resolved to an actor when the origin check fails"
    );
}

/// The same mismatch, reached via `ingest_url`: the fetched document's own
/// `id` differs from both the fetch `url` and its claimed `attributedTo`
/// host, and must be rejected identically to the `ingest_document` case.
#[tokio::test]
async fn ingest_url_rejects_a_note_whose_id_host_differs_from_attributed_to_host() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let forged_uri = "https://evil.example/notes/forged";
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": forged_uri,
        "type": "Note",
        "attributedTo": REMOTE_ALICE,
        "content": "evil.example claims this was written by remote.example/alice",
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
    })));
    let service = service_for(&app, mock, remote_actors);

    let error = service.ingest_url(forged_uri).await.expect_err(
        "a fetched Note whose id host differs from its attributedTo host must be rejected",
    );
    assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);

    let persisted = status_repository::find_by_uri(&app.pool, forged_uri)
        .await
        .expect("find_by_uri must succeed");
    assert!(
        persisted.is_none(),
        "a rejected origin-mismatched Note must not be persisted as a Status"
    );
}

/// Requirement 14.2: `ingest_url` fetches the document over the network
/// (via `FederationHttpClient::fetch`) and ingests it identically to
/// `ingest_document`.
#[tokio::test]
async fn ingest_url_fetches_and_ingests_a_note() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(ok_response(note_document(NOTE_URI)));
    let service = service_for(&app, mock, remote_actors);

    let status = service
        .ingest_url(NOTE_URI)
        .await
        .expect("a successful fetch of a well-formed Note must ingest successfully");

    assert_eq!(status.uri, NOTE_URI);
    assert_eq!(status.content, "hello from an out-of-dispatch fetch");
}

/// A non-success upstream fetch status maps to a caller-facing `404`,
/// mirroring `RemoteAccountFetcher::fetch_and_upsert`'s identical mapping.
#[tokio::test]
async fn ingest_url_maps_a_non_success_fetch_to_not_found() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let mock = Arc::new(MockFederationHttpClient::new());
    mock.queue_fetch_response(HttpResponse {
        status: StatusCode::NOT_FOUND,
        headers: HeaderMap::new(),
        body: Vec::new(),
    });
    let service = service_for(&app, mock, remote_actors);

    let error = service
        .ingest_url(NOTE_URI)
        .await
        .expect_err("a 404 upstream fetch must not be treated as a successful ingestion");
    assert_eq!(error.status, StatusCode::NOT_FOUND);
}

/// Task 6.2's own observable-completion criterion: "受信ハンドラ経路と同一
/// 結果になる" -- the identical `Note` object, ingested via
/// `CreateNoteHandler` (wrapped in a `Create`, as the inbound dispatch path
/// receives it) and via `StatusIngestService::ingest_document` (the bare
/// `Note`, as an out-of-dispatch caller would fetch it), must produce
/// identical observable `Status` content -- because both paths call the
/// exact same `ingest_note_object` function.
#[tokio::test]
async fn ingest_document_matches_the_inbound_dispatch_path_for_the_same_note() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));

    // Path 1: the inbound Create(Note) dispatch handler (task 6.1).
    let dispatch_uri = "https://remote.example/notes/via-dispatch";
    let handler = CreateNoteHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::clone(&remote_actors),
    );
    let ctx = InboundContext {
        signer: VerifiedSigner {
            key_id: format!("{REMOTE_ALICE}#main-key"),
            actor_uri: REMOTE_ALICE.to_string(),
        },
    };
    let create_activity = crate::federation::jsonld::ParsedActivity {
        id: format!("{dispatch_uri}/activity"),
        activity_type: "Create".to_string(),
        raw: json!({
            "id": format!("{dispatch_uri}/activity"),
            "type": "Create",
            "actor": REMOTE_ALICE,
            "object": note_document(dispatch_uri),
        }),
    };
    crate::federation::inbound::dispatcher::InboundActivityHandler::handle(
        &handler,
        &create_activity,
        &ctx,
    )
    .await
    .expect("CreateNoteHandler must ingest the wrapped Note");
    let via_dispatch = status_repository::find_by_uri(&app.pool, dispatch_uri)
        .await
        .expect("find_by_uri must succeed")
        .expect("CreateNoteHandler must have persisted the Note");

    // Path 2: StatusIngestService, given the identical Note document
    // directly (no Create wrapper, as an out-of-dispatch fetch would see).
    let out_of_dispatch_uri = "https://remote.example/notes/via-ingest-service";
    let service = service_for(
        &app,
        Arc::new(MockFederationHttpClient::new()),
        Arc::clone(&remote_actors),
    );
    let via_service = service
        .ingest_document(&note_document(out_of_dispatch_uri))
        .await
        .expect("StatusIngestService must ingest the identical Note");

    // Same actor (both authored by REMOTE_ALICE, resolved via the identical
    // RemoteActorResolver), same content/visibility/sensitivity -- the only
    // expected difference is the uri identity of the two distinct fixtures.
    assert_eq!(via_dispatch.actor_id, via_service.actor_id);
    assert_eq!(via_dispatch.content, via_service.content);
    assert_eq!(via_dispatch.visibility, via_service.visibility);
    assert_eq!(via_dispatch.sensitive, via_service.sensitive);
    assert_eq!(via_dispatch.spoiler_text, via_service.spoiler_text);
    assert!(!via_dispatch.local);
    assert!(!via_service.local);
}

/// Inserts a real `statuses` row directly, owned by `actor_id` -- mirrors
/// `inbound_handlers/tests.rs::insert_test_status`'s identical helper
/// (narrowed to this module's own reply-reflection test's needs).
async fn insert_test_status(app: &TestApp, actor_id: Id, visibility: Visibility) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let status = Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: Some(format!("https://kawasemi.example/@actor/{}", id.as_i64())),
        content: "a local parent post".to_string(),
        visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: now,
        edited_at: None,
    };
    status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    status
}
