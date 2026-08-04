//! `NotificationRepository` (design.md "Data / データ層" ->
//! `NotificationRepository`; Requirements 2.1, 2.2, 2.3, 2.4, 3.1, 4.1, 4.2,
//! 4.4, 8.1, 8.2; task 1.3, `Boundary: NotificationRepository`): the
//! notification's own persistence — dedup-idempotent insert, recipient-
//! scoped/dismissed-excluded/cursor-paginated/type-and-account-filtered
//! list, single (id, recipient) fetch, dismiss, and clear — against
//! `notifications` (`migrations/0009_notifications.sql`, task 1.1, already
//! applied, unmodified by this task).
//!
//! ## Scope
//! This module owns exactly design.md's `NotificationRepository` Service
//! Interface: [`insert_dedup`], [`list`], [`find_for_recipient`],
//! [`dismiss`], [`clear`], plus this module's own [`InsertOutcome`] and
//! [`ListFilter`] types. No `NotificationEventSink`/`NotificationDeliverySink`
//! (task 2.2), no `NotificationFilter` (task 2.3), no `NotificationGenerator`
//! (task 2.4), no `NotificationSerializer` (task 2.1), no `NotificationService`
//! (task 3.2), no HTTP surface (task 4.1), and no `NotificationModule`
//! wiring (task 4.x) live here — those consume this repository but are out
//! of scope for task 1.3 (`Boundary: NotificationRepository`). [`model`]'s
//! types (`NotificationType`, `Notification`, `NotificationEvent`) are
//! imported, not redefined, per task 1.2's already-established precedent.
//!
//! ## IDs/timestamps: caller-minted, never generated in this module
//! [`insert_dedup`]'s design.md-specified signature takes an already-built
//! `n: &Notification` — `Notification::id`/`Notification::created_at` are
//! therefore read straight off that value, never minted here. This mirrors
//! this crate's crate-wide convention (`RuntimeContext`'s `IdGenerator`/
//! `Clock` are always consulted by the *caller*, never inside a
//! repository): `status_repository.rs::insert_status(pool, status: &Status)`
//! and `social_graph/repository.rs::upsert_follow(pool, id: Id, f: &Follow)`
//! both take already-built values / caller-supplied ids the exact same way.
//! [`Notification`] itself already carries `created_at`, so — unlike
//! `upsert_follow`'s separate `id: Id` parameter (`Follow` has no `id`
//! field of its own) — no second id parameter is needed here: `n.id` is
//! this row's caller-minted primary key directly.
//!
//! ## Duplicate handling: [`InsertOutcome`], not a raised error (mirrors
//! `InteractionRepository`'s idempotent-`bool` convention, generalized to
//! carry the created value)
//! `statuses/interaction_repository.rs`'s own doc comment ("Duplicate
//! handling: silent idempotent `bool`, not a raised error") documents this
//! crate's established convention for an expected, ordinary duplicate (a
//! client retry / upstream event resend, not a data-integrity problem):
//! every write here uses `INSERT ... ON CONFLICT ... DO NOTHING` and reports
//! "was this actually new" via `rows_affected()`, never a raw `INSERT` that
//! could raise a unique-violation `sqlx::Error`. design.md's own comment on
//! this task's `insert_dedup` signature — `// Created(Notification) |
//! Duplicate` — asks for slightly more than `InteractionRepository`'s bare
//! `bool`: the created [`Notification`] itself on the "new" branch (useful
//! to `NotificationGenerator`, task 2.4, which needs the persisted value to
//! hand to `NotificationDeliverySink`). [`InsertOutcome`] is this module's
//! own type (no existing enum in this crate has this exact "which branch,
//! carrying a value on one arm only" shape — `InteractionRepository`'s
//! `bool` carries nothing, and no other repository's insert is dedup'd).
//!
//! ## `ON CONFLICT` target: partial unique index, not a plain column list
//! Unlike `favourites`/`bookmarks`/`pins`' plain `(actor_id, status_id)`
//! unique constraints, `notifications_dedup_idx`
//! (`migrations/0009_notifications.sql`) is a **partial** unique index
//! scoped `WHERE NOT dismissed`. Postgres requires an `ON CONFLICT` target
//! to name the exact same column/expression list *and* the exact same
//! `WHERE` predicate as the index it's inferring against — so [`insert_dedup`]'s
//! `ON CONFLICT (recipient_id, kind, origin_kind, origin_id, COALESCE(status_id,
//! 0)) WHERE NOT dismissed DO NOTHING` reproduces `notifications_dedup_idx`'s
//! own definition verbatim (migration's own physical model, design.md's
//! "Consistency"). This is also exactly why a dismissed row's key does not
//! block a fresh insert with the same key (Requirement 8.1's "取り消し→
//! 再実行の再通知"): the partial index — and therefore this `ON CONFLICT`
//! target — only ever considers not-yet-dismissed rows, so once a row with
//! a given key is dismissed, a new row with the identical key inserts
//! cleanly instead of hitting `DO NOTHING`.
//!
//! ## `AccountRef` <-> `(origin_kind, origin_id)` conversion (mirrors
//! `social_graph/repository.rs`'s established convention)
//! `social_graph/repository.rs`'s own `account_kind`/`account_id`/
//! `account_ref_from` private helpers are the established precedent for
//! this crate's `AccountRef::Local(Id)`/`Remote(Id)` <-> `('local'|'remote',
//! BIGINT)` column-pair mapping (that module's own doc comment: "the
//! `(kind, id)` pair every `AccountRef` needs"). Those functions are
//! private to their own module, so this module defines its own identically
//! shaped copies rather than loosening that sibling file's visibility for
//! this module's sole benefit — the same "small, deliberate duplicate, not
//! a shared import" judgment call `statuses/interaction_repository.rs`'s
//! own doc comment and `timelines/candidate_repository.rs`'s own doc
//! comment (`row_to_status`/`visibility_from_str`/`map_server_error`, all
//! duplicated there for the identical reason) already document and apply.
//! `tests/notifications_migrations_it.rs` (task 1.1) independently
//! confirms `'local'`/`'remote'` as the exact on-the-wire string literals
//! this module's queries must produce/parse.
//!
//! ## `kind` <-> `NotificationType` conversion
//! `notifications.kind` is `TEXT` (migration's own column comment:
//! "mention/follow/follow_request/favourite/reblog/poll/status/update").
//! [`kind_to_str`]/[`kind_from_str`] map [`NotificationType`]'s eight v1
//! variants to/from those exact literals — the same strings
//! `tests/notifications_migrations_it.rs` already exercises (`"follow"`,
//! `"favourite"`) and design.md's own `type` field naming (Requirement
//! 1.5's v1 kind set). [`kind_from_str`] panics on any other value, mirroring
//! `status_repository.rs::visibility_from_str`'s/`candidate_repository.rs`'s
//! identical "impossible per this module's own write path; a data-corruption
//! invariant violation if it ever fires" convention — nothing in this crate
//! ever writes a `kind` value this module's own [`kind_to_str`] doesn't
//! produce.
//!
//! ## `list`'s dynamic `WHERE` clause: `sqlx::QueryBuilder`, not string
//! concatenation
//! `list`'s three independent optional filters (`types`/`exclude_types`/
//! `account_id`, design.md's `ListFilter`) compose an arbitrary subset of
//! extra `AND` conditions on top of the always-present recipient-scope +
//! dismissed-exclusion predicate. `timelines/candidate_repository.rs::fetch_candidates`
//! already establishes this crate's precedent for exactly this shape
//! (`sqlx::QueryBuilder<Postgres>`, `push`/`push_bind` per optional
//! condition) rather than either hand-building a raw SQL string (parameter-
//! index bookkeeping errors) or writing out every filter-combination as its
//! own literal query.
//!
//! ## `list`'s cursor: reuses `StatusIdCursor`, no new cursor type
//! `crate::api::pagination::StatusIdCursor`'s own doc comment already names
//! `GET /api/v1/notifications` as a motivating example of its "most lists
//! page by an entity's own id" default — a notification's list cursor
//! *is* `notifications.id` itself (design.md "Temporal": "一覧カーソルは
//! `id` 降順"), unlike `BookmarkCursor`'s deliberately-distinct-from-status-id
//! shape. No new `Cursor` impl is warranted here.
//!
//! `list` pages **in memory** via `crate::api::pagination::paginate` after a
//! single filtered `SELECT ... ORDER BY id DESC` — mirrors
//! `social_graph/repository.rs::list_inbound_requests`'s and
//! `statuses/interaction_repository.rs::list_bookmarks`'s identical
//! "one recipient-scoped list, no cross-recipient merge" precedent (as
//! opposed to `candidate_repository.rs::fetch_candidates`'s SQL-level cursor
//! bound, which exists there only because that function fetches a
//! multi-source *batch* for a later merge step — not this repository's
//! single-source, single-recipient list).
//!
//! ## `find_for_recipient` excludes dismissed rows (Requirement 4.4)
//! design.md's own `NotificationRepository` Responsibilities prose states
//! "消去は以降の取得から除外（4.4）" directly after describing dismiss/clear
//! — and Requirement 4.4's acceptance-criterion text is explicit that
//! *both* list and single fetch ("一覧取得・単一取得") exclude a dismissed
//! notification, not list alone. [`find_for_recipient`] therefore applies
//! `AND NOT dismissed` the same way [`list`] does, rather than only
//! filtering by `(id, recipient)`.
//!
//! ## `dismiss` does not require the row to be not-yet-dismissed
//! [`dismiss`] matches design.md's own wording ("dismiss は単一を消去...
//! returns whether it found/updated a row"): its `WHERE` clause is scoped to
//! `(id, recipient)` only (no `AND NOT dismissed`), so a repeat `dismiss`
//! call against an already-dismissed notification the caller still owns
//! remains an idempotent success (`Ok(true)`) rather than becoming
//! indistinguishable from "no such notification for this recipient"
//! (`Ok(false)`, which the service layer, task 3.2, is expected to map to
//! Requirement 4.3's 404). This mirrors `interaction_repository.rs`'s
//! `remove_favourite`/`remove_bookmark` "absence is not an error, idempotent
//! `bool`" convention, generalized to "already in the target state is not an
//! error either" for an ownership-scoped update rather than a delete.

