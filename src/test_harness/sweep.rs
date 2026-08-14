//! Orphan-schema reclaim: deciding which abandoned schemas from earlier runs
//! are safe to drop, and dropping them.
//!
//! This module owns both halves: the predicate deciding
//! whether one schema name is old enough that no live process can still be
//! using it ([`is_reclaimable`]), and the pass that enumerates the
//! server's schemas, applies the predicate, and drops what it claims — once
//! per process, from the first fixture created ([`sweep_orphans`]).
//!
//! ## Why the name is the only evidence
//! The reaper (`super::reaper`) is best-effort at process exit, so residue
//! appears within seconds of a run ending. Distinguishing that residue from a
//! schema a *concurrently running* test process is actively using needs an
//! age, and the only age available is the wall-clock nanosecond timestamp
//! [`super::unique_schema_name`] already embeds in every name it mints. That
//! keeps the sweeper free of any DB-side bookkeeping table — no metadata to
//! add, migrate, or keep in sync with the schemas it describes —
//! at the price of a hard coupling to the naming convention, made structural
//! here by owning [`HARNESS_SCHEMA_PREFIX`], which `unique_schema_name`
//! consumes, rather than re-spelling the literal on the reading side where it
//! could silently drift.
//!
//! ## Why every doubt resolves to "keep"
//! The shared `kawasemi_test` database holds schema families this module does
//! not own: `src/migrate/tests.rs`'s `kawasemi_migrate_test_*`,
//! `src/db/tests.rs`'s own fixtures, and whatever a developer created by
//! hand. It also, at any moment, may hold schemas belonging to another
//! process mid-run. Dropping one of those is destructive and silent; failing
//! to drop stale residue merely defers reclaim to the next run, which the
//! next run will do. So the predicate is deliberately asymmetric: anything it
//! cannot fully parse as this crate's own harness naming, and anything whose
//! embedded timestamp is not strictly in the past by at least the threshold,
//! is kept.
//!
//! ## Determinism
//! `now` is a parameter, never `SystemTime::now()` read inside (steering
//! `tech.md`, 決定性の強制). That is what lets the boundary cases below —
//! exactly-at-threshold, and a timestamp in the *future*, which clock skew
//! between machines sharing one test database really does produce — be tested
//! as pure logic with no database and no sleeping.
//!
//! ## Why the sweep sits on the critical path
//! [`sweep_orphans`] is awaited *before* the first fixture is handed out, so
//! every test binary in this workspace pays for it once. That placement is what
//! lets a suite run without a manual pre-run cleanup step or a wrapper
//! script, and it is why every step here is bounded:
//! [`SWEEP_TIMEOUT`] caps the whole pass and [`SWEEP_LOCK_TIMEOUT_MS`] caps any
//! single lock wait, so a database in a bad state costs a fixed number of
//! seconds once rather than hanging a suite that has not run a line of test
//! code yet.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::Row;
use tokio::sync::OnceCell;

use crate::db;

#[cfg(test)]
mod tests;

/// Fixed leading segment of every isolated schema [`super::unique_schema_name`]
/// mints, and therefore the only family of schema names this sweeper may
/// claim. Defined here rather than in the generator because the *reader* is
/// the side that turns a wrong value into data loss: a prefix that drifted
/// too broad would start matching `src/migrate/tests.rs`'s
/// `kawasemi_migrate_test_*` schemas or a developer's own, while one that
/// drifted too narrow would only stop reclaiming. `tests/harness_release_it.rs`
/// keeps its own copy because integration binaries see only this crate's
/// public surface.
pub(crate) const HARNESS_SCHEMA_PREFIX: &str = "kawasemi_test_harness_";

