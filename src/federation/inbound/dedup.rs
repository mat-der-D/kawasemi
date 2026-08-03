//! `ReceivedActivityStore` (design.md "ReceivedActivityStore" -> Service
//! Interface; Requirement 7.4; task 3.1, `Boundary: ReceivedActivityStore`):
//! records each inbound Activity's own `id` in `received_activities`
//! (`migrations/0004_federation.sql`) the first time it is received, and
//! reports whether a given `id` was new or already known — the idempotency
//! ledger Requirement 7.4 requires ("既に受理済みの識別子を持つ Activity が
//! 再度投函されたとき...その Activity を重複として扱い、業務処理を二重に
//! 実行しない").
//!
//! ## `record_if_new` is the single source of truth for "new vs. known"
//! `record_if_new` both records and answers "was this new" in one atomic
//! step (`INSERT ... ON CONFLICT (activity_id) DO NOTHING`, checking the
//! number of affected rows) rather than a separate
//! read-then-write, because two concurrent deliveries of the very same
//! Activity id (a real possibility: a sender's shared-inbox delivery and a
//! per-actor-inbox delivery of the same Activity, or a naive retry) racing a
//! SELECT-then-INSERT could both observe "not yet known" and both proceed to
//! double-dispatch — exactly what Requirement 7.4 forbids. `activity_id` is
//! this table's primary key (`migrations/0004_federation.sql`), so the
//! database itself serializes concurrent inserts for the same id: at most
//! one concurrent `record_if_new` call for a given id can ever observe
//! "0 rows affected" as false (rows_affected() == 0, i.e. the conflict
//! branch) turn into `false`; the other observes the actual insert and
//! returns `true`, regardless of call ordering.
//!
//! ## Retention is a constructor parameter, not yet config-wired
//! design.md names the retention window's config key as
//! `federation.received_activity_retention_days` (既定 14 日), but wiring
//! core-runtime's TOML+DB config layer into a `federation` config section is
//! task 5.4's boundary (`_Boundary: FederationModule, Bootstrap, AppState,
//! Config_`), not this task's (`_Boundary: ReceivedActivityStore_`). This
//! module therefore accepts `retention: time::Duration` as a plain
//! constructor parameter ([`DbReceivedActivityStore::new`]), mirroring
//! `key_resolver.rs`'s own `cache_ttl` constructor parameter for
//! `federation.public_key_cache_ttl`; [`DEFAULT_RECEIVED_ACTIVITY_RETENTION`]
//! captures the documented default value for task 5.4's bootstrap wiring to
//! apply once it exists, without this module reaching into config itself.
//!
//! ## Pruning
//! [`ReceivedActivityStore::prune_expired`] deletes every row whose
//! `received_at` is older than `retention` measured against this store's
//! injected [`Clock`] (never `OffsetDateTime::now_utc()`/`SystemTime::now()`
//! directly — per steering's clock/id/rng/signing-key DI boundary,
//! `.kiro/steering/tech.md`: "時刻・ID・乱数・署名鍵は注入可能（DI）にする"),
//! and reports how many rows were deleted (observability/tests). Task 3.1
//! only provides this capability; scheduling it to run periodically is
//! later wiring (task 5.4/bootstrap), not this task's boundary.
//!
//! ## Error mapping
//! Any DB failure recording or pruning maps to a `Server` (5xx) [`AppError`]
//! with `StatusCode::INTERNAL_SERVER_ERROR`, mirroring `key_resolver.rs`'s
//! and `actor/keys/repository.rs`'s convention for unexpected database
//! failures.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::Duration;

use crate::error::AppError;
use crate::runtime::Clock;

/// Documented default for `federation.received_activity_retention_days`
/// (design.md/task 3.1: "既定 14 日"). Not applied automatically anywhere in
/// this module — see this module's doc comment ("Retention is a constructor
/// parameter") for why; exposed so task 5.4's bootstrap wiring has a single
/// source of truth for the documented default instead of re-deriving
/// `Duration::days(14)` itself.
pub const DEFAULT_RECEIVED_ACTIVITY_RETENTION: Duration = Duration::days(14);

/// Inbound Activity-id idempotency ledger port (design.md's exact
/// `ReceivedActivityStore` Service Interface; Requirement 7.4): records an
/// Activity id the first time it is seen and reports new-vs-known, and
/// prunes rows older than the configured retention window. See this
/// module's doc comment for the full contract.
///
/// `#[allow(async_fn_in_trait)]`: mirrors `PublicKeyResolver`'s own
/// documented rationale in `key_resolver.rs` (design.md pins
/// `record_if_new` as literal `async fn`; boxing/`Send`-pinning concerns
/// belong to whichever later task actually needs
/// `Arc<dyn ReceivedActivityStore>` across a `tokio::spawn` boundary — e.g.
/// task 4.1's `InboxService` — not this task's `ReceivedActivityStore`
/// boundary).
#[allow(async_fn_in_trait)]
pub trait ReceivedActivityStore: Send + Sync {
    /// Records `activity_id` if not already known. Returns `true` when this
    /// call recorded it for the first time (new), `false` when it was
    /// already known (the caller must not re-run business-logic dispatch
    /// for it — Requirement 7.4).
    async fn record_if_new(&self, activity_id: &str) -> Result<bool, AppError>;

    /// Deletes every recorded row older than this store's configured
    /// retention window (measured against this store's injected [`Clock`]),
    /// returning how many rows were deleted.
    async fn prune_expired(&self) -> Result<u64, AppError>;
}

/// `ReceivedActivityStore` implementation backed by `received_activities`,
/// with recording and pruning both judged against an injected [`Clock`]
/// (never wall-clock time directly, per steering's non-determinism DI
/// boundary).
pub struct DbReceivedActivityStore {
    pool: PgPool,
    clock: Arc<dyn Clock>,
    retention: Duration,
}

impl DbReceivedActivityStore {
    /// Builds a store against `pool` (the `received_activities` table's
    /// connection pool), `clock` (the "now" used both for `received_at`
    /// timestamps and pruning's cutoff), and `retention` (the pruning
    /// window — see this module's doc comment, "Retention is a constructor
    /// parameter", for why this is a parameter here rather than read from
    /// config; pass [`DEFAULT_RECEIVED_ACTIVITY_RETENTION`] for the
    /// documented default).
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>, retention: Duration) -> Self {
        Self {
            pool,
            clock,
            retention,
        }
    }
}

impl ReceivedActivityStore for DbReceivedActivityStore {
    /// See this module's doc comment ("`record_if_new` is the single
    /// source of truth") for why this is a single atomic
    /// `INSERT ... ON CONFLICT DO NOTHING`, never a SELECT-then-INSERT.
    async fn record_if_new(&self, activity_id: &str) -> Result<bool, AppError> {
        let result = sqlx::query(
            "INSERT INTO received_activities (activity_id, received_at) \
             VALUES ($1, $2) \
             ON CONFLICT (activity_id) DO NOTHING",
        )
        .bind(activity_id)
        .bind(self.clock.now())
        .execute(&self.pool)
        .await
        .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(result.rows_affected() > 0)
    }

    /// See this module's doc comment ("Pruning") for the exact cutoff
    /// contract.
    async fn prune_expired(&self) -> Result<u64, AppError> {
        let cutoff = self.clock.now() - self.retention;
        let result = sqlx::query("DELETE FROM received_activities WHERE received_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(result.rows_affected())
    }
}
