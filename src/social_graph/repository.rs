//! `RelationshipRepository` (design.md "Data / データ層" ->
//! `RelationshipRepository`; Requirements 1.6, 2.2, 4.3, 8.1, 8.4, 9.1, 9.2,
//! 9.3; task 1.3, `Boundary: RelationshipRepository`, minimally extended by
//! task 2.2, same boundary): persistence access for `follows` /
//! `follow_requests` / `mutes` / `blocks` (`migrations/0012_social_graph.sql`,
//! task 1.1, unmodified) — upsert / delete / existence, the inbound
//! pending-request page, viewer+target-group batched reverse lookup for
//! relationship-flag derivation, and the block/blocked-by/mute(expiry-aware)
//! /follow filter sets Requirement 9 exposes to timelines/notifications.
//!
//! ## Scope
//! This module owns exactly design.md's `RelationshipRepository` Service
//! Interface: [`upsert_follow`], [`delete_follow`], [`upsert_request`],
//! [`delete_request`], [`list_inbound_requests`], [`upsert_mute`],
//! [`delete_mute`], [`upsert_block`], [`delete_block`], [`load_states`],
//! [`blocked_targets`], [`blocked_by`], [`muted_targets`],
//! [`following_targets`], [`count_followers`], [`count_following`], plus
//! [`take_request`] (task 2.2's minimal, additive extension — see this
//! module's doc comment, "Task 2.2 additions") and [`is_blocked`] (task
//! 4.2's minimal, additive extension — see that function's own doc
//! comment). No `FollowApprovalPolicy`,
//! `ActivityBuilder`,
//! `RelationshipMapper`, business service, inbound Activity handler, or HTTP
//! surface lives here — those consume this module but are out of scope for
//! task 1.3 (`Boundary: RelationshipRepository`).
//!
//! ## Judgment calls (documented deviations from design.md's illustrative
//! sketch, resolved against this repo's actual established conventions —
//! not silently worked around)
//!
//! - **Free functions taking `pool: &PgPool`, not a `&self` struct method.**
//!   design.md's sketch writes `RelationshipRepository` as
//!   `pub async fn upsert_follow(&self, f: &Follow) -> Result<(), AppError>`
//!   etc. Every repository file that actually exists in this crate
//!   (`statuses/interaction_repository.rs`, `statuses/poll_repository.rs`,
//!   `statuses/tag_repository.rs`, `accounts/profile_repository.rs`,
//!   `accounts/remote_repository.rs`, `accounts/emoji_repository.rs`,
//!   `accounts/settings_repository.rs`) is, without a single exception, a
//!   set of free `pub async fn name(pool: &PgPool, ...)` functions — there
//!   is no `&self`-based repository struct/trait anywhere in this codebase.
//!   This module follows that exclusive, repo-wide convention instead of
//!   design.md's illustrative `&self` shape, keeping every method **name**
//!   and the rest of each parameter list/return type exactly as design.md
//!   specifies (per this task's own instruction to not invent different
//!   names/signatures) and only replacing `&self` with a leading
//!   `pool: &PgPool` parameter, matching e.g. `tag_repository.rs::upsert_tag`.
//! - **`RelationshipState` lives here, not in `model.rs`.** design.md's File
//!   Structure Plan lists `RelationshipState` alongside `Follow`/
//!   `FollowRequest`/`Mute`/`Block` in `model.rs`, but this task's own
//!   Constraints forbid modifying `model.rs` (already implemented, reviewed,
//!   task 1.2's closed boundary). `RelationshipState` is this repository's
//!   own output type (a not-yet-built consumer, `RelationshipMapper`, task
//!   2.4, needs it later) and has no reason to require touching a sibling
//!   task's already-approved file, so it is defined in this module instead.
//!   Its fields are kept minimal and directly traceable to the four tables:
//!   the (viewer, target) [`Follow`] row if established (backs `following`/
//!   `showing_reblogs`/`notifying`/`languages`), whether the reverse
//!   direction exists (`followed_by`), the (viewer, target) and (target,
//!   viewer) `blocks` rows (`blocking`/`blocked_by`), the (viewer, target)
//!   [`Mute`] row already expiry-filtered (backs `muting`/
//!   `muting_notifications`), and the two `follow_requests` directions
//!   (`requested`/`requested_by`). No `endorsed`/`note` field: this spec's
//!   tables have no such data source, and design.md's own `RelationshipMapper`
//!   note says those two Relationship flags default to `false`/empty
//!   regardless — carrying them here would just be dead weight this
//!   struct's only real consumer never reads from `RelationshipState`.
//! - **`id: Id` parameter added to every `upsert_*` function.**
//!   design.md's sketch gives `upsert_follow`/`upsert_request`/`upsert_mute`/
//!   `upsert_block` no `id` parameter, but `follows.id` / `follow_requests.id`
//!   / `mutes.id` / `blocks.id` are all `BIGINT PRIMARY KEY` with no
//!   database-side default (`migrations/0012_social_graph.sql`), and none of
//!   [`Follow`]/[`FollowRequest`]/[`Mute`]/[`Block`] (task 1.2, `model.rs`)
//!   carry an `id` field to source one from. This mirrors
//!   `interaction_repository.rs::add_bookmark`'s identical, already-reviewed
//!   precedent and doc-comment rationale for the exact same gap
//!   (`bookmarks.id`): the caller-minted primary key (via
//!   `RuntimeContext.ids`, steering's determinism rule — never generated
//!   inside the repository) must be threaded through explicitly. No `now`
//!   parameter is similarly needed: unlike `interaction_repository.rs`'s
//!   per-actor interaction rows (which have no dedicated domain struct),
//!   [`Follow`]/[`FollowRequest`]/[`Mute`]/[`Block`] already carry
//!   `created_at: OffsetDateTime` themselves (task 1.2), so the caller's
//!   injected `Clock` value flows in through the struct instead of a
//!   separate parameter.
//! - **Idempotent upsert = `ON CONFLICT ... DO UPDATE`, not `DO NOTHING`.**
//!   This task's own completion condition is "同一関係の二重 upsert が一意
//!   制約で冪等になり" (Requirement 1.6: a second follow request against an
//!   existing follow returns the *current* state idempotently) — but 1.6
//!   also requires (via `FollowOptions`, Requirement 1.5) that a follow's
//!   `reblogs`/`notify`/`languages` options actually take effect, including
//!   on a request that re-follows with changed options. A plain
//!   `DO NOTHING` would silently keep stale option values on a second
//!   upsert with different options, which is not "idempotent", it's "stuck".
//!   Every `upsert_*` function therefore uses `ON CONFLICT (<the relation's
//!   own UNIQUE constraint columns>) DO UPDATE SET <mutable fields> =
//!   EXCLUDED.<mutable fields>` — refreshing exactly the fields a repeat
//!   call legitimately wants to change (`show_reblogs`/`notify`/`languages`/
//!   `activity_id` for `follows`; `activity_id` for `follow_requests`/
//!   `blocks`; `notifications`/`expires_at` for `mutes`) while leaving `id`
//!   and `created_at` untouched (the relationship's *identity* and original
//!   creation time do not change on a duplicate/refresh upsert) — so the
//!   relationship's existence never duplicates (the `UNIQUE` constraint's
//!   job) while its content still stays current (this function's job).
//! - **No feature flag.** This is a new repository with no caller anywhere
//!   in the crate yet (`FollowService`/`MuteService`/`BlockService`/
//!   `InboundHandler`, tasks 2.x–5.x, do not exist yet) — there is no
//!   user-visible behavior to gate. No existing `*_repository.rs` file in
//!   this codebase is itself flag-gated (grepped: none reference a feature
//!   flag), so this module follows that same established repository-layer
//!   convention: plain RED→GREEN, no flag.
//!
//! ## Batched reverse lookup (Requirement 8.4)
//! [`load_states`] issues exactly 7 queries total regardless of
//! `targets.len()` (never one query per target): one each for
//! viewer→targets `follows` (for `following`/`showing_reblogs`/`notifying`/
//! `languages`), targets→viewer `follows` (`followed_by`), viewer→targets
//! `blocks` (`blocking`), targets→viewer `blocks` (`blocked_by`),
//! viewer→targets `mutes` already expiry-filtered (`muting`/
//! `muting_notifications`), viewer→targets outbound `follow_requests`
//! (`requested`), and targets→viewer inbound `follow_requests`
//! (`requested_by`). Each query matches the target group via the
//! `WHERE (kind_col, id_col) IN (SELECT * FROM UNNEST($k::text[], $k+1::bigint[]))`
//! idiom (parallel-array bind, matching `emoji_repository.rs::resolve_emojis`'s
//! established `= ANY($1)` single-column batch-bind precedent, generalized
//! to the `(kind, id)` pair every `AccountRef` needs).
//!
//! ## Expired-mute exclusion (Requirements 4.3, 9.3)
//! [`muted_targets`] and [`load_states`]'s mute half both filter
//! `expires_at IS NULL OR expires_at > $now` in SQL, using the caller's
//! injected `now: OffsetDateTime` parameter — never `OffsetDateTime::now_utc()`
//! called inside this module (steering's determinism rule).
//!
//! ## Task 2.2 additions (`Boundary: Transitions, RelationshipRepository` —
//! this task's own boundary explicitly includes `RelationshipRepository`,
//! not just `Transitions`)
//! Two small, additive extensions `transitions.rs` (task 2.2) needs, neither
//! changing an existing call site's behavior for its already-passing
//! callers:
//!
//! - **[`upsert_follow`] / [`upsert_request`] now report "was this call the
//!   one that actually inserted a new row?"** (`Result<bool, AppError>`
//!   instead of `Result<(), AppError>`). `transitions.rs::establish_follow`/
//!   `record_pending` must emit their `notifications::NotificationEvent`
//!   exactly once *per newly-established relationship*, never on a repeat/
//!   idempotent re-application (design.md's "`Transitions` ... 通知イベント
//!   emit" — "同一遷移の二重適用が...二重に生成しない"). Because both
//!   functions must use `ON CONFLICT ... DO UPDATE` rather than `DO NOTHING`
//!   (this module's own "Idempotent upsert" doc section above — a repeat
//!   call must still refresh options), `sqlx::query(...).execute(pool)`'s
//!   `rows_affected() > 0` cannot distinguish a fresh `INSERT` from a
//!   conflict-triggered `UPDATE` (both affect exactly one row). Both
//!   functions instead append `RETURNING (xmax = 0) AS inserted` — Postgres's
//!   standard idiom for "was this specific row version just created by this
//!   statement's `INSERT` branch, or did it already exist" (`xmax` is unset,
//!   i.e. `0`, only for a row a transaction has not yet marked as
//!   superseded) — and `fetch_one` that single boolean column instead of
//!   discarding `execute`'s row count. Mirrors
//!   `interaction_service.rs::favourite`'s identical established
//!   precedent/rationale for `interaction_repository::add_favourite`'s own
//!   `is_new: bool` return (that function gets away with a plain
//!   `rows_affected() > 0` only because it uses `DO NOTHING`, not `DO
//!   UPDATE` — the two are genuinely different situations, not an
//!   inconsistency). Existing callers that ignore the return value (every
//!   current call in `repository/tests.rs`) are unaffected: a discarded
//!   `bool` compiles exactly like a discarded `()`.
//! - **[`delete_follow`] / [`delete_request`] / [`upsert_block`] now take a
//!   generic `executor: E where E: sqlx::PgExecutor<'e>` instead of a
//!   concrete `pool: &PgPool`.** `transitions.rs::apply_block`/
//!   `mark_blocked_by` must remove both-direction follows, both-direction
//!   pending requests, *and* commit the block row as one atomic transaction
//!   (design.md, "`apply_block` は単一トランザクションで...解消してから確
//!   定"; Requirement 5.2) — a genuine cross-statement atomicity need this
//!   module's existing single-statement functions cannot serve while pinned
//!   to a concrete `&PgPool` (a `PgPool` reference cannot join an
//!   already-open `sqlx::Transaction`). Mirrors
//!   `status_repository.rs::fetch_raw`'s identical established
//!   `<'e, E: sqlx::PgExecutor<'e>>` idiom precedent — callers unaware of
//!   any transaction keep passing a bare `&PgPool` exactly as before (a
//!   `&PgPool` itself satisfies `PgExecutor<'_>`, so every existing
//!   `repository/tests.rs` call site is source-compatible unchanged); only
//!   `transitions.rs`'s new transaction-bound call sites pass `&mut *tx`
//!   instead. The other upsert/delete functions ([`upsert_follow`],
//!   [`upsert_request`], [`upsert_mute`], [`delete_mute`], [`delete_block`])
//!   are left as concrete `&PgPool` takers: no `transitions.rs` function
//!   needs them inside a shared transaction (each of `establish_follow`,
//!   `record_pending`, `promote_pending`, `drop_pending`, `clear_block`,
//!   `clear_blocked_by` performs exactly one relationship-table write, which
//!   is already atomic as a single statement without an explicit
//!   transaction).
//! - **[`take_request`] is new**: `transitions.rs::promote_pending` (Accept
//!   received, Requirement 2.5) needs the original outbound
//!   [`FollowRequest`]'s `activity_id` (the Follow Activity id the newly-
//!   established [`Follow`]'s eventual Undo(Follow) must reference) at the
//!   exact moment it consumes (deletes) that pending row — a separate
//!   "check if it exists" query followed by [`delete_request`] would leave a
//!   check-then-delete race window under concurrent access. A single
//!   `DELETE ... RETURNING` statement fetches and removes the row
//!   atomically, reusing [`row_to_request_pair`]'s existing row-to-domain-type
//!   mapping.

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::api::pagination::{Cursor, Page, PageParams};
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::social_graph::model::{Block, Follow, FollowRequest, FollowRequestDirection, Mute};

