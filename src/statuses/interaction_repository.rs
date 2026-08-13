//! `InteractionRepository` (design.md "Data / データ層" ->
//! `InteractionRepository`; Requirements 9.1, 9.3, 9.4, 10.1, 10.3, 10.4,
//! 11.1, 11.2, 11.3, 12.1, 12.2; task 2.2, `Boundary: InteractionRepository`):
//! per-actor operation state persistence — favourite/bookmark/pin record,
//! revoke, existence check, plus the bookmark list's own creation-order
//! cursor — against `favourites` / `bookmarks` / `pins`
//! (`migrations/0007_statuses.sql`, already applied, unmodified by this
//! task).
//!
//! ## Reblog scope (this task's own note, verified against the schema)
//! `migrations/0007_statuses.sql`'s own header comment is explicit: "the
//! four per-actor interaction tables (favourite/bookmark/pin — reblog is
//! represented as its own dedicated `statuses` row, not a separate
//! interaction table...)". There is no `reblogs` table in the migration —
//! a boost is persisted purely as a `statuses` row with `reblog_of_id` set,
//! which is `StatusRepository::insert_status`/`delete_status`'s job
//! (task 2.1, already implemented, not touched here). design.md's own
//! `InteractionRepository` Service Interface (design.md lines 417-425)
//! confirms this split: it lists no `add_reblog`/`remove_reblog`, only
//! [`find_reblog`] — a *read-only* existence check ("同一アクターが既に
//! ブースト済みの投稿を再びブースト要求したとき...重複したブーストを作成
//! しない", Requirement 9.3) that the future `InteractionService` calls
//! before delegating the actual record/revoke to `StatusRepository`. This
//! module therefore owns exactly that one reblog-related function and
//! nothing else for reblog.
//!
//! ## Scope
//! This module owns exactly: [`add_favourite`] / [`remove_favourite`] /
//! [`exists_favourite`] (Requirements 9.3 via reuse for dup-prevention
//! symmetry is favourite's own 10.1/10.3/10.4), [`add_bookmark`] /
//! [`remove_bookmark`] / [`exists_bookmark`] / [`list_bookmarks`]
//! (Requirements 11.1, 11.2, 11.3), [`set_pin`] / [`exists_pin`]
//! (Requirements 12.1, 12.2), and [`find_reblog`] (Requirement 9.3's
//! read-only half, see above), plus the batched forms of those four existence
//! checks — [`favourited_status_ids`] / [`bookmarked_status_ids`] /
//! [`pinned_status_ids`] / [`reblogged_status_ids`] (added later, so a list
//! endpoint's per-viewer interaction lookups stop scaling with the number of
//! statuses it renders; the singular checks stay, they have other callers).
//! No `StatusRepository`/`TagRepository` functionality, no
//! `PollRepository`/`IdempotencyStore` (task 2.3), no visibility/addressing
//! policy, no Activity generation, no scope/ownership enforcement
//! (`write:favourites`/`write:bookmarks`/`12.3`'s ownership check/`12.4`'s
//! direct-visibility rejection — those are `InteractionService`
//! /`StatusEndpoints` concerns, task 5.x/6.x, operating *above* this
//! repository), and no serialization live here.
//!
//! ## Duplicate handling: silent idempotent `bool`, not a raised error
//! (self-review point, design.md's own convention)
//! Unlike `StatusRepository::insert_status`'s `statuses_uri_key` violation
//! (which *is* raised as a client-facing `409`, since a duplicate URI is a
//! genuine data-integrity problem), a duplicate favourite/bookmark/pin is
//! expected, ordinary client behavior (a user double-clicking "favourite"),
//! and design.md's own Service Interface sketch says as much directly in
//! its own comment on [`add_favourite`]/[`add_bookmark`]: "新規 true / 既存
//! false" — no error variant at all. Every write function in this module
//! therefore uses `INSERT ... ON CONFLICT (actor_id, status_id) DO NOTHING`
//! (never a raw `INSERT` that could raise a unique-violation error) and
//! reports "was this actually new" via `rows_affected()` instead — the
//! Postgres unique-constraint violation this table enforces (Requirements
//! 9.3, 10.4, 12.1's "(actor_id, status_id) 一意") never actually surfaces
//! as an `sqlx::Error` on this path at all, so there is no raw-DB-error
//! leak to guard against here in the first place. The same idempotent-`bool`
//! shape is used symmetrically for every `remove_*` function ("was a row
//! actually deleted") — an unbookmark/unfavourite/unpin of something not
//! currently favourited/bookmarked/pinned is a no-op success, not an error,
//! matching `StatusRepository::delete_status`'s established "absence is not
//! an error at this layer" convention.
//!
//! ## `now: OffsetDateTime` / `id: Id` parameters design.md's sketch omits
//! (CONCERN — documented judgment call, same pattern as task 2.1's
//! `StatusEdit::id` resolution)
//! design.md's Service Interface sketch (design.md lines 419-424) gives
//! [`add_favourite`]/[`add_bookmark`]/[`set_pin`] no `created_at`/`id`
//! parameters at all. Unlike `StatusRepository`'s functions, there is no
//! `Favourite`/`Bookmark`/`Pin` domain struct to carry those values instead
//! (`src/statuses/model.rs`'s own doc comment confirms this is deliberate:
//! "Per-actor interaction records...deliberately have no dedicated domain
//! struct"). Since this crate's "callers mint ids/timestamps via
//! `RuntimeContext`, repositories never call `Utc::now()`/generate ids
//! themselves" convention is load-bearing (`status_repository.rs`'s
//! `apply_edit` takes an explicit `now: OffsetDateTime` for exactly this
//! reason), this module adds:
//! - `now: OffsetDateTime` to [`add_favourite`], [`add_bookmark`], and
//!   [`set_pin`] (each writes a `created_at` column that must come from the
//!   caller's injected `Clock`, never a SQL-side `NOW()`).
//! - `id: Id` to [`add_bookmark`] only: `bookmarks.id BIGINT PRIMARY KEY`
//!   has no database-side default (same convention as every other table in
//!   `migrations/0007_statuses.sql`), so its caller-minted primary key must
//!   be threaded through explicitly. `favourites`/`pins` need no such
//!   parameter — both are keyed by the composite `(actor_id, status_id)`
//!   alone, with no separate `id` column at all.
//!
//! ## `remove_bookmark` / `exists_favourite` / `exists_bookmark` /
//! `exists_pin` design.md's sketch omits (CONCERN — documented judgment
//! call)
//! design.md's sketch lists `add_favourite` **and** `remove_favourite` as a
//! pair, but only `add_bookmark` (no `remove_bookmark`) and only `set_pin`
//! (a single add/remove toggle, no separate exists check) — an inconsistent
//! level of detail across three structurally identical operations, not a
//! deliberate asymmetry in the actual required behavior: this task's own
//! instruction text is uniform across all three ("favourite/reblog/bookmark/
//! pin の記録・取消・存在判定"), Requirement 11.2 explicitly requires
//! bookmark *revocation* ("ブックマーク解除...bookmarked=false"), and
//! Requirement 1.2 requires the eventual `Status` JSON to reflect
//! `favourited`/`bookmarked`/`pinned` actor-state, which needs a genuine
//! read-only existence check for each (a write function's "was this new"
//! `bool` return does not serve that purpose — it only reports on a write
//! attempt just made, not a pure query). This module therefore fills the
//! gap by symmetry with [`remove_favourite`]/[`find_reblog`]:
//! [`remove_bookmark`] (mirrors `remove_favourite`), and [`exists_favourite`]
//! / [`exists_bookmark`] / [`exists_pin`] (mirror `find_reblog`'s read-only
//! existence-check role for the three tables that need only a `bool`, not a
//! full `Status`).
//!
//! ## Bookmark listing's own cursor ([`BookmarkCursor`], Requirement 11.3)
//! `migrations/0007_statuses.sql`'s own naming-note on `bookmarks.id`
//! documents exactly why: "`list_bookmarks` (Requirements 11.3) needs a
//! monotonic, bookmark-creation-order cursor distinct from the *status's*
//! own id/`created_at` (a status can be bookmarked long after it was
//! posted, so paging 'by when I bookmarked it' is not the same order as
//! paging 'by when the post was made')". [`BookmarkCursor`] wraps
//! `bookmarks.id` (not `statuses.id`) as the paged value, reusing
//! `crate::api::pagination`'s [`Cursor`]/[`PageParams`]/[`Page`]/[`paginate`]
//! toolkit (api-foundation, already built) exactly the way design.md's own
//! `list_bookmarks` signature (`page: PageParams`, unparsed — "recipient
//! decodes" discipline, matching `accounts/account_service.rs`'s
//! `StatusesQueryInput::page` precedent) calls for, rather than inventing a
//! parallel pagination mechanism.
//!
//! ## Row-to-`Status` reconstruction is a small, deliberate duplicate of
//! `status_repository.rs`'s private helpers, not a shared import (CONCERN)
//! [`find_reblog`] and [`list_bookmarks`] both need to read `statuses`
//! columns back into a [`Status`]. `status_repository.rs`'s equivalent
//! helpers (`StatusRow`, `status_columns!`, `row_to_status`,
//! `visibility_from_str`) are private (`fn`/`struct` with no `pub`) and this
//! task's own Boundary is explicit: "stay within this scope; do not touch
//! StatusRepository, TagRepository...read them as your pattern reference but
//! do not modify them." Rather than loosening that sibling file's
//! visibility (a change to a file this task is instructed not to touch, for
//! the sole benefit of this module), this module keeps its own small,
//! self-contained row-mapping duplicate. The duplication is intentionally
//! minimal in surface area (one row struct, one conversion function,
//! reused by both of this module's two `Status`-returning queries) rather
//! than copied twice.

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::api::db::map_server_error;
use crate::api::pagination::{Cursor, Page, PageParams};
use crate::domain::{Id, Visibility};
use crate::error::AppError;
use crate::statuses::model::Status;

