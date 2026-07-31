//! Integration tests for the feature-level `kiro-validate-impl` NO-GO
//! remediation (round 1, 2026-07-31): `.kiro/specs/social-graph/design.md`
//! line 39's "Boundary Commitments" entry names social-graph as the real
//! supplier of statuses-core's `RelationshipQuery` delegation port
//! (`src/statuses/visibility.rs`, `viewer_relation()`/`followers_of()`),
//! which `VisibilityPolicy::is_visible`'s `private` branch and
//! `Addressing::derive_recipients`'s followers-collection addressing both
//! depend on — but until this remediation, nothing ever registered a real
//! implementation: `crate::statuses::build_statuses_module` permanently
//! hardcoded `NoRelationshipQuery` (always "not a follower" / always empty
//! followers), so `private`-visibility posts were unconditionally
//! invisible to *every* viewer except the author, regardless of actual
//! follow relationships, and DM-style followers-collection addressing
//! always resolved to zero recipients.
//!
//! This file proves both halves of the fix are genuinely wired end-to-end
//! through `spawn_test_app`'s real composition root
//! (`crate::social_graph::build_social_graph_module` registering
//! `crate::social_graph::providers::RelationshipQueryImpl` into
//! `crate::statuses::visibility::RelationshipQueryRegistry`), not merely
//! unit-tested in isolation:
//!
//! - [`private_status_becomes_visible_to_a_real_follower_and_stays_invisible_to_a_non_follower`]
//!   drives `POST /api/v1/accounts/:id/follow` (social-graph's own real
//!   HTTP API) and `GET /api/v1/statuses/:id` (statuses-core's own real HTTP
//!   API) together — the two specs' independently-committed, independently
//!   -reviewed endpoints — and shows a `private` post's visibility outcome
//!   flips from 404 to 200 the moment a real follow relationship is
//!   established, and flips back to 404 the moment it is removed. This is
//!   this task's own "before" (`RED_PHASE`) evidence: before this
//!   remediation, the post-follow assertion in this same test would have
//!   failed (404 forever, since `NoRelationshipQuery::viewer_relation`
//!   always answers `is_follower: false`).
//! - [`relationship_query_registry_reflects_real_local_and_remote_follower_state`]
//!   exercises `AppState::statuses().relationship_query_registry()`
//!   directly — the exact registry `StatusService`/`InteractionService`/
//!   `PollService` already hold — proving `viewer_relation`/`followers_of`
//!   both answer from real `follows`-table state (including a genuinely
//!   remote follower, mapped to a `Recipient::Remote`), not a
//!   fixed/default value.
//!
//! Mirrors `tests/social_graph_relationship_provider_it.rs`'s own
//! established "prove `spawn_test_app()`'s own *default*, already-
//! registered implementation genuinely reflects real state" technique,
//! applied here to the `RelationshipQuery` port instead of
//! `RelationshipStateProvider`; and `tests/status_crud_it.rs`'s own
//! `retrieval_is_filtered_by_visibility_and_viewer` test's fixture/request
//! conventions, extended with a real follow relationship that test
//! deliberately did not exercise (its own comments only ever compare
//! "author" vs. "non-follower" vs. "unauthenticated" — never a genuine
//! follower, because until this remediation no viewer could ever become
//! one from `VisibilityPolicy::is_visible`'s own point of view).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use kawasemi::accounts::model::{ProfileField, RemoteAccount};
use kawasemi::accounts::remote_repository::upsert_remote;
use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor, ResolvedActor};
use kawasemi::domain::{AccountRef, Id};
use kawasemi::federation::Recipient;
use kawasemi::oauth::app_repository::{self, NewApp};
use kawasemi::oauth::model::ScopeSet as ModelScopeSet;
use kawasemi::oauth::token_repository::{self, NewAccessToken};
use kawasemi::server;
use kawasemi::social_graph::Follow;
use kawasemi::social_graph::repository::upsert_follow;
use kawasemi::statuses::visibility::RelationshipQuery;
use kawasemi::test_harness::{TestApp, spawn_test_app};

