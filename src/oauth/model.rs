//! OAuth domain types and single-actor request context (`model` component,
//! design.md "OAuth Domain / ドメイン層" -> `model`, Requirements 1.1, 2.3,
//! 3.1, 3.5, 3.6, 5.3; task 2.1).
//!
//! Scope: this module owns exactly the five domain value types design.md's
//! `model` component sketches — no persistence (`*_repository.rs`, tasks
//! 3.1-3.3), no business logic (`OauthService`, task 4.x), no scope
//! inclusion judgment (`Scope`, task 2.2) and no PKCE verification
//! (`Pkce`, task 2.3). Those consume the types defined here but are out of
//! scope for task 2.1 (`Boundary: model`).
//!
//! [`ScopeSet`] and [`PkceChallenge`] are minimal placeholder stand-ins
//! co-located here (rather than in their own `scope.rs`/`pkce.rs` files)
//! purely so the domain structs below have a concrete value type to hold
//! and can be constructed/compared in this module's tests. Neither
//! implements its real behavior — no scope inclusion judgment/hierarchy for
//! `ScopeSet`, no S256 challenge/verifier matching for `PkceChallenge` —
//! that is task 2.2's and task 2.3's job respectively, each of which owns
//! its own `scope.rs`/`pkce.rs` file per design.md's File Structure Plan and
//! is free to choose its own representation without inheriting these
//! placeholders.
//!
//! The central structural invariant this module enforces is
//! [`RequestActorContext`]: it carries exactly one `actor_id: Id` field (not
//! a collection), so a single access token can never be bound to more than
//! one actor at the type level (Requirement 5.3). This is checked
//! structurally in this module's tests via exhaustive field destructuring
//! (no `..`), which fails to compile if `actor_id` were ever widened into a
//! collection or a second actor-identifying field were added.
//!
//! Every secret-bearing field (`OauthApp::client_secret`,
//! `AuthorizationCode::code`) is wrapped in [`Secret`] so that formatting a
//! value containing one (`{:?}`) never exposes the plaintext (Requirement
//! 3.6) — see `crate::config::secret` for why wrapping is sufficient
//! (`Secret`'s `Debug`/`Display` impls do not require `T: Debug`, so a
//! `#[derive(Debug)]` on a containing struct calls `Secret`'s own masking
//! impl for that field rather than reaching into `T`).
//! [`AccessToken`] instead holds `token_hash: Vec<u8>` — an already-hashed
//! value, never the raw bearer token — per design.md's exact type sketch;
//! computing that hash is a later repository task's job (3.3), not this
//! module's.

use std::collections::BTreeSet;

use time::OffsetDateTime;

use crate::config::Secret;
use crate::domain::Id;

/// Minimal placeholder for the real Mastodon-compatible scope hierarchy and
/// inclusion judgment (design.md's `scope.rs`; Requirements 4.1-4.5 own the
/// real type — task 2.2, not this one).
///
/// Task 2.1 (`model` component) only needs a concrete value type it can put
/// behind [`OauthApp::scopes`] / [`AuthorizationCode::scopes`] /
/// [`AccessToken::scopes`] / [`RequestActorContext::scopes`] so those domain
/// types actually compile and can be constructed and compared in tests. It
/// deliberately does **not** implement scope inclusion (`is_satisfied_by`),
/// the upper/child scope hierarchy, or the fixed Mastodon scope vocabulary
/// (`read`/`write`/`follow`/`push` and their children) — task 2.2 owns all
/// of that in its own `src/oauth/scope.rs`.
///
/// A `BTreeSet` is used only so `Debug`/equality output is stable/sorted for
/// tests — this is not a claim about scope inclusion semantics, which this
/// task does not implement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeSet(BTreeSet<String>);

impl ScopeSet {
    /// Builds a `ScopeSet` from any iterable of scope-name-like values.
    pub fn new(scopes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self(scopes.into_iter().map(Into::into).collect())
    }