/// Reconstructs a [`Visibility`] from an already-persisted
/// `statuses.visibility` column value. Mirrors
/// `status_repository.rs::visibility_from_str`'s identical convention (see
/// this module's doc comment, "Row-to-`Status` reconstruction...", for why
/// this is a deliberate small duplicate rather than a shared import).
fn visibility_from_str(raw: &str) -> Visibility {
    match raw {
        "public" => Visibility::Public,
        "unlisted" => Visibility::Unlisted,
        "private" => Visibility::Private,
        "direct" => Visibility::Direct,
        other => panic!(
            "statuses.visibility contained unexpected value {other:?}; expected one of \
             'public'/'unlisted'/'private'/'direct'"
        ),
    }
}

/// A `statuses` row as read directly off the wire by this module's own
/// queries, before reconstructing its typed [`Status`] form. Field order
/// matches [`STATUS_COLUMNS`]'s column list.
#[derive(sqlx::FromRow)]
struct StatusRow {
    id: i64,
    actor_id: i64,
    uri: String,
    url: Option<String>,
    content: String,
    visibility: String,
    sensitive: bool,
    spoiler_text: String,
    in_reply_to_id: Option<i64>,
    in_reply_to_account_id: Option<i64>,
    reblog_of_id: Option<i64>,
    poll_id: Option<i64>,
    language: Option<String>,
    reblogs_count: i64,
    favourites_count: i64,
    replies_count: i64,
    local: bool,
    created_at: OffsetDateTime,
    edited_at: Option<OffsetDateTime>,
}

