//! `PollRepository` (design.md "Data / データ層" -> `PollRepository /
//! IdempotencyStore`; Requirements 5.1, 5.2, 13.1, 13.2, 13.3, 13.4, 13.5;
//! task 2.3, `Boundary: PollRepository, IdempotencyStore`): the poll's own
//! persistence — poll/option insertion, vote recording with its
//! deadline/range/single-vs-multiple/duplicate validation, and tally
//! (aggregate) retrieval — against `polls` / `poll_options` / `poll_votes`
//! (`migrations/0007_statuses.sql`, already applied, unmodified by this
//! task). `IdempotencyStore` (the sibling half of this task's boundary) is
//! [`crate::statuses::idempotency`], a separate module against a disjoint
//! table (`status_idempotency_keys`).
//!
//! Scope: this module owns exactly [`insert_poll`], [`record_vote`], and
//! [`tally`] — design.md's `PollRepository` Service Interface (design.md
//! lines 443-445) — plus [`find_poll_by_id`], a thin additive read function
//! task 5.3 (`PollService`) added on top (see that function's own doc
//! comment for why), plus the batched forms of those two reads,
//! [`find_polls_by_ids`] and [`tally_many`] (added later, by
//! structural-refactor task 4.4, Requirement 5.1, so a list endpoint's poll
//! lookups stop scaling with the number of statuses it renders; the singular
//! reads stay, they have other callers). No
//! `StatusRepository`/`InteractionRepository`/
//! `TagRepository` functionality, no `IdempotencyStore` (sibling module, not
//! this file), no `PollService`/`StatusActivityBuilder`/`PollSerializer`
//! orchestration, and no HTTP surface lives here.
//!
//! ## `VoteOutcome` (CONCERN — documented judgment call)
//! design.md's own comment on `record_vote` reads "締切/範囲/重複は AppError
//! or Skipped" — deliberately leaving open whether a rejection is reported
//! via `Err(AppError)` or via a non-error `VoteOutcome` variant. This task's
//! own observable-completion text settles it for all four rejection cases
//! uniformly: "締切後/範囲外/重複投票が拒否され" ("rejected") groups
//! deadline-passed, out-of-range, single/multiple violation, *and* duplicate
//! resubmission under the same "拒否" (reject) language — unlike, say,
//! `InteractionRepository::add_favourite`'s duplicate-favourite case (which
//! Requirement 10.4 calls a silent "作成しない", never "拒否", and which
//! design.md's own Service Interface comment spells out as "新規 true / 既存
//! false", not an error). So every rejection path here (deadline, range,
//! single-vs-multiple, and duplicate-vote) is reported as an
//! `AppError::client` (422 per design.md's "Error Categories and Responses":
//! "投票範囲外/締切後...→ 422"), and [`VoteOutcome`] carries only the single
//! success variant `Recorded` — kept as its own enum (rather than
//! collapsing `record_vote`'s return type to `Result<(), AppError>`) purely
//! to keep design.md's literal `Result<VoteOutcome, AppError>` signature
//! shape intact for a future caller that wants to match on it (e.g. if a
//! richer success shape is needed later).
//!
//! ## Vote validation order and duplicate-request-choice handling
//! [`record_vote`] validates, in order, inside one transaction: (1) at least
//! one choice supplied, (2) the poll exists, (3) the deadline has not passed
//! (13.3), (4) single-choice polls receive at most one *distinct* choice
//! (13.4's "単一選択の投票へ複数選択肢が指定された"), (5) every choice index
//! is a real, persisted `poll_options.idx` for this poll (13.4's "範囲外の
//! 選択肢インデックス"), (6) the actor has not already voted in this poll at
//! all (13.5 — checked against *any* prior `poll_votes` row for
//! `(poll_id, actor_id)`, not merely the exact same `choice`, since a
//! resubmission with a *different* selection is still a resubmission, not a
//! legitimate additional vote). A request's own `choices` slice is
//! deduplicated (sorted + `dedup`) before any of the above checks run, so a
//! caller-side accidental repeat of the same index within one request (e.g.
//! `[0, 0]`) is treated as a single choice rather than being double-counted
//! or spuriously tripping the single-choice check.
//!
//! ## Counting: real rows, not a cached total (13.2's "更新後の集計を反映")
//! Every vote both inserts a `poll_votes` row (the individual-ballot record
//! [`tally`]'s `own_votes` reads back) *and* atomically increments the
//! matching `poll_options.votes_count` (the per-option running total
//! [`tally`]'s `options` reads back) — mirroring
//! `status_repository.rs::adjust_counts`'s "never a read-modify-write pair"
//! atomic-`UPDATE` convention, just inlined per-choice here rather than
//! factored into a shared helper (this module's only counter, unlike
//! `StatusRepository`'s three).
//!
//! ## Closing the duplicate-vote race (review round 1 finding, fixed)
//! `poll_votes`' actual primary key is `(poll_id, actor_id, choice)`
//! (`migrations/0007_statuses.sql`), *not* `(poll_id, actor_id)` — so, unlike
//! `favourites`/`bookmarks`/`pins`' `(actor_id, status_id)` primary keys
//! (which let `interaction_repository.rs` rely on `ON CONFLICT DO NOTHING`
//! alone for dedup), the database itself does not block two concurrent
//! `record_vote` calls for the same `(poll_id, actor_id)` with two
//! *different* `choice` values from both inserting. A plain SELECT-then-
//! INSERT `already_voted` check (no lock) would leave exactly the same
//! TOCTOU window `src/oauth/code_repository.rs::consume_code`'s own doc
//! comment warns against for authorization-code redemption ("A
//! SELECT-then-UPDATE would leave a race window... allowing the same
//! authorization code to be redeemed twice"): two concurrent transactions
//! could both observe `already_voted == false` before either commits, then
//! both insert, producing two `poll_votes` rows for one actor (violating
//! Requirement 13.5).
//!
//! [`record_vote`] closes this the same way `consume_code` closes its own
//! analogous race, adapted to this function's shape: rather than a single
//! atomic `UPDATE ... RETURNING` (there is no existing row to update here —
//! a vote is a fresh `INSERT`, not a state transition on an existing row),
//! the initial `polls` row fetch takes `FOR UPDATE`, row-locking that poll
//! for the remainder of the transaction. A second, concurrent
//! `record_vote(poll_id, actor_id, ...)` call (for the same actor **or any
//! other actor** — the lock is per-poll, not per-`(poll_id, actor_id)`) then
//! blocks at its own `FOR UPDATE` fetch until the first transaction commits
//! or rolls back, so its own `already_voted` check always observes the
//! first transaction's committed outcome before proceeding — the two checks
//! can never both observe "not yet voted" for the same actor. This
//! serializes *all* voters of one poll against each other (coarser than a
//! per-`(poll_id, actor_id)` advisory lock would be), which is an accepted
//! precision-for-correctness trade at this repository layer: correctness
//! (Requirement 13.5 truly holding under concurrency) matters more here than
//! maximizing per-poll vote throughput, and a poll's vote volume is not a
//! hot path this task's requirements call out for tuning.
//! This module's own
//! `record_vote_serializes_concurrent_votes_by_the_same_actor` test (in
//! `poll_repository/tests.rs`) is this fix's regression test, following
//! `code_repository/tests.rs`'s own
//! `concurrent_consumption_of_the_same_code_lets_exactly_one_caller_win`
//! established `tokio::spawn` + cloned-`PgPool` pattern for provoking a
//! genuine concurrent race against real Postgres rather than an in-process
//! ordering assumption.

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::api::db::map_server_error;
use crate::domain::Id;
use crate::error::{AppError, ErrorTag};
use crate::statuses::model::{Poll, PollOption};

