//! Orphan-schema reclaim judgement (OrphanSchemaSweeper boundary,
//! test-infrastructure Requirement 3.2, design.md's "Components and
//! Interfaces" -> "Test Infrastructure" -> "OrphanSchemaSweeper").
//!
//! Scope of *this* module today is the predicate alone (task 2.1): given a
//! schema name read out of `information_schema`, decide whether it is old
//! enough that no live process can still be using it. The sweep itself —
//! enumerating schemas, choosing the threshold, issuing the drops, running
//! once per process — is task 2.2 and lands here next.
//!
//! ## Why the name is the only evidence
//! The reaper (`super::reaper`) is best-effort at process exit, so residue
//! appears within seconds of a run ending. Distinguishing that residue from a
//! schema a *concurrently running* test process is actively using needs an
//! age, and the only age available is the wall-clock nanosecond timestamp
//! [`super::unique_schema_name`] already embeds in every name it mints. That
//! keeps the sweeper free of any DB-side bookkeeping table (design.md: 「閾値
//! 判定は名前に埋まったナノ秒だけで行うため、DB へのメタデータ追加を必要としない」)
//! at the price of a hard coupling to the naming convention — made structural
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

use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Decides whether `schema_name` names a harness schema old enough to reclaim
/// (Requirement 3.2: 「現在実行中のテストが使用しているスキーマ... を回収対象に
/// 含めない」).
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
/// `#[allow(dead_code)]`: the only production caller is `sweep_orphans`,
/// which task 2.2 adds. **Remove this attribute in task 2.2.**
#[allow(dead_code)]
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