/// The `statuses` column list [`StatusRow`]'s field set matches exactly.
/// See `status_repository.rs::status_columns!`'s identical doc comment for
/// why this is a `concat!`-friendly literal rather than a runtime-built
/// string (`sqlx`'s `SqlSafeStr` bound).
macro_rules! status_columns {
    () => {
        "id, actor_id, uri, url, content, visibility, sensitive, spoiler_text, in_reply_to_id, \
         in_reply_to_account_id, reblog_of_id, poll_id, language, reblogs_count, \
         favourites_count, replies_count, local, created_at, edited_at"
    };
}

fn row_to_status(row: StatusRow) -> Status {
    Status {
        id: Id::from_i64(row.id),
        actor_id: Id::from_i64(row.actor_id),
        uri: row.uri,
        url: row.url,
        content: row.content,
        visibility: visibility_from_str(&row.visibility),
        sensitive: row.sensitive,
        spoiler_text: row.spoiler_text,
        in_reply_to_id: row.in_reply_to_id.map(Id::from_i64),
        in_reply_to_account_id: row.in_reply_to_account_id.map(Id::from_i64),
        reblog_of_id: row.reblog_of_id.map(Id::from_i64),
        poll_id: row.poll_id.map(Id::from_i64),
        language: row.language,
        reblogs_count: row.reblogs_count,
        favourites_count: row.favourites_count,
        replies_count: row.replies_count,
        local: row.local,
        created_at: row.created_at,
        edited_at: row.edited_at,
    }
}