fn map_server_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

// -- AccountRef <-> (kind, id) mapping --------------------------------------
//
// Mirrors `migrations/0012_social_graph.sql`'s own doc comment: every
// account reference is a logical `(kind, id)` TEXT/BIGINT pair, matching
// `AccountRef::Local(Id)`/`Remote(Id)`'s shape (`src/domain/primitives.rs`).

fn account_kind(account: &AccountRef) -> &'static str {
    match account {
        AccountRef::Local(_) => "local",
        AccountRef::Remote(_) => "remote",
    }
}

fn account_id(account: &AccountRef) -> i64 {
    match account {
        AccountRef::Local(id) | AccountRef::Remote(id) => id.as_i64(),
    }
}

/// Reconstructs an [`AccountRef`] from a `(kind, id)` pair already read off
/// the wire. Panics on any value other than `'local'`/`'remote'` — this
/// crate's own migration/write path never persists anything else, so any
/// other value indicates genuine data corruption, mirroring
/// `status_repository.rs`'s `visibility_from_str` panic-on-corruption
/// convention for the same class of "database-owned enum" column.
fn account_ref_from(kind: &str, id: i64) -> AccountRef {
    match kind {
        "local" => AccountRef::Local(Id::from_i64(id)),
        "remote" => AccountRef::Remote(Id::from_i64(id)),
        other => panic!(
            "social_graph account kind column contained unexpected value {other:?}; expected \
             'local' or 'remote'"
        ),
    }
}

