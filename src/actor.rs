//! Actor domain module (actor-model spec).
//!
//! Scope so far:
//! - Task 1.2 (`Boundary: model`): the pure domain/value types that
//!   represent a local actor, the management-layer owner concept, and the
//!   protocol-layer reference types downstream (api-foundation /
//!   federation-core) will consume — see [`model`].
//! - Task 2.1 (`Boundary: OwnerRepository`): the owner concept's
//!   persistence — creation and lookup against a plain `&PgPool` — see
//!   [`owner`].
//! - Task 2.2 (`Boundary: ActorRepository`): the local actor's persistence,
//!   state transitions, and handle/id/owner-scoped lookups — see
//!   [`repository`].
//! - Task 2.3 (`Boundary: ActorSigningKeyRepository`): the per-actor signing
//!   key's persistence — active-key insertion, retirement, active-public-key
//!   lookup, and the startup bulk load of every active key — see the `keys`
//!   submodule's [`keys::repository`].
//! - Task 5.1 (`Boundary: ActorService`): actor creation (handle validation
//!   via the `Handle` type -> owner-existence check -> active-initialized
//!   insert -> signing-key provisioning, all in one transaction) and basic
//!   lifecycle (deactivation) — see [`service`].
//! - Task 5.2 (`Boundary: ActorDirectory`): downstream-facing actor
//!   reference operations — management-layer owner-scoped listing
//!   (`list_actors_for_owner`) and protocol-layer handle resolution /
//!   public-key supply (`resolve_actor_by_handle`, `actor_public_key`),
//!   neither of which surfaces owner information — see [`directory`].
//! - Task 6.1 (`Boundary: ActorModule, Bootstrap, AppState, Config`): wires
//!   the already-implemented pieces above into a running application —
//!   [`ActorModule`] bundles [`ActorService`]/[`keys::service::SigningKeyService`]/
//!   [`ActorDirectory`] behind one handle `AppState` stores, and
//!   [`load_key_cache`]/[`build_actor_module`] are the composition-root
//!   steps `src/bootstrap.rs` (production) and `src/test_harness.rs` (tests)
//!   both call to build one, so the actual DB-backed
//!   `keys::provider::DbSigningKeyProvider` — not
//!   `crate::runtime::signing_key::FixedSigningKeyProvider`'s placeholder —
//!   backs `RuntimeContext.keys` end to end (Requirements 6.1, 6.4).
//!
//! `keys`'s `material`/`cipher`/`service`/`cache`/`provider` submodules are
//! later/already-landed tasks per design.md's File Structure Plan.

use std::sync::Arc;

use sqlx::postgres::PgPool;

use self::keys::cache::KeyCache;
use self::keys::cipher::KeyCipher;
use self::keys::service::SigningKeyService;
use crate::error::AppError;
use crate::runtime::RuntimeContext;
use crate::runtime::signing_key::{KeyRef, SigningKey};

pub mod directory;
pub mod keys;
pub mod model;
pub mod owner;
pub mod repository;
pub mod service;

pub use directory::ActorDirectory;
pub use model::{
    ActorPublicKey, ActorState, ActorSummary, ActorType, Handle, LocalActor, Owner, ResolvedActor,
};
pub use service::{ActorService, NewActor};

/// Bundles the three actor-model services `AppState` shares across
/// concurrent request handlers (design.md's "ActorModule(bootstrap wiring)"
/// component, Requirements 6.1, 6.4): [`ActorService`] (actor creation/
/// lifecycle), [`SigningKeyService`] (key generation/rotation), and
/// [`ActorDirectory`] (downstream-facing reference operations). Cheap to
/// clone — every field is already an `Arc`, so cloning an `ActorModule`
/// only bumps three reference counts, matching `AppState`'s own
/// cheap-to-clone discipline.
///
/// This type does not construct its own dependencies: building the real
/// `KeyCache`/`DbSigningKeyProvider`/`RuntimeContext` and wiring the three
/// services together is [`build_actor_module`]'s job (task 6.1's own
/// `src/bootstrap.rs`/`src/test_harness.rs` changes).
#[derive(Clone)]
pub struct ActorModule {
    actor_service: Arc<ActorService>,
    signing_key_service: Arc<SigningKeyService>,
    directory: Arc<ActorDirectory>,
}