fn rejected(message: &'static str) -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, message)
}

/// Persists `poll` and its `options` as new `polls`/`poll_options` rows in
/// one transaction (Requirement 13.1). Caller is responsible for minting
/// `poll.id` and each `option.poll_id`/`idx` (this crate's established
/// "callers mint ids, repositories never do" convention, mirroring
/// `status_repository.rs::insert_status`); `option.votes_count` is expected
/// to be `0` for a freshly-created poll, but this function does not itself
/// enforce that — it persists whatever `options` already carries, matching
/// `insert_status`'s "persists whatever `status` already carries" precedent.
pub async fn insert_poll(
    pool: &PgPool,
    poll: &Poll,
    options: &[PollOption],
) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(map_server_error)?;

    sqlx::query("INSERT INTO polls (id, status_id, expires_at, multiple) VALUES ($1, $2, $3, $4)")
        .bind(poll.id.as_i64())
        .bind(poll.status_id.as_i64())
        .bind(poll.expires_at)
        .bind(poll.multiple)
        .execute(&mut *tx)
        .await
        .map_err(map_server_error)?;

    for option in options {
        sqlx::query(
            "INSERT INTO poll_options (poll_id, idx, title, votes_count) VALUES ($1, $2, $3, $4)",
        )
        .bind(option.poll_id.as_i64())
        .bind(option.idx)
        .bind(&option.title)
        .bind(option.votes_count)
        .execute(&mut *tx)
        .await
        .map_err(map_server_error)?;
    }

    tx.commit().await.map_err(map_server_error)?;
    Ok(())
}