fn direction_as_str(direction: FollowRequestDirection) -> &'static str {
    match direction {
        FollowRequestDirection::Outbound => "outbound",
        FollowRequestDirection::Inbound => "inbound",
    }
}

fn direction_from_str(raw: &str) -> FollowRequestDirection {
    match raw {
        "outbound" => FollowRequestDirection::Outbound,
        "inbound" => FollowRequestDirection::Inbound,
        other => panic!(
            "follow_requests.direction contained unexpected value {other:?}; expected \
             'outbound' or 'inbound'"
        ),
    }
}

/// Builds `follows.languages`'/`mutes`-adjacent JSONB array-of-strings
/// representation from a `Vec<String>`. Mirrors
/// `settings_repository.rs::string_array_from_json`'s / `profile_repository.rs`
/// `fields_to_json`'s established hand-rolled JSONB convention (this crate
/// has no `#[derive(Serialize, Deserialize)]` on [`Follow::languages`] to
/// rely on instead — task 1.2's `model.rs` deliberately keeps these types
/// serde-free).
fn languages_to_json(languages: &[String]) -> serde_json::Value {
    serde_json::Value::Array(
        languages
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    )
}

/// Parses `follows.languages`' JSONB array-of-strings representation back
/// into a `Vec<String>`. Panics on a malformed array — mirrors
/// `settings_repository.rs::string_array_from_json`'s identical precedent: a
/// column this repository itself always writes via [`languages_to_json`]
/// should never be malformed under normal operation.
fn languages_from_json(value: &serde_json::Value) -> Vec<String> {
    let items = value
        .as_array()
        .unwrap_or_else(|| panic!("follows.languages must be a JSON array, got {value:?}"));

    items
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| {
                    panic!("follows.languages array item must be a string, got {item:?}")
                })
                .to_string()
        })
        .collect()
}

// -- follows ------------------------------------------------------------

/// Idempotently records `f` (Requirement 1.6): a first call for
/// `(f.follower, f.followee)` inserts a new row under caller-minted `id`; a
/// repeat call refreshes `show_reblogs`/`notify`/`languages`/`activity_id`
/// on the existing row (this module's doc comment, "Idempotent upsert")
/// rather than creating a duplicate or silently ignoring changed options.
/// Returns `Ok(true)` when this call's `INSERT` branch actually fired (a
/// genuinely new follow), `Ok(false)` when it instead hit the `DO UPDATE`
/// conflict branch (an already-established follow, options refreshed) —
/// see this module's doc comment ("Task 2.2 additions") for why a plain
/// `rows_affected()` count cannot make this distinction under `DO UPDATE`.
/// `transitions.rs::establish_follow` (task 2.2) uses this to decide whether
/// to emit a `follow` notification.
pub async fn upsert_follow(pool: &PgPool, id: Id, f: &Follow) -> Result<bool, AppError> {
    let (inserted,): (bool,) = sqlx::query_as(
        "INSERT INTO follows \
             (id, follower_id, follower_kind, followee_id, followee_kind, show_reblogs, notify, \
              languages, activity_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (follower_kind, follower_id, followee_kind, followee_id) DO UPDATE SET \
             show_reblogs = EXCLUDED.show_reblogs, \
             notify = EXCLUDED.notify, \
             languages = EXCLUDED.languages, \
             activity_id = EXCLUDED.activity_id \
         RETURNING (xmax = 0) AS inserted",
    )
    .bind(id.as_i64())
    .bind(account_id(&f.follower))
    .bind(account_kind(&f.follower))
    .bind(account_id(&f.followee))
    .bind(account_kind(&f.followee))
    .bind(f.reblogs)
    .bind(f.notify)
    .bind(languages_to_json(&f.languages))
    .bind(&f.activity_id)
    .bind(f.created_at)
    .fetch_one(pool)
    .await
    .map_err(map_server_error)?;

    Ok(inserted)
}