// -- favourite ----------------------------------------------------------

/// Records `actor_id`'s favourite of `status_id` (Requirement 10.1).
/// Returns `Ok(true)` when this call actually inserted a new row, `Ok(false)`
/// when `(actor_id, status_id)` was already favourited (Requirement 10.4's
/// "重複したお気に入りを作成しない" — a silent idempotent no-op, not an
/// error; see this module's doc comment, "Duplicate handling").
///
/// Generic over `executor` so `InteractionService::favourite` can drive it
/// against an open `sqlx::Transaction` (`&mut *tx`) together with the
/// matching `status_repository::adjust_counts`, so the row and its counter
/// can never diverge; every pre-existing caller keeps passing a bare
/// `&PgPool` unchanged.
pub async fn add_favourite<'e, E>(
    executor: E,
    actor_id: Id,
    status_id: Id,
    now: OffsetDateTime,
) -> Result<bool, AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let result = sqlx::query(
        "INSERT INTO favourites (actor_id, status_id, created_at) VALUES ($1, $2, $3) \
         ON CONFLICT (actor_id, status_id) DO NOTHING",
    )
    .bind(actor_id.as_i64())
    .bind(status_id.as_i64())
    .bind(now)
    .execute(executor)
    .await
    .map_err(map_server_error)?;

    Ok(result.rows_affected() > 0)
}

/// Revokes `actor_id`'s favourite of `status_id` (Requirement 10.3).
/// Returns `Ok(true)` when a row was actually deleted, `Ok(false)` when
/// `(actor_id, status_id)` was not favourited to begin with — an idempotent
/// no-op success, mirroring `StatusRepository::delete_status`'s "absence is
/// not an error at this layer" convention.
///
/// Generic over `executor` for the same reason as [`add_favourite`] — every
/// pre-existing caller keeps passing a bare
/// `&PgPool` unchanged.
pub async fn remove_favourite<'e, E>(
    executor: E,
    actor_id: Id,
    status_id: Id,
) -> Result<bool, AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let result = sqlx::query("DELETE FROM favourites WHERE actor_id = $1 AND status_id = $2")
        .bind(actor_id.as_i64())
        .bind(status_id.as_i64())
        .execute(executor)
        .await
        .map_err(map_server_error)?;

    Ok(result.rows_affected() > 0)
}

/// Reports whether `actor_id` currently has `status_id` favourited
/// (Requirement 1.2's `favourited` actor-state, and the read-only half of
/// this module's "存在判定" instruction — see this module's doc comment).
pub async fn exists_favourite(
    pool: &PgPool,
    actor_id: Id,
    status_id: Id,
) -> Result<bool, AppError> {
    let (exists,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM favourites WHERE actor_id = $1 AND status_id = $2)",
    )
    .bind(actor_id.as_i64())
    .bind(status_id.as_i64())
    .fetch_one(pool)
    .await
    .map_err(map_server_error)?;

    Ok(exists)
}

// -- bookmark -------------------------------------------------------------

/// Records `actor_id`'s bookmark of `status_id` under caller-minted `id`
/// (Requirement 11.1). Returns `Ok(true)` when this call actually inserted
/// a new row, `Ok(false)` when `(actor_id, status_id)` was already
/// bookmarked (Requirement 11.1's own "(actor_id, status_id) 一意" — a
/// silent idempotent no-op; see this module's doc comment, "Duplicate
/// handling").
///
/// `id` is this new `bookmarks` row's own primary key (distinct from
/// `status_id`) — see this module's doc comment ("`now`/`id` parameters
/// design.md's sketch omits") for why it must be caller-minted rather than
/// generated here.
pub async fn add_bookmark(
    pool: &PgPool,
    id: Id,
    actor_id: Id,
    status_id: Id,
    now: OffsetDateTime,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        "INSERT INTO bookmarks (id, actor_id, status_id, created_at) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (actor_id, status_id) DO NOTHING",
    )
    .bind(id.as_i64())
    .bind(actor_id.as_i64())
    .bind(status_id.as_i64())
    .bind(now)
    .execute(pool)
    .await
    .map_err(map_server_error)?;

    Ok(result.rows_affected() > 0)
}

