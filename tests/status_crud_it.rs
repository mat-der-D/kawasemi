//! Integration tests for task 8.1 (`.kiro/specs/statuses-core/tasks.md`,
//! "8.1 統合テスト（CRUD・冪等・context・操作・投票）を整備する"): status
//! creation (CW/sensitive/media ownership/replies/language+hashtag
//! extraction), empty-content rejection, visibility-filtered retrieval,
//! delete/edit ownership enforcement, edit history/source, and
//! `Idempotency-Key` resend semantics — driven as real HTTP requests
//! through the fully-wired application router
//! (`crate::server::build_router`) booted by `spawn_test_app`
//! (Requirements 3.1-3.6, 5.1-5.3, 6.1, 6.4, 7.1, 7.2, 8.1, 8.2, 8.3, 8.5).
//!
//! Mirrors `tests/statuses_bootstrap_wiring_it.rs`'s own established
//! "drive `server::build_router(app.state.clone())` in-process via
//! `tower::ServiceExt::oneshot`" technique (real `AppState`, real Postgres,
//! real bearer/scope enforcement) rather than raw TCP sockets, so JSON
//! bodies are easy to assert on. Context (ancestors/descendants/visibility
//! exclusion) lives in `tests/status_context_it.rs`; reblog/favourite/
//! bookmark/pin live in `tests/interactions_it.rs`; poll voting lives in
//! `tests/polls_it.rs` — each a sibling, self-contained file per this
//! crate's own documented "small helper duplication across sibling test
//! modules" convention (see `tasks.md`'s Implementation Notes for 3.2/task
//! 4.1's `UndoKind`).
//!
//! Requirement 13.1 note: `StatusService::create_status` persists a
//! caller-supplied poll (see `status_service.rs`'s own "Poll handling" doc
//! comment — wired by a feature-level remediation round after this spec's
//! own tasks 5.1/5.3 had deliberately deferred it). This file's
//! `create_status_with_a_poll_is_created_and_mutual_exclusivity_with_media_holds`
//! test exercises real poll creation through the public HTTP API (a
//! poll-bearing create request succeeds and the response embeds a real
//! `poll` object), and separately confirms poll+media mutual exclusivity
//! still 422s; poll *voting* (13.2-13.5) is fully covered in
//! `tests/polls_it.rs` against directly-fixtured polls, the same
//! "insert fixtures directly, bypass the creating service" pattern
//! `poll_service.rs`'s own doc comment documents as the accepted technique
//! for exercising voting without re-deriving a poll-bearing status through
//! the endpoint each time.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::media::{Focus, Media, MediaState, MediaType, insert_media};
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::statuses::notification_sink::{
    NotificationEvent, NotificationEventSink, NotificationType,
};
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (each `tests/*.rs` file is its own compiled crate,
// so this deliberately duplicates `tests/statuses_bootstrap_wiring_it.rs`'s
// own established conventions rather than importing them — this crate's own
// documented convention). ----

async fn insert_actor_fixture(app: &TestApp, handle_str: &str) -> ResolvedActor {
    let owner_id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new(handle_str).expect("test handle must be valid"),
            actor_type: ActorType::Person,
            display_name: format!("Status CRUD IT {handle_str}"),
            summary: "an actor used by the status_crud_it integration test".to_string(),
        })
        .await
        .expect("create_actor (with signing key provisioning) must succeed");

    app.actor
        .directory()
        .resolve_actor_by_handle(&actor.handle)
        .await
        .expect("resolving the just-created actor must succeed")
        .expect("the just-created actor must be resolvable")
}

async fn register_test_app(app: &TestApp) -> Id {
    let now = app.runtime.clock.now();
    let registered = app_repository::register_app(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewApp {
            name: "Status CRUD IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write"]),
        },
    )
    .await
    .expect("register_app must succeed");
    registered.id
}

async fn issue_test_token(app: &TestApp, app_id: Id, actor_id: Id, scopes: &[&str]) -> String {
    let now = app.runtime.clock.now();
    let issued = token_repository::issue_token(
        &app.pool,
        app.runtime.ids.as_ref(),
        app.runtime.rng.as_ref(),
        app.state.oauth().token_hash_key(),
        now,
        NewAccessToken {
            app_id,
            actor_id,
            scopes: ModelScopeSet::new(scopes.iter().copied()),
        },
    )
    .await
    .expect("issue_token must succeed");
    issued.plaintext.expose_secret().to_string()
}

