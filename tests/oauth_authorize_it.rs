//! Integration tests for `AuthorizeEndpoint` (Requirements 2.1, 2.2, 2.3,
//! 2.4; task 5.2, this task's own chosen file name — design.md's File
//! Structure Plan's `tests/oauth_flow_it.rs` covers the *full* flow across
//! tasks 5.2+5.3 (authorize -> token exchange); this file is scoped to
//! task 5.2 alone: authorize/consent/CSRF, with no token-exchange coverage
//! (task 5.3 not yet built)).
//!
//! `AuthorizeEndpoint` is not mounted on any router yet (task 7.1, out of
//! this task's boundary — see `src/oauth/authorize_endpoint.rs`'s own doc
//! comment). These tests therefore call
//! `kawasemi::oauth::authorize_endpoint::authorize_get`/`authorize_post`
//! directly as ordinary async functions, constructing the axum extractor
//! values (`State`, `Query`, `Form`, `HeaderMap`) by hand, against a real
//! Postgres instance via `spawn_test_app` — mirroring
//! `tests/oauth_apps_it.rs`'s own established convention for a not-yet-wired
//! OAuth component.

use std::sync::Arc;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};

use kawasemi::actor::owner::create_owner;
use kawasemi::actor::{ActorType, Handle, NewActor};
use kawasemi::config::Secret;
use kawasemi::oauth::authorize_endpoint::{
    self, AuthorizeEndpointState, AuthorizeQuery, AuthorizeSubmission,
};
use kawasemi::oauth::hash::TokenHashKey;
use kawasemi::oauth::owner_gate::OwnerCredential;
use kawasemi::oauth::service::{NewApp, OauthService};
use kawasemi::test_harness::{TestApp, spawn_test_app};

const OWNER_PASSWORD: &str = "the-real-owner-passphrase";
const REDIRECT_URI: &str = "https://client.example/callback";

fn test_token_hash_key() -> TokenHashKey {
    Secret::new([0x55; 32])
}

fn owner_credential() -> OwnerCredential {
    OwnerCredential {
        password: Secret::new(OWNER_PASSWORD.to_string()),
    }
}

async fn build_state(app: &TestApp) -> AuthorizeEndpointState {
    let key = test_token_hash_key();
    let service = Arc::new(OauthService::new(
        app.pool.clone(),
        app.runtime.clone(),
        key.clone(),
    ));
    AuthorizeEndpointState {
        service,
        pool: app.pool.clone(),
        owner_credential: owner_credential(),
        directory: app.actor.directory().clone(),
        token_hash_key: key,
        runtime: app.runtime.clone(),
        cookie_secure: false,
    }
}

/// Registers a test OAuth app, returning its `client_id`.
async fn register_test_app(state: &AuthorizeEndpointState) -> String {
    let registered = state
        .service
        .register_app(NewApp {
            name: "Test Client".to_string(),
            redirect_uris: vec![REDIRECT_URI.to_string()],
            scopes: "read write".to_string(),
        })
        .await
        .expect("registering the test OAuth app must succeed");
    registered.client_id
}

/// Creates the sole owner fixture and one actor belonging to it, returning
/// `(owner_id, actor_id)`.
async fn create_owner_with_actor(
    app: &TestApp,
    handle: &str,
) -> (kawasemi::domain::Id, kawasemi::domain::Id) {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner fixture must succeed");

    let actor = app
        .actor
        .actor_service()
        .create_actor(NewActor {
            owner_id,
            handle: Handle::new(handle).expect("valid handle"),
            actor_type: ActorType::Person,
            display_name: "Test Actor".to_string(),
            summary: "an authorize-flow integration test fixture".to_string(),
        })
        .await
        .expect("creating the owner's actor fixture must succeed");

    (owner_id, actor.id)
}

fn authorize_query(client_id: &str) -> AuthorizeQuery {
    AuthorizeQuery {
        client_id: client_id.to_string(),
        redirect_uri: REDIRECT_URI.to_string(),
        scope: "read write".to_string(),
        response_type: "code".to_string(),
    }
}

