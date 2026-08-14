//! The lightweight fixture tier: an isolated, migrated database with no
//! running instance around it.
//!
//! Scope: this module owns [`TestDb`] and [`spawn_test_db`]. What it owns is
//! best described by what it deliberately does *not* do, since it is the
//! cheap half of a two-tier fixture design. [`super::spawn_test_app`] boots a
//! real instance — it builds the nine application modules, generates and
//! caches real signing keys, binds an ephemeral TCP listener, spawns an
//! `axum::serve` task plus the federation and media background loops. None of
//! that happens here: a `TestDb` is an isolated schema, a pool pinned to it
//! with the embedded migrations applied, and a deterministic
//! [`RuntimeContext`]. Nothing else exists to be started, and the struct has
//! no field a running instance could be reached through.
//!
//! ## When to use which fixture
//! A test that sends an HTTP request, touches the router, uses an OAuth
//! token, or reaches a module through `AppState` needs [`super::TestApp`] —
//! and cannot be written against `TestDb` at all, because those handles are
//! simply absent (a misjudged migration fails to compile rather than
//! silently testing less). Everything else — repository and service tests
//! that are SQL plus domain types — belongs here.
//!
//! ## Shared setup, one implementation
//! The schema creation, `search_path` pinning, pool establishment and
//! migration sequence is not repeated here: both fixtures call
//! [`super::establish_isolated_db`]. The isolation guarantee they both rest
//! on depends on those details being identical, which a second copy could not
//! keep true over time.
//!
//! ## Deterministic injection
//! `clock`/`ids`/`rng` follow exactly the convention `spawn_test_app`
//! established — [`RuntimeContext::deterministic`] built from the same fixed
//! seed (`super::default_test_seed`), so a `TestDb` and a `TestApp` in the
//! same binary observe the same sequences. `keys` is where the two
//! deliberately differ: `spawn_test_app` swaps in the real, DB-backed
//! `DbSigningKeyProvider` so an integration test can exercise actor creation
//! through the real key-supply path, whereas this fixture generates no
//! signing keys at all and therefore keeps
//! [`crate::runtime::FixedSigningKeyProvider`], the placeholder
//! `RuntimeContext::deterministic` supplies. A caller that needs the real
//! supply boundary needs a real instance, which is precisely the criterion
//! for reaching for `TestApp` instead.
//!
//! ## Release path
//! Identical in shape to [`super::TestApp`]'s, minus the listener it does not
//! have: [`TestDb::cleanup`] closes the pool and then drops the schema, in
//! that order, and has provably finished by the time it returns; `Drop` hands
//! a pool clone and the schema name to [`super::reaper::HarnessReaper`],
//! which reclaims them on its own resident runtime after the per-test runtime
//! is gone. See [`super`]'s own doc comment ("Release path") for why a
//! destructor cannot do this work itself.

#[cfg(test)]
mod tests;

use sqlx::postgres::PgPool;

use crate::runtime::RuntimeContext;

use super::{IsolatedDb, default_test_seed, drop_schema, establish_isolated_db, reaper};

/// An isolated, migrated database and the deterministic injection boundaries
/// to go with it — the fixture for tests that need real SQL but no running
/// instance.
///
/// [`TestDb::cleanup`] is the explicit release path, and the only one that
/// has provably finished by the time it returns. Omitting it does not leak:
/// `Drop` delegates to [`super::reaper::HarnessReaper`] instead, best-effort
/// at process exit (a request still queued when the test binary exits leaves
/// a stale schema for the startup sweep to reclaim on a later run).
pub struct TestDb {
    /// The connection pool for this fixture's isolated schema, established
    /// via [`crate::db::establish_pool`] with the embedded migrations
    /// already applied.
    pub pool: PgPool,
    /// The deterministic non-determinism injection boundaries this fixture
    /// was built with. Unlike [`super::TestApp::runtime`], `keys` is *not*
    /// swapped for the real DB-backed provider — see this module's doc
    /// comment ("Deterministic injection").
    pub runtime: RuntimeContext,
    /// Name of this fixture's isolated PostgreSQL schema. `Some` until
    /// whichever of [`TestDb::cleanup`] or `Drop` runs first takes it, so the
    /// release is requested exactly once even though `Drop` always runs
    /// (including immediately after a successful `cleanup()`, which is the
    /// case where `Drop` finds `None` and submits nothing). Private, like
    /// [`super::TestApp`]'s own equivalent: it is release plumbing, not part
    /// of the fixture's interface.
    schema: Option<String>,
}

/// Builds a [`TestDb`]: creates a fresh isolated schema, establishes a pool
/// pinned to it, applies the embedded migrations, and pairs it with a
/// deterministic [`RuntimeContext`] — without binding a listener, spawning a
/// task, composing any module, or generating any signing key.
///
/// Panics if the shared test database (see `super::base_test_db_url`,
/// overridable via `KAWASEMI_TEST_DATABASE_URL`) is not reachable, or if any
/// setup step fails, for the same reason [`super::spawn_test_app`] does:
/// this returns `TestDb` directly rather than a `Result`, so
/// callers that need to skip in an environment with no local PostgreSQL check
/// reachability themselves first (this crate's own
/// `should_run_against_real_database` convention).
pub async fn spawn_test_db() -> TestDb {
    // Also triggers the once-per-process startup sweep:
    // `spawn_test_db` is an entry point of its own, and a suite made entirely
    // of lightweight fixtures must still not require a manual pre-run
    // cleanup.
    let IsolatedDb { pool, schema, .. } = establish_isolated_db().await;

    TestDb {
        pool,
        runtime: RuntimeContext::deterministic(default_test_seed()),
        schema: Some(schema),
    }
}

impl TestDb {
    /// The explicit async release path: closes the connection pool, then
    /// drops this fixture's isolated schema — in that order, so the schema is
    /// only torn down once nothing still holds a connection pinned to it.
    pub async fn cleanup(mut self) {
        self.pool.close().await;
        if let Some(schema) = self.schema.take() {
            drop_schema(&schema).await;
        }
    }
}

impl Drop for TestDb {
    /// Delegates release to the process-resident
    /// [`super::reaper::HarnessReaper`], covering the case where a test
    /// panics or otherwise returns without calling [`TestDb::cleanup`]. Every
    /// step is synchronous, non-blocking and infallible, because a panic in a
    /// destructor running during an unwind aborts the process.
    fn drop(&mut self) {
        let Some(schema) = self.schema.take() else {
            // Already released by `cleanup()`. Submitting anyway would be
            // harmless (the reclaim is idempotent) but would queue work
            // behind the fixtures that genuinely leaked.
            return;
        };
        // A `Pool` clone shares one inner state with the handle this struct
        // still owns, so the reaper closing its clone closes this pool — and
        // cloning is a refcount bump, not an operation that can block or
        // fail.
        reaper::HarnessReaper::global()
            .submit(reaper::ReclaimRequest::new(self.pool.clone(), schema));
    }
}
