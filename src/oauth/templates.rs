//! Minimal HTML rendering for the OAuth owner-login screen and the
//! actor-selection consent screen (`templates` — design.md's File Structure
//! Plan: "承認画面・オーナーログインの最小 HTML レンダリング"; Requirements
//! 2.2, 2.3; task 5.2). Consumed exclusively by
//! `crate::oauth::authorize_endpoint`, which owns all HTTP-level
//! orchestration (query/form parsing, cookie handling, redirects); this
//! module owns only turning already-validated domain values into an HTML
//! string.
//!
//! Scope: two pure string-building functions —
//! [`render_login_form`]/[`render_consent_form`] — plus the private
//! [`html_escape`] helper both rely on. No I/O, no `AppError`, nothing
//! `async`: every input is a value the caller has already validated
//! (`AuthorizeContext`'s fields have already passed
//! `authorize_endpoint`'s `validate_authorize_context`, `actors` already
//! come from `ActorDirectory::list_actors_for_owner`, `csrf_token` is
//! already-derived hex).
//!
//! ## Why every embedded value is still HTML-escaped
//! `client_id`/`redirect_uri` are echoed from the authorization request;
//! `redirect_uri` in particular is only required to pass
//! `OauthService::register_app`'s minimal "has a `scheme://`, both halves
//! non-empty" format bar (see `service.rs`'s own doc comment, "Redirect URI
//! format validation bar") — nothing stops a registered app's redirect URI
//! from containing `"`, `<`, or `&`. Actor `display_name` is free-form
//! owner-authored text with no charset restriction (unlike `Handle`, which
//! *is* charset-restricted — see `crate::actor::model::Handle::new` — but is
//! escaped here anyway for uniformity rather than special-casing "this one
//! field is provably safe"). Every value this module places inside an HTML
//! attribute or text node therefore goes through [`html_escape`] first, with
//! no exceptions carved out for values that merely look safe today.
//!
//! ## Both forms POST back to `/oauth/authorize` itself
//! There is no separate login-submission route in design.md's File
//! Structure Plan (only `GET`/`POST /oauth/authorize`) — see
//! `authorize_endpoint`'s own module doc comment ("Design judgment calls")
//! for the full reasoning. Both rendered forms' `action` attribute is
//! therefore the literal string `/oauth/authorize`, and both carry the same
//! four hidden fields (`client_id`/`redirect_uri`/`scope`/`response_type`)
//! so the original authorization request survives the round trip through
//! either form's submission.
//!
//! ## `approved_scopes` defaults to the originally requested `scope`
//! [`render_consent_form`] pre-fills a hidden `approved_scopes` field with
//! the same value as `scope` — this minimal consent screen does not offer
//! scope-narrowing UI (an owner approves the requested scope as a whole or
//! denies it entirely). The field is still named `approved_scopes` on the
//! wire (matching design.md's API Contract table for `POST /oauth/authorize`)
//! so a later task could add real narrowing UI without changing the wire
//! shape, but that UI does not exist in this minimal implementation.

use crate::actor::{ActorState, ActorSummary, ActorType};

/// The four authorization-request fields both rendered forms carry as
/// hidden fields so they survive the login/consent round trip back to
/// `POST /oauth/authorize` (see this module's doc comment).
#[derive(Debug, Clone)]
pub struct AuthorizeContext {
    pub client_id: String,
    pub redirect_uri: String,
    pub scope: String,
    pub response_type: String,
}

/// Renders the minimal owner-login HTML form (design.md's `templates.rs`:
/// "オーナーログインの最小 HTML レンダリング"; Requirement 2.2's implicit
/// login precondition). Submits `password` (the single field
/// `OwnerLogin`/`OwnerConfig` model, see `owner_gate.rs`) plus `ctx`'s four
/// fields back to `POST /oauth/authorize`.
pub fn render_login_form(ctx: &AuthorizeContext) -> String {
    format!(
        r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Owner Login</title></head>
<body>
<h1>Owner Login</h1>
<form method="post" action="/oauth/authorize">
<input type="hidden" name="client_id" value="{client_id}">
<input type="hidden" name="redirect_uri" value="{redirect_uri}">
<input type="hidden" name="scope" value="{scope}">
<input type="hidden" name="response_type" value="{response_type}">
<label>Password: <input type="password" name="password"></label>
<button type="submit">Log in</button>
</form>
</body>
</html>"#,
        client_id = html_escape(&ctx.client_id),
        redirect_uri = html_escape(&ctx.redirect_uri),
        scope = html_escape(&ctx.scope),
        response_type = html_escape(&ctx.response_type),
    )
}