// ---- Fixture plumbing (duplicated per this crate's own documented
// sibling-test-file convention — see `tests/status_crud_it.rs`'s own doc
// comment). ----

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
            display_name: format!("Social Graph Visibility Query IT {handle_str}"),
            summary: "an actor used by the social_graph_visibility_query_it integration test"
                .to_string(),
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
            name: "Social Graph Visibility Query IT Client".to_string(),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ModelScopeSet::new(["read", "write", "follow"]),
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
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
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

async fn create_remote_follower(app: &TestApp, actor_uri: &str) -> Id {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    upsert_remote(
        &app.pool,
        &RemoteAccount {
            id,
            actor_uri: actor_uri.to_string(),
            username: actor_uri.rsplit('/').next().unwrap_or("remote").to_string(),
            domain: "remote-follower.example".to_string(),
            display_name: "Social Graph Visibility Query IT Remote Follower".to_string(),
            note: String::new(),
            url: actor_uri.to_string(),
            avatar_url: None,
            header_url: None,
            fields: Vec::<ProfileField>::new(),
            bot: false,
            locked: false,
            fetched_at: now,
        },
    )
    .await
    .expect("upsert_remote must succeed");
    id
}

// ==========================================================================
// HTTP-level: real follow flips a `private` post from 404 to 200 and back
// ==========================================================================

/// Requirement coverage: design.md line 39's Boundary Commitments
/// ("statuses-core が...のために定義・所有する委譲ポート契約
/// `RelationshipQuery`...への本実装供給"), statuses-core Requirement 4.1
/// ("`private` の可視判定は viewer が投稿者のフォロワーかどうか").
///
/// Drives the real follow endpoint and the real status-retrieval endpoint
/// together, proving `VisibilityPolicy::is_visible`'s `private` branch now
/// consults social-graph's real `follows` table through the registered
/// `RelationshipQueryImpl`, not the permanently-`false` `NoRelationshipQuery`
/// default it used before this remediation.
#[tokio::test]
async fn private_status_becomes_visible_to_a_real_follower_and_stays_invisible_to_a_non_follower() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_relq").await;
    let bob = insert_actor_fixture(&app, "bob_relq").await;
    let carol = insert_actor_fixture(&app, "carol_relq").await;
    let oauth_app_id = register_test_app(&app).await;

    let alice_token = issue_test_token(&app, oauth_app_id, alice.id, &["write:statuses"]).await;
    let bob_token =
        issue_test_token(&app, oauth_app_id, bob.id, &["read:statuses", "follow"]).await;
    let carol_token = issue_test_token(&app, oauth_app_id, carol.id, &["read:statuses"]).await;

    // Alice posts a `private` status.
    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(json!({"status": "only my followers should see this", "visibility": "private"})),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "creating the private post: {body:?}"
    );
    let status_path = format!("/api/v1/statuses/{}", body["id"].as_str().unwrap());

    // Before any follow exists: neither Bob nor Carol can see it (this is
    // the "before" state `NoRelationshipQuery` produced unconditionally,
    // and still produces for a genuine non-follower after this
    // remediation).
    for (label, token) in [
        ("bob (not yet a follower)", &bob_token),
        ("carol", &carol_token),
    ] {
        let (status, body) = send(&router, req("GET", &status_path, Some(token), None)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{label} must not see the private post before following: {body:?}"
        );
    }

    // Bob really follows Alice through social-graph's own HTTP API
    // (same-server follow: established immediately, no approval step).
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/follow", alice.id.as_i64()),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bob following alice: {body:?}");
    assert_eq!(
        body["following"].as_bool(),
        Some(true),
        "the follow must be established immediately (same-server): {body:?}"
    );

    // Now Bob (a real follower) sees the private post; Carol (still not a
    // follower) still does not. This is this task's own "after" proof that
    // RelationshipQueryImpl is genuinely wired into VisibilityPolicy.
    let (status, body) = send(&router, req("GET", &status_path, Some(&bob_token), None)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bob, now a real follower, must see the private post: {body:?}"
    );

    let (status, body) = send(&router, req("GET", &status_path, Some(&carol_token), None)).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "carol, still not a follower, must not see the private post: {body:?}"
    );

    // Bob unfollows: the very next request observes live state again (no
    // caching), matching RelationshipQueryImpl's own "fresh query every
    // call" contract.
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/unfollow", alice.id.as_i64()),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bob unfollowing alice: {body:?}");

    let (status, body) = send(&router, req("GET", &status_path, Some(&bob_token), None)).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "bob, no longer a follower, must lose visibility immediately: {body:?}"
    );
}

