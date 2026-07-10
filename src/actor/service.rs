//! `ActorService` (design.md "Service / サービス層" -> "ActorService";
//! Requirements 1.1, 1.3, 1.5, 1.6, 2.2, 2.3, 7.2, 7.3, 7.5; task 5.1): the
//! business service that creates a local actor (handle-format validation ->
//! owner-existence check -> active-initialized insert -> signing-key
//! generation, all in a single transaction) and drives its basic lifecycle
//! (deactivation).
//!
//! Scope: this module owns exactly the two operations design.md's Service
//! Interface specifies — [`ActorService::create_actor`] and
//! [`ActorService::deactivate_actor`] — orchestrating `owner`'s
//! [`find_owner`] (existence check, Requirement 2.3), `repository`'s
//! [`insert_actor`]/[`update_state`]/[`find_by_id`] (already implemented by
//! tasks 2.1/2.2), and `keys::service`'s
//! [`SigningKeyService::provision_key`] (already implemented by task 4.1)
//! against the *same* transaction `create_actor` opens. It does not
//! implement `ActorDirectory` (downstream reference operations, task 5.2)
//! or bootstrap/`AppState` wiring (task 6.1, out of this task's boundary).
//!
//! ## `NewActor.handle` is a pre-validated [`Handle`], not a raw `String`
//! design.md's sequence diagram for actor creation reads "validate handle
//! and resolve owner" as the service's first step, and its Service
//! Interface literally types `NewActor.handle` as `Handle`
//! (`src/actor/model.rs`, task 1.2), not `String`. [`Handle::new`] already
//! performs the entirety of the format-validation Requirement 1.6 asks for
//! (rejecting an empty string or a disallowed character with a caller-facing
//! `400 Bad Request`), and it is impossible to construct a `Handle` value
//! any other way (no public field, no other constructor). So "validate
//! handle" is satisfied by the type system at the point a caller builds a
//! `NewActor` — by the time `create_actor` receives one, the handle has
//! already been validated; this module does not re-validate it (there is
//! nothing left to check), and re-tests only the remaining, service-owned
//! steps (owner resolution, insertion, key provisioning) against a
//! `Handle` that is valid by construction. Requirement 1.6's own behavior
//! (empty/disallowed-character rejection) is exercised by
//! `src/actor/model.rs`'s own test module, not duplicated here.
//!
//! ## Single-transaction boundary (Requirements 1.3, 2.3, 4.1)
//! `create_actor` opens one transaction via `self.pool.begin()`, drives
//! [`insert_actor`] and [`SigningKeyService::provision_key`] through it, and
//! only calls `.commit()` once both have succeeded. Neither step calls
//! `.rollback()` explicitly on failure — mirroring
//! `src/actor/keys/service.rs`'s own `rotate_key` precedent (which likewise
//! relies on an early `?`-return dropping its transaction rather than an
//! explicit rollback call): dropping an uncommitted `sqlx::Transaction`
//! without committing it never persists its writes, so a key-generation (or
//! DB) failure after `insert_actor` succeeded rolls the actor insertion back
//! too, exactly as design.md's System Flow requires ("鍵生成失敗時は
//! アクター作成もロールバックする"). Owner-existence (Requirement 2.3) and
//! handle-uniqueness (Requirement 1.3, surfaced by [`insert_actor`]'s own
//! `409 Conflict` mapping) are both checked/enforced *before* the
//! transaction's writes are ever attempted or already inside it via the
//! database's own unique constraint — neither requires this module to add
//! its own duplicate-detection logic.
//!
//! ## ID/time injection (Requirements 1.5, 7.5)
//! `create_actor` mints the new actor's `id` via `self.runtime.ids.next_id()`
//! and stamps `created_at`/`updated_at` via `self.runtime.clock.now()` —
//! never `OffsetDateTime::now_utc()` directly — so both are swappable for a
//! deterministic, reproducible sequence under test
//! ([`crate::runtime::RuntimeContext::deterministic`]), matching every
//! sibling module's (`OwnerRepository`, `ActorRepository`,
//! `SigningKeyService`) established convention.
//!
//! ## Owner-not-found / actor-not-found mapping
//! Both are mapped to a caller-facing (`ErrorKind::Client`) `404 Not Found`
//! [`AppError`] — the natural HTTP status for "the referenced resource does
//! not exist", consistent with `SigningKeyService::rotate_key`'s own
//! precedent for its symmetric "actor not found" rejection (Requirement
//! 5.5).
//!
//! ## `deactivate_actor`'s re-fetch
//! [`repository::update_state`] itself already stamps `updated_at` from the
//! caller-supplied `now` and reports whether a row actually changed
//! (`Ok(bool)`, not the updated row). `deactivate_actor` re-fetches via
//! [`find_by_id`] after a successful update rather than hand-constructing
//! the return value from `input`/`now`, so the value handed back to the
//! caller is provably what is actually persisted (the same round-trip
//! discipline `OwnerRepository::create_owner`'s own doc comment argues for),
//! not merely an echo of the arguments this method happened to pass to
//! `update_state`.
//!
//! ## Feature-flag protocol: not applicable
//! `ActorService` is a brand-new internal service module with no existing
//! callers or previously-observable behavor to gate — nothing in the
//! running application invokes `create_actor`/`deactivate_actor` yet (that
//! wiring is task 6.1's bootstrap/`AppState` job, out of this task's
//! boundary), so there is no regression risk a flag would protect against.
//! A standard RED -> GREEN -> REFACTOR cycle against a real Postgres
//! instance (via `spawn_test_app`) is the appropriate verification method
//! here, matching every sibling service/repository module's own testing
//! convention in this crate.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;