#[cfg(test)]
mod tests;

use sqlx::postgres::PgPool;
use sqlx::{Postgres, QueryBuilder};

use crate::api::db::map_server_error;
use crate::api::pagination::{Page, PageParams, StatusIdCursor};
use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::notifications::model::{Notification, NotificationType};

// -- `AccountRef` <-> `(kind, id)` mapping (mirrors
// `social_graph/repository.rs`'s identical private helpers; see this
// module's doc comment) --------------------------------------------------

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
/// module never writes any other value (see [`account_kind`]).
fn account_ref_from(kind: &str, id: i64) -> AccountRef {
    match kind {
        "local" => AccountRef::Local(Id::from_i64(id)),
        "remote" => AccountRef::Remote(Id::from_i64(id)),
        other => panic!(
            "notifications.origin_kind contained unexpected value {other:?}; expected \
             'local' or 'remote'"
        ),
    }
}

// -- `NotificationType` <-> `kind` text mapping (see this module's doc
// comment) ----------------------------------------------------------------

fn kind_to_str(kind: NotificationType) -> &'static str {
    match kind {
        NotificationType::Mention => "mention",
        NotificationType::Follow => "follow",
        NotificationType::FollowRequest => "follow_request",
        NotificationType::Favourite => "favourite",
        NotificationType::Reblog => "reblog",
        NotificationType::Poll => "poll",
        NotificationType::Status => "status",
        NotificationType::Update => "update",
    }
}