/// Renders the minimal actor-selection consent screen (design.md's
/// `templates.rs`: "承認画面...の最小 HTML レンダリング"; Requirements 2.2,
/// 2.3): lists `actors` (the authenticated owner's candidates, from
/// [`crate::actor::ActorDirectory::list_actors_for_owner`]) as radio buttons
/// (`selected_actor`), embeds `csrf_token` as a hidden field (design.md's
/// Security Considerations: "オーナーセッションに紐づく CSRF トークン...
/// フォームへ埋め込んで描画する"), and offers two submit buttons sharing the
/// `decision` field name (`approve` / `deny`) — standard HTML "two buttons,
/// one form" convention; whichever is clicked sets `decision`'s submitted
/// value.
pub fn render_consent_form(
    ctx: &AuthorizeContext,
    actors: &[ActorSummary],
    csrf_token: &str,
) -> String {
    let mut actor_rows = String::new();
    for actor in actors {
        actor_rows.push_str(&format!(
            r#"<label><input type="radio" name="selected_actor" value="{id}"> {handle} ({actor_type}, {state})</label><br>
"#,
            id = actor.id.as_i64(),
            handle = html_escape(actor.handle.as_str()),
            actor_type = actor_type_label(actor.actor_type),
            state = actor_state_label(actor.state),
        ));
    }

    format!(
        r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Authorize Access</title></head>
<body>
<h1>Authorize Access</h1>
<p>Requested scope: {scope}</p>
<form method="post" action="/oauth/authorize">
<input type="hidden" name="client_id" value="{client_id}">
<input type="hidden" name="redirect_uri" value="{redirect_uri}">
<input type="hidden" name="scope" value="{scope}">
<input type="hidden" name="response_type" value="{response_type}">
<input type="hidden" name="approved_scopes" value="{scope}">
<input type="hidden" name="csrf_token" value="{csrf_token}">
{actor_rows}
<button type="submit" name="decision" value="approve">Approve</button>
<button type="submit" name="decision" value="deny">Deny</button>
</form>
</body>
</html>"#,
        client_id = html_escape(&ctx.client_id),
        redirect_uri = html_escape(&ctx.redirect_uri),
        scope = html_escape(&ctx.scope),
        response_type = html_escape(&ctx.response_type),
        csrf_token = html_escape(csrf_token),
        actor_rows = actor_rows,
    )
}

/// Display label for [`ActorType`] (neither variant implements `Display`
/// today — `crate::actor::model` owns that type and adding one is out of
/// this task's boundary). Matches this crate's existing lowercase wire/log
/// vocabulary convention (mirrors `crate::oauth::scope`'s lowercase scope
/// tokens) rather than inventing a differently-cased label.
fn actor_type_label(actor_type: ActorType) -> &'static str {
    match actor_type {
        ActorType::Person => "person",
        ActorType::Service => "service",
    }
}

/// Display label for [`ActorState`]; see [`actor_type_label`]'s doc comment
/// for the same "no `Display` impl exists yet" rationale.
fn actor_state_label(state: ActorState) -> &'static str {
    match state {
        ActorState::Active => "active",
        ActorState::Deactivated => "deactivated",
    }
}

/// Escapes `input` for safe embedding inside an HTML attribute value or text
/// node: `&` (must be escaped first, or a later replacement's own `&`
/// characters would be re-escaped), `<`, `>`, `"`, `'`. See this module's
/// doc comment ("Why every embedded value is still HTML-escaped") for why
/// this is applied unconditionally, with no "this value is provably safe"
/// exceptions.
fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Id;

    fn sample_ctx() -> AuthorizeContext {
        AuthorizeContext {
            client_id: "client-123".to_string(),
            redirect_uri: "https://client.example/callback".to_string(),
            scope: "read write".to_string(),
            response_type: "code".to_string(),
        }
    }

    fn sample_actor(id: i64, handle: &str) -> ActorSummary {
        ActorSummary {
            id: Id::from_i64(id),
            handle: crate::actor::Handle::new(handle).unwrap(),
            actor_type: ActorType::Person,
            display_name: "Display Name".to_string(),
            state: ActorState::Active,
        }
    }

    #[test]
    fn login_form_embeds_all_four_hidden_context_fields() {
        let html = render_login_form(&sample_ctx());
        assert!(html.contains(r#"name="client_id" value="client-123""#));
        assert!(html.contains(r#"name="redirect_uri" value="https://client.example/callback""#));
        assert!(html.contains(r#"name="scope" value="read write""#));
        assert!(html.contains(r#"name="response_type" value="code""#));
        assert!(html.contains(r#"name="password""#));
        assert!(html.contains(r#"action="/oauth/authorize""#));
    }

    #[test]
    fn login_form_escapes_a_hostile_redirect_uri() {
        let mut ctx = sample_ctx();
        ctx.redirect_uri = r#"https://evil.example/"><script>alert(1)</script>"#.to_string();
        let html = render_login_form(&ctx);
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn consent_form_lists_each_actor_and_embeds_csrf_token() {
        let ctx = sample_ctx();
        let actors = vec![sample_actor(1, "alice"), sample_actor(2, "bob")];
        let html = render_consent_form(&ctx, &actors, "deadbeef");

        assert!(html.contains(r#"value="1""#));
        assert!(html.contains("alice"));
        assert!(html.contains(r#"value="2""#));
        assert!(html.contains("bob"));
        assert!(html.contains(r#"name="csrf_token" value="deadbeef""#));
        assert!(html.contains(r#"name="decision" value="approve""#));
        assert!(html.contains(r#"name="decision" value="deny""#));
    }

    #[test]
    fn consent_form_escapes_a_hostile_display_name_via_handle_or_context() {
        // Handle itself is charset-restricted (cannot carry HTML syntax),
        // but redirect_uri (also rendered on this screen) is not -- reuse
        // the same hostile-value check as the login form to prove the
        // consent screen escapes it too.
        let mut ctx = sample_ctx();
        ctx.redirect_uri = r#"https://evil.example/"><script>alert(1)</script>"#.to_string();
        let html = render_consent_form(&ctx, &[], "token");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn consent_form_with_no_actors_still_renders_a_valid_form() {
        let html = render_consent_form(&sample_ctx(), &[], "token");
        assert!(html.contains(r#"action="/oauth/authorize""#));
        assert!(html.contains(r#"name="decision" value="approve""#));
    }
}