/// Revokes `actor_id`'s bookmark of `status_id` (Requirement 11.2).
/// Returns `Ok(true)` when a row was actually deleted, `Ok(false)` when
/// `(actor_id, status_id)` was not bookmarked to begin with — same
/// idempotent-no-op convention as [`remove_favourite`].
pub async fn remove_bookmark(pool: &PgPool, actor_id: Id, status_id: Id) -> Result<bool, AppError> {
    let result = sqlx::query("DELETE FROM bookmarks WHERE actor_id = $1 AND status_id = $2")
        .bind(actor_id.as_i64())
        .bind(status_id.as_i64())
        .execute(pool)
        .await
        .map_err(map_server_error)?;

    Ok(result.rows_affected() > 0)
}

/// Reports whether `actor_id` currently has `status_id` bookmarked
/// (Requirement 1.2's `bookmarked` actor-state).
pub async fn exists_bookmark(pool: &PgPool, actor_id: Id, status_id: Id) -> Result<bool, AppError> {
    let (exists,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM bookmarks WHERE actor_id = $1 AND status_id = $2)",
    )
    .bind(actor_id.as_i64())
    .bind(status_id.as_i64())
    .fetch_one(pool)
    .await
    .map_err(map_server_error)?;

    Ok(exists)
}

/// [`Cursor`] over `bookmarks.id` — the bookmark row's *own* creation-order
/// primary key, deliberately distinct from the bookmarked status's own id
/// (Requirement 11.3's "ブックマーク固有カーソル"; see this module's doc
/// comment, "Bookmark listing's own cursor").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BookmarkCursor(pub u64);

impl Cursor for BookmarkCursor {
    fn encode(&self) -> String {
        self.0.to_string()
    }

    fn decode(raw: &str) -> Result<Self, AppError> {
        raw.parse::<u64>().map(BookmarkCursor).map_err(|_| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid cursor value: '{raw}'"),
            )
        })
    }
}

/// A `bookmarks` row joined with its bookmarked `statuses` row, as read
/// directly off the wire by [`list_bookmarks`]'s own query.
#[derive(sqlx::FromRow)]
struct BookmarkedStatusRow {
    bookmark_id: i64,
    status_id: i64,
    status_actor_id: i64,
    uri: String,
    url: Option<String>,
    content: String,
    visibility: String,
    sensitive: bool,
    spoiler_text: String,
    in_reply_to_id: Option<i64>,
    in_reply_to_account_id: Option<i64>,
    reblog_of_id: Option<i64>,
    poll_id: Option<i64>,
    language: Option<String>,
    reblogs_count: i64,
    favourites_count: i64,
    replies_count: i64,
    local: bool,
    created_at: OffsetDateTime,
    edited_at: Option<OffsetDateTime>,
}

fn bookmarked_row_to_pair(row: BookmarkedStatusRow) -> (BookmarkCursor, Status) {
    let status = Status {
        id: Id::from_i64(row.status_id),
        actor_id: Id::from_i64(row.status_actor_id),
        uri: row.uri,
        url: row.url,
        content: row.content,
        visibility: visibility_from_str(&row.visibility),
        sensitive: row.sensitive,
        spoiler_text: row.spoiler_text,
        in_reply_to_id: row.in_reply_to_id.map(Id::from_i64),
        in_reply_to_account_id: row.in_reply_to_account_id.map(Id::from_i64),
        reblog_of_id: row.reblog_of_id.map(Id::from_i64),
        poll_id: row.poll_id.map(Id::from_i64),
        language: row.language,
        reblogs_count: row.reblogs_count,
        favourites_count: row.favourites_count,
        replies_count: row.replies_count,
        local: row.local,
        created_at: row.created_at,
        edited_at: row.edited_at,
    };
    // `bookmarks.id` is BIGINT but always non-negative (caller-minted via
    // the same monotonically-increasing `IdGenerator` every other entity in
    // this crate uses, `runtime/ids.rs`'s own doc comment) — the same
    // int64-payload-as-u64 assumption `crate::api::pagination::StatusIdCursor`
    // already relies on for its own `Id`-shaped cursors.
    (BookmarkCursor(row.bookmark_id as u64), status)
}

