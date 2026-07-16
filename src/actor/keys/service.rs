//! `SigningKeyService` (design.md "Service / サービス層" -> "SigningKeyService";
//! Requirements 4.1, 4.5, 5.1, 5.2, 5.3, 5.5, 6.4; task 4.1): the business
//! service that generates, seals, and persists an actor's signing key at
//! creation time, and drives at-most-one-active-key rotation, keeping
//! [`KeyCache`] in sync with every write it makes.
//!
//! Scope: this module owns exactly the two operations design.md's Service
//! Interface specifies — [`SigningKeyService::provision_key`] and
//! [`SigningKeyService::rotate_key`] — orchestrating `keys::material`'s
//! [`generate_keypair`](super::material::generate_keypair) (key generation),
//! `keys::cipher`'s [`KeyCipher::seal`] (at-rest sealing),
//! `keys::repository`'s `insert_active_key`/`retire_active_key`
//! (persistence, already implemented by task 2.3), and [`KeyCache::upsert`]
//! (cache consistency). It does not implement core-runtime's
//! `SigningKeyProvider` trait (`DbSigningKeyProvider`, task 4.2, a separate
//! boundary that only *reads* [`KeyCache`]) or decide when `provision_key`
//! is called during actor creation (`ActorService::create_actor`, task
//! 5.1, which opens the transaction this service's `provision_key` writes
//! into).
//!
//! ## `provision_key` takes a caller-supplied transaction; `rotate_key` opens its own
//! design.md's Service Interface gives these two operations different
//! transaction-ownership shapes on purpose, matching the two System Flows
//! diagrams: "アクター作成（鍵生成連動）" shows `ActorService` opening one
//! transaction and driving both the actor insert and `provision_key`'s key
//! insert through it (so a key-generation failure rolls the actor insertion
//! back too — "鍵生成失敗時はアクター作成もロールバックする"), while
//! "署名鍵ローテーション" is a fully self-contained flow (`Start[rotate
//! requested for actor] --> ... --> Done`) with no outer caller-owned
//! transaction to join. `provision_key` therefore takes `tx: &mut
//! PgTransaction<'_>` (reusing [`crate::actor::repository::PgTransaction`],
//! per the precedent task 2.3 set for the same type — see
//! `src/actor/keys/repository.rs`'s doc comment), while `rotate_key` opens,
//! drives, and commits its own transaction internally.
//!
//! ## Cache upsert timing (CONCERN: no rollback reconciliation)
//! Both operations upsert [`KeyCache`] as their last synchronous step,
//! exactly as both System Flows diagrams show it (`KSvc->>Cache: upsert
//! active key for actor` inside the create-actor sequence diagram, before
//! `KSvc-->>Svc: ok`; `Cache[update key cache active key] --> Done` inside
//! the rotation flowchart) — i.e. the cache is updated as part of this
//! service's own call, not gated on whether a caller-owned transaction
//! later actually commits. For `rotate_key` this is safe: its own
//! transaction is committed (`tx.commit().await?`) *before* the cache
//! upsert runs, so the cache is only ever updated after the DB write is
//! durable. For `provision_key`, however, the transaction is caller-owned
//! (`ActorService::create_actor`, task 5.1) and is *not yet committed* when
//! `provision_key` returns — if that outer transaction is later rolled
//! back (e.g. a downstream step in `create_actor` fails after
//! `provision_key` returns `Ok`), the cache would then hold an active key
//! for an actor that was never actually persisted. design.md's flow
//! diagrams and Service Interface give no reconciliation mechanism for
//! this race (no "on rollback, evict" hook exists anywhere in the spec),
//! so this task implements the literal sequence diagram as written rather
//! than inventing one. Flagged prominently here and in this task's
//! `CONCERNS` for the reviewer — the resolution (e.g. only upserting after
//! the *caller's* commit, which would require `provision_key`'s signature
//! to change) is out of this task's boundary to decide unilaterally.
//!
//! ## `rotate_key`'s not-found mapping (Requirement 5.5)
//! design.md's Error Strategy places "アクター不在ローテーション（5.5）"
//! under "利用者起因（4xx 相当）". This module maps it to a caller-facing
//! (`ErrorKind::Client`) `404 Not Found` [`AppError`] — the natural HTTP
//! status for "the referenced resource does not exist", and consistent
//! with `ActorRepository`'s own precedent of using a specific 4xx status
//! (`409 Conflict` for duplicate handle) rather than a generic 400 for a
//! specific, named failure condition.
//!
//! ## Atomic rotation ordering (Requirements 5.2, 5.3)
//! `rotate_key` retires the current active key *before* inserting the new
//! one, both against the same transaction, mirroring
//! `src/actor/keys/repository.rs`'s own doc comment
//! ("design.md's rotation flow... always retires the current active key
//! before inserting a new one within the same transaction"). This ordering
//! is what makes the DB's partial unique index
//! (`actor_signing_keys_active_unique`) never reject the new insert: at the
//! instant the new row is inserted, the old row has already transitioned to
//! `status = 'retired'` within the same not-yet-committed transaction, so
//! at most one `active` row for the actor ever exists, even transiently
//! (Requirement 5.3).

