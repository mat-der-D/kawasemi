//! Mastodon-compatible OAuth scope vocabulary and the single shared scope
//! inclusion judgment (`Scope` component, design.md "OAuth Domain /
//! ドメイン層" -> `Scope`, Requirements 4.1, 4.2, 4.3, 4.4, 4.5, 1.3; task
//! 2.2).
//!
//! Scope: this module owns exactly the scope vocabulary (top-level
//! `read`/`write`/`follow`/`push` scopes and their granular children) and
//! the inclusion judgment that decides whether a set of granted scopes
//! satisfies a set of required scopes (Requirement 4.1, 4.4). It does not
//! own app registration, authorization-code issuance, token exchange, or
//! Bearer-token resolution (`OauthService` / `BearerAuthMiddleware`, later
//! tasks) — those consume [`ScopeSet::parse`] and
//! [`ScopeSet::is_satisfied_by`] as the single shared implementation
//! (Requirement 4.5) rather than duplicating scope logic of their own.
//!
//! This type supersedes task 2.1's `crate::oauth::model::ScopeSet`
//! placeholder (a bare `BTreeSet<String>` with no inclusion judgment, see
//! that module's doc comment). Per task 2.1/2.2's documented boundary,
//! wiring `crate::oauth::model`'s domain structs (`OauthApp`,
//! `AuthorizationCode`, `AccessToken`, `RequestActorContext`) to use *this*
//! `ScopeSet` instead of the placeholder is explicitly deferred to a later
//! task — this module does not modify `model.rs`.
//!
//! ## Scope vocabulary and extension point
//!
//! [`Scope::Read`] / [`Scope::Write`] / [`Scope::Follow`] / [`Scope::Push`]
//! are the Mastodon-compatible top-level scopes (Requirement 4.1). `read`
//! and `write` each subsume a granular child vocabulary
//! ([`ReadScope`] / [`WriteScope`]) modeled after Mastodon's real
//! `read:*` / `write:*` OAuth scope tokens (e.g. `read:accounts`,
//! `read:statuses`, `write:statuses`, `write:media`). The lists below are a
//! representative, non-exhaustive subset of Mastodon's vocabulary — the
//! acceptance criteria for this task require only that a top-level scope
//! subsumes its own granular children, not that every Mastodon scope that
//! will ever be needed is enumerated up front. `follow` and `push` are
//! taken as flat top-level scopes with no granular children, matching how
//! Mastodon exposes them today. To extend the vocabulary later: add a new
//! variant to [`ReadScope`]/[`WriteScope`] (or a new granular enum alongside
//! them for a future top-level scope), then extend [`ReadScope::parse`] /
//! [`WriteScope::parse`] (or the analogous parser) and their `as_str`
//! counterparts — [`ScopeSet::parse`] and [`ScopeSet::is_satisfied_by`] need
//! no changes since both operate generically over [`Scope`].

use axum::http::StatusCode;
use std::collections::BTreeSet;
use std::fmt;

use crate::error::AppError;

/// Granular `read:*` scopes (subset of Mastodon's real vocabulary; see this
/// module's doc comment for the extension point).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReadScope {
    Accounts,
    Blocks,
    Bookmarks,
    Favourites,
    Filters,
    Follows,
    Lists,
    Mutes,
    Notifications,
    Search,
    Statuses,
}

impl ReadScope {
    const ALL: &'static [ReadScope] = &[
        ReadScope::Accounts,
        ReadScope::Blocks,
        ReadScope::Bookmarks,
        ReadScope::Favourites,
        ReadScope::Filters,
        ReadScope::Follows,
        ReadScope::Lists,
        ReadScope::Mutes,
        ReadScope::Notifications,
        ReadScope::Search,
        ReadScope::Statuses,
    ];

    fn as_str(self) -> &'static str {
        match self {
            ReadScope::Accounts => "accounts",
            ReadScope::Blocks => "blocks",
            ReadScope::Bookmarks => "bookmarks",
            ReadScope::Favourites => "favourites",
            ReadScope::Filters => "filters",
            ReadScope::Follows => "follows",
            ReadScope::Lists => "lists",
            ReadScope::Mutes => "mutes",
            ReadScope::Notifications => "notifications",
            ReadScope::Search => "search",
            ReadScope::Statuses => "statuses",
        }
    }

    fn parse(suffix: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == suffix)
    }
}

/// Granular `write:*` scopes (subset of Mastodon's real vocabulary; see this
/// module's doc comment for the extension point).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WriteScope {
    Accounts,
    Blocks,
    Bookmarks,
    Conversations,
    Favourites,
    Filters,
    Follows,
    Lists,
    Media,
    Mutes,
    Notifications,
    Reports,
    Statuses,
}