impl ActorModule {
    /// Bundles already-constructed service handles. Callers ([`build_actor_module`],
    /// or a future spec's own composition) are responsible for constructing
    /// each of these first — this constructor only bundles them, mirroring
    /// `AppState::new`'s own "bundle, don't build" contract.
    pub fn new(
        actor_service: Arc<ActorService>,
        signing_key_service: Arc<SigningKeyService>,
        directory: Arc<ActorDirectory>,
    ) -> Self {
        Self {
            actor_service,
            signing_key_service,
            directory,
        }
    }

    /// The shared [`ActorService`] handle, for actor creation/lifecycle
    /// operations.
    pub fn actor_service(&self) -> &Arc<ActorService> {
        &self.actor_service
    }

    /// The shared [`SigningKeyService`] handle, for direct key
    /// provisioning/rotation calls (e.g. an operator-triggered rotation
    /// endpoint a downstream spec adds).
    pub fn signing_key_service(&self) -> &Arc<SigningKeyService> {
        &self.signing_key_service
    }

    /// The shared [`ActorDirectory`] handle, for downstream-facing
    /// reference operations (owner-scoped listing, handle resolution,
    /// public-key supply).
    pub fn directory(&self) -> &Arc<ActorDirectory> {
        &self.directory
    }
}

/// Loads every currently active signing key from `pool` and opens each
/// one's sealed private key via `cipher`, returning a [`KeyCache`]
/// pre-warmed with the resulting `(KeyRef, SigningKey)` pairs (design.md's
/// "署名鍵供給（同期境界）" flow: "鍵は起動時にキャッシュへロードされ").
///
/// Shared by the Bootstrap composition root (`src/bootstrap.rs`) and the
/// test harness (`src/test_harness.rs`), so both warm the actor-model's key
/// supply the same way (task 6.1, Requirement 6.1) instead of each
/// reimplementing this loading/opening sequence independently.
pub async fn load_key_cache(pool: &PgPool, cipher: &dyn KeyCipher) -> Result<KeyCache, AppError> {
    let stored_keys = keys::repository::load_all_active(pool).await?;

    let mut entries = Vec::with_capacity(stored_keys.len());
    for stored in stored_keys {
        let opened = cipher.open(&stored.sealed_private_key)?;
        let signing_key = SigningKey::from_pem_bytes(opened.expose_secret().as_bytes().to_vec());
        entries.push((KeyRef(stored.actor_id), signing_key));
    }

    Ok(KeyCache::from_entries(entries))
}

/// Assembles the three actor-model services (`SigningKeyService`,
/// `ActorService`, `ActorDirectory`), all sharing `pool`/`runtime`/`cipher`/
/// `cache`, bundled as an [`ActorModule`] (design.md's
/// "ActorModule(bootstrap wiring)" component). Pure composition — no I/O of
/// its own; `cache` should already be warmed (typically via
/// [`load_key_cache`]) before calling this, and `runtime.keys` should
/// already be the [`keys::provider::DbSigningKeyProvider`] built from the
/// same `cache` (so the synchronous supply boundary and this module's own
/// write path observe the same underlying map).
///
/// Shared by `src/bootstrap.rs` (production) and `src/test_harness.rs`
/// (`spawn_test_app`), so both wire the three services identically.
pub fn build_actor_module(
    pool: PgPool,
    runtime: RuntimeContext,
    cipher: Arc<dyn KeyCipher>,
    cache: KeyCache,
) -> ActorModule {
    let signing_key_service = Arc::new(SigningKeyService::new(
        pool.clone(),
        runtime.clone(),
        cipher,
        cache,
    ));
    let actor_service = Arc::new(ActorService::new(
        pool.clone(),
        runtime,
        signing_key_service.clone(),
    ));
    let directory = Arc::new(ActorDirectory::new(pool));

    ActorModule::new(actor_service, signing_key_service, directory)
}