fn kind_from_str(raw: &str) -> NotificationType {
    match raw {
        "mention" => NotificationType::Mention,
        "follow" => NotificationType::Follow,
        "follow_request" => NotificationType::FollowRequest,
        "favourite" => NotificationType::Favourite,
        "reblog" => NotificationType::Reblog,
        "poll" => NotificationType::Poll,
        "status" => NotificationType::Status,
        "update" => NotificationType::Update,
        other => panic!(
            "notifications.kind contained unexpected value {other:?}; expected one of \
             'mention'/'follow'/'follow_request'/'favourite'/'reblog'/'poll'/'status'/'update'"
        ),
    }
}

/// A `notifications` row as read directly off the wire, before
/// reconstructing its typed [`Notification`] form. Field order matches
/// [`NOTIFICATION_COLUMNS`]'s column list.
#[derive(sqlx::FromRow)]
struct NotificationRow {
    id: i64,
    recipient_id: i64,
    kind: String,
    origin_kind: String,
    origin_id: i64,
    status_id: Option<i64>,
    dismissed: bool,
    created_at: time::OffsetDateTime,
}

/// The `notifications` column list [`NotificationRow`]'s field set matches
/// exactly — mirrors `status_repository.rs::status_columns!`'s identical
/// `concat!`-friendly-literal convention (`sqlx`'s `SqlSafeStr` bound).
macro_rules! notification_columns {
    () => {
        "id, recipient_id, kind, origin_kind, origin_id, status_id, dismissed, created_at"
    };
}