    /// Iterates the contained scope names as `&str`.
    pub fn as_strs(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

/// A registered OAuth client application (Requirement 1.1).
///
/// `client_secret` is wrapped in [`Secret`] so it never appears in `Debug`
/// output (Requirement 3.6). `redirect_uris` is authoritative for the
/// authorization-flow redirect-URI exact-match check (Requirement 1.4,
/// enforced by a later service task, not here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthApp {
    pub id: Id,
    pub client_id: String,
    pub client_secret: Secret<String>,
    pub redirect_uris: Vec<String>,
    pub scopes: ScopeSet,
    pub name: String,
    pub created_at: OffsetDateTime,
}

/// Minimal placeholder for the real PKCE (S256) challenge/verifier type
/// (design.md's `pkce.rs`; Requirements 2.6, 3.3 own the real
/// generation/verification logic — task 2.3, not this one).
///
/// Task 2.1 (`model` component) only needs a concrete value type it can put
/// behind [`AuthorizationCode::pkce`] so that type actually compiles and
/// can be constructed/compared in tests. It deliberately does **not**
/// implement S256 hashing or challenge/verifier matching — task 2.3 owns
/// that in its own `src/oauth/pkce.rs`.
///
/// Carries no verification method yet: matching a `code_verifier` presented
/// at the token endpoint against this challenge is task 2.3's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceChallenge {
    challenge: String,
}

impl PkceChallenge {
    /// Wraps `challenge` (the client-supplied `code_challenge` value) as a
    /// `PkceChallenge`.
    pub fn new(challenge: impl Into<String>) -> Self {
        Self {
            challenge: challenge.into(),
        }
    }

    /// Returns the raw challenge string.
    pub fn as_str(&self) -> &str {
        &self.challenge
    }
}

/// A short-lived authorization code bound to a single actor and its
/// approved scopes (Requirements 2.3, 2.5, 2.6, 3.1, 3.5).
///
/// `code` is wrapped in [`Secret`] so it never appears in `Debug` output
/// (Requirement 3.6). `actor_id` is the actor the owner selected during
/// consent (Requirement 2.3) — unlike [`RequestActorContext`], nothing here
/// bars a *repository* from tracking many outstanding codes; the single-
/// actor invariant is about one code/token binding to exactly one actor,
/// which this flat (non-collection) `actor_id: Id` field already
/// establishes. `consumed` records single-use exhaustion (Requirement 2.5,
/// enforced by a later repository task, not this value type). `pkce` is
/// `None` when the authorization request carried no PKCE challenge
/// (Requirement 2.6 is `Where`-conditional).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationCode {
    pub code: Secret<String>,
    pub app_id: Id,
    pub actor_id: Id,
    pub scopes: ScopeSet,
    pub redirect_uri: String,
    pub pkce: Option<PkceChallenge>,
    pub expires_at: OffsetDateTime,
    pub consumed: bool,
}

/// An issued access token bound to a single actor and its approved scopes
/// (Requirements 3.1, 3.5).
///
/// Holds `token_hash: Vec<u8>` rather than the raw bearer token value
/// itself: the raw token is only ever handed to the client once, at issuance
/// time, and never persisted or reconstructed from this type (Requirement
/// 3.6) — hashing it is a later repository task's concern (3.3), not this
/// module's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessToken {
    pub id: Id,
    pub token_hash: Vec<u8>,
    pub app_id: Id,
    pub actor_id: Id,
    pub scopes: ScopeSet,
    pub created_at: OffsetDateTime,
    pub revoked: bool,
}

/// The single-actor request context a resolved bearer token supplies to
/// downstream handlers (Requirements 3.5, 5.3).
///
/// Structurally carries exactly one `actor_id: Id` (never a collection),
/// making "one access token, one actor, no simultaneous multi-actor
/// operation" a type-level fact rather than a convention callers must
/// remember to uphold — see this module's tests for a compile-time check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestActorContext {
    pub actor_id: Id,
    pub scopes: ScopeSet,
}