impl WriteScope {
    const ALL: &'static [WriteScope] = &[
        WriteScope::Accounts,
        WriteScope::Blocks,
        WriteScope::Bookmarks,
        WriteScope::Conversations,
        WriteScope::Favourites,
        WriteScope::Filters,
        WriteScope::Follows,
        WriteScope::Lists,
        WriteScope::Media,
        WriteScope::Mutes,
        WriteScope::Notifications,
        WriteScope::Reports,
        WriteScope::Statuses,
    ];

    fn as_str(self) -> &'static str {
        match self {
            WriteScope::Accounts => "accounts",
            WriteScope::Blocks => "blocks",
            WriteScope::Bookmarks => "bookmarks",
            WriteScope::Conversations => "conversations",
            WriteScope::Favourites => "favourites",
            WriteScope::Filters => "filters",
            WriteScope::Follows => "follows",
            WriteScope::Lists => "lists",
            WriteScope::Media => "media",
            WriteScope::Mutes => "mutes",
            WriteScope::Notifications => "notifications",
            WriteScope::Reports => "reports",
            WriteScope::Statuses => "statuses",
        }
    }

    fn parse(suffix: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == suffix)
    }
}

/// A single Mastodon-compatible OAuth scope token: either a top-level scope
/// (`read`, `write`, `follow`, `push`) or a granular child of `read`/`write`
/// (Requirement 4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Read,
    ReadGranular(ReadScope),
    Write,
    WriteGranular(WriteScope),
    Follow,
    Push,
}

impl Scope {
    /// Parses a single scope token (no whitespace) such as `"read"` or
    /// `"write:statuses"`. Returns `None` for any token outside the known
    /// vocabulary — the caller ([`ScopeSet::parse`]) turns that into a
    /// rejecting [`AppError`] (Requirement 1.3, 4.1).
    fn parse_token(token: &str) -> Option<Self> {
        match token {
            "read" => Some(Scope::Read),
            "write" => Some(Scope::Write),
            "follow" => Some(Scope::Follow),
            "push" => Some(Scope::Push),
            other => {
                if let Some(suffix) = other.strip_prefix("read:") {
                    ReadScope::parse(suffix).map(Scope::ReadGranular)
                } else if let Some(suffix) = other.strip_prefix("write:") {
                    WriteScope::parse(suffix).map(Scope::WriteGranular)
                } else {
                    None
                }
            }
        }
    }

    /// The top-level scope that subsumes this scope, if any (Requirement
    /// 4.4): a granular scope's parent is its top-level counterpart; a
    /// top-level scope has none (it is already maximal).
    fn subsuming_top_level(self) -> Option<Scope> {
        match self {
            Scope::ReadGranular(_) => Some(Scope::Read),
            Scope::WriteGranular(_) => Some(Scope::Write),
            Scope::Read | Scope::Write | Scope::Follow | Scope::Push => None,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Read => write!(f, "read"),
            Scope::ReadGranular(sub) => write!(f, "read:{}", sub.as_str()),
            Scope::Write => write!(f, "write"),
            Scope::WriteGranular(sub) => write!(f, "write:{}", sub.as_str()),
            Scope::Follow => write!(f, "follow"),
            Scope::Push => write!(f, "push"),
        }
    }
}

/// A parsed, deduplicated set of [`Scope`]s — either the scopes a client
/// requests/an app registers, or the scopes a token has been granted
/// (Requirement 4.1).
///
/// This is the single implementation authorization, token issuance, and
/// endpoint protection are all expected to share for scope inclusion
/// judgment (Requirement 4.5): no other module in this crate re-implements
/// [`ScopeSet::is_satisfied_by`]'s logic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeSet(BTreeSet<Scope>);

impl ScopeSet {
    /// Parses a Mastodon-style space-separated scope string (e.g.
    /// `"read write:statuses follow"`) into a [`ScopeSet`].
    ///
    /// Any unrecognized scope token causes the whole request to be rejected
    /// with an [`AppError`] (422 Unprocessable Entity) instead of silently
    /// dropping the unknown token — this is what Requirement 1.3 relies on
    /// for app-registration scope validation, and what Requirement 4.1
    /// relies on for the scope vocabulary itself. An empty or
    /// whitespace-only string parses to an empty [`ScopeSet`].
    pub fn parse(raw: &str) -> Result<ScopeSet, AppError> {
        let mut scopes = BTreeSet::new();
        for token in raw.split_whitespace() {
            match Scope::parse_token(token) {
                Some(scope) => {
                    scopes.insert(scope);
                }
                None => {
                    return Err(AppError::client(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!("unknown OAuth scope: {token}"),
                    ));
                }
            }
        }
        Ok(ScopeSet(scopes))
    }

    /// Builds a [`ScopeSet`] directly from an iterable of [`Scope`]s
    /// (mainly for tests and callers that already hold parsed scopes).
    pub fn from_scopes(scopes: impl IntoIterator<Item = Scope>) -> Self {
        ScopeSet(scopes.into_iter().collect())
    }

    /// Returns `true` if every scope in `self` (the *required* set) is
    /// satisfied by `granted` — either present directly, or covered by a
    /// top-level scope in `granted` that subsumes it (Requirements 4.2,
    /// 4.4). Returns `false` as soon as any required scope is unsatisfied
    /// (Requirement 4.3). An empty required set is trivially satisfied by
    /// any granted set, including an empty one.
    ///
    /// This is the single inclusion judgment shared by authorization, token
    /// issuance, and endpoint protection (Requirement 4.5).
    pub fn is_satisfied_by(&self, granted: &ScopeSet) -> bool {
        self.0.iter().all(|required| {
            granted.0.contains(required)
                || required
                    .subsuming_top_level()
                    .is_some_and(|top| granted.0.contains(&top))
        })
    }