use super::keys::service::SigningKeyService;
use super::model::{ActorState, ActorType, Handle, LocalActor};
use super::owner::find_owner;
use super::repository::{find_by_id, insert_actor, update_state};
use crate::domain::Id;
use crate::error::AppError;
use crate::runtime::RuntimeContext;

/// Input to [`ActorService::create_actor`] (design.md's exact Service
/// Interface shape). `handle` is already a validated [`Handle`] — see this
/// module's doc comment ("`NewActor.handle` is a pre-validated `Handle`")
/// for why no separate raw-string format-validation step exists here.
pub struct NewActor {
    pub owner_id: Id,
    pub handle: Handle,
    pub actor_type: ActorType,
    pub display_name: String,
    pub summary: String,
}

/// The business service collecting actor creation (handle validation via
/// the `Handle` type, owner-existence check, active-initialized insertion,
/// and signing-key provisioning, all in one transaction) and basic
/// lifecycle transitions (design.md's exact Service Interface):
/// [`create_actor`](Self::create_actor) and
/// [`deactivate_actor`](Self::deactivate_actor).
pub struct ActorService {
    pool: PgPool,
    runtime: RuntimeContext,
    signing_key_service: Arc<SigningKeyService>,
}

impl ActorService {
    /// Builds a service bound to `pool` (for its own self-opened
    /// create-actor transaction and `deactivate_actor`'s state update),
    /// `runtime` (the injected id/clock boundaries, Requirements 1.5, 7.5),
    /// and `signing_key_service` (Requirement 4.1's key provisioning,
    /// driven through the same transaction `create_actor` opens).
    /// `signing_key_service` is constructor-injected behind `Arc` — mirroring
    /// this same crate's convention for a service shared across call sites
    /// (see `src/actor/keys/service.rs`'s own `cipher: Arc<dyn KeyCipher>`
    /// field) and matching how bootstrap wiring (task 6.1, out of this
    /// task's boundary) is expected to hand a single, shared
    /// `SigningKeyService` instance to multiple consumers.
    pub fn new(pool: PgPool, runtime: RuntimeContext, signing_key_service: Arc<SigningKeyService>) -> Self {
        Self {
            pool,
            runtime,
            signing_key_service,
        }
    }

    /// Creates a new local actor (Requirements 1.1, 1.3, 1.5, 1.6, 2.2, 2.3,
    /// 7.2, 7.5): rejects with a caller-facing `404 Not Found` if
    /// `input.owner_id` does not reference an existing owner (Requirement
    /// 2.3); otherwise mints a fresh `id`/`created_at`/`updated_at` from
    /// `self.runtime` (Requirements 1.5, 7.5), inserts the actor row
    /// initialized to [`ActorState::Active`] (Requirement 7.2) and
    /// provisions its signing key (Requirement 4.1), both against the same
    /// transaction (see this module's doc comment, "Single-transaction
    /// boundary") — a duplicate handle (Requirement 1.3) or a key-generation
    /// failure both roll the whole transaction back, and only a fully
    /// successful sequence commits and returns the persisted [`LocalActor`].
    pub async fn create_actor(&self, input: NewActor) -> Result<LocalActor, AppError> {
        let owner = find_owner(&self.pool, input.owner_id).await?;
        if owner.is_none() {
            return Err(AppError::client(
                StatusCode::NOT_FOUND,
                "owner not found for actor creation",
            ));
        }

        let now = self.runtime.clock.now();
        let actor = LocalActor {
            id: self.runtime.ids.next_id(),
            owner_id: input.owner_id,
            handle: input.handle,
            actor_type: input.actor_type,
            display_name: input.display_name,
            summary: input.summary,
            state: ActorState::Active,
            created_at: now,
            updated_at: now,
        };

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        insert_actor(&mut tx, &actor).await?;
        self.signing_key_service
            .provision_key(&mut tx, actor.id)
            .await?;

        tx.commit()
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(actor)
    }

    /// Deactivates the actor persisted under `actor_id` (Requirement 7.3):
    /// transitions its state to [`ActorState::Deactivated`] and stamps
    /// `updated_at` via `self.runtime.clock.now()` (Requirement 7.5),
    /// rejecting with a caller-facing `404 Not Found` if `actor_id` does not
    /// reference an existing actor. Returns the actor as actually persisted
    /// after the transition (re-fetched via `find_by_id`, per this module's
    /// doc comment "`deactivate_actor`'s re-fetch").
    pub async fn deactivate_actor(&self, actor_id: Id) -> Result<LocalActor, AppError> {
        let now = self.runtime.clock.now();
        let updated = update_state(&self.pool, actor_id, ActorState::Deactivated, now).await?;
        if !updated {
            return Err(AppError::client(
                StatusCode::NOT_FOUND,
                "actor not found for deactivation",
            ));
        }

        find_by_id(&self.pool, actor_id).await?.ok_or_else(|| {
            AppError::server(
                StatusCode::INTERNAL_SERVER_ERROR,
                std::io::Error::other(
                    "actor vanished between update_state reporting success and the re-fetch",
                ),
            )
        })
    }
}