/// Returns `actor_id`'s bookmarked posts, newest-bookmarked-first,
/// paginated by [`BookmarkCursor`] (`bookmarks.id`, not the status's own id
/// — Requirement 11.3).
///
/// `page` is taken **unparsed** (raw [`PageParams`], not a decoded
/// `ParsedPageParams<C>`) — the same "recipient decodes" discipline
/// `accounts/account_service.rs::StatusesQueryInput::page`'s doc comment
/// establishes: this repository is the one place that knows bookmark
/// listing's concrete cursor type ([`BookmarkCursor`]), so it is also the
/// one place that should call [`PageParams::parse`].
pub async fn list_bookmarks(
    pool: &PgPool,
    actor_id: Id,
    page: PageParams,
) -> Result<Page<Status>, AppError> {
    let parsed = page.parse::<BookmarkCursor>()?;

    let rows: Vec<BookmarkedStatusRow> = sqlx::query_as(
        "SELECT bookmarks.id AS bookmark_id, \
             statuses.id AS status_id, statuses.actor_id AS status_actor_id, statuses.uri, \
             statuses.url, statuses.content, statuses.visibility, statuses.sensitive, \
             statuses.spoiler_text, statuses.in_reply_to_id, statuses.in_reply_to_account_id, \
             statuses.reblog_of_id, statuses.poll_id, statuses.language, statuses.reblogs_count, \
             statuses.favourites_count, statuses.replies_count, statuses.local, \
             statuses.created_at, statuses.edited_at \
         FROM bookmarks INNER JOIN statuses ON statuses.id = bookmarks.status_id \
         WHERE bookmarks.actor_id = $1 \
         ORDER BY bookmarks.id DESC",
    )
    .bind(actor_id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    let pool_items: Vec<(BookmarkCursor, Status)> =
        rows.into_iter().map(bookmarked_row_to_pair).collect();

    let paged = crate::api::pagination::paginate(&pool_items, |item| item.0, &parsed);

    Ok(Page {
        items: paged.items.into_iter().map(|(_, status)| status).collect(),
        prev_cursor: paged.prev_cursor,
        next_cursor: paged.next_cursor,
    })
}

// -- pin --------------------------------------------------------------------

/// Records or revokes `actor_id`'s pin of `status_id` (Requirements 12.1,
/// 12.2), depending on `pinned`. Returns `Ok(true)` when this call actually
/// changed persisted state (a new row inserted when `pinned == true`, or an
/// existing row deleted when `pinned == false`); `Ok(false)` when the
/// requested state already held — a silent idempotent no-op in both
/// directions (see this module's doc comment, "Duplicate handling").
///
/// `now` is only consulted when `pinned == true` (the new row's
/// `created_at`); see this module's doc comment ("`now`/`id` parameters
/// design.md's sketch omits").
///
/// Ownership verification (Requirement 12.3: pin target must belong to the
/// requesting actor) and direct-visibility rejection (Requirement 12.4) are
/// deliberately **not** enforced here — both need the target `Status`
/// itself (owner/visibility), which is `InteractionService`'s job (task
/// 5.x) sitting above this repository, exactly like `InteractionService`'s
/// documented visibility-check-before-record responsibility for
/// reblog/favourite (design.md: "可視性チェック...→重複防止...→記録").
pub async fn set_pin(
    pool: &PgPool,
    actor_id: Id,
    status_id: Id,
    pinned: bool,
    now: OffsetDateTime,
) -> Result<bool, AppError> {
    if pinned {
        let result = sqlx::query(
            "INSERT INTO pins (actor_id, status_id, created_at) VALUES ($1, $2, $3) \
             ON CONFLICT (actor_id, status_id) DO NOTHING",
        )
        .bind(actor_id.as_i64())
        .bind(status_id.as_i64())
        .bind(now)
        .execute(pool)
        .await
        .map_err(map_server_error)?;

        Ok(result.rows_affected() > 0)
    } else {
        let result = sqlx::query("DELETE FROM pins WHERE actor_id = $1 AND status_id = $2")
            .bind(actor_id.as_i64())
            .bind(status_id.as_i64())
            .execute(pool)
            .await
            .map_err(map_server_error)?;

        Ok(result.rows_affected() > 0)
    }
}

/// Reports whether `actor_id` currently has `status_id` pinned (Requirement
/// 1.2's `pinned` actor-state).
pub async fn exists_pin(pool: &PgPool, actor_id: Id, status_id: Id) -> Result<bool, AppError> {
    let (exists,): (bool,) =
        sqlx::query_as("SELECT EXISTS(SELECT 1 FROM pins WHERE actor_id = $1 AND status_id = $2)")
            .bind(actor_id.as_i64())
            .bind(status_id.as_i64())
            .fetch_one(pool)
            .await
            .map_err(map_server_error)?;

    Ok(exists)
}

// -- reblog (read-only existence check only; see this module's doc comment) -

/// Looks up `actor_id`'s own boost (reblog) of `status_id`, if any — the
/// read-only "is this a duplicate reblog" check design.md's
/// `InteractionRepository` Service Interface assigns to this module for
/// Requirement 9.3 ("同一アクターが既にブースト済みの投稿を再びブースト
/// 要求したとき...重複したブーストを作成しない"). A boost is a `statuses`
/// row with `actor_id` set to the booster and `reblog_of_id` set to the
/// boosted post — see this module's doc comment ("Reblog scope") for why
/// recording/revoking that row itself is `StatusRepository`'s job, not
/// this function's.
///
/// Generic over `executor` for the same reason
/// [`add_favourite`]/[`remove_favourite`] are:
/// `InteractionService::reblog`'s "has this actor already boosted?"
/// branch decides whether the boost row is inserted and `reblogs_count`
/// incremented at all, so it has to be read *inside* the same transaction
/// those two writes run in — otherwise the branch is taken against a snapshot
/// the transaction never sees. Every pre-existing caller keeps passing a bare
/// `&PgPool` unchanged.
pub async fn find_reblog<'e, E>(
    executor: E,
    actor_id: Id,
    status_id: Id,
) -> Result<Option<Status>, AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let row: Option<StatusRow> = sqlx::query_as(concat!(
        "SELECT ",
        status_columns!(),
        " FROM statuses WHERE actor_id = $1 AND reblog_of_id = $2 LIMIT 1"
    ))
    .bind(actor_id.as_i64())
    .bind(status_id.as_i64())
    .fetch_optional(executor)
    .await
    .map_err(map_server_error)?;

    Ok(row.map(row_to_status))
}