/// Fetches poll `poll_id`'s own row (`status_id`/`expires_at`/`multiple`),
/// independent of any option/tally data (Requirement 13.2's "締切前の可視な
/// 投票に対し" — a caller needs the poll's owning `status_id` *before* it can
/// even ask whether that status is visible, which [`tally`] alone cannot
/// supply since it assumes the poll is already known to exist/be visible).
/// Added by task 5.3 (`PollService`, this repository's first caller that
/// needs a poll's owning status resolved *before* running any visibility
/// check) — an additive, read-only function alongside this module's
/// existing three; mirrors task 5.1's identical precedent of adding thin
/// `pub` read wrappers (`status_repository::find_by_id`,
/// `ancestors_unfiltered`, `descendants_unfiltered`) to an earlier task's
/// repository module without altering any existing function's signature or
/// behavior. Returns `Ok(None)` (not an error) when `poll_id` matches no
/// row, mirroring `status_repository::find_by_id`'s identical
/// existence-vs-error convention — the caller, not this function, decides
/// whether a missing poll is a `404`.
pub async fn find_poll_by_id(pool: &PgPool, poll_id: Id) -> Result<Option<Poll>, AppError> {
    let row: Option<(i64, Option<OffsetDateTime>, bool)> =
        sqlx::query_as("SELECT status_id, expires_at, multiple FROM polls WHERE id = $1")
            .bind(poll_id.as_i64())
            .fetch_optional(pool)
            .await
            .map_err(map_server_error)?;

    Ok(row.map(|(status_id, expires_at, multiple)| Poll {
        id: poll_id,
        status_id: Id::from_i64(status_id),
        expires_at,
        multiple,
    }))
}

/// The batched form of [`find_poll_by_id`] (structural-refactor task 4.4,
/// Requirement 5.1): resolves every id in `poll_ids` in one query instead of
/// one query per poll, so a list endpoint's poll lookups stop scaling with
/// the number of statuses it renders.
///
/// Equivalent to calling [`find_poll_by_id`] once per id, by construction:
/// same table, same columns, same `WHERE id = ...` scoping (a `polls` row
/// carries no viewer/visibility dimension for either function to disagree
/// about), just widened to `= ANY`. Ordering is unconstrained because the
/// result is a map keyed by poll id, and `polls` has exactly one row per id
/// — there is no per-key sequence for the two forms to disagree about.
///
/// An id matching no `polls` row has **no entry** in the returned map rather
/// than an error (the returned keys are the subset of `poll_ids` that exist)
/// — the same "no row -> absent" contract
/// [`tags_for_statuses`](crate::statuses::tag_repository::tags_for_statuses)
/// carries, and the batched counterpart of [`find_poll_by_id`]'s own
/// `Ok(None)`: the caller, not this function, decides whether a missing poll
/// is a `404`. An empty `poll_ids` returns an empty map without issuing a
/// query at all: `= ANY` on an empty array would match nothing anyway, so
/// the round trip would be pure cost.
pub async fn find_polls_by_ids(
    pool: &PgPool,
    poll_ids: &[Id],
) -> Result<HashMap<Id, Poll>, AppError> {
    if poll_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let raw_ids: Vec<i64> = poll_ids.iter().map(|id| id.as_i64()).collect();
    let rows: Vec<(i64, i64, Option<OffsetDateTime>, bool)> = sqlx::query_as(
        "SELECT id, status_id, expires_at, multiple FROM polls WHERE id = ANY($1::bigint[])",
    )
    .bind(&raw_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows
        .into_iter()
        .map(|(id, status_id, expires_at, multiple)| {
            let id = Id::from_i64(id);
            (
                id,
                Poll {
                    id,
                    status_id: Id::from_i64(status_id),
                    expires_at,
                    multiple,
                },
            )
        })
        .collect())
}