fn real_router(app: &TestApp) -> Router {
    server::build_router(app.state.clone())
}

fn req(method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    req_with_headers(method, path, token, body, &[])
}

fn req_with_headers(
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
    extra_headers: &[(&str, &str)],
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&value).expect("serialize body"),
            ))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    }
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router must not fail to produce a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body must be valid JSON")
    };
    (status, value)
}

fn assert_error_shape(body: &Value) {
    assert!(
        body.get("error").and_then(Value::as_str).is_some(),
        "expected a Mastodon-compatible {{\"error\": ...}} body, got: {body}"
    );
}

/// Inserts a `Ready` media row owned by `actor_id`, bypassing the real
/// upload/processing pipeline entirely (`media_repository::find_owned`,
/// which `StatusService::create_status`'s ownership check consults, has no
/// state filter — only `id`+`actor_id` matter — so a directly-inserted row
/// is a legitimate, minimal fixture for this file's own media-ownership
/// scenarios).
async fn insert_owned_media(app: &TestApp, actor_id: Id) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let media = Media {
        id,
        actor_id,
        media_type: MediaType::Image,
        state: MediaState::Ready,
        description: None,
        focus: Focus::default(),
        meta: None,
        blurhash: None,
        created_at: now,
    };
    insert_media(&app.pool, &media, "status-crud-it-object-key", "image/png")
        .await
        .expect("insert media fixture");
    id
}

/// Records every [`NotificationEventSink::emit`] call made through the real,
/// fully-wired router — task 10.5's own "observe task 9.2's mention emit
/// through the public API" technique. Registered via
/// `app.state.statuses().notification_sink_registry().set_sink(..)` (see
/// `src/statuses/notification_sink.rs`'s own doc comment, "Registry: one
/// replaceable slot" — every composition-root caller, including
/// `spawn_test_app`'s, shares one instance, so replacing the sink here is
/// visible to the exact `StatusService` the router dispatches to).
#[derive(Default)]
struct RecordingNotificationSink {
    events: Mutex<Vec<NotificationEvent>>,
}

impl RecordingNotificationSink {
    fn events(&self) -> Vec<NotificationEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl NotificationEventSink for RecordingNotificationSink {
    fn emit<'a>(
        &'a self,
        event: NotificationEvent,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), kawasemi::error::AppError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.events.lock().unwrap().push(event);
            Ok(())
        })
    }
}

// ==== Creation (Requirements 3.1-3.6) ====

#[tokio::test]
async fn create_status_reflects_content_warning_and_sensitive_flag() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_cw").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&token),
            Some(json!({
                "status": "a spicy take",
                "spoiler_text": "cw: spicy",
                "sensitive": true
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body["content"], "a spicy take");
    assert_eq!(body["spoiler_text"], "cw: spicy");
    assert_eq!(body["sensitive"], true);
    assert_eq!(body["visibility"], "public");
    assert!(body["edited_at"].is_null());

    app.cleanup().await;
}

#[tokio::test]
async fn create_status_attaches_owned_media_and_rejects_foreign_or_unknown_media() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_media").await;
    let bob = insert_actor_fixture(&app, "bob_media").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let alice_media = insert_owned_media(&app, alice.id).await;
    let bob_media = insert_owned_media(&app, bob.id).await;

    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({
                "status": "look at this",
                "media_ids": [alice_media.as_i64().to_string()]
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    let attachments = body["media_attachments"]
        .as_array()
        .expect("media_attachments array");
    assert_eq!(attachments.len(), 1, "got: {attachments:?}");
    assert_eq!(
        attachments[0]["id"].as_str(),
        Some(alice_media.as_i64().to_string()).as_deref()
    );

    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({
                "status": "not mine",
                "media_ids": [bob_media.as_i64().to_string()]
            })),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "foreign-owned media must be rejected: {body:?}"
    );
    assert_error_shape(&body);

    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({
                "status": "ghost media",
                "media_ids": ["999999999999"]
            })),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a nonexistent media id must be rejected: {body:?}"
    );
    assert_error_shape(&body);

    app.cleanup().await;
}