use std::sync::Arc;

use axum::http::StatusCode;
use sqlx::PgPool;

use super::cache::KeyCache;
use super::cipher::KeyCipher;
use super::material::{KeyAlgorithm, generate_keypair};
use super::repository::{SigningKeyStatus, StoredSigningKey, insert_active_key, retire_active_key};
use crate::actor::repository::{PgTransaction, find_by_id};
use crate::config::Secret;
use crate::domain::Id;
use crate::error::AppError;
use crate::runtime::RuntimeContext;
use crate::runtime::signing_key::{KeyRef, SigningKey};

/// Maps a [`KeyAlgorithm`] to its `actor_signing_keys.algorithm` `TEXT`
/// column representation (`migrations/0002_actors.sql`'s column comment:
/// `'rsa-2048'`), mirroring the same `TEXT`-column round-trip convention
/// `src/actor/repository.rs`'s `actor_type_as_str` established. Only an
/// encoding direction is needed here (never decoded back into a
/// `KeyAlgorithm`): `ActorSigningKeyRepository::StoredSigningKey::algorithm`
/// is a plain `String` (task 2.3's deliberate choice, not yet wired to this
/// typed enum — see that module's doc comment), so this service only ever
/// writes this string, never reads it back into a `KeyAlgorithm`.
fn key_algorithm_as_str(algorithm: KeyAlgorithm) -> &'static str {
    match algorithm {
        KeyAlgorithm::Rsa2048 => "rsa-2048",
    }
}

/// Generates a fresh key pair, seals its private key, and returns both the
/// ready-to-persist [`StoredSigningKey`] row and the plaintext
/// [`SigningKey`] to hand to [`KeyCache::upsert`] — shared by
/// [`SigningKeyService::provision_key`] and
/// [`SigningKeyService::rotate_key`], which differ only in what they do
/// with the transaction, not in how a new key is produced (Requirements
/// 4.2, 4.5).
///
/// The cache's plaintext [`SigningKey`] is built directly from the
/// just-generated `private_key_pem` (never by re-opening the sealed
/// bytes just written) — the service already holds the plaintext in memory
/// at this point, so a redundant `KeyCipher::open` round trip is
/// unnecessary.
fn build_new_active_key(
    runtime: &RuntimeContext,
    cipher: &dyn KeyCipher,
    actor_id: Id,
) -> Result<(StoredSigningKey, SigningKey), AppError> {
    let generated = generate_keypair(runtime.rng.as_ref())?;

    let plaintext_bytes = generated
        .private_key_pem
        .expose_secret()
        .as_bytes()
        .to_vec();
    let sealed_private_key =
        cipher.seal(&Secret::new(plaintext_bytes.clone()), runtime.rng.as_ref())?;

    let stored = StoredSigningKey {
        id: runtime.ids.next_id(),
        actor_id,
        algorithm: key_algorithm_as_str(generated.algorithm).to_string(),
        public_key_pem: generated.public_key_pem,
        sealed_private_key,
        status: SigningKeyStatus::Active,
        created_at: runtime.clock.now(),
    };
    let cached = SigningKey::from_pem_bytes(plaintext_bytes);

    Ok((stored, cached))
}