fn row_to_notification(row: NotificationRow) -> Notification {
    Notification {
        id: Id::from_i64(row.id),
        recipient_id: Id::from_i64(row.recipient_id),
        kind: kind_from_str(&row.kind),
        origin: account_ref_from(&row.origin_kind, row.origin_id),
        status_id: row.status_id.map(Id::from_i64),
        dismissed: row.dismissed,
        created_at: row.created_at,
    }
}

/// Outcome of [`insert_dedup`] (design.md's own comment on that function's
/// signature: `// Created(Notification) | Duplicate`) — see this module's
/// doc comment, "Duplicate handling", for why this is its own enum rather
/// than `InteractionRepository`'s bare idempotent `bool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    /// This call actually inserted a new row; carries the same value that
    /// was passed in (a plain echo, since `insert_dedup` mints nothing
    /// itself — see this module's doc comment, "IDs/timestamps").
    Created(Notification),
    /// A not-yet-dismissed notification with the identical dedup key
    /// (`recipient_id`, `kind`, `origin_kind`, `origin_id`,
    /// `COALESCE(status_id, 0)`) already existed; no row was inserted
    /// (Requirement 8.1, 8.2).
    Duplicate,
}

/// design.md's `ListFilter` (Data / データ層 -> `NotificationRepository`'s
/// Service Interface): [`list`]'s optional narrowing beyond recipient scope
/// and dismissed exclusion (both always applied, not part of this struct).
/// `account_id` is an already-resolved [`AccountRef`] — this layer performs
/// no string-id-to-`AccountRef` resolution itself (that is the endpoint
/// layer's job in a later task, per this task's own boundary note).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListFilter {
    pub types: Option<Vec<NotificationType>>,
    pub exclude_types: Option<Vec<NotificationType>>,
    pub account_id: Option<AccountRef>,
}

/// Idempotently inserts `n` (Requirement 8.1, 8.2): a first call for a given
/// dedup key (`recipient_id`, `kind`, `origin_kind`, `origin_id`,
/// `COALESCE(status_id, 0)`) among not-yet-dismissed rows inserts a new row
/// and returns [`InsertOutcome::Created`]; a repeat call for the identical
/// key while that row remains not-yet-dismissed is silently ignored
/// (`ON CONFLICT ... WHERE NOT dismissed DO NOTHING`, matching
/// `notifications_dedup_idx`'s own partial-index definition exactly — see
/// this module's doc comment, "`ON CONFLICT` target") and returns
/// [`InsertOutcome::Duplicate`]. Once the existing row with that key has
/// been dismissed (via [`dismiss`]/[`clear`]), the key is free again and a
/// fresh call with the same key inserts cleanly as `Created` (Requirement
/// 8.1's "取り消し→再実行の再通知").
///
/// `n.id`/`n.created_at` are used as-is (caller-minted via `RuntimeContext`
/// — see this module's doc comment, "IDs/timestamps"), never generated
/// here.
pub async fn insert_dedup(pool: &PgPool, n: &Notification) -> Result<InsertOutcome, AppError> {
    let result = sqlx::query(
        "INSERT INTO notifications \
         (id, recipient_id, kind, origin_kind, origin_id, status_id, dismissed, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (recipient_id, kind, origin_kind, origin_id, COALESCE(status_id, 0)) \
         WHERE NOT dismissed DO NOTHING",
    )
    .bind(n.id.as_i64())
    .bind(n.recipient_id.as_i64())
    .bind(kind_to_str(n.kind))
    .bind(account_kind(&n.origin))
    .bind(account_id(&n.origin))
    .bind(n.status_id.map(|id| id.as_i64()))
    .bind(n.dismissed)
    .bind(n.created_at)
    .execute(pool)
    .await
    .map_err(map_server_error)?;

    Ok(if result.rows_affected() > 0 {
        InsertOutcome::Created(n.clone())
    } else {
        InsertOutcome::Duplicate
    })
}