/// How long a harness schema must have existed before this module will claim
/// it — "do not reclaim what a running test is still using", stated as a
/// number.
///
/// Two hours, chosen from the asymmetry of the two ways it can be wrong. Too
/// short destroys a concurrently running process's database state, silently and
/// unrecoverably; too long only defers reclaim to a later run, which will do it.
/// So the bound is set from the longest a schema could *plausibly* still be in
/// use, with a large multiplier on top:
///
/// - A schema never outlives the process that made it, and the longest run in
///   this workspace was the full `cargo test --lib` suite at ~891 seconds when
///   this bound was chosen. Two hours is roughly eight times that, so
///   even a machine several times slower than the one that produced that
///   figure stays inside it.
/// - The one case that legitimately holds a *single* fixture open far longer
///   than the suite takes is a developer stopped in a debugger. Two hours
///   covers a long interactive session; minutes would not.
///
/// Against that, the cost of waiting: an unreclaimed schema consumes catalog
/// rows and disk, not connections — the resource whose exhaustion actually
/// stops a suite —
/// so residue that lingers a couple of hours changes nothing about whether a
/// suite can run. That is why the number is biased long rather than split down
/// the middle.
const RECLAIM_THRESHOLD: Duration = Duration::from_secs(2 * 60 * 60);

/// Upper bound on one whole sweep, drops included.
///
/// The sweep runs before the first fixture of a test binary is handed out, so
/// an unbounded one would turn any database-side stall into a suite that hangs
/// having executed no test at all — reintroducing, in a new place, exactly the
/// failure shape this module removes. Thirty seconds is far above what the
/// work costs (one
/// catalog query plus a `DROP SCHEMA` per orphan, all on one connection: a
/// backlog of 239 schemas drains in well under a second) and far
/// below anything a caller would experience as a hang.
const SWEEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Server-side cap, in milliseconds, on how long any statement this module
/// issues waits for a lock.
///
/// `DROP SCHEMA ... CASCADE` needs `ACCESS EXCLUSIVE` on everything inside the
/// schema, and a schema old enough to be claimed here can still be pinned by a
/// backend whose client is gone. Without this, one such schema would consume
/// the entire [`SWEEP_TIMEOUT`] and take every orphan queued behind it down
/// with it; with it, that schema alone fails, is logged, and the pass
/// continues. Three seconds is generous for a lock that a live process would
/// grant immediately and short enough that even several blocked schemas fit
/// inside the overall bound.
const SWEEP_LOCK_TIMEOUT_MS: u32 = 3_000;

/// Number of startup sweeps that have actually run in this process. Only
/// [`sweep_orphans`]'s one-shot initializer increments it, so it is `1` for the
/// entire life of any process that created at least one fixture, and `0`
/// before that.
static STARTUP_SWEEPS: AtomicUsize = AtomicUsize::new(0);

/// The one-shot guard behind [`sweep_orphans`].
///
/// `tokio::sync::OnceCell` rather than `std::sync::Once` because the
/// initializer awaits: concurrent callers arriving during the first sweep wait
/// for *that* sweep to finish rather than starting a second one, which is what
/// makes "once per process" mean once, and keeps the number of admin
/// connections this module opens at one no matter how many fixtures race to
/// be first.
static STARTUP_SWEEP: OnceCell<()> = OnceCell::const_new();

/// Reclaims schemas earlier runs left behind, once per process.
///
/// Reached from every fixture constructor through
/// `super::establish_isolated_db`; the guard lives here rather than
/// at the call sites so that adding a constructor cannot accidentally add a
/// sweep. The first caller in the process performs the pass and every later
/// one returns immediately.
///
/// Never fails and never panics: everything it could not reclaim is logged and
/// left for a future run, so a reclaim failure never keeps a test from
/// running.
pub(crate) async fn sweep_orphans() {
    STARTUP_SWEEP
        .get_or_init(|| async {
            STARTUP_SWEEPS.fetch_add(1, Ordering::Relaxed);
            sweep_orphans_now().await;
        })
        .await;
}

/// How many startup sweeps have run in this process. Exposed so that
/// `tests/harness_sweep_it.rs` can assert the once-per-process property over
/// the real trigger (`spawn_test_app`) instead of over an internal flag it
/// would have to trust.
pub fn startup_sweeps_performed() -> usize {
    STARTUP_SWEEPS.load(Ordering::Relaxed)
}