/// Deletes the `follower` -> `followee` follow, if any. Returns `Ok(true)`
/// when a row was actually deleted, `Ok(false)` when no such follow existed
/// — an idempotent no-op success, mirroring this crate's established
/// `remove_favourite`-style convention (absence is not an error at this
/// layer). Generic over `executor` (this module's doc comment, "Task 2.2
/// additions") so `transitions.rs::apply_block`/`mark_blocked_by` can drive
/// it against an open `sqlx::Transaction` (`&mut *tx`), while every
/// pre-existing caller keeps passing a bare `&PgPool` unchanged.
pub async fn delete_follow<'e, E>(
    executor: E,
    follower: &AccountRef,
    followee: &AccountRef,
) -> Result<bool, AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let result = sqlx::query(
        "DELETE FROM follows WHERE follower_kind = $1 AND follower_id = $2 \
             AND followee_kind = $3 AND followee_id = $4",
    )
    .bind(account_kind(follower))
    .bind(account_id(follower))
    .bind(account_kind(followee))
    .bind(account_id(followee))
    .execute(executor)
    .await
    .map_err(map_server_error)?;

    Ok(result.rows_affected() > 0)
}

// -- follow_requests ------------------------------------------------------

/// Idempotently records pending request `r` (Requirements 1.6, 2.1): a first
/// call for `(r.requester, r.target, r.direction)` inserts a new row under
/// caller-minted `id`; a repeat call refreshes `activity_id` on the existing
/// row rather than duplicating it. `direction` is part of the conflict
/// target, so an outbound and an inbound row for the same
/// `(requester, target)` pair coexist without colliding
/// (`migrations/0012_social_graph.sql`'s own doc comment).
/// Returns `Ok(true)` when this call's `INSERT` branch actually fired (a
/// genuinely new pending request), `Ok(false)` when it instead hit the
/// `DO UPDATE` conflict branch (already pending, `activity_id` refreshed) —
/// same `xmax`-based technique as [`upsert_follow`] (this module's doc
/// comment, "Task 2.2 additions"). `transitions.rs::record_pending` (task
/// 2.2) uses this to decide whether to emit a `follow_request` notification.
pub async fn upsert_request(pool: &PgPool, id: Id, r: &FollowRequest) -> Result<bool, AppError> {
    let (inserted,): (bool,) = sqlx::query_as(
        "INSERT INTO follow_requests \
             (id, requester_id, requester_kind, target_id, target_kind, direction, activity_id, \
              created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (requester_kind, requester_id, target_kind, target_id, direction) \
             DO UPDATE SET activity_id = EXCLUDED.activity_id \
         RETURNING (xmax = 0) AS inserted",
    )
    .bind(id.as_i64())
    .bind(account_id(&r.requester))
    .bind(account_kind(&r.requester))
    .bind(account_id(&r.target))
    .bind(account_kind(&r.target))
    .bind(direction_as_str(r.direction))
    .bind(&r.activity_id)
    .bind(r.created_at)
    .fetch_one(pool)
    .await
    .map_err(map_server_error)?;

    Ok(inserted)
}

/// Deletes the pending `requester` -> `target` request in direction `dir`,
/// if any. Returns `Ok(true)` when a row was actually deleted, `Ok(false)`
/// when no such pending request existed — idempotent no-op success, same
/// convention as [`delete_follow`]. Generic over `executor` for the same
/// reason as [`delete_follow`] (this module's doc comment, "Task 2.2
/// additions") — `transitions.rs::apply_block`/`mark_blocked_by` drive this
/// against an open transaction.
pub async fn delete_request<'e, E>(
    executor: E,
    requester: &AccountRef,
    target: &AccountRef,
    dir: FollowRequestDirection,
) -> Result<bool, AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let result = sqlx::query(
        "DELETE FROM follow_requests WHERE requester_kind = $1 AND requester_id = $2 \
             AND target_kind = $3 AND target_id = $4 AND direction = $5",
    )
    .bind(account_kind(requester))
    .bind(account_id(requester))
    .bind(account_kind(target))
    .bind(account_id(target))
    .bind(direction_as_str(dir))
    .execute(executor)
    .await
    .map_err(map_server_error)?;

    Ok(result.rows_affected() > 0)
}

/// Atomically removes and returns the pending `requester` -> `target`
/// request in direction `dir`, if any — a combined `DELETE ... RETURNING`
/// rather than a separate existence check followed by [`delete_request`]
/// (this module's doc comment, "Task 2.2 additions": avoids a
/// check-then-delete race). `transitions.rs::promote_pending` (Accept
/// received, Requirement 2.5) uses this to recover the original outbound
/// request's `activity_id` (the Follow Activity id the newly-established
/// [`Follow`]'s eventual Undo(Follow) must reference) in the same step that
/// consumes the pending row.
pub async fn take_request(
    pool: &PgPool,
    requester: &AccountRef,
    target: &AccountRef,
    dir: FollowRequestDirection,
) -> Result<Option<FollowRequest>, AppError> {
    let row: Option<FollowRequestRow> = sqlx::query_as(
        "DELETE FROM follow_requests WHERE requester_kind = $1 AND requester_id = $2 \
             AND target_kind = $3 AND target_id = $4 AND direction = $5 \
         RETURNING id, requester_id, requester_kind, target_id, target_kind, direction, \
             activity_id, created_at",
    )
    .bind(account_kind(requester))
    .bind(account_id(requester))
    .bind(account_kind(target))
    .bind(account_id(target))
    .bind(direction_as_str(dir))
    .fetch_optional(pool)
    .await
    .map_err(map_server_error)?;

    Ok(row.map(|r| row_to_request_pair(r).1))
}