/// The (currently single-variant) success report for [`record_vote`] — see
/// this module's doc comment ("`VoteOutcome`") for why every rejection is
/// instead reported via `Err(AppError)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteOutcome {
    /// The vote was validated and persisted (Requirement 13.2).
    Recorded,
}

/// Records `actor_id`'s vote for `choices` in poll `poll_id`, applying every
/// validation rule design.md/this task assign to this layer (Requirements
/// 13.2-13.5) — see this module's doc comment ("Vote validation order") for
/// the exact rule sequence. `now` is the caller-injected current time
/// (`RuntimeContext::clock`, never a SQL-side `NOW()`), consulted against the
/// poll's `expires_at` for the deadline check (13.3).
///
/// On success, both the individual `poll_votes` row(s) and the matching
/// `poll_options.votes_count` counter(s) are updated atomically in one
/// transaction (see this module's doc comment, "Counting").
pub async fn record_vote(
    pool: &PgPool,
    poll_id: Id,
    actor_id: Id,
    choices: &[i32],
    now: OffsetDateTime,
) -> Result<VoteOutcome, AppError> {
    if choices.is_empty() {
        return Err(rejected("at least one poll option must be selected"));
    }

    let mut deduped: Vec<i32> = choices.to_vec();
    deduped.sort_unstable();
    deduped.dedup();

    let mut tx = pool.begin().await.map_err(map_server_error)?;

    // `FOR UPDATE` locks this `polls` row for the rest of the transaction —
    // see this module's doc comment ("Closing the duplicate-vote race") for
    // why this is load-bearing, not incidental: it serializes concurrent
    // `record_vote` calls against the same poll, so a second, concurrent
    // caller's own `already_voted` check below can never run until the
    // first caller has committed (or rolled back) its own vote.
    let poll_row: Option<(Option<OffsetDateTime>, bool)> =
        sqlx::query_as("SELECT expires_at, multiple FROM polls WHERE id = $1 FOR UPDATE")
            .bind(poll_id.as_i64())
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_server_error)?;

    let Some((expires_at, multiple)) = poll_row else {
        let _ = tx.rollback().await;
        return Err(AppError::client(StatusCode::NOT_FOUND, "poll not found"));
    };

    if let Some(expires_at) = expires_at
        && now >= expires_at
    {
        let _ = tx.rollback().await;
        return Err(rejected("poll has already closed"));
    }

    if !multiple && deduped.len() > 1 {
        let _ = tx.rollback().await;
        return Err(rejected(
            "a single-choice poll accepts only one selected option",
        ));
    }

    let valid_indices: Vec<i32> =
        sqlx::query_scalar("SELECT idx FROM poll_options WHERE poll_id = $1")
            .bind(poll_id.as_i64())
            .fetch_all(&mut *tx)
            .await
            .map_err(map_server_error)?;

    if deduped.iter().any(|choice| !valid_indices.contains(choice)) {
        let _ = tx.rollback().await;
        return Err(rejected("selected option index is out of range"));
    }

    let (already_voted,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM poll_votes WHERE poll_id = $1 AND actor_id = $2)",
    )
    .bind(poll_id.as_i64())
    .bind(actor_id.as_i64())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_server_error)?;

    if already_voted {
        let _ = tx.rollback().await;
        // Tagged, unlike this function's three other rejections, because
        // one caller has to tell it apart from them: an inbound vote
        // Activity that loops back to a local poll's author reports
        // "already voted" for a vote that was in fact just recorded
        // successfully, and must be treated as idempotent rather than as a
        // 422 for the voter (see `inbound_handlers.rs`'s `VoteHandler`).
        // The message text is unchanged and still what the client sees —
        // only the way that caller recognizes this case has moved off it.
        return Err(AppError::client_tagged(
            StatusCode::UNPROCESSABLE_ENTITY,
            "actor has already voted in this poll",
            ErrorTag::DuplicateVote,
        ));
    }

    for choice in &deduped {
        sqlx::query(
            "INSERT INTO poll_votes (poll_id, actor_id, choice, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(poll_id.as_i64())
        .bind(actor_id.as_i64())
        .bind(choice)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_server_error)?;

        sqlx::query(
            "UPDATE poll_options SET votes_count = votes_count + 1 WHERE poll_id = $1 AND idx = $2",
        )
        .bind(poll_id.as_i64())
        .bind(choice)
        .execute(&mut *tx)
        .await
        .map_err(map_server_error)?;
    }

    tx.commit().await.map_err(map_server_error)?;
    Ok(VoteOutcome::Recorded)
}