/// Performs one reclaim pass immediately, bypassing [`sweep_orphans`]'s
/// once-per-process guard.
///
/// Public because the behaviour under test — old schemas gone, newer and
/// foreign ones untouched — can only be observed by planting schemas and then
/// making a sweep happen on demand, and the guarded entry point deliberately
/// refuses to sweep a second time. Ordinary callers want [`sweep_orphans`];
/// this is the harness's own test surface, and like the rest of
/// `super` it leaves the crate entirely in a build without the
/// `test-harness` feature.
///
/// Bounded by [`SWEEP_TIMEOUT`] and, like the guarded entry point, infallible
/// from the caller's point of view.
pub async fn sweep_orphans_now() {
    let pass = reclaim_orphans(SystemTime::now(), RECLAIM_THRESHOLD);
    if tokio::time::timeout(SWEEP_TIMEOUT, pass).await.is_err() {
        eprintln!(
            "test_harness: the orphan-schema sweep exceeded {SWEEP_TIMEOUT:?} and was abandoned; \
             whatever it had not reclaimed is left for a future run"
        );
    }
}

/// The pass itself, with the clock and the threshold injected so the "which
/// schemas would this claim" decision stays testable without waiting on real
/// time (steering `tech.md`, 決定性の強制).
///
/// Everything runs on a single connection, held for the whole pass. Not a
/// convenience: `super::drop_schema` opens and closes an admin pool per schema,
/// which is right for the reaper's one-off requests but would mean a couple of
/// hundred connection handshakes here, and fanning the drops out concurrently
/// would spend exactly the shared server's connection budget this whole
/// mechanism exists to protect. One connection also lets the
/// `lock_timeout` below apply to every statement in the pass, which a
/// pool-per-drop arrangement could not guarantee.
async fn reclaim_orphans(now: SystemTime, threshold: Duration) {
    let admin_pool = match db::establish_pool(&super::admin_db_config()).await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!(
                "test_harness: could not open an admin connection for the orphan-schema sweep \
                 ({err}); skipping this run's reclaim"
            );
            return;
        }
    };
    let mut conn = match admin_pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => {
            eprintln!(
                "test_harness: could not acquire the orphan-schema sweep's connection ({err}); \
                 skipping this run's reclaim"
            );
            admin_pool.close().await;
            return;
        }
    };

    if let Err(err) = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SET lock_timeout = {SWEEP_LOCK_TIMEOUT_MS}"
    )))
    .execute(&mut *conn)
    .await
    {
        // Not fatal: without the cap the pass is still correct, just able to
        // spend its whole budget on one stuck schema. Worth saying out loud
        // because that is what the next timeout message would otherwise look
        // like for no visible reason.
        eprintln!(
            "test_harness: could not bound the orphan-schema sweep's lock waits ({err}); \
             continuing with the server's default"
        );
    }

    let candidates = match list_schema_names(&mut conn).await {
        Ok(names) => names,
        Err(err) => {
            eprintln!(
                "test_harness: could not list schemas for the orphan-schema sweep ({err}); \
                 skipping this run's reclaim"
            );
            drop(conn);
            admin_pool.close().await;
            return;
        }
    };

    let mut reclaimed = 0usize;
    let mut failed = 0usize;
    for schema in candidates
        .iter()
        .filter(|name| is_reclaimable(name, now, threshold))
    {
        // `IF EXISTS` because another process's sweep may have reclaimed this
        // same schema between the listing above and this statement, which is
        // not an error but the two passes agreeing. Interpolation is safe
        // by construction:
        // `is_reclaimable` accepted this name, and it accepts only the fixed
        // prefix followed by two runs of ASCII digits.
        let dropped = sqlx::query(sqlx::AssertSqlSafe(format!(
            r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE"#
        )))
        .execute(&mut *conn)
        .await;
        match dropped {
            Ok(_) => reclaimed += 1,
            Err(err) => {
                // One schema failing says nothing about the next, so the loop
                // continues rather than returning: a single schema pinned by a
                // stuck backend must not cost the run the rest of its reclaim.
                failed += 1;
                eprintln!(
                    "test_harness: the orphan-schema sweep could not reclaim {schema} ({err}); \
                     leaving it for a future run"
                );
            }
        }
    }
    if reclaimed > 0 || failed > 0 {
        eprintln!(
            "test_harness: orphan-schema sweep reclaimed {reclaimed} schema(s) older than \
             {threshold:?}, {failed} could not be dropped"
        );
    }

    drop(conn);
    admin_pool.close().await;
}