    /// Iterates the contained scopes.
    pub fn iter(&self) -> impl Iterator<Item = &Scope> {
        self.0.iter()
    }

    /// Returns `true` if this set contains no scopes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(raw: &str) -> ScopeSet {
        ScopeSet::parse(raw).unwrap_or_else(|e| panic!("expected {raw:?} to parse, got {e:?}"))
    }

    // ---- ScopeSet::parse ----

    #[test]
    fn parse_accepts_each_top_level_scope() {
        for raw in ["read", "write", "follow", "push"] {
            let set = parse_ok(raw);
            assert_eq!(
                set.iter().count(),
                1,
                "expected exactly one scope in {raw:?}"
            );
        }
    }

    #[test]
    fn parse_accepts_granular_read_and_write_scopes() {
        let set = parse_ok("read:accounts write:statuses");
        let scopes: Vec<Scope> = set.iter().copied().collect();
        assert!(scopes.contains(&Scope::ReadGranular(ReadScope::Accounts)));
        assert!(scopes.contains(&Scope::WriteGranular(WriteScope::Statuses)));
    }

    #[test]
    fn parse_accepts_space_separated_multi_scope_strings() {
        let set = parse_ok("read write:media follow push");
        assert_eq!(set.iter().count(), 4);
    }

    #[test]
    fn parse_deduplicates_repeated_tokens() {
        let set = parse_ok("read read write write");
        assert_eq!(set.iter().count(), 2);
    }

    #[test]
    fn parse_empty_string_yields_empty_set() {
        let set = parse_ok("   ");
        assert!(set.is_empty());
    }

    #[test]
    fn parse_rejects_unknown_top_level_scope() {
        let err = ScopeSet::parse("read bogus_scope")
            .expect_err("unknown top-level scope must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(err.public_message.contains("bogus_scope"));
    }

    #[test]
    fn parse_rejects_unknown_granular_scope() {
        let err = ScopeSet::parse("read:not_a_real_thing")
            .expect_err("unknown granular scope must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_rejects_unknown_namespace_prefix() {
        let err =
            ScopeSet::parse("admin:accounts").expect_err("unknown namespace must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn parse_rejects_malformed_token_with_no_colon_separator() {
        let err = ScopeSet::parse("readfoo").expect_err("malformed token must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // ---- ScopeSet::is_satisfied_by ----

    #[test]
    fn top_level_grant_satisfies_its_own_granular_children() {
        let required = parse_ok("write:statuses write:media");
        let granted = parse_ok("write");
        assert!(required.is_satisfied_by(&granted));
    }

    #[test]
    fn exact_match_satisfies() {
        let required = parse_ok("read:accounts");
        let granted = parse_ok("read:accounts");
        assert!(required.is_satisfied_by(&granted));
    }

    #[test]
    fn missing_requirement_is_rejected() {
        let required = parse_ok("write:statuses");
        let granted = parse_ok("read write:media");
        assert!(!required.is_satisfied_by(&granted));
    }

    #[test]
    fn unrelated_granted_scopes_do_not_spuriously_satisfy() {
        let required = parse_ok("write");
        let granted = parse_ok("read follow push");
        assert!(!required.is_satisfied_by(&granted));
    }

    #[test]
    fn granular_grant_does_not_satisfy_sibling_granular_requirement() {
        let required = parse_ok("read:statuses");
        let granted = parse_ok("read:accounts");
        assert!(!required.is_satisfied_by(&granted));
    }

    #[test]
    fn granular_grant_does_not_satisfy_top_level_requirement() {
        let required = parse_ok("read");
        let granted = parse_ok("read:accounts");
        assert!(!required.is_satisfied_by(&granted));
    }

    #[test]
    fn empty_requirement_is_satisfied_by_anything_including_empty_grant() {
        let required = ScopeSet::default();
        let granted = ScopeSet::default();
        assert!(required.is_satisfied_by(&granted));

        let granted_nonempty = parse_ok("read");
        assert!(required.is_satisfied_by(&granted_nonempty));
    }

    #[test]
    fn multiple_requirements_all_must_be_satisfied() {
        let required = parse_ok("read:statuses write:statuses follow");
        let granted_missing_follow = parse_ok("read write");
        assert!(!required.is_satisfied_by(&granted_missing_follow));

        let granted_all = parse_ok("read write follow");
        assert!(required.is_satisfied_by(&granted_all));
    }

    #[test]
    fn scope_display_round_trips_through_parse() {
        for raw in [
            "read",
            "write",
            "follow",
            "push",
            "read:accounts",
            "write:statuses",
        ] {
            let set = parse_ok(raw);
            let scope = set.iter().next().copied().unwrap();
            assert_eq!(scope.to_string(), raw);
        }
    }
}