/// The aggregate view of one poll's current state (Requirement 13.2's
/// "更新後の集計を反映した Poll", and the read side of Requirement 2.2's
/// `voted`/`own_votes` actor-state, one layer down from the eventual
/// `PollSerializer`). Not a design.md model type (design.md's model excerpt
/// does not list it — only this Service Interface's return type names it),
/// so it is defined here, matching this module's own boundary rather than
/// `crate::statuses::model`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollTally {
    pub poll_id: Id,
    /// Every option, in `idx` order, with its current `votes_count`.
    pub options: Vec<PollOption>,
    /// Count of distinct actors who have voted at all (Requirement 2.1's
    /// `voters_count` — distinct from the sum of `options[].votes_count`,
    /// which can exceed `voters_count` on a multiple-choice poll).
    pub voters_count: i64,
    /// `viewer`'s own selected choice indices, ascending; empty when
    /// `viewer` is `None` or has not voted (Requirement 2.2's `own_votes`).
    pub own_votes: Vec<i32>,
}

/// Fetches the current aggregate state of poll `poll_id` (Requirement 13.2),
/// including `viewer`'s own selections when `viewer` is `Some` (Requirement
/// 2.2). Returns a caller-facing (`ErrorKind::Client`) `404 Not Found` when
/// `poll_id` matches no row.
pub async fn tally(pool: &PgPool, poll_id: Id, viewer: Option<Id>) -> Result<PollTally, AppError> {
    let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM polls WHERE id = $1")
        .bind(poll_id.as_i64())
        .fetch_optional(pool)
        .await
        .map_err(map_server_error)?;
    if exists.is_none() {
        return Err(AppError::client(StatusCode::NOT_FOUND, "poll not found"));
    }

    let option_rows: Vec<(i32, String, i64)> = sqlx::query_as(
        "SELECT idx, title, votes_count FROM poll_options WHERE poll_id = $1 ORDER BY idx",
    )
    .bind(poll_id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    let options = option_rows
        .into_iter()
        .map(|(idx, title, votes_count)| PollOption {
            poll_id,
            idx,
            title,
            votes_count,
        })
        .collect();

    let (voters_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(DISTINCT actor_id) FROM poll_votes WHERE poll_id = $1")
            .bind(poll_id.as_i64())
            .fetch_one(pool)
            .await
            .map_err(map_server_error)?;

    let own_votes = if let Some(viewer) = viewer {
        sqlx::query_scalar(
            "SELECT choice FROM poll_votes WHERE poll_id = $1 AND actor_id = $2 ORDER BY choice",
        )
        .bind(poll_id.as_i64())
        .bind(viewer.as_i64())
        .fetch_all(pool)
        .await
        .map_err(map_server_error)?
    } else {
        Vec::new()
    };

    Ok(PollTally {
        poll_id,
        options,
        voters_count,
        own_votes,
    })
}

/// The batched form of [`tally`] (structural-refactor task 4.4, Requirement
/// 5.1): resolves every id in `poll_ids` with a fixed number of queries
/// instead of a fixed number *per poll*, so a list endpoint's poll-aggregate
/// lookups stop scaling with the number of statuses it renders.
///
/// Equivalent to calling [`tally`] once per id, by construction: [`tally`]'s
/// four queries each widen to `= ANY` with the same `WHERE` scoping and the
/// same `ORDER BY`, and none of them is folded into another — in particular
/// the per-option `votes_count` (a `poll_options` column) and the
/// `voters_count` (a `COUNT(DISTINCT actor_id)` over `poll_votes`) stay
/// separate queries rather than becoming one join, which would multiply
/// option rows by vote rows and inflate both. The leading `poll_id` added to
/// each `ORDER BY` only groups one poll's rows together; it cannot reorder
/// rows *within* one poll, which is the ordering [`tally`] actually promises
/// (`ORDER BY idx` for options, `ORDER BY choice` for `own_votes`). The
/// result is keyed off the `polls` rows themselves — the batched form of
/// [`tally`]'s own existence check — rather than off option or vote rows, so
/// an existing poll with no options and no votes still gets an entry (an
/// empty `options`, a zero `voters_count`), exactly as [`tally`] reports it.
///
/// `viewer`'s own selections populate each entry's `own_votes` when `viewer`
/// is `Some` (the extra query is skipped entirely when it is `None`, mirroring
/// [`tally`]'s own branch); a poll `viewer` has not voted in gets the same
/// empty `own_votes` [`tally`] returns for it.
///
/// An id matching no `polls` row has **no entry** in the returned map rather
/// than the `404` [`tally`] raises for it — the same "no row -> absent"
/// contract [`find_polls_by_ids`] carries, and what lets one batched call
/// stand in for N singular ones over a list whose ids are not all guaranteed
/// to resolve. An empty `poll_ids` returns an empty map without issuing a
/// query at all: `= ANY` on an empty array would match nothing anyway, so the
/// round trips would be pure cost.
pub async fn tally_many(
    pool: &PgPool,
    poll_ids: &[Id],
    viewer: Option<Id>,
) -> Result<HashMap<Id, PollTally>, AppError> {
    if poll_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let raw_ids: Vec<i64> = poll_ids.iter().map(|id| id.as_i64()).collect();

    let existing: Vec<(i64,)> = sqlx::query_as("SELECT id FROM polls WHERE id = ANY($1::bigint[])")
        .bind(&raw_ids)
        .fetch_all(pool)
        .await
        .map_err(map_server_error)?;

    let mut tallies: HashMap<Id, PollTally> = existing
        .into_iter()
        .map(|(id,)| {
            let poll_id = Id::from_i64(id);
            (
                poll_id,
                PollTally {
                    poll_id,
                    options: Vec::new(),
                    voters_count: 0,
                    own_votes: Vec::new(),
                },
            )
        })
        .collect();

    let option_rows: Vec<(i64, i32, String, i64)> = sqlx::query_as(
        "SELECT poll_id, idx, title, votes_count FROM poll_options \
         WHERE poll_id = ANY($1::bigint[]) ORDER BY poll_id, idx",
    )
    .bind(&raw_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    for (poll_id, idx, title, votes_count) in option_rows {
        let poll_id = Id::from_i64(poll_id);
        if let Some(entry) = tallies.get_mut(&poll_id) {
            entry.options.push(PollOption {
                poll_id,
                idx,
                title,
                votes_count,
            });
        }
    }

    let voter_rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT poll_id, COUNT(DISTINCT actor_id) FROM poll_votes \
         WHERE poll_id = ANY($1::bigint[]) GROUP BY poll_id",
    )
    .bind(&raw_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    for (poll_id, voters_count) in voter_rows {
        if let Some(entry) = tallies.get_mut(&Id::from_i64(poll_id)) {
            entry.voters_count = voters_count;
        }
    }

    if let Some(viewer) = viewer {
        let vote_rows: Vec<(i64, i32)> = sqlx::query_as(
            "SELECT poll_id, choice FROM poll_votes \
             WHERE poll_id = ANY($1::bigint[]) AND actor_id = $2 ORDER BY poll_id, choice",
        )
        .bind(&raw_ids)
        .bind(viewer.as_i64())
        .fetch_all(pool)
        .await
        .map_err(map_server_error)?;

        for (poll_id, choice) in vote_rows {
            if let Some(entry) = tallies.get_mut(&Id::from_i64(poll_id)) {
                entry.own_votes.push(choice);
            }
        }
    }

    Ok(tallies)
}