// -- batched interaction state ---------------------------------------------

/// Runs one of the four batched interaction-state queries below and collects
/// its single-column result into a set.
///
/// `sql` must select exactly the status id column to report as "present",
/// take the viewer as `$1` and the status-id array as `$2`, and otherwise
/// carry the same `WHERE` scoping as the singular check it batches — that
/// equivalence is what this module's per-function comparison tests pin down.
/// Ordering is deliberately unconstrained: the result is a [`HashSet`], so
/// unlike `tags_for_statuses` there is no per-key sequence for a batched
/// query to disagree with its singular counterpart about, and a duplicate row
/// (possible for [`reblogged_status_ids`], whose underlying `statuses` table
/// has no unique constraint over `(actor_id, reblog_of_id)`) collapses on
/// insertion rather than needing a `DISTINCT`.
async fn status_id_set(
    pool: &PgPool,
    sql: &'static str,
    viewer: Id,
    status_ids: &[Id],
) -> Result<HashSet<Id>, AppError> {
    if status_ids.is_empty() {
        return Ok(HashSet::new());
    }

    let raw_ids: Vec<i64> = status_ids.iter().map(|id| id.as_i64()).collect();
    let rows: Vec<(i64,)> = sqlx::query_as(sql)
        .bind(viewer.as_i64())
        .bind(&raw_ids)
        .fetch_all(pool)
        .await
        .map_err(map_server_error)?;

    Ok(rows.into_iter().map(|(id,)| Id::from_i64(id)).collect())
}