fn cookie_header(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("kawasemi_owner_session={value}").parse().unwrap(),
    );
    headers
}

fn set_cookie_value(response: &axum::response::Response) -> String {
    let raw = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("response must carry a Set-Cookie header")
        .to_str()
        .expect("Set-Cookie header must be valid UTF-8");
    // "kawasemi_owner_session=<value>; HttpOnly; ..." -> "<value>"
    let after_name = raw
        .strip_prefix("kawasemi_owner_session=")
        .expect("Set-Cookie header must start with the owner session cookie name");
    after_name
        .split(';')
        .next()
        .expect("Set-Cookie header must have at least one segment")
        .to_string()
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collecting the response body must succeed");
    String::from_utf8(bytes.to_vec()).expect("response body must be valid UTF-8")
}

async fn authorization_code_row_count(app: &TestApp) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM oauth_authorization_codes")
        .fetch_one(&app.pool)
        .await
        .expect("counting authorization codes must succeed")
}

// ---- (a) GET with no owner session renders a login form ----

#[tokio::test]
async fn get_with_no_owner_session_renders_a_login_form() {
    let app = spawn_test_app().await;
    let state = build_state(&app).await;
    let client_id = register_test_app(&state).await;

    let response = authorize_endpoint::authorize_get(
        State(state),
        Query(authorize_query(&client_id)),
        HeaderMap::new(),
    )
    .await
    .expect("a valid, unauthenticated GET must render the login form, not error");

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#"name="password""#));
    assert!(body.contains(&format!(r#"name="client_id" value="{client_id}""#)));

    app.cleanup().await;
}

// ---- (b) after successful owner login, renders consent with actor
// candidates and an embedded CSRF token ----

#[tokio::test]
async fn post_login_with_correct_password_renders_consent_with_actors_and_csrf_token() {
    let app = spawn_test_app().await;
    let state = build_state(&app).await;
    let client_id = register_test_app(&state).await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "alice").await;

    let response = authorize_endpoint::authorize_post(
        State(state),
        HeaderMap::new(),
        Form(AuthorizeSubmission {
            client_id: client_id.clone(),
            redirect_uri: REDIRECT_URI.to_string(),
            scope: "read write".to_string(),
            response_type: "code".to_string(),
            password: OWNER_PASSWORD.to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect("a correct-password login submission must succeed");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get(header::SET_COOKIE).is_some(),
        "a successful login must set the owner session cookie"
    );
    let body = body_text(response).await;
    assert!(body.contains(&format!(r#"value="{}""#, actor_id.as_i64())));
    assert!(body.contains("alice"));
    assert!(body.contains(r#"name="csrf_token" value=""#));
    assert!(!body.contains(r#"name="csrf_token" value="""#));

    app.cleanup().await;
}

#[tokio::test]
async fn post_login_with_wrong_password_is_rejected_with_401() {
    let app = spawn_test_app().await;
    let state = build_state(&app).await;
    let client_id = register_test_app(&state).await;
    create_owner_with_actor(&app, "bob").await;

    let err = authorize_endpoint::authorize_post(
        State(state),
        HeaderMap::new(),
        Form(AuthorizeSubmission {
            client_id: client_id.clone(),
            redirect_uri: REDIRECT_URI.to_string(),
            scope: "read write".to_string(),
            response_type: "code".to_string(),
            password: "definitely-the-wrong-password".to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a wrong password must be rejected");
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);

    app.cleanup().await;
}

// ---- (c) GET with invalid client_id/redirect_uri is rejected before any
// owner-auth/consent step ----

#[tokio::test]
async fn get_with_unknown_client_id_is_rejected_with_400_before_rendering_anything() {
    let app = spawn_test_app().await;
    let state = build_state(&app).await;

    let err = authorize_endpoint::authorize_get(
        State(state),
        Query(authorize_query("no-such-client-was-ever-registered")),
        HeaderMap::new(),
    )
    .await
    .expect_err("an unknown client_id must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

#[tokio::test]
async fn get_with_mismatched_redirect_uri_is_rejected_with_400() {
    let app = spawn_test_app().await;
    let state = build_state(&app).await;
    let client_id = register_test_app(&state).await;

    let mut query = authorize_query(&client_id);
    query.redirect_uri = "https://not-the-registered-uri.example/callback".to_string();

    let err = authorize_endpoint::authorize_get(State(state), Query(query), HeaderMap::new())
        .await
        .expect_err("a redirect_uri not matching the registered one must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    app.cleanup().await;
}

// ---- full round trip helper: log in, then consent-approve ----

/// Drives the login leg of a login -> consent round trip, returning the
/// session cookie value and the CSRF token embedded in the resulting
/// consent screen, so callers can build their own final consent-decision
/// `POST` (approve/deny/tamper with CSRF) from these two values.
struct LoggedInConsent {
    cookie_value: String,
    csrf_token: String,
}

async fn log_in_and_reach_consent(
    state: &AuthorizeEndpointState,
    client_id: &str,
) -> LoggedInConsent {
    let response = authorize_endpoint::authorize_post(
        State(state.clone()),
        HeaderMap::new(),
        Form(AuthorizeSubmission {
            client_id: client_id.to_string(),
            redirect_uri: REDIRECT_URI.to_string(),
            scope: "read write".to_string(),
            response_type: "code".to_string(),
            password: OWNER_PASSWORD.to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect("login submission must succeed");

    let cookie_value = set_cookie_value(&response);
    let body = body_text(response).await;
    let csrf_token = extract_csrf_token(&body);

    LoggedInConsent {
        cookie_value,
        csrf_token,
    }
}

fn extract_csrf_token(html: &str) -> String {
    let marker = r#"name="csrf_token" value=""#;
    let start = html
        .find(marker)
        .expect("consent HTML must embed a csrf_token field")
        + marker.len();
    let rest = &html[start..];
    let end = rest.find('"').expect("csrf_token value must be quoted");
    rest[..end].to_string()
}

// ---- (d) POST with matching CSRF, valid selected actor, approval issues a
// code bound to that actor and redirects with the code ----

#[tokio::test]
async fn post_approve_with_valid_csrf_and_owned_actor_issues_code_and_redirects() {
    let app = spawn_test_app().await;
    let state = build_state(&app).await;
    let client_id = register_test_app(&state).await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "carol").await;

    let logged_in = log_in_and_reach_consent(&state, &client_id).await;

    let before = authorization_code_row_count(&app).await;

    let response = authorize_endpoint::authorize_post(
        State(state),
        cookie_header(&logged_in.cookie_value),
        Form(AuthorizeSubmission {
            client_id: client_id.clone(),
            redirect_uri: REDIRECT_URI.to_string(),
            scope: "read write".to_string(),
            response_type: "code".to_string(),
            csrf_token: logged_in.csrf_token.clone(),
            selected_actor: actor_id.as_i64().to_string(),
            approved_scopes: "read write".to_string(),
            decision: "approve".to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect("an approved, CSRF-valid, owner-owned-actor consent must succeed");

    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("an approval must redirect")
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with(REDIRECT_URI));
    assert!(location.contains("code="));
    assert!(!location.contains("error="));

    let after = authorization_code_row_count(&app).await;
    assert_eq!(
        after,
        before + 1,
        "exactly one authorization code must be issued"
    );

    app.cleanup().await;
}

/// This is a single-owner-per-instance server
/// (`ActorDirectory::sole_owner`'s own documented invariant: exactly one
/// `owners` row must exist), so a genuine "a different owner's actor"
/// scenario cannot be constructed without itself violating that invariant
/// (and correctly failing login with a 5xx, per
/// `owner_gate::authenticate_owner`'s own tested behavior). The
/// security-relevant scenario this test proves instead: a client-supplied
/// `selected_actor` that does not correspond to *any* real actor at all
/// (fabricated/guessed) must be rejected exactly the same way — the
/// endpoint never trusts a client-supplied actor id merely because it
/// parses as an integer.
#[tokio::test]
async fn post_approve_with_a_nonexistent_actor_id_is_rejected_and_issues_no_code() {
    let app = spawn_test_app().await;
    let state = build_state(&app).await;
    let client_id = register_test_app(&state).await;
    create_owner_with_actor(&app, "dave").await;

    let logged_in = log_in_and_reach_consent(&state, &client_id).await;

    let before = authorization_code_row_count(&app).await;
    let never_created_actor_id = kawasemi::domain::Id::from_i64(999_999_999);

    let err = authorize_endpoint::authorize_post(
        State(state),
        cookie_header(&logged_in.cookie_value),
        Form(AuthorizeSubmission {
            client_id: client_id.clone(),
            redirect_uri: REDIRECT_URI.to_string(),
            scope: "read write".to_string(),
            response_type: "code".to_string(),
            csrf_token: logged_in.csrf_token.clone(),
            selected_actor: never_created_actor_id.as_i64().to_string(),
            approved_scopes: "read write".to_string(),
            decision: "approve".to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a fabricated selected_actor id must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    let after = authorization_code_row_count(&app).await;
    assert_eq!(after, before, "no code may be issued for an unowned actor");

    app.cleanup().await;
}

// ---- (e) POST with CSRF token mismatch -> 403, no code issued ----

#[tokio::test]
async fn post_approve_with_mismatched_csrf_token_is_rejected_with_403_and_issues_no_code() {
    let app = spawn_test_app().await;
    let state = build_state(&app).await;
    let client_id = register_test_app(&state).await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "erin").await;

    let logged_in = log_in_and_reach_consent(&state, &client_id).await;

    let before = authorization_code_row_count(&app).await;

    let err = authorize_endpoint::authorize_post(
        State(state),
        cookie_header(&logged_in.cookie_value),
        Form(AuthorizeSubmission {
            client_id: client_id.clone(),
            redirect_uri: REDIRECT_URI.to_string(),
            scope: "read write".to_string(),
            response_type: "code".to_string(),
            csrf_token: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            selected_actor: actor_id.as_i64().to_string(),
            approved_scopes: "read write".to_string(),
            decision: "approve".to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a mismatched csrf_token must be rejected");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let after = authorization_code_row_count(&app).await;
    assert_eq!(
        after, before,
        "no authorization code may be issued on a CSRF mismatch"
    );

    app.cleanup().await;
}

// ---- (f) POST with decision=deny -> OAuth-compliant access-denied
// redirect, no code issued ----

#[tokio::test]
async fn post_deny_redirects_with_access_denied_and_issues_no_code() {
    let app = spawn_test_app().await;
    let state = build_state(&app).await;
    let client_id = register_test_app(&state).await;
    let (_owner_id, actor_id) = create_owner_with_actor(&app, "frank").await;

    let logged_in = log_in_and_reach_consent(&state, &client_id).await;

    let before = authorization_code_row_count(&app).await;

    let response = authorize_endpoint::authorize_post(
        State(state),
        cookie_header(&logged_in.cookie_value),
        Form(AuthorizeSubmission {
            client_id: client_id.clone(),
            redirect_uri: REDIRECT_URI.to_string(),
            scope: "read write".to_string(),
            response_type: "code".to_string(),
            csrf_token: logged_in.csrf_token.clone(),
            selected_actor: actor_id.as_i64().to_string(),
            approved_scopes: "read write".to_string(),
            decision: "deny".to_string(),
            ..Default::default()
        }),
    )
    .await
    .expect("a denial must still succeed as a redirect, not an error");

    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("a denial must redirect")
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with(REDIRECT_URI));
    assert!(location.contains("error=access_denied"));
    assert!(!location.contains("code="));

    let after = authorization_code_row_count(&app).await;
    assert_eq!(
        after, before,
        "no authorization code may be issued on denial"
    );

    app.cleanup().await;
}