/// The business service collecting key generation, at-rest sealing,
/// persistence, and cache consistency behind two operations (design.md's
/// exact Service Interface): [`provision_key`](Self::provision_key) (actor
/// creation time) and [`rotate_key`](Self::rotate_key) (Requirement 5.1's
/// operator-triggered rotation).
pub struct SigningKeyService {
    pool: PgPool,
    runtime: RuntimeContext,
    cipher: Arc<dyn KeyCipher>,
    cache: KeyCache,
}

impl SigningKeyService {
    /// Builds a service bound to `pool` (for [`rotate_key`](Self::rotate_key)'s
    /// self-opened transaction and its actor-existence check), `runtime`
    /// (the injected rng/clock/id-generator boundaries), `cipher` (the
    /// at-rest sealing boundary — already KEK-bound by its own constructor,
    /// per `src/actor/keys/cipher.rs`), and `cache` (the shared [`KeyCache`]
    /// handle this service is the sole writer of, per design.md's "単一書込
    /// 経路で整合").
    pub fn new(
        pool: PgPool,
        runtime: RuntimeContext,
        cipher: Arc<dyn KeyCipher>,
        cache: KeyCache,
    ) -> Self {
        Self {
            pool,
            runtime,
            cipher,
            cache,
        }
    }

    /// Generates a fresh signing key pair, seals its private key, and
    /// inserts it as `actor_id`'s active key, all against the
    /// caller-supplied `tx` (Requirement 4.1: "アクター専用の署名鍵ペアを1
    /// つ生成し、有効な鍵として保管する"; Requirement 4.5: sealed-at-rest
    /// storage). The caller (`ActorService::create_actor`, task 5.1) owns
    /// opening and committing `tx` — this method never calls
    /// `.commit()`/`.rollback()` itself, so a key-generation/DB failure
    /// here propagates as an `Err` the caller can use to roll the whole
    /// transaction back (design.md: "鍵生成失敗時はアクター作成もロール
    /// バックする").
    ///
    /// Updates [`KeyCache`] with the new active key as its last step,
    /// *before* the caller's transaction is necessarily committed — see
    /// this module's doc comment ("Cache upsert timing") for the rollback
    /// race this implies and why it is implemented literally per design.md
    /// regardless.
    pub async fn provision_key(
        &self,
        tx: &mut PgTransaction<'_>,
        actor_id: Id,
    ) -> Result<(), AppError> {
        let (stored, cached) = build_new_active_key(&self.runtime, self.cipher.as_ref(), actor_id)?;

        insert_active_key(tx, &stored).await?;

        self.cache.upsert(KeyRef(actor_id), cached);

        Ok(())
    }

    /// Rotates `actor_id`'s active signing key: rejects with a
    /// caller-facing `404 Not Found` [`AppError`] if `actor_id` does not
    /// exist (Requirement 5.5), otherwise generates a new key pair, and —
    /// within a single self-opened-and-committed transaction — retires the
    /// current active key (Requirement 5.2) before inserting the new one as
    /// active (Requirement 5.1), so at most one active key ever exists for
    /// the actor, even transiently (Requirement 5.3; see this module's doc
    /// comment "Atomic rotation ordering"). Updates [`KeyCache`] with the
    /// new active key only after the transaction has committed (Requirement
    /// 6.4).
    pub async fn rotate_key(&self, actor_id: Id) -> Result<(), AppError> {
        let actor = find_by_id(&self.pool, actor_id).await?;
        if actor.is_none() {
            return Err(AppError::client(
                StatusCode::NOT_FOUND,
                "actor not found for signing key rotation",
            ));
        }

        let (stored, cached) = build_new_active_key(&self.runtime, self.cipher.as_ref(), actor_id)?;

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
        retire_active_key(&mut tx, actor_id, stored.created_at).await?;
        insert_active_key(&mut tx, &stored).await?;
        tx.commit()
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        self.cache.upsert(KeyRef(actor_id), cached);

        Ok(())
    }
}

#[cfg(test)]
mod tests;