#[tokio::test]
async fn create_status_establishes_reply_relationship_and_increments_parent_replies_count() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_reply").await;
    let bob = insert_actor_fixture(&app, "bob_reply").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let (_, parent) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({"status": "a parent post"})),
        ),
    )
    .await;
    let parent_id = parent["id"].as_str().unwrap().to_string();

    let (status, reply) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&bob_token),
            Some(json!({"status": "a reply", "in_reply_to_id": parent_id})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {reply:?}");
    assert_eq!(reply["in_reply_to_id"].as_str(), Some(parent_id.as_str()));
    assert_eq!(
        reply["in_reply_to_account_id"].as_str(),
        Some(alice.id.as_i64().to_string()).as_deref()
    );

    let (status, refreshed_parent) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{parent_id}"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {refreshed_parent:?}");
    assert_eq!(refreshed_parent["replies_count"], 1);

    app.cleanup().await;
}

#[tokio::test]
async fn create_status_records_language_and_extracts_hashtags() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_lang").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&token),
            Some(json!({
                "status": "learning #rust and #kawasemi today",
                "language": "en"
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body["language"], "en");
    let tags = body["tags"].as_array().expect("tags array");
    let names: Vec<&str> = tags.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"rust"), "got tags: {tags:?}");
    assert!(names.contains(&"kawasemi"), "got tags: {tags:?}");

    app.cleanup().await;
}

#[tokio::test]
async fn create_status_rejects_empty_content_with_422() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_empty").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&token),
            Some(json!({"status": "   "})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "got: {body:?}");
    assert_error_shape(&body);

    app.cleanup().await;
}