/// [`Cursor`] over `follow_requests.id` — the pending-request row's own
/// creation-order primary key. Mirrors
/// `interaction_repository.rs::BookmarkCursor`'s identical pattern
/// (a dedicated per-list cursor over the row's own id, reusing
/// `crate::api::pagination`'s toolkit rather than inventing a parallel one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FollowRequestCursor(pub u64);

impl Cursor for FollowRequestCursor {
    fn encode(&self) -> String {
        self.0.to_string()
    }

    fn decode(raw: &str) -> Result<Self, AppError> {
        raw.parse::<u64>().map(FollowRequestCursor).map_err(|_| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid cursor value: '{raw}'"),
            )
        })
    }
}

#[derive(sqlx::FromRow)]
struct FollowRequestRow {
    id: i64,
    requester_id: i64,
    requester_kind: String,
    target_id: i64,
    target_kind: String,
    direction: String,
    activity_id: String,
    created_at: OffsetDateTime,
}

fn row_to_request_pair(row: FollowRequestRow) -> (FollowRequestCursor, FollowRequest) {
    let requester = account_ref_from(&row.requester_kind, row.requester_id);
    let target = account_ref_from(&row.target_kind, row.target_id);
    let direction = direction_from_str(&row.direction);
    // `follow_requests.id` is BIGINT but always non-negative (caller-minted
    // via the same monotonically-increasing `IdGenerator` every entity in
    // this crate uses) — same convention
    // `interaction_repository.rs::bookmarked_row_to_pair`'s identical cast
    // comment documents for `bookmarks.id`.
    (
        FollowRequestCursor(row.id as u64),
        FollowRequest {
            requester,
            target,
            direction,
            activity_id: row.activity_id,
            created_at: row.created_at,
        },
    )
}

/// Returns `target`'s pending **inbound** follow requests (Requirement
/// 2.2), newest-first, paginated by [`FollowRequestCursor`].
///
/// `page` is taken by reference, matching design.md's own
/// `RelationshipRepository` Service Interface signature exactly
/// (`page: &PageParams`) — [`PageParams::parse`] only needs `&self`, so no
/// ownership transfer is required.
pub async fn list_inbound_requests(
    pool: &PgPool,
    target: &AccountRef,
    page: &PageParams,
) -> Result<Page<FollowRequest>, AppError> {
    let parsed = page.parse::<FollowRequestCursor>()?;

    let rows: Vec<FollowRequestRow> = sqlx::query_as(
        "SELECT id, requester_id, requester_kind, target_id, target_kind, direction, \
             activity_id, created_at \
         FROM follow_requests \
         WHERE target_kind = $1 AND target_id = $2 AND direction = 'inbound' \
         ORDER BY id DESC",
    )
    .bind(account_kind(target))
    .bind(account_id(target))
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    let pool_items: Vec<(FollowRequestCursor, FollowRequest)> =
        rows.into_iter().map(row_to_request_pair).collect();

    let paged = crate::api::pagination::paginate(&pool_items, |item| item.0, &parsed);

    Ok(Page {
        items: paged.items.into_iter().map(|(_, req)| req).collect(),
        prev_cursor: paged.prev_cursor,
        next_cursor: paged.next_cursor,
    })
}

// -- mutes ------------------------------------------------------------

/// Idempotently records `m` (Requirement 1.6's idempotency principle applied
/// to mutes): a first call for `(m.muter, m.muted)` inserts a new row under
/// caller-minted `id`; a repeat call refreshes `notifications`/`expires_at`
/// on the existing row rather than duplicating it or ignoring a changed
/// duration/notify option.
pub async fn upsert_mute(pool: &PgPool, id: Id, m: &Mute) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO mutes \
             (id, muter_id, muter_kind, muted_id, muted_kind, notifications, expires_at, \
              created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (muter_kind, muter_id, muted_kind, muted_id) DO UPDATE SET \
             notifications = EXCLUDED.notifications, \
             expires_at = EXCLUDED.expires_at",
    )
    .bind(id.as_i64())
    .bind(account_id(&m.muter))
    .bind(account_kind(&m.muter))
    .bind(account_id(&m.muted))
    .bind(account_kind(&m.muted))
    .bind(m.notifications)
    .bind(m.expires_at)
    .bind(m.created_at)
    .execute(pool)
    .await
    .map_err(map_server_error)?;

    Ok(())
}

/// Deletes the `muter` -> `muted` mute, if any. Returns `Ok(true)` when a
/// row was actually deleted, `Ok(false)` when no such mute existed —
/// idempotent no-op success.
pub async fn delete_mute(
    pool: &PgPool,
    muter: &AccountRef,
    muted: &AccountRef,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        "DELETE FROM mutes WHERE muter_kind = $1 AND muter_id = $2 \
             AND muted_kind = $3 AND muted_id = $4",
    )
    .bind(account_kind(muter))
    .bind(account_id(muter))
    .bind(account_kind(muted))
    .bind(account_id(muted))
    .execute(pool)
    .await
    .map_err(map_server_error)?;

    Ok(result.rows_affected() > 0)
}

// -- blocks -----------------------------------------------------------

/// Idempotently records `b` (Requirement 1.6's idempotency principle applied
/// to blocks): a first call for `(b.blocker, b.blocked)` inserts a new row
/// under caller-minted `id`; a repeat call refreshes `activity_id` on the
/// existing row rather than duplicating it.
/// Generic over `executor` (this module's doc comment, "Task 2.2
/// additions") — `transitions.rs::apply_block`/`mark_blocked_by` commit this
/// as the final step of the single transaction that also clears
/// both-direction follows/pending requests (Requirement 5.2).
pub async fn upsert_block<'e, E>(executor: E, id: Id, b: &Block) -> Result<(), AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query(
        "INSERT INTO blocks (id, blocker_id, blocker_kind, blocked_id, blocked_kind, \
             activity_id, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (blocker_kind, blocker_id, blocked_kind, blocked_id) \
             DO UPDATE SET activity_id = EXCLUDED.activity_id",
    )
    .bind(id.as_i64())
    .bind(account_id(&b.blocker))
    .bind(account_kind(&b.blocker))
    .bind(account_id(&b.blocked))
    .bind(account_kind(&b.blocked))
    .bind(&b.activity_id)
    .bind(b.created_at)
    .execute(executor)
    .await
    .map_err(map_server_error)?;

    Ok(())
}

