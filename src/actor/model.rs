//! Actor/owner domain types and protocol-layer reference types
//! (`model` component, design.md "Domain / モデル層", Requirements 1.1,
//! 1.4, 1.6, 3.1, 7.1).
//!
//! Scope: this module owns only the pure domain/value types below — no
//! persistence (`OwnerRepository` / `ActorRepository`, task 2.x), no
//! business logic (`ActorService` / `SigningKeyService`, tasks 4.x/5.x),
//! and no downstream reference operations (`ActorDirectory`, task 5.2).
//! Those consume the types defined here but are out of scope for task 1.2
//! (`Boundary: model`).
//!
//! The central structural invariant this module enforces is the split
//! between the management-layer type ([`LocalActor`], which carries
//! `owner_id`) and the protocol-layer/public reference types
//! ([`ResolvedActor`], [`ActorPublicKey`], [`ActorSummary`]), none of which
//! carry any owner field (Requirement 3.1). This is checked structurally in
//! this module's tests via exhaustive field destructuring (no `..`), which
//! fails to compile if an owner-related field is ever added to one of those
//! types.

use time::OffsetDateTime;

use crate::domain::Id;
use crate::error::AppError;
use axum::http::StatusCode;

/// A local actor's handle (local username), validated at construction.
///
/// Format rules (Requirement 1.6): non-empty, and every character must be
/// an ASCII letter, ASCII digit, or underscore. This charset is not
/// further specified by requirements.md/design.md beyond "空文字・規定外の
/// 文字を含む等"; it follows the conventional ActivityPub/Mastodon-style
/// username charset as a concrete, testable default. Instance-wide
/// uniqueness of the handle (Requirement 1.2) is a data-layer concern
/// (`local_actors_handle_unique`, `migrations/0002_actors.sql`), not
/// something a single `Handle` value can check by itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Handle(String);

impl Handle {
    /// Validates and constructs a `Handle` from `raw`.
    ///
    /// Rejects an empty string and any string containing a character
    /// outside the allowed charset (ASCII alphanumeric or `_`), returning a
    /// caller-facing [`AppError`] (`400 Bad Request`) describing the
    /// violation (Requirement 1.6). Accepts any non-empty string drawn
    /// entirely from the allowed charset.
    pub fn new(raw: impl Into<String>) -> Result<Self, AppError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(AppError::client(
                StatusCode::BAD_REQUEST,
                "handle must not be empty",
            ));
        }
        if let Some(bad) = raw.chars().find(|c| !is_allowed_handle_char(*c)) {
            return Err(AppError::client(
                StatusCode::BAD_REQUEST,
                format!(
                    "handle {raw:?} contains disallowed character {bad:?}; only ASCII letters, digits, and underscore are allowed"
                ),
            ));
        }
        Ok(Handle(raw))
    }

    /// Returns the validated handle as a plain string slice, e.g. to bind
    /// as a `TEXT` query parameter in a later persistence task.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Returns whether `c` is part of [`Handle`]'s allowed character set (ASCII
/// letters, ASCII digits, or underscore).
fn is_allowed_handle_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A local actor's type: a human-operated persona, or an automated/bot
/// actor (Requirement 1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorType {
    /// A human-operated actor persona.
    Person,
    /// An automated actor (BOT).
    Service,
}

/// A local actor's lifecycle state (Requirement 7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorState {
    /// The actor is active and resolvable/usable downstream.
    Active,
    /// The actor has been deactivated (Requirement 7.3); downstream
    /// resolution must still be able to distinguish this state
    /// (Requirement 7.4), but this module owns only the value, not the
    /// resolution behavior.
    Deactivated,
}

/// The management-layer-only "owner" concept: a single administrator that
/// may hold multiple [`LocalActor`]s (Requirement 2.1).
///
/// Deliberately has no fields beyond identity/creation metadata — the
/// owner-to-actor relationship itself lives on [`LocalActor::owner_id`],
/// not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub id: Id,
    pub created_at: OffsetDateTime,
}

/// A local actor's full management-layer record, including its owner
/// relationship (Requirement 1.1).
///
/// `owner_id` deliberately appears only on this management-layer type, not
/// on [`ResolvedActor`] / [`ActorPublicKey`] / [`ActorSummary`]
/// (Requirement 3.1) — see this module's doc comment and tests for how
/// that split is enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalActor {
    pub id: Id,
    /// Management-layer-only relationship to the owning [`Owner`]. Must
    /// never be surfaced through a protocol-layer/public reference type
    /// (Requirement 3.1).
    pub owner_id: Id,
    pub handle: Handle,
    pub actor_type: ActorType,
    pub display_name: String,
    pub summary: String,
    pub state: ActorState,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Protocol-layer/public reference to a single actor, as returned by
/// handle/id resolution (Requirement 3.1, 3.2, 8.2).
///
/// Structurally carries no owner information — see this module's tests for
/// a compile-time check of that invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActor {
    pub id: Id,
    pub handle: Handle,
    pub actor_type: ActorType,
    pub display_name: String,
    pub summary: String,
    pub state: ActorState,
}