// ==========================================================================
// Direct registry-level: viewer_relation/followers_of reflect real state,
// including a genuinely remote follower
// ==========================================================================

/// Requirement coverage: design.md line 39's Boundary Commitments
/// (`followers_of` supplying `Addressing::derive_recipients`'s
/// followers-collection addressing); statuses-core Requirement 6.1
/// (`viewer_relation`).
///
/// Calls `AppState::statuses().relationship_query_registry()` directly —
/// the exact registry every statuses-core service already holds — proving
/// both trait methods answer from real `follows`-table state rather than
/// `NoRelationshipQuery`'s fixed defaults, and that `followers_of` maps a
/// local follower to `Recipient::Local` and a remote follower to
/// `Recipient::Remote` (the same `{actor_uri}/inbox` interim convention
/// `FollowService::resolve_target` already established).
#[tokio::test]
async fn relationship_query_registry_reflects_real_local_and_remote_follower_state() {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_relq2").await;
    let bob = insert_actor_fixture(&app, "bob_relq2").await;
    let carol = insert_actor_fixture(&app, "carol_relq2").await;
    let oauth_app_id = register_test_app(&app).await;
    let bob_token = issue_test_token(&app, oauth_app_id, bob.id, &["follow"]).await;

    let registry = app.state.statuses().relationship_query_registry();

    // Nobody follows Alice yet: every viewer resolves to "not a follower",
    // and there are no followers to address at all -- matching
    // NoRelationshipQuery's own safe-default shape structurally, but this
    // time because live `follows`-table state genuinely has zero rows, not
    // because the port is unwired.
    let rel = registry
        .viewer_relation(alice.id, Some(bob.id))
        .await
        .expect("viewer_relation must succeed");
    assert!(!rel.is_follower, "bob is not yet a follower of alice");

    let rel = registry
        .viewer_relation(alice.id, None)
        .await
        .expect("viewer_relation must succeed for an unauthenticated viewer");
    assert!(
        !rel.is_follower,
        "an unauthenticated viewer is never a follower"
    );

    let followers = registry
        .followers_of(alice.id)
        .await
        .expect("followers_of must succeed");
    assert!(
        followers.is_empty(),
        "alice has no followers yet: {followers:?}"
    );

    // Bob really follows Alice (real HTTP API, same-server, established
    // immediately).
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/follow", alice.id.as_i64()),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bob following alice: {body:?}");

    // A genuinely remote account also follows Alice -- seeded directly at
    // the repository layer (mirrors
    // `tests/social_graph_relationship_provider_it.rs`'s own established
    // "seed a relationship row directly, then observe the real supplied
    // port" technique), since establishing a real remote follow would
    // require a second full test-harness instance and HTTP-signed
    // federation round trip (`tests/social_graph_inbound_it.rs`'s own much
    // heavier fixture), which is not needed to prove this port's own
    // local/remote `Recipient` mapping.
    let remote_actor_uri = "https://remote-follower.example/users/dave_relq2";
    let remote_id = create_remote_follower(&app, remote_actor_uri).await;
    upsert_follow(
        &app.pool,
        app.runtime.ids.next_id(),
        &Follow {
            follower: AccountRef::Remote(remote_id),
            followee: AccountRef::Local(alice.id),
            reblogs: true,
            notify: false,
            languages: Vec::new(),
            activity_id: "https://remote-follower.example/activities/follow-1".to_string(),
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("seeding the remote follow row must succeed");

    // Bob (local) resolves as a real follower now; Carol (never followed)
    // still does not.
    let rel = registry
        .viewer_relation(alice.id, Some(bob.id))
        .await
        .expect("viewer_relation must succeed");
    assert!(rel.is_follower, "bob is now a real follower of alice");

    let rel = registry
        .viewer_relation(alice.id, Some(carol.id))
        .await
        .expect("viewer_relation must succeed");
    assert!(!rel.is_follower, "carol never followed alice");

    // followers_of now includes both the real local follower (as a
    // Recipient::Local, by handle) and the real remote follower (as a
    // Recipient::Remote, at the documented interim inbox convention) -- and
    // nothing else.
    let followers = registry
        .followers_of(alice.id)
        .await
        .expect("followers_of must succeed");
    assert_eq!(
        followers.len(),
        2,
        "alice must have exactly her two real followers, no more, no fewer: {followers:?}"
    );
    assert!(
        followers.contains(&Recipient::Local(bob.handle.clone())),
        "bob must be resolved as a Recipient::Local by handle: {followers:?}"
    );
    assert!(
        followers.contains(&Recipient::Remote {
            inbox: format!("{remote_actor_uri}/inbox"),
            shared_inbox: None,
        }),
        "the remote follower must be resolved as a Recipient::Remote at the documented interim \
         inbox convention: {followers:?}"
    );
}

// ==========================================================================
// HTTP-level: GET /api/v1/accounts/:id/statuses (`AccountStatusesProviderImpl`)
// also flips a `private` post from hidden to visible for a real follower
// ==========================================================================

/// Requirement coverage: same as
/// [`private_status_becomes_visible_to_a_real_follower_and_stays_invisible_to_a_non_follower`]
/// above, but exercised through `GET /api/v1/accounts/:id/statuses`
/// (`AccountStatusesProviderImpl::visible_to`,
/// `src/statuses/account_provider.rs`) instead of `GET /api/v1/statuses/:id`
/// (`StatusService`) — the two are structurally separate call paths (
/// `AccountStatusesProviderImpl` does not take a generic `R:
/// RelationshipQuery` parameter at all; it holds its own
/// `RelationshipQueryRegistry` field supplied by
/// `crate::statuses::register_account_ports`), so proving the direct
/// `GET /api/v1/statuses/:id` path is wired does not by itself prove this
/// one is. This is the feature-level `kiro-validate-impl` remediation round
/// 1 follow-up's own "before" (`RED_PHASE`) evidence: before this
/// follow-up, `AccountStatusesProviderImpl::visible_to` constructed a bare
/// `NoRelationshipQuery` directly, so the post-follow assertion below would
/// have failed (the private post would still be absent from the page, since
/// `NoRelationshipQuery::viewer_relation` always answers `is_follower:
/// false` regardless of the real `follows`-table state this test
/// establishes).
#[tokio::test]
async fn accounts_statuses_list_reveals_a_private_status_to_a_real_follower_and_hides_it_from_a_non_follower()
 {
    let app = spawn_test_app().await;
    let router = real_router(&app);

    let alice = insert_actor_fixture(&app, "alice_relq3").await;
    let bob = insert_actor_fixture(&app, "bob_relq3").await;
    let carol = insert_actor_fixture(&app, "carol_relq3").await;
    let oauth_app_id = register_test_app(&app).await;

    let alice_token = issue_test_token(&app, oauth_app_id, alice.id, &["write:statuses"]).await;
    let bob_token =
        issue_test_token(&app, oauth_app_id, bob.id, &["read:statuses", "follow"]).await;
    let carol_token = issue_test_token(&app, oauth_app_id, carol.id, &["read:statuses"]).await;

    // Alice posts a `private` status.
    let (status, body) = send(
        &router,
        req(
            "POST",
            "/api/v1/statuses",
            Some(&alice_token),
            Some(
                json!({"status": "only my followers should see this via the accounts list", "visibility": "private"}),
            ),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "creating the private post: {body:?}"
    );
    let private_id = body["id"].as_str().unwrap().to_string();
    let accounts_statuses_path = format!("/api/v1/accounts/{}/statuses", alice.id.as_i64());

    // Before any follow exists: neither Bob nor Carol see the private post
    // in alice's accounts-statuses list (this is the "before" state
    // `NoRelationshipQuery` produced unconditionally, and still produces for
    // a genuine non-follower after this remediation).
    for (label, token) in [
        ("bob (not yet a follower)", &bob_token),
        ("carol", &carol_token),
    ] {
        let (status, body) = send(
            &router,
            req("GET", &accounts_statuses_path, Some(token), None),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{label} listing alice's statuses: {body:?}"
        );
        let ids: Vec<&str> = body
            .as_array()
            .expect("page body must be a JSON array")
            .iter()
            .map(|item| item["id"].as_str().expect("id must be a string"))
            .collect();
        assert!(
            !ids.contains(&private_id.as_str()),
            "{label} must not see the private post in alice's accounts-statuses list before \
             following: {ids:?}"
        );
    }

    // Bob really follows Alice through social-graph's own HTTP API
    // (same-server follow: established immediately, no approval step).
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/follow", alice.id.as_i64()),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bob following alice: {body:?}");
    assert_eq!(
        body["following"].as_bool(),
        Some(true),
        "the follow must be established immediately (same-server): {body:?}"
    );

    // Now Bob (a real follower) sees the private post in alice's
    // accounts-statuses list; Carol (still not a follower) still does not.
    // This is this follow-up's own "after" proof that
    // `AccountStatusesProviderImpl` is genuinely wired into the same
    // `RelationshipQueryRegistry` the direct-retrieval path already is.
    let (status, body) = send(
        &router,
        req("GET", &accounts_statuses_path, Some(&bob_token), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bob listing alice's statuses: {body:?}"
    );
    let bob_ids: Vec<&str> = body
        .as_array()
        .expect("page body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert!(
        bob_ids.contains(&private_id.as_str()),
        "bob, now a real follower, must see the private post in alice's accounts-statuses list: \
         {bob_ids:?}"
    );

    let (status, body) = send(
        &router,
        req("GET", &accounts_statuses_path, Some(&carol_token), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "carol listing alice's statuses: {body:?}"
    );
    let carol_ids: Vec<&str> = body
        .as_array()
        .expect("page body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert!(
        !carol_ids.contains(&private_id.as_str()),
        "carol, still not a follower, must not see the private post in alice's \
         accounts-statuses list: {carol_ids:?}"
    );

    // Bob unfollows: the very next request observes live state again (no
    // caching), matching RelationshipQueryImpl's own "fresh query every
    // call" contract.
    let (status, body) = send(
        &router,
        req(
            "POST",
            &format!("/api/v1/accounts/{}/unfollow", alice.id.as_i64()),
            Some(&bob_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bob unfollowing alice: {body:?}");

    let (status, body) = send(
        &router,
        req("GET", &accounts_statuses_path, Some(&bob_token), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bob listing alice's statuses: {body:?}"
    );
    let bob_ids_after_unfollow: Vec<&str> = body
        .as_array()
        .expect("page body must be a JSON array")
        .iter()
        .map(|item| item["id"].as_str().expect("id must be a string"))
        .collect();
    assert!(
        !bob_ids_after_unfollow.contains(&private_id.as_str()),
        "bob, no longer a follower, must lose visibility in alice's accounts-statuses list \
         immediately: {bob_ids_after_unfollow:?}"
    );
}