/// Deletes the `blocker` -> `blocked` block, if any. Returns `Ok(true)` when
/// a row was actually deleted, `Ok(false)` when no such block existed —
/// idempotent no-op success. Still used by `transitions.rs::clear_blocked_by`
/// (Requirement 7.6, receipt of an Undo(Block) naming this instance's own
/// local actor as the original target), which never needs the removed row's
/// `activity_id` back — this instance never sent its own Undo(Block) for a
/// block *against* it, so there is nothing for it to reference. See
/// [`take_block`] for the sibling case that does need it.
pub async fn delete_block(
    pool: &PgPool,
    blocker: &AccountRef,
    blocked: &AccountRef,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        "DELETE FROM blocks WHERE blocker_kind = $1 AND blocker_id = $2 \
             AND blocked_kind = $3 AND blocked_id = $4",
    )
    .bind(account_kind(blocker))
    .bind(account_id(blocker))
    .bind(account_kind(blocked))
    .bind(account_id(blocked))
    .execute(pool)
    .await
    .map_err(map_server_error)?;

    Ok(result.rows_affected() > 0)
}

/// Atomically removes and returns the `blocker` -> `blocked` block row, if
/// any — a combined `DELETE ... RETURNING`, mirroring [`take_request`]'s
/// identical pattern (this module's doc comment, "Task 2.2 additions": avoids
/// a check-then-delete race). `transitions.rs::clear_block` (task 3.4,
/// `BlockService::unblock`, Requirement 5.4) uses this to recover the removed
/// block's own outbound `activity_id` — the original Block Activity id an
/// eventual Undo(Block) must reference — in the same atomic step that
/// consumes the row. This is the exact same "the state read doesn't carry the
/// referenced Activity id, so the consuming transition function must
/// surface it instead" situation task 3.2's own `promote_pending`/
/// `drop_pending` widening (this module's doc comment, "Task 2.2 additions")
/// already solved for pending follow requests, recurring here for `Block`:
/// unlike [`RelationshipState::follow`] (which embeds the full [`Follow`]
/// row, `activity_id` included), [`RelationshipState::blocking`] is a bare
/// `bool` with nowhere to carry an `activity_id` — so `BlockService::unblock`
/// cannot recover it from a preceding `load_states` read the way
/// `FollowService::unfollow` recovers `state.follow`'s `activity_id`.
pub async fn take_block(
    pool: &PgPool,
    blocker: &AccountRef,
    blocked: &AccountRef,
) -> Result<Option<Block>, AppError> {
    let row: Option<BlockRow> = sqlx::query_as(
        "DELETE FROM blocks WHERE blocker_kind = $1 AND blocker_id = $2 \
             AND blocked_kind = $3 AND blocked_id = $4 \
         RETURNING blocker_id, blocker_kind, blocked_id, blocked_kind, activity_id, created_at",
    )
    .bind(account_kind(blocker))
    .bind(account_id(blocker))
    .bind(account_kind(blocked))
    .bind(account_id(blocked))
    .fetch_optional(pool)
    .await
    .map_err(map_server_error)?;

    Ok(row.map(row_to_block))
}

/// Reports whether a `blocks` row exists for `blocker -> blocked`
/// (task 4.2, `Boundary: BlockPolicyImpl`, Requirements 6.1, 6.2, 6.3, 6.4):
/// the narrow existence check `providers.rs::BlockPolicyImpl::is_blocked`
/// needs, distinct from [`blocked_targets`]/[`blocked_by`] (which each
/// return `viewer`'s *entire* block set — the wrong shape and an unneeded
/// full-table-for-viewer scan for a single-pair yes/no judgment made on
/// every signed inbound request). Live DB read each call, no caching: a
/// deleted block row is reflected on the very next call (Requirement 6.4),
/// exactly like every other query in this module. Mirrors
/// `interaction_repository.rs::exists_favourite`'s identical
/// `SELECT EXISTS(...)` idiom.
pub async fn is_blocked(
    pool: &PgPool,
    blocker: &AccountRef,
    blocked: &AccountRef,
) -> Result<bool, AppError> {
    let (exists,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM blocks \
             WHERE blocker_kind = $1 AND blocker_id = $2 \
                 AND blocked_kind = $3 AND blocked_id = $4)",
    )
    .bind(account_kind(blocker))
    .bind(account_id(blocker))
    .bind(account_kind(blocked))
    .bind(account_id(blocked))
    .fetch_one(pool)
    .await
    .map_err(map_server_error)?;

    Ok(exists)
}

#[derive(sqlx::FromRow)]
struct BlockRow {
    blocker_id: i64,
    blocker_kind: String,
    blocked_id: i64,
    blocked_kind: String,
    activity_id: String,
    created_at: OffsetDateTime,
}

fn row_to_block(row: BlockRow) -> Block {
    Block {
        blocker: account_ref_from(&row.blocker_kind, row.blocker_id),
        blocked: account_ref_from(&row.blocked_kind, row.blocked_id),
        activity_id: row.activity_id,
        created_at: row.created_at,
    }
}

// -- RelationshipState / load_states (Requirement 8.4) ---------------------

/// One (viewer, target) pair's relationship state, derived from `follows` /
/// `follow_requests` / `mutes` / `blocks` row presence — this repository's
/// own output type feeding the not-yet-built `RelationshipMapper` (task
/// 2.4). See this module's doc comment ("`RelationshipState` lives here,
/// not in `model.rs`") for why it is defined in this file rather than
/// `model.rs`, and for why its fields stop exactly at what the four tables
/// can support (no `endorsed`/`note`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipState {
    /// The target account this state is about.
    pub target: AccountRef,
    /// The viewer's own established follow of `target`, if any — backs
    /// `following` (`Some`) and, from its fields, `showing_reblogs`
    /// (`reblogs`) / `notifying` (`notify`) / `languages`.
    pub follow: Option<Follow>,
    /// Whether `target` follows the viewer back.
    pub followed_by: bool,
    /// Whether the viewer has blocked `target`.
    pub blocking: bool,
    /// Whether `target` has blocked the viewer.
    pub blocked_by: bool,
    /// The viewer's own mute of `target`, already filtered for expiry
    /// against the `now` passed to [`load_states`] (Requirements 4.3, 9.3)
    /// — `None` when unmuted *or* when the mute has expired. Backs `muting`
    /// (`Some`) and `muting_notifications` (its `notifications` field).
    pub mute: Option<Mute>,
    /// Whether the viewer has an outbound pending follow request to
    /// `target` awaiting `target`'s authorize/reject.
    pub requested: bool,
    /// Whether `target` has an inbound pending follow request to the
    /// viewer, awaiting the viewer's authorize/reject.
    pub requested_by: bool,
}