#[tokio::test]
async fn create_status_requires_write_statuses_scope() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_scope").await;
    let app_id = register_test_app(&app).await;

    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            None,
            Some(json!({"status": "hi"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "got: {body:?}");
    assert_error_shape(&body);

    let wrong_scope_token = issue_test_token(&app, app_id, alice.id, &["read:statuses"]).await;
    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&wrong_scope_token),
            Some(json!({"status": "hi"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "got: {body:?}");
    assert_error_shape(&body);

    app.cleanup().await;
}

/// See this file's own doc comment (Requirement 13.1 note): a poll-bearing
/// create request succeeds and the response embeds a real `poll` object
/// with the given options; poll+media mutual exclusivity still holds.
#[tokio::test]
async fn create_status_with_a_poll_is_created_and_mutual_exclusivity_with_media_holds() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_poll_create").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&token),
            Some(json!({
                "status": "vote now",
                "poll": {"options": ["a", "b"], "multiple": false}
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    let poll = body
        .get("poll")
        .expect("a poll-bearing create response must embed a poll object");
    assert!(!poll["id"].as_str().unwrap_or_default().is_empty());
    assert_eq!(poll["multiple"], json!(false));
    assert_eq!(poll["voted"], json!(false));
    let option_titles: Vec<&str> = poll["options"]
        .as_array()
        .expect("poll.options must be an array")
        .iter()
        .map(|option| option["title"].as_str().expect("option.title"))
        .collect();
    assert_eq!(option_titles, vec!["a", "b"]);
    for option in poll["options"].as_array().unwrap() {
        assert_eq!(option["votes_count"], json!(0));
    }

    let media_id = insert_owned_media(&app, alice.id).await;
    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&token),
            Some(json!({
                "status": "vote now",
                "media_ids": [media_id.as_i64().to_string()],
                "poll": {"options": ["a", "b"], "multiple": false}
            })),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a poll and media together must be rejected: {body:?}"
    );
    assert_error_shape(&body);

    app.cleanup().await;
}

// ==== Idempotency (Requirements 5.1-5.3) ====

#[tokio::test]
async fn idempotency_key_resend_returns_the_original_status_without_duplicating() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_idem").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let (status, first) = send(
        &router,
        req_with_headers(
            "POST",
            "/api/v1/statuses",
            Some(&token),
            Some(json!({"status": "idempotent post"})),
            &[("Idempotency-Key", "abc-123")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {first:?}");
    let first_id = first["id"].as_str().unwrap().to_string();

    let (status, second) = send(
        &router,
        req_with_headers(
            "POST",
            "/api/v1/statuses",
            Some(&token),
            Some(json!({"status": "a different body that must be ignored on resend"})),
            &[("Idempotency-Key", "abc-123")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {second:?}");
    assert_eq!(second["id"].as_str(), Some(first_id.as_str()));
    assert_eq!(
        second["content"], "idempotent post",
        "a resend with the same Idempotency-Key must return the original response, not a new post"
    );

    app.cleanup().await;
}

#[tokio::test]
async fn no_idempotency_key_always_creates_a_new_status() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_no_idem").await;
    let app_id = register_test_app(&app).await;
    let token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let (status, first) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&token),
            Some(json!({"status": "post one"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {first:?}");
    let (status, second) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&token),
            Some(json!({"status": "post two"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {second:?}");
    assert_ne!(
        first["id"], second["id"],
        "without an Idempotency-Key, every create request must produce a new post"
    );

    app.cleanup().await;
}

// ==== Retrieval / visibility filtering (Requirements 6.1, 6.4) ====

#[tokio::test]
async fn show_status_filters_by_visibility_for_authenticated_and_unauthenticated_viewers() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_vis").await;
    let bob = insert_actor_fixture(&app, "bob_vis").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["read:statuses"]).await;

    let mut ids = std::collections::HashMap::new();
    for vis in ["public", "unlisted", "private", "direct"] {
        let (status, body) = send(
            &router,
            req(
                "POST",
                "/api/v1/statuses",
                Some(&alice_token),
                Some(json!({"status": format!("a {vis} post"), "visibility": vis})),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "creating a {vis} post: {body:?}");
        ids.insert(vis, body["id"].as_str().unwrap().to_string());
    }

    // The author sees every visibility of their own post.
    for vis in ["public", "unlisted", "private", "direct"] {
        let (status, body) = send(
            &router,
            req(
                "GET",
                &format!("/api/v1/statuses/{}", ids[vis]),
                Some(&alice_token),
                None,
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the author must always see their own {vis} post: {body:?}"
        );
    }

    // An authenticated non-follower sees public/unlisted only.
    for (vis, expected) in [
        ("public", StatusCode::OK),
        ("unlisted", StatusCode::OK),
        ("private", StatusCode::NOT_FOUND),
        ("direct", StatusCode::NOT_FOUND),
    ] {
        let (status, body) = send(
            &router,
            req(
                "GET",
                &format!("/api/v1/statuses/{}", ids[vis]),
                Some(&bob_token),
                None,
            ),
        )
        .await;
        assert_eq!(
            status, expected,
            "non-follower viewing a {vis} post: {body:?}"
        );
    }

    // An unauthenticated viewer sees public only.
    for (vis, expected) in [
        ("public", StatusCode::OK),
        ("unlisted", StatusCode::NOT_FOUND),
        ("private", StatusCode::NOT_FOUND),
        ("direct", StatusCode::NOT_FOUND),
    ] {
        let (status, body) = send(
            &router,
            req("GET", &format!("/api/v1/statuses/{}", ids[vis]), None, None),
        )
        .await;
        assert_eq!(
            status, expected,
            "unauthenticated viewer viewing a {vis} post: {body:?}"
        );
    }

    let (status, _) = send(
        &router,
        req("GET", "/api/v1/statuses/999999999999", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an unknown id must be 404");

    app.cleanup().await;
}

// ==== Deletion (Requirements 7.1, 7.2) ====

#[tokio::test]
async fn delete_status_enforces_ownership_scope_and_removes_the_post() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_del").await;
    let bob = insert_actor_fixture(&app, "bob_del").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    let (_, created) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({"status": "delete me"})),
        ),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, body) = send(
        &router,
        req("DELETE", &format!("/api/v1/statuses/{id}"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "got: {body:?}");

    let (status, body) = send(
        &router,
        req(
            "DELETE",
            &format!("/api/v1/statuses/{id}"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a non-owner must not be able to delete another actor's post: {body:?}"
    );

    let (status, body) = send(
        &router,
        req(
            "DELETE",
            &format!("/api/v1/statuses/{id}"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");
    assert_eq!(body["content"], "delete me");

    let (status, _) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{id}"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a deleted post must no longer be retrievable, even by its own author"
    );

    app.cleanup().await;
}

// ==== Edit / history / source (Requirements 8.1, 8.2, 8.3, 8.5) ====

#[tokio::test]
async fn editing_a_status_updates_it_records_history_and_exposes_source_owner_only() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_edit").await;
    let bob = insert_actor_fixture(&app, "bob_edit").await;
    let app_id = register_test_app(&app).await;
    let alice_token =
        issue_test_token(&app, app_id, alice.id, &["write:statuses", "read:statuses"]).await;
    let bob_token =
        issue_test_token(&app, app_id, bob.id, &["write:statuses", "read:statuses"]).await;

    let (_, created) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({"status": "original content", "spoiler_text": "orig cw"})),
        ),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    assert!(created["edited_at"].is_null());

    // A non-owner cannot edit.
    let (status, body) = send(
        &router,
        req(
            "PUT",
            &format!("/api/v1/statuses/{id}"),
            Some(&bob_token),
            Some(json!({
                "status": "hijacked", "spoiler_text": "", "sensitive": false, "media_ids": []
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "got: {body:?}");

    // The owner edits.
    let (status, edited) = send(
        &router,
        req(
            "PUT",
            &format!("/api/v1/statuses/{id}"),
            Some(&alice_token),
            Some(json!({
                "status": "edited content", "spoiler_text": "new cw", "sensitive": true,
                "media_ids": []
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {edited:?}");
    assert_eq!(edited["content"], "edited content");
    assert_eq!(edited["spoiler_text"], "new cw");
    assert_eq!(edited["sensitive"], true);
    assert!(!edited["edited_at"].is_null());

    // The pre-edit version is retained in history.
    let (status, history) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{id}/history"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {history:?}");
    let versions = history.as_array().expect("history array");
    assert_eq!(versions.len(), 1, "got: {history:?}");
    assert_eq!(versions[0]["content"], "original content");
    assert_eq!(versions[0]["spoiler_text"], "orig cw");
    assert_eq!(versions[0]["sensitive"], false);
    assert!(versions[0]["created_at"].is_string());

    // Source returns the current raw text/CW, owner-only.
    let (status, source) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{id}/source"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {source:?}");
    assert_eq!(source["text"], "edited content");
    assert_eq!(source["spoiler_text"], "new cw");

    let (status, body) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{id}/source"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "source must be owner-only: {body:?}"
    );

    let (status, body) = send(
        &router,
        req("GET", &format!("/api/v1/statuses/{id}/source"), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "got: {body:?}");

    app.cleanup().await;
}

/// Task 10.5 (`.kiro/specs/statuses-core/tasks.md`, "7.1 で指摘済みの未テスト
/// ハンドラ...に...専用テストを追加する"): `GET /api/v1/statuses/:id/history`
/// (`status_history`, `design.md`'s API Contract row: "Bearer 任意" / errors
/// "404" only — no scope, no owner-only gate) is, per
/// `StatusService::history`'s own doc comment, "subject to the same
/// visibility rule as `show`" — not owner-scoped like `source`. The
/// `editing_a_status_...` test above only ever exercises the *owner*
/// fetching their own post's history, so this test proves the actually
/// distinct behavior design.md specifies: a public post's history is visible
/// to anyone (even unauthenticated), while a private post's history 404s for
/// everyone except its author (Requirements 6.1, 6.2, 8.2).
#[tokio::test]
async fn status_history_endpoint_is_gated_by_the_same_visibility_rule_as_show() {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_hist_vis").await;
    let bob = insert_actor_fixture(&app, "bob_hist_vis").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;
    let bob_token = issue_test_token(&app, app_id, bob.id, &["write:statuses"]).await;

    // A public post, edited once so it has a non-empty history.
    let (_, created) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({"status": "public original", "visibility": "public"})),
        ),
    )
    .await;
    let public_id = created["id"].as_str().unwrap().to_string();
    let (status, _) = send(
        &router,
        req(
            "PUT",
            &format!("/api/v1/statuses/{public_id}"),
            Some(&alice_token),
            Some(json!({
                "status": "public edited", "spoiler_text": "", "sensitive": false,
                "media_ids": []
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A private post, edited once too.
    let (_, created) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({"status": "private original", "visibility": "private"})),
        ),
    )
    .await;
    let private_id = created["id"].as_str().unwrap().to_string();
    let (status, _) = send(
        &router,
        req(
            "PUT",
            &format!("/api/v1/statuses/{private_id}"),
            Some(&alice_token),
            Some(json!({
                "status": "private edited", "spoiler_text": "", "sensitive": false,
                "media_ids": []
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The public post's history is visible to a non-owner...
    let (status, history) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{public_id}/history"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {history:?}");
    assert_eq!(history.as_array().map(Vec::len), Some(1));
    assert_eq!(history[0]["content"], "public original");

    // ...and to an unauthenticated caller (design.md's "Bearer 任意").
    let (status, history) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{public_id}/history"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {history:?}");
    assert_eq!(history.as_array().map(Vec::len), Some(1));

    // The private post's history is invisible to a non-owner...
    let (status, body) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{private_id}/history"),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a private post's history must not leak to a non-follower: {body:?}"
    );

    // ...and to an unauthenticated caller.
    let (status, body) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{private_id}/history"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a private post's history must not leak unauthenticated: {body:?}"
    );

    // ...but remains visible to its own author.
    let (status, history) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/statuses/{private_id}/history"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {history:?}");
    assert_eq!(history.as_array().map(Vec::len), Some(1));

    app.cleanup().await;
}

// ==== Combination: poll creation + mention notification (task 10.5) ====

/// Task 10.5's own combination-path requirement: "投票を含む投稿がメンション
/// も同時に含む場合に 9.1 remediation の poll 作成ブロックと 9.2 の
/// メンション通知 emit ブロックが正しく共存することを直接証明する結合テスト"
/// (Requirements 13.1, 9.1, 9.2). A single `POST /api/v1/statuses` request
/// whose content both mentions a second local actor and carries a `poll`
/// input exercises two independent remediation blocks inside
/// `StatusService::create_status` back to back (see that module's own doc
/// comment / `tasks.md`'s "9.2 追補" and "9.2" Implementation Notes): the
/// poll-persistence block (`poll_repository::insert_poll`, wired by the
/// 9.2-追補 remediation for Requirement 13.1) and the mention-notification
/// emit loop (task 9.2, only exercised for a poll-less post in every existing
/// test). This proves neither block's control flow accidentally short-
/// circuits or skips the other when both fire in the same request:
/// - the poll is actually persisted (independently queryable via
///   `GET /api/v1/polls/:id`, not merely echoed back in the create response)
/// - exactly one `Mention` `NotificationEvent` is emitted for the mentioned
///   actor, observed through the real `NotificationSinkRegistry` the router's
///   own `StatusService` dispatches to (task 9.2's established test-double
///   technique, driven here through the public HTTP API rather than an
///   in-process service call).
#[tokio::test]
async fn create_status_with_a_poll_and_a_mention_persists_the_poll_and_emits_the_mention_notification()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);
    let alice = insert_actor_fixture(&app, "alice_poll_mention").await;
    let bob = insert_actor_fixture(&app, "bob_poll_mention_target").await;
    let app_id = register_test_app(&app).await;
    let alice_token = issue_test_token(&app, app_id, alice.id, &["write:statuses"]).await;

    let sink = Arc::new(RecordingNotificationSink::default());
    app.state
        .statuses()
        .notification_sink_registry()
        .set_sink(Arc::clone(&sink) as Arc<dyn NotificationEventSink>);

    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({
                "status": "hey @bob_poll_mention_target, pick one",
                "poll": {"options": ["red", "blue"], "multiple": false}
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body:?}");

    // The poll block fired: the response embeds a real poll...
    let poll = body
        .get("poll")
        .expect("a poll-bearing create response must embed a poll object");
    let poll_id = poll["id"].as_str().expect("poll.id must be a string");
    let option_titles: Vec<&str> = poll["options"]
        .as_array()
        .expect("poll.options must be an array")
        .iter()
        .map(|option| option["title"].as_str().expect("option.title"))
        .collect();
    assert_eq!(option_titles, vec!["red", "blue"]);

    // ...and it is independently, durably persisted (not just echoed back):
    // a fresh `GET /api/v1/polls/:id` round-trip proves real persistence.
    let (status, fetched_poll) = send(
        &router,
        req(
            "GET",
            &format!("/api/v1/polls/{poll_id}"),
            Some(&alice_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {fetched_poll:?}");
    assert_eq!(fetched_poll["id"], *poll_id);
    assert_eq!(
        fetched_poll["options"].as_array().map(Vec::len),
        Some(2),
        "the persisted poll must retain both options: {fetched_poll:?}"
    );

    // The mention block also fired, in the same request: exactly one
    // `Mention` NotificationEvent for bob, not skipped/short-circuited by
    // the poll block running first.
    let events = sink.events();
    let mention_events: Vec<&NotificationEvent> = events
        .iter()
        .filter(|event| event.kind == NotificationType::Mention)
        .collect();
    assert_eq!(
        mention_events.len(),
        1,
        "expected exactly one Mention NotificationEvent, got: {events:?}"
    );
    let mention = mention_events[0];
    assert_eq!(mention.recipient, AccountRef::Local(bob.id));
    assert_eq!(mention.origin, AccountRef::Local(alice.id));

    app.cleanup().await;
}