/// Returns `recipient`'s notifications, newest-first, excluding dismissed
/// ones (Requirement 2.4), narrowed by `filter`'s optional `types`/
/// `exclude_types`/`account_id` (Requirements 2.2, 2.3), and paginated by
/// `notifications.id` (Requirement 2.1, `crate::api::pagination`'s
/// `max_id`/`since_id`/`min_id` convention — see this module's doc comment,
/// "`list`'s cursor").
///
/// `page` is taken **unparsed** (raw [`PageParams`]) — the same "recipient
/// decodes" discipline `interaction_repository.rs::list_bookmarks`'s doc
/// comment establishes: this repository is the one place that knows this
/// list's concrete cursor type ([`StatusIdCursor`]).
pub async fn list(
    pool: &PgPool,
    recipient: Id,
    page: &PageParams,
    filter: &ListFilter,
) -> Result<Page<Notification>, AppError> {
    let parsed = page.parse::<StatusIdCursor>()?;

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(concat!(
        "SELECT ",
        notification_columns!(),
        " FROM notifications WHERE recipient_id = "
    ));
    qb.push_bind(recipient.as_i64());
    qb.push(" AND NOT dismissed");

    if let Some(types) = &filter.types {
        let kinds: Vec<String> = types.iter().map(|k| kind_to_str(*k).to_string()).collect();
        qb.push(" AND kind = ANY(");
        qb.push_bind(kinds);
        qb.push(")");
    }

    if let Some(exclude_types) = &filter.exclude_types {
        let kinds: Vec<String> = exclude_types
            .iter()
            .map(|k| kind_to_str(*k).to_string())
            .collect();
        qb.push(" AND NOT (kind = ANY(");
        qb.push_bind(kinds);
        qb.push("))");
    }

    if let Some(account) = &filter.account_id {
        qb.push(" AND origin_kind = ");
        qb.push_bind(account_kind(account));
        qb.push(" AND origin_id = ");
        qb.push_bind(account_id(account));
    }

    qb.push(" ORDER BY id DESC");

    let rows: Vec<NotificationRow> = qb
        .build_query_as()
        .fetch_all(pool)
        .await
        .map_err(map_server_error)?;
    let notifications: Vec<Notification> = rows.into_iter().map(row_to_notification).collect();

    let paged = crate::api::pagination::paginate(
        &notifications,
        |n| StatusIdCursor(n.id.as_i64() as u64),
        &parsed,
    );

    Ok(paged)
}

/// Fetches the single notification `id`, scoped to `recipient` (Requirement
/// 3.1): `None` when `id` does not exist, belongs to a different recipient,
/// or has been dismissed (Requirement 4.4 — see this module's doc comment,
/// "`find_for_recipient` excludes dismissed rows").
pub async fn find_for_recipient(
    pool: &PgPool,
    id: Id,
    recipient: Id,
) -> Result<Option<Notification>, AppError> {
    let row: Option<NotificationRow> = sqlx::query_as(concat!(
        "SELECT ",
        notification_columns!(),
        " FROM notifications WHERE id = $1 AND recipient_id = $2 AND NOT dismissed"
    ))
    .bind(id.as_i64())
    .bind(recipient.as_i64())
    .fetch_optional(pool)
    .await
    .map_err(map_server_error)?;

    Ok(row.map(row_to_notification))
}

/// Marks notification `id` dismissed, scoped to `recipient` (Requirement
/// 4.2). Returns `Ok(true)` when a row matching `(id, recipient)` was
/// found/updated (regardless of whether it was already dismissed — see this
/// module's doc comment, "`dismiss` does not require..."), `Ok(false)` when
/// no such notification exists for this recipient (the service layer, task
/// 3.2, is expected to map this to Requirement 4.3's 404).
pub async fn dismiss(pool: &PgPool, id: Id, recipient: Id) -> Result<bool, AppError> {
    let result = sqlx::query(
        "UPDATE notifications SET dismissed = TRUE WHERE id = $1 AND recipient_id = $2",
    )
    .bind(id.as_i64())
    .bind(recipient.as_i64())
    .execute(pool)
    .await
    .map_err(map_server_error)?;

    Ok(result.rows_affected() > 0)
}

/// Marks every one of `recipient`'s notifications dismissed (Requirement
/// 4.1). Idempotent and always succeeds (including when `recipient` has no
/// notifications at all) — mirrors [`dismiss`]'s "already in the target
/// state is not an error" convention, generalized to the whole-recipient
/// case; design.md's own signature returns `Result<(), AppError>`, not a
/// count or bool, so there is nothing further for a caller to branch on.
pub async fn clear(pool: &PgPool, recipient: Id) -> Result<(), AppError> {
    sqlx::query("UPDATE notifications SET dismissed = TRUE WHERE recipient_id = $1")
        .bind(recipient.as_i64())
        .execute(pool)
        .await
        .map_err(map_server_error)?;

    Ok(())
}