#[derive(sqlx::FromRow)]
struct FollowStateRow {
    followee_kind: String,
    followee_id: i64,
    show_reblogs: bool,
    notify: bool,
    languages: serde_json::Value,
    activity_id: String,
    created_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct MuteStateRow {
    muted_kind: String,
    muted_id: i64,
    notifications: bool,
    expires_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

/// A bare `(kind, id)` pair as read off the wire, reused for every batched
/// query in [`load_states`] that only needs to know "does a row exist for
/// this target" (the SQL aliases each query's own column pair to
/// `kind`/`id` so this single row type can serve all of them).
#[derive(sqlx::FromRow)]
struct AccountKeyRow {
    kind: String,
    id: i64,
}

/// Batched reverse lookup for `viewer`'s relationship to every account in
/// `targets` (Requirement 8.4) — exactly 7 queries total regardless of
/// `targets.len()`, never one query per target. See this module's doc
/// comment ("Batched reverse lookup") for the query breakdown. `now` drives
/// the mute-expiry filter (Requirements 4.3, 9.3) via the injected `Clock`
/// value, never `OffsetDateTime::now_utc()`.
///
/// Returns one [`RelationshipState`] per entry of `targets`, in the same
/// order, including a default (all-`false`/`None`) state for any target
/// with no relationship rows at all.
pub async fn load_states(
    pool: &PgPool,
    viewer: &AccountRef,
    targets: &[AccountRef],
    now: OffsetDateTime,
) -> Result<Vec<RelationshipState>, AppError> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let target_kinds: Vec<String> = targets
        .iter()
        .map(|t| account_kind(t).to_string())
        .collect();
    let target_ids: Vec<i64> = targets.iter().map(account_id).collect();
    let viewer_kind = account_kind(viewer);
    let viewer_id = account_id(viewer);

    // 1. viewer -> targets follows: following / showing_reblogs / notifying / languages.
    let follow_rows: Vec<FollowStateRow> = sqlx::query_as(
        "SELECT followee_kind, followee_id, show_reblogs, notify, languages, activity_id, \
             created_at \
         FROM follows \
         WHERE follower_kind = $1 AND follower_id = $2 \
             AND (followee_kind, followee_id) IN (SELECT * FROM UNNEST($3::text[], $4::bigint[]))",
    )
    .bind(viewer_kind)
    .bind(viewer_id)
    .bind(&target_kinds)
    .bind(&target_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    // 2. targets -> viewer follows: followed_by.
    let followed_by_rows: Vec<AccountKeyRow> = sqlx::query_as(
        "SELECT follower_kind AS kind, follower_id AS id \
         FROM follows \
         WHERE followee_kind = $1 AND followee_id = $2 \
             AND (follower_kind, follower_id) IN (SELECT * FROM UNNEST($3::text[], $4::bigint[]))",
    )
    .bind(viewer_kind)
    .bind(viewer_id)
    .bind(&target_kinds)
    .bind(&target_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    // 3. viewer -> targets blocks: blocking.
    let blocking_rows: Vec<AccountKeyRow> = sqlx::query_as(
        "SELECT blocked_kind AS kind, blocked_id AS id \
         FROM blocks \
         WHERE blocker_kind = $1 AND blocker_id = $2 \
             AND (blocked_kind, blocked_id) IN (SELECT * FROM UNNEST($3::text[], $4::bigint[]))",
    )
    .bind(viewer_kind)
    .bind(viewer_id)
    .bind(&target_kinds)
    .bind(&target_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    // 4. targets -> viewer blocks: blocked_by.
    let blocked_by_rows: Vec<AccountKeyRow> = sqlx::query_as(
        "SELECT blocker_kind AS kind, blocker_id AS id \
         FROM blocks \
         WHERE blocked_kind = $1 AND blocked_id = $2 \
             AND (blocker_kind, blocker_id) IN (SELECT * FROM UNNEST($3::text[], $4::bigint[]))",
    )
    .bind(viewer_kind)
    .bind(viewer_id)
    .bind(&target_kinds)
    .bind(&target_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    // 5. viewer -> targets mutes, expiry-filtered: muting / muting_notifications.
    let mute_rows: Vec<MuteStateRow> = sqlx::query_as(
        "SELECT muted_kind, muted_id, notifications, expires_at, created_at \
         FROM mutes \
         WHERE muter_kind = $1 AND muter_id = $2 AND (expires_at IS NULL OR expires_at > $3) \
             AND (muted_kind, muted_id) IN (SELECT * FROM UNNEST($4::text[], $5::bigint[]))",
    )
    .bind(viewer_kind)
    .bind(viewer_id)
    .bind(now)
    .bind(&target_kinds)
    .bind(&target_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    // 6. viewer -> targets outbound follow_requests: requested.
    let requested_rows: Vec<AccountKeyRow> = sqlx::query_as(
        "SELECT target_kind AS kind, target_id AS id \
         FROM follow_requests \
         WHERE requester_kind = $1 AND requester_id = $2 AND direction = 'outbound' \
             AND (target_kind, target_id) IN (SELECT * FROM UNNEST($3::text[], $4::bigint[]))",
    )
    .bind(viewer_kind)
    .bind(viewer_id)
    .bind(&target_kinds)
    .bind(&target_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    // 7. targets -> viewer inbound follow_requests: requested_by.
    let requested_by_rows: Vec<AccountKeyRow> = sqlx::query_as(
        "SELECT requester_kind AS kind, requester_id AS id \
         FROM follow_requests \
         WHERE target_kind = $1 AND target_id = $2 AND direction = 'inbound' \
             AND (requester_kind, requester_id) IN \
                 (SELECT * FROM UNNEST($3::text[], $4::bigint[]))",
    )
    .bind(viewer_kind)
    .bind(viewer_id)
    .bind(&target_kinds)
    .bind(&target_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    let mut follow_map: HashMap<(String, i64), Follow> = HashMap::new();
    for row in follow_rows {
        let followee = account_ref_from(&row.followee_kind, row.followee_id);
        let key = (row.followee_kind, row.followee_id);
        follow_map.insert(
            key,
            Follow {
                follower: *viewer,
                followee,
                reblogs: row.show_reblogs,
                notify: row.notify,
                languages: languages_from_json(&row.languages),
                activity_id: row.activity_id,
                created_at: row.created_at,
            },
        );
    }

    let mut mute_map: HashMap<(String, i64), Mute> = HashMap::new();
    for row in mute_rows {
        let muted = account_ref_from(&row.muted_kind, row.muted_id);
        let key = (row.muted_kind, row.muted_id);
        mute_map.insert(
            key,
            Mute {
                muter: *viewer,
                muted,
                notifications: row.notifications,
                expires_at: row.expires_at,
                created_at: row.created_at,
            },
        );
    }

    let followed_by_set: HashSet<(String, i64)> = followed_by_rows
        .into_iter()
        .map(|r| (r.kind, r.id))
        .collect();
    let blocking_set: HashSet<(String, i64)> =
        blocking_rows.into_iter().map(|r| (r.kind, r.id)).collect();
    let blocked_by_set: HashSet<(String, i64)> = blocked_by_rows
        .into_iter()
        .map(|r| (r.kind, r.id))
        .collect();
    let requested_set: HashSet<(String, i64)> =
        requested_rows.into_iter().map(|r| (r.kind, r.id)).collect();
    let requested_by_set: HashSet<(String, i64)> = requested_by_rows
        .into_iter()
        .map(|r| (r.kind, r.id))
        .collect();

    let states = targets
        .iter()
        .map(|target| {
            let key = (account_kind(target).to_string(), account_id(target));
            RelationshipState {
                target: *target,
                follow: follow_map.remove(&key),
                followed_by: followed_by_set.contains(&key),
                blocking: blocking_set.contains(&key),
                blocked_by: blocked_by_set.contains(&key),
                mute: mute_map.remove(&key),
                requested: requested_set.contains(&key),
                requested_by: requested_by_set.contains(&key),
            }
        })
        .collect();

    Ok(states)
}

// -- filter sets (Requirement 9) ---------------------------------------

/// Accounts `viewer` has blocked (Requirement 9.1).
pub async fn blocked_targets(
    pool: &PgPool,
    viewer: &AccountRef,
) -> Result<Vec<AccountRef>, AppError> {
    let rows: Vec<AccountKeyRow> = sqlx::query_as(
        "SELECT blocked_kind AS kind, blocked_id AS id FROM blocks \
         WHERE blocker_kind = $1 AND blocker_id = $2",
    )
    .bind(account_kind(viewer))
    .bind(account_id(viewer))
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows
        .into_iter()
        .map(|r| account_ref_from(&r.kind, r.id))
        .collect())
}

/// Accounts that have blocked `viewer` (Requirement 9.1).
pub async fn blocked_by(pool: &PgPool, viewer: &AccountRef) -> Result<Vec<AccountRef>, AppError> {
    let rows: Vec<AccountKeyRow> = sqlx::query_as(
        "SELECT blocker_kind AS kind, blocker_id AS id FROM blocks \
         WHERE blocked_kind = $1 AND blocked_id = $2",
    )
    .bind(account_kind(viewer))
    .bind(account_id(viewer))
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows
        .into_iter()
        .map(|r| account_ref_from(&r.kind, r.id))
        .collect())
}

/// Accounts `viewer` currently mutes (Requirements 9.1, 9.3): rows whose
/// `expires_at` has passed relative to the caller's injected `now` are
/// excluded (never included then filtered client-side). When
/// `notifications_only` is `true`, only mutes with `notifications = true`
/// are returned (the notification-mute subset a notifications-filter caller
/// needs).
pub async fn muted_targets(
    pool: &PgPool,
    viewer: &AccountRef,
    now: OffsetDateTime,
    notifications_only: bool,
) -> Result<Vec<AccountRef>, AppError> {
    let rows: Vec<AccountKeyRow> = sqlx::query_as(
        "SELECT muted_kind AS kind, muted_id AS id FROM mutes \
         WHERE muter_kind = $1 AND muter_id = $2 \
             AND (expires_at IS NULL OR expires_at > $3) \
             AND ($4 = FALSE OR notifications = TRUE)",
    )
    .bind(account_kind(viewer))
    .bind(account_id(viewer))
    .bind(now)
    .bind(notifications_only)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows
        .into_iter()
        .map(|r| account_ref_from(&r.kind, r.id))
        .collect())
}

/// Accounts `viewer` currently follows (Requirement 9.2), for home timeline
/// construction.
pub async fn following_targets(
    pool: &PgPool,
    viewer: &AccountRef,
) -> Result<Vec<AccountRef>, AppError> {
    let rows: Vec<AccountKeyRow> = sqlx::query_as(
        "SELECT followee_kind AS kind, followee_id AS id FROM follows \
         WHERE follower_kind = $1 AND follower_id = $2",
    )
    .bind(account_kind(viewer))
    .bind(account_id(viewer))
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows
        .into_iter()
        .map(|r| account_ref_from(&r.kind, r.id))
        .collect())
}

// -- counts (Boundary Commitments: AccountCountsProvider followers/following) -

/// Number of accounts currently following `target` (established `follows`
/// rows with `target` as `followee`) — the `AccountCountsProviderImpl`
/// supply source for `followers_count`.
pub async fn count_followers(pool: &PgPool, target: &AccountRef) -> Result<i64, AppError> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM follows WHERE followee_kind = $1 AND followee_id = $2",
    )
    .bind(account_kind(target))
    .bind(account_id(target))
    .fetch_one(pool)
    .await
    .map_err(map_server_error)?;

    Ok(count)
}

/// Number of accounts `target` currently follows (established `follows`
/// rows with `target` as `follower`) — the `AccountCountsProviderImpl`
/// supply source for `following_count`.
pub async fn count_following(pool: &PgPool, target: &AccountRef) -> Result<i64, AppError> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM follows WHERE follower_kind = $1 AND follower_id = $2",
    )
    .bind(account_kind(target))
    .bind(account_id(target))
    .fetch_one(pool)
    .await
    .map_err(map_server_error)?;

    Ok(count)
}