/// Every schema name the server currently reports.
///
/// Deliberately unfiltered rather than `LIKE 'kawasemi_test_harness_%'`:
/// `_` is a single-character wildcard in `LIKE`, so that pattern is *wider*
/// than the prefix it appears to spell and would hand [`is_reclaimable`]
/// names the convention never produced. Filtering in Rust against the real
/// prefix keeps one definition of "is this ours", and the list is a few dozen
/// rows even on a badly littered server.
async fn list_schema_names(
    conn: &mut sqlx::postgres::PgConnection,
) -> Result<Vec<String>, sqlx::Error> {
    // `::text` because `information_schema.schemata.schema_name` is a domain
    // over `name`, not a `text` column.
    let rows =
        sqlx::query("SELECT schema_name::text AS schema_name FROM information_schema.schemata")
            .fetch_all(conn)
            .await?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<String, _>("schema_name"))
        .collect())
}

/// Decides whether `schema_name` names a harness schema old enough to
/// reclaim — old enough that no currently running test can still be using
/// it.
///
/// Returns `true` only when all of the following hold:
/// - the name is exactly `{HARNESS_SCHEMA_PREFIX}{nanos}_{seq}` with both
///   fields non-empty runs of ASCII digits and nothing else,
/// - `nanos` denotes a representable instant after the Unix epoch,
/// - and `now` is at least `threshold` past that instant.
///
/// The threshold comparison is **inclusive**: an age of exactly `threshold`
/// reclaims. The choice is immaterial to safety (real ages never land on an
/// exact nanosecond boundary) and inclusive keeps the predicate's meaning
/// readable as "has survived at least `threshold`".
///
/// Never panics and never allocates, on any input: it is fed arbitrary
/// strings straight out of `information_schema`, including names from schema
/// families this crate does not own.
///
pub(crate) fn is_reclaimable(schema_name: &str, now: SystemTime, threshold: Duration) -> bool {
    let Some(created_at) = parse_creation_time(schema_name) else {
        // Unparseable: not ours, or ours under a naming convention this
        // module has not been taught. Either way, keep it.
        return false;
    };
    match now.duration_since(created_at) {
        Ok(age) => age >= threshold,
        // `created_at` is in the future relative to `now`. Clock skew between
        // machines sharing one test database makes this reachable, and a
        // future timestamp is the *last* thing to reclaim: it most likely
        // belongs to a process running right now on the faster clock.
        Err(_) => false,
    }
}

/// Recovers the instant [`super::unique_schema_name`] stamped into
/// `schema_name`, or `None` if the name is not that function's output.
///
/// Validation is total rather than best-effort. `str::parse` alone would
/// accept `+123`, and a `rsplit`-based split would accept extra segments; the
/// sibling temp-*directory* name `unique_media_storage_root` builds
/// (`kawasemi_test_harness_media_storage_{nanos}_{seq}`) shares this module's
/// prefix and is rejected here on the digits check, so that even if such a
/// name ever reached a schema listing it could not be misread as an ancient
/// schema (`media` parses as no number at all, but a name that merely *looks*
/// numeric after a loose split is the failure mode worth closing).
fn parse_creation_time(schema_name: &str) -> Option<SystemTime> {
    let suffix = schema_name.strip_prefix(HARNESS_SCHEMA_PREFIX)?;
    let (nanos_field, seq_field) = suffix.split_once('_')?;
    if !is_ascii_digits(nanos_field) || !is_ascii_digits(seq_field) {
        return None;
    }
    // `u128` matches what `unique_schema_name` formats (`Duration::as_nanos`).
    // A longer digit run is not a harness name and must not saturate into a
    // very old — i.e. reclaimable — instant, so overflow returns `None`.
    let nanos: u128 = nanos_field.parse().ok()?;
    // Split before converting: `Duration::from_nanos` takes `u64`, which
    // today's epoch nanos still fit but need not forever.
    let seconds = u64::try_from(nanos / 1_000_000_000).ok()?;
    let subsec_nanos = (nanos % 1_000_000_000) as u32;
    UNIX_EPOCH.checked_add(Duration::new(seconds, subsec_nanos))
}

/// True for a non-empty run of ASCII digits. Rejects the sign prefixes and
/// non-ASCII digit characters `str::parse` would otherwise accept, so that
/// only names byte-identical to what the generator emits are claimed.
fn is_ascii_digits(field: &str) -> bool {
    !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit())
}