/// The batched form of [`exists_favourite`]: reports which of `status_ids`
/// `viewer` has favourited, in one query instead of one query per status.
///
/// Equivalent to calling [`exists_favourite`] once per id, by construction —
/// same table, same `actor_id = $1 AND status_id = ...` scoping, just
/// widened to `= ANY`. An id `viewer` has *not* favourited is simply **absent
/// from the returned set** (the returned ids are the subset of `status_ids`
/// `viewer` favourited), which also means an id with no `statuses` row at all
/// is not distinguished from an existing but unfavourited one — the same
/// "no row -> absent" contract [`tags_for_statuses`](crate::statuses::tag_repository::tags_for_statuses)
/// carries. An empty `status_ids` returns an empty set without issuing a
/// query at all: `= ANY` on an empty array would match nothing anyway, so the
/// round trip would be pure cost.
pub async fn favourited_status_ids(
    pool: &PgPool,
    viewer: Id,
    status_ids: &[Id],
) -> Result<HashSet<Id>, AppError> {
    status_id_set(
        pool,
        "SELECT status_id FROM favourites WHERE actor_id = $1 AND status_id = ANY($2::bigint[])",
        viewer,
        status_ids,
    )
    .await
}

/// The batched form of [`exists_bookmark`]: reports which of `status_ids`
/// `viewer` has bookmarked, in one query.
///
/// Equivalent to calling [`exists_bookmark`] once per id, by construction
/// (same table, same `actor_id`/`status_id` scoping widened to `= ANY`), with
/// the same "no row -> absent" contract and the same empty-slice
/// short-circuit as [`favourited_status_ids`].
///
/// Selects `bookmarks.status_id`, never `bookmarks.id` — this module's own
/// [`BookmarkCursor`] doc comment is explicit that a bookmark row's primary
/// key is deliberately distinct from the bookmarked status's id, and callers
/// here are asking about the latter.
pub async fn bookmarked_status_ids(
    pool: &PgPool,
    viewer: Id,
    status_ids: &[Id],
) -> Result<HashSet<Id>, AppError> {
    status_id_set(
        pool,
        "SELECT status_id FROM bookmarks WHERE actor_id = $1 AND status_id = ANY($2::bigint[])",
        viewer,
        status_ids,
    )
    .await
}

/// The batched form of [`exists_pin`]: reports which of `status_ids` `viewer`
/// has pinned, in one query.
///
/// Equivalent to calling [`exists_pin`] once per id, by construction, with
/// the same "no row -> absent" contract and empty-slice short-circuit as
/// [`favourited_status_ids`].
///
/// A pin is conventionally an author's pin of their *own* post rather than a
/// viewer-to-anyone relation, but that is a policy `InteractionService`
/// enforces above this repository (see [`set_pin`]'s own doc comment) — the
/// `pins` table itself is keyed `(actor_id, status_id)` exactly like
/// `favourites`/`bookmarks`, and [`exists_pin`] scopes purely by `actor_id`.
/// This function therefore takes the same `viewer` parameter as its three
/// siblings and mirrors that same `WHERE`, with no ownership predicate the
/// singular version does not have.
pub async fn pinned_status_ids(
    pool: &PgPool,
    viewer: Id,
    status_ids: &[Id],
) -> Result<HashSet<Id>, AppError> {
    status_id_set(
        pool,
        "SELECT status_id FROM pins WHERE actor_id = $1 AND status_id = ANY($2::bigint[])",
        viewer,
        status_ids,
    )
    .await
}

/// The batched form of [`find_reblog`]'s existence half: reports which of
/// `status_ids` `viewer` has boosted, in one query.
///
/// Unlike its three siblings there is no interaction table to read — a boost
/// is a `statuses` row of its own, with `actor_id` set to the booster and
/// `reblog_of_id` set to the boosted post (see this module's doc comment,
/// "Reblog scope"). This query therefore mirrors [`find_reblog`]'s own
/// `actor_id = $1 AND reblog_of_id = ...` `WHERE` against `statuses`, and
/// returns `reblog_of_id` — the **boosted** status's id, which is what the
/// caller passed in — rather than the boost row's own id. [`find_reblog`]'s
/// `LIMIT 1` has no batched counterpart because the set collapses duplicates
/// on its own (see [`status_id_set`]).
///
/// Same "no row -> absent" contract (an id `viewer` has not boosted is absent
/// from the set, exactly as [`find_reblog`] returns `None` for it) and same
/// empty-slice short-circuit as [`favourited_status_ids`].
pub async fn reblogged_status_ids(
    pool: &PgPool,
    viewer: Id,
    status_ids: &[Id],
) -> Result<HashSet<Id>, AppError> {
    status_id_set(
        pool,
        "SELECT reblog_of_id FROM statuses \
         WHERE actor_id = $1 AND reblog_of_id = ANY($2::bigint[])",
        viewer,
        status_ids,
    )
    .await
}