/// A short-lived owner authentication session, gating the consent screen
/// (design.md's `OwnerGate`, referenced by Requirement 2.2's "オーナー認証
/// 後にのみ提示").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerSession {
    pub owner_id: Id,
    pub expires_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Secret;
    use crate::domain::Id;
    use time::OffsetDateTime;

    const PLAINTEXT_SECRET: &str = "s3cr3t-client-secret-plaintext";
    const PLAINTEXT_CODE: &str = "s3cr3t-authorization-code-plaintext";

    fn sample_time() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }

    #[test]
    fn oauth_app_can_be_constructed_with_all_fields() {
        let app = OauthApp {
            id: Id::from_i64(1),
            client_id: "client-123".to_string(),
            client_secret: Secret::new(PLAINTEXT_SECRET.to_string()),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ScopeSet::new(["read", "write"]),
            name: "Test Client".to_string(),
            created_at: sample_time(),
        };
        assert_eq!(app.client_id, "client-123");
        assert_eq!(app.redirect_uris.len(), 1);
    }

    #[test]
    fn oauth_app_debug_does_not_expose_client_secret_plaintext() {
        let app = OauthApp {
            id: Id::from_i64(1),
            client_id: "client-123".to_string(),
            client_secret: Secret::new(PLAINTEXT_SECRET.to_string()),
            redirect_uris: vec!["https://client.example/callback".to_string()],
            scopes: ScopeSet::new(["read"]),
            name: "Test Client".to_string(),
            created_at: sample_time(),
        };
        let formatted = format!("{app:?}");
        assert!(
            !formatted.contains(PLAINTEXT_SECRET),
            "OauthApp Debug output leaked client_secret plaintext: {formatted}"
        );
    }

    #[test]
    fn authorization_code_can_be_constructed_with_all_fields() {
        let code = AuthorizationCode {
            code: Secret::new(PLAINTEXT_CODE.to_string()),
            app_id: Id::from_i64(1),
            actor_id: Id::from_i64(2),
            scopes: ScopeSet::new(["read"]),
            redirect_uri: "https://client.example/callback".to_string(),
            pkce: Some(PkceChallenge::new("challenge-value")),
            expires_at: sample_time(),
            consumed: false,
        };
        assert_eq!(code.app_id, Id::from_i64(1));
        assert_eq!(code.actor_id, Id::from_i64(2));
        assert!(!code.consumed);
        assert!(code.pkce.is_some());
    }

    #[test]
    fn authorization_code_debug_does_not_expose_code_plaintext() {
        let code = AuthorizationCode {
            code: Secret::new(PLAINTEXT_CODE.to_string()),
            app_id: Id::from_i64(1),
            actor_id: Id::from_i64(2),
            scopes: ScopeSet::new(["read"]),
            redirect_uri: "https://client.example/callback".to_string(),
            pkce: None,
            expires_at: sample_time(),
            consumed: false,
        };
        let formatted = format!("{code:?}");
        assert!(
            !formatted.contains(PLAINTEXT_CODE),
            "AuthorizationCode Debug output leaked code plaintext: {formatted}"
        );
    }

    #[test]
    fn access_token_can_be_constructed_with_all_fields() {
        let token = AccessToken {
            id: Id::from_i64(1),
            token_hash: vec![0xAB, 0xCD, 0xEF],
            app_id: Id::from_i64(2),
            actor_id: Id::from_i64(3),
            scopes: ScopeSet::new(["read", "write"]),
            created_at: sample_time(),
            revoked: false,
        };
        assert_eq!(token.actor_id, Id::from_i64(3));
        assert!(!token.revoked);
    }

    #[test]
    fn owner_session_can_be_constructed_with_all_fields() {
        let session = OwnerSession {
            owner_id: Id::from_i64(1),
            expires_at: sample_time(),
        };
        assert_eq!(session.owner_id, Id::from_i64(1));
    }

    /// Requirement 5.3: `RequestActorContext` must be structurally
    /// incapable of representing more than one actor at a time. Exhaustive
    /// destructuring (no `..`) is a compile-time proof of the type's exact
    /// field set: it has exactly one `actor_id: Id` field (not a `Vec<Id>`
    /// or any other collection), plus `scopes`. If a field were ever added
    /// or `actor_id` ever became a collection, this destructuring would fail
    /// to compile, forcing this invariant to be revisited.
    #[test]
    fn request_actor_context_structurally_holds_exactly_one_actor_id() {
        let ctx = RequestActorContext {
            actor_id: Id::from_i64(42),
            scopes: ScopeSet::new(["read"]),
        };
        let RequestActorContext { actor_id, scopes } = ctx;
        let _: Id = actor_id;
        let _: ScopeSet = scopes;
        assert_eq!(actor_id, Id::from_i64(42));
    }

    #[test]
    fn scope_set_new_deduplicates_and_holds_the_given_scope_names() {
        let scopes = ScopeSet::new(["write", "read", "write"]);
        let names: Vec<&str> = scopes.as_strs().collect();
        assert_eq!(names, vec!["read", "write"]);
    }

    #[test]
    fn scope_set_default_is_empty() {
        let scopes = ScopeSet::default();
        assert_eq!(scopes.as_strs().count(), 0);
    }

    #[test]
    fn pkce_challenge_new_holds_the_given_challenge_string() {
        let pkce = PkceChallenge::new("challenge-value");
        assert_eq!(pkce.as_str(), "challenge-value");
    }
}