/// Protocol-layer/public reference to an actor's active signing key
/// material, as returned by public-key supply (Requirement 3.1, 8.3).
///
/// Structurally carries no owner information — see this module's tests for
/// a compile-time check of that invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorPublicKey {
    pub actor_id: Id,
    pub key_id: Id,
    pub public_key_pem: String,
}

/// Management-layer listing entry for "actors owned by this owner"
/// (Requirement 8.1), used as the basis for actor selection by downstream
/// specs (api-foundation).
///
/// Despite being a management-layer type (only reachable via an
/// owner-scoped query), it still carries no `owner_id` field of its own:
/// the owner is already the query key the caller supplied, not part of the
/// entry's own shape. See this module's tests for a compile-time check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorSummary {
    pub id: Id,
    pub handle: Handle,
    pub actor_type: ActorType,
    pub display_name: String,
    pub state: ActorState,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_time() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }

    // --- Handle::new format validation (Requirements 1.6, 1.2) ---

    #[test]
    fn handle_new_rejects_empty_string() {
        let err = Handle::new("").expect_err("empty handle must be rejected");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn handle_new_rejects_disallowed_characters() {
        for bad in ["alice bot", "alice@bot", "alice.bot", "アリス", "alice-bot", "alice/bot"] {
            let err = Handle::new(bad)
                .expect_err(&format!("handle {bad:?} with disallowed characters must be rejected"));
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn handle_new_accepts_allowed_characters() {
        for good in ["alice", "ALICE", "alice_bot", "alice123", "_", "123"] {
            let handle = Handle::new(good).expect("allowed-charset handle must be accepted");
            assert_eq!(handle.as_str(), good);
        }
    }

    // --- ActorType / ActorState variants (Requirements 1.4, 7.1) ---

    #[test]
    fn actor_type_distinguishes_person_from_service() {
        assert_ne!(ActorType::Person, ActorType::Service);
    }

    #[test]
    fn actor_state_distinguishes_active_from_deactivated() {
        assert_ne!(ActorState::Active, ActorState::Deactivated);
    }

    // --- LocalActor carries owner_id (management-layer type) ---

    #[test]
    fn local_actor_carries_owner_id() {
        let now = sample_time();
        let actor = LocalActor {
            id: Id::from_i64(1),
            owner_id: Id::from_i64(2),
            handle: Handle::new("alice").unwrap(),
            actor_type: ActorType::Person,
            display_name: "Alice".to_string(),
            summary: String::new(),
            state: ActorState::Active,
            created_at: now,
            updated_at: now,
        };
        assert_eq!(actor.owner_id, Id::from_i64(2));
    }

    // --- Reference types structurally lack an owner field (Requirement 3.1) ---
    //
    // Exhaustive destructuring (no `..`) is a compile-time proof that each
    // type has exactly the fields listed: if an `owner_id` (or any other)
    // field were ever added to one of these types, the destructuring below
    // would fail to compile (E0027 "pattern does not mention field"),
    // forcing this invariant to be revisited per design.md's Revalidation
    // Triggers.

    #[test]
    fn resolved_actor_has_no_owner_field() {
        let resolved = ResolvedActor {
            id: Id::from_i64(1),
            handle: Handle::new("alice").unwrap(),
            actor_type: ActorType::Person,
            display_name: "Alice".to_string(),
            summary: String::new(),
            state: ActorState::Active,
        };
        let ResolvedActor {
            id,
            handle,
            actor_type,
            display_name,
            summary,
            state,
        } = resolved;
        let _ = (id, handle, actor_type, display_name, summary, state);
    }

    #[test]
    fn actor_public_key_has_no_owner_field() {
        let public_key = ActorPublicKey {
            actor_id: Id::from_i64(1),
            key_id: Id::from_i64(2),
            public_key_pem: "pem".to_string(),
        };
        let ActorPublicKey {
            actor_id,
            key_id,
            public_key_pem,
        } = public_key;
        let _ = (actor_id, key_id, public_key_pem);
    }

    #[test]
    fn actor_summary_has_no_owner_field() {
        let summary = ActorSummary {
            id: Id::from_i64(1),
            handle: Handle::new("alice").unwrap(),
            actor_type: ActorType::Person,
            display_name: "Alice".to_string(),
            state: ActorState::Active,
        };
        let ActorSummary {
            id,
            handle,
            actor_type,
            display_name,
            state,
        } = summary;
        let _ = (id, handle, actor_type, display_name, state);
    }

    // --- Owner (management-layer concept) ---

    #[test]
    fn owner_holds_id_and_created_at() {
        let now = sample_time();
        let owner = Owner {
            id: Id::from_i64(42),
            created_at: now,
        };
        assert_eq!(owner.id, Id::from_i64(42));
        assert_eq!(owner.created_at, now);
    }
}
