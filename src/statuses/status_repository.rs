//! `StatusRepository` (design.md "Data / データ層" -> `StatusRepository`;
//! Requirements 3.1, 3.5, 3.6, 6.1, 6.2, 7.1, 7.4, 8.1, 8.2; task 2.1,
//! `Boundary: StatusRepository, TagRepository`): the post's own persistence
//! — insert, visible-scope fetch, ancestor/descendant thread traversal,
//! delete (with its two explicit self-referential cleanup steps), edit-apply
//! plus history, and atomic counter updates — against `statuses` /
//! `status_edits` (`migrations/0007_statuses.sql`, already applied,
//! unmodified by this task).
//!
//! `TagRepository` (hashtag persistence against `tags` / `status_tags`) is
//! a separate component sharing this task's boundary; it lives in the
//! sibling module `crate::statuses::tag_repository` rather than in this
//! file, since it operates on entirely disjoint tables/types (`Tag`, not
//! `Status`/`StatusEdit`) and design.md's Boundary Commitments describe it
//! as its own read/write surface ("タグ関連付けの照会可能な読み取り境界").
//!
//! Scope: this module owns exactly the six operations design.md's Service
//! Interface lists for `StatusRepository` — [`insert_status`],
//! [`find_visible`], [`ancestors`], [`descendants`], [`delete_status`],
//! [`apply_edit`], [`list_edits`], [`adjust_counts`] (`list_edits` is listed
//! alongside `apply_edit` in the same bullet's "編集前後の内容を編集履歴と
//! して保持し、履歴取得要求に対して...を返す" responsibility, so it is
//! included here even though it is not separately itemized in this task's
//! own one-line instruction). No `InteractionRepository` (task 2.2, separate
//! boundary), `PollRepository`/`IdempotencyStore` (task 2.3), visibility/
//! addressing policy (`VisibilityPolicy`, task 3.1), Activity generation,
//! serialization, or service-layer orchestration lives here.
//!
//! ## `StatusEdit::id` (CONCERN — documented judgment call, resolved here)
//! Task 1.1/1.2 flagged a design.md self-contradiction (model excerpt omits
//! `StatusEdit::id`, Physical Data Model SQL declares
//! `status_edits.id BIGINT PRIMARY KEY`) for this task to resolve. Resolved
//! in `crate::statuses::model` by adding `StatusEdit::id` — see that
//! struct's own doc comment for the full reasoning (in short: every other
//! entity type in this crate already carries a caller-minted `id`, and
//! `status_edits.id` has no database-side default, so a repository-only
//! id-generation scheme would break the crate-wide "callers mint ids via
//! `RuntimeContext::ids`, repositories never do" convention).
//!
//! ## `apply_edit`'s `edit: &StatusEdit` argument (CONCERN — documented
//! judgment call)
//! design.md's Service Interface gives `apply_edit` a single `edit: &StatusEdit`
//! parameter (plus `id`/`now`) to do two things at once: (1) archive the
//! *pre*-edit content into a new `status_edits` row, and (2) write the *new*
//! content into the live `statuses` row. Since `StatusEdit`'s field shape
//! mirrors `status_edits`' columns exactly, only one shape is available to
//! carry the *new* content — so [`apply_edit`] reads this module's
//! `edit: &StatusEdit` argument as **the new content to apply**
//! (`edit.content`/`edit.spoiler_text`/`edit.sensitive`), reserving only
//! `edit.id` from that same argument as the **caller-minted primary key**
//! for the archived history row it creates internally. Concretely:
//! - `edit.content`/`edit.spoiler_text`/`edit.sensitive` -> written into the
//!   live `statuses` row (the "apply the edit" half).
//! - `edit.id` -> the primary key of the new `status_edits` row this
//!   function inserts to archive what was *previously* live before this call
//!   (the "save edit history" half) — the *content* stored in that archived
//!   row is read internally from the current `statuses` row, not from
//!   `edit`, since the whole point is to preserve what is about to be
//!   overwritten.
//! - `edit.status_id` is expected to equal this function's own `id`
//!   parameter (not separately consulted — `id` is the source of truth for
//!   both the `WHERE` target and the archived row's `status_id` column).
//! - `edit.created_at` is not consulted by this function at all: the
//!   archived row's `created_at` is derived internally as the current row's
//!   pre-edit "effective since" timestamp (`edited_at` if it was already
//!   edited before, else the original `created_at`) — not `edit.created_at`,
//!   which instead only has meaning on the *read* side ([`list_edits`]'s
//!   return values, where each returned `StatusEdit::created_at` is
//!   meaningful).
//!
//! This keeps design.md's literal 3-parameter shape intact rather than
//! inventing a 5th "new content" parameter design.md never lists, at the
//! cost of `edit`'s fields not all being consulted the same way — flagged
//! here, in `apply_edit`'s own doc comment, and in this task's status
//! report CONCERNS, per this task's "do not silently work around a mismatch"
//! instruction.
//!
//! ## Visible-scope filtering without `VisibilityPolicy` (CONCERN —
//! documented judgment call)
//! design.md's Responsibilities prose says `find_visible`'s visible scope
//! should reflect "`VisibilityPolicy` 判定" (task 3.1, a separate, equally
//! `(P)` — parallel, not depended-on — task with no `_Depends:_` edge from
//! this one). `VisibilityPolicy::is_visible` does not exist yet at this
//! task's implementation time, and even once it does, its `private` branch
//! needs a live `ViewerRelation` (a `RelationshipQuery::viewer_relation`
//! call — a social-graph delegation, entirely out of a data-layer
//! repository's boundary) and its `direct` branch needs a mentions/addressee
//! set this migration does not persist at all (no `mentions` table exists in
//! `migrations/0007_statuses.sql`). This module therefore implements its own
//! minimal, self-contained, **fail-closed** visibility rule ([`is_visible_to`]):
//! `public`/`unlisted` are visible to anyone (including an unauthenticated
//! `viewer: None`, satisfying Requirement 6.4's "未認証は公開のみ可視"); the
//! `private`/`direct` are visible only to the post's own author
//! (`viewer == Some(status.actor_id)`) — deliberately *narrower* than the
//! eventual full policy (which will additionally admit a `private` post's
//! followers and a `direct` post's mentioned recipients), never broader.
//! `StatusService` (task 5.x), once `VisibilityPolicy`/`RelationshipQuery`
//! exist and mentions are threaded through, is expected to layer additional
//! *inclusion* on top of what this repository already includes (author's own
//! posts) — this repository's rule is a safe, non-leaking subset, not a
//! wrong one, and satisfies this task's own Requirement 6.1/6.2/6.3/6.4
//! acceptance criteria as written (viewer-scoped visibility; unauthenticated
//! sees public/unlisted only; ancestors/descendants exclude what the viewer
//! cannot see).
//!
//! ## Delete's two explicit self-referential steps only (Requirement 7.4)
//! [`delete_status`] implements exactly the two steps design.md's
//! Responsibilities prose and this task's own text enumerate — (a) cascade
//! delete boost rows referencing the deleted status via `reblog_of_id`, (b)
//! decrement the parent's `replies_count` by 1 when the deleted status was
//! itself a reply — in one transaction. It deliberately does *not* also
//! decrement a boosted status's `reblogs_count` when one of its boost rows
//! is cascade-deleted here, nor a favourited status's `favourites_count`
//! when a `favourites` row disappears via the real FK `ON DELETE CASCADE`:
//! neither is named by design.md's Responsibilities prose or this task's own
//! instruction (which enumerates exactly "(a)...(b)..." and nothing else),
//! and adding it would be scope creep into counter-maintenance semantics
//! that belong to `InteractionService`'s own reblog/unreblog path (task
//! 2.2/5.x), not this repository's `delete_status`.
//!
//! ## Closing the double-delete race (run-scope review finding, fixed)
//! [`delete_status`] first `SELECT`s the target row's `in_reply_to_id` with
//! no lock, then later `DELETE`s the target row itself. Two concurrent
//! `delete_status(id)` calls for the *same* `id` (a realistic scenario: a
//! client retry, or duplicate federation `Delete` activity delivery for the
//! same status) can both complete that initial `SELECT` and observe the same
//! `in_reply_to_id` before either commits. Under Postgres READ COMMITTED,
//! their respective `DELETE FROM statuses WHERE id = $1` statements then
//! serialize on the row lock the first `DELETE` takes: the first to commit
//! actually removes the row; the second blocks, wakes once the first
//! commits, and — because READ COMMITTED re-evaluates the `WHERE` clause
//! against the now-current snapshot — finds zero matching rows (already
//! gone), so *its own* `DELETE` affects zero rows. Without checking that,
//! step (b)'s `replies_count` decrement would run unconditionally for both
//! calls, double-decrementing the parent for what was really only one actual
//! deletion.
//!
//! [`delete_status`] closes this using only the row lock Postgres already
//! takes for the `DELETE` itself — no additional explicit locking (e.g. a
//! `SELECT ... FOR UPDATE` on the target row) is needed, unlike
//! `poll_repository.rs::record_vote`'s analogous fix (see that module's own
//! "Closing the duplicate-vote race" doc comment), whose race could not be
//! closed by a plain `INSERT`'s own locking because a vote is a fresh row,
//! not a state transition on an existing one. Here the `DELETE FROM statuses
//! WHERE id = $1` result's `rows_affected()` is captured, and step (b) only
//! runs `if deleted.rows_affected() > 0` — i.e. only the call that actually
//! removed the row performs the parent decrement; a racer that lost the
//! `DELETE` (0 rows affected) skips it entirely. Verified with a genuine
//! concurrent-DB regression test (`tests.rs`,
//! `delete_status_serializes_concurrent_deletes_of_the_same_reply`) that
//! reproduces the double-decrement reliably without the fix and passes
//! reliably with it.
//!
//! ## Ordering of `ancestors`/`descendants`
//! `ancestors` returns oldest (root) first, immediate parent last — the
//! conventional "read top to bottom" thread order. `descendants` returns a
//! flattened, `created_at`-then-`id` ascending list of the full reply tree
//! rooted at `id` (not merely direct replies) — both traverse the full
//! `in_reply_to_id`/child chain regardless of per-node visibility (so a
//! visible great-grandchild is not lost behind an invisible parent reply),
//! filtering only the returned list per Requirement 6.3, and both guard
//! against a cyclic `in_reply_to_id` chain (which should never occur from
//! this repository's own writes, but is not assumed impossible) with a
//! visited-id set.
//!
//! ## Executor genericity
//! [`insert_status`], [`attach_media`] and [`adjust_counts`] are generic over
//! the executor they run against instead of taking a concrete `pool:
//! &PgPool`. `StatusService::create_status` must persist the post row, its
//! media attachments, its poll, its tags and the parent's `replies_count`
//! increment as **one** transaction — a genuine cross-statement atomicity
//! need these functions cannot serve while pinned to a concrete `&PgPool` (a
//! `PgPool` reference cannot join an already-open `sqlx::Transaction`). The
//! genericity is purely additive: `&PgPool` itself satisfies both bounds
//! used here, so **every existing call site compiles unchanged**; only a
//! transaction-bound caller passes `&mut *tx` instead.
//!
//! Two different sqlx bounds are used, chosen per function rather than
//! unified:
//! - Single-statement writers ([`insert_status`], [`adjust_counts`]) take
//!   `E: sqlx::PgExecutor<'e>`, this crate's established idiom (see
//!   [`fetch_raw`] here, and `social_graph/repository.rs::delete_follow` for
//!   the same precedent).
//! - [`attach_media`] loops one `INSERT` per `media_id`, and a `PgExecutor`
//!   is consumed by the single statement it drives. It therefore takes `A:
//!   sqlx::Acquire<'a, Database = Postgres>` and acquires the connection
//!   once, reusing the borrow across the loop — same emitted statements as
//!   before, no transaction of its own added (see its own doc comment).
//!
//! The other writers here ([`apply_edit`], [`delete_status`],
//! [`replace_media`], [`insert_mentions`], [`insert_remote_attachments`])
//! keep their concrete `&PgPool` parameter: no composite write currently
//! needs them inside a shared transaction, and converting them speculatively
//! would widen the change past its "purely additive" boundary.

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::api::db::map_server_error;
use crate::domain::{Id, Visibility};
use crate::error::AppError;
use crate::statuses::model::{Status, StatusEdit};

/// Postgres constraint name enforcing one AP object URI per post
/// (`migrations/0007_statuses.sql`'s `statuses.uri TEXT NOT NULL UNIQUE`,
/// implicit default constraint name for a single-column `UNIQUE`).
const URI_UNIQUE_CONSTRAINT: &str = "statuses_uri_key";

/// Maps a failed `INSERT INTO statuses` to an [`AppError`]: a unique
/// violation on [`URI_UNIQUE_CONSTRAINT`] becomes a caller-facing
/// (`ErrorKind::Client`) `409 Conflict`; anything else becomes a `Server`
/// (5xx) `AppError` — mirrors `actor/repository.rs::map_insert_error`'s
/// identical convention.
fn map_insert_error(source: sqlx::Error) -> AppError {
    if let Some(db_error) = source.as_database_error()
        && db_error.is_unique_violation()
        && db_error.constraint() == Some(URI_UNIQUE_CONSTRAINT)
    {
        return AppError::client(
            StatusCode::CONFLICT,
            "a status with this uri already exists",
        );
    }
    map_server_error(source)
}

/// Maps a [`Visibility`] to its `statuses.visibility` `TEXT` column
/// representation (`migrations/0007_statuses.sql`'s column comment:
/// `public/unlisted/private/direct`).
fn visibility_as_str(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Public => "public",
        Visibility::Unlisted => "unlisted",
        Visibility::Private => "private",
        Visibility::Direct => "direct",
    }
}

/// Reconstructs a [`Visibility`] from an already-persisted
/// `statuses.visibility` column value. Panics on any other value — such a
/// row could only exist if something wrote outside this module's own
/// [`visibility_as_str`] mapping, a data-corruption invariant violation, not
/// a normal error path (mirrors `actor/repository.rs::actor_type_from_str`'s
/// identical precedent).
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

/// A `statuses` row as read directly off the wire, before reconstructing its
/// typed [`Status`] form.
///
/// A `#[derive(sqlx::FromRow)]` struct rather than this module's usual
/// tuple-of-columns convention (see e.g. `actor/repository.rs::LocalActorRow`):
/// `Status` has 19 fields, one more than `sqlx`'s built-in `FromRow` tuple
/// impls go up to (16, `sqlx-core-0.9.0/src/from_row.rs`), so a tuple simply
/// does not compile here. Field order/names match [`status_columns!`]'s
/// column list exactly (`FromRow`'s derive matches by column name, so exact
/// declaration order is not load-bearing, but is kept aligned for
/// readability).
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

/// The column list every `SELECT` against `statuses` in this module shares,
/// matching [`StatusRow`]'s field set exactly. A `macro_rules!`-based
/// textual constant (not a `const &str`), mirroring
/// `accounts/profile_repository.rs::profile_columns!`'s identical precedent:
/// `sqlx::query_as`'s `SqlSafeStr` bound requires a `'static`-literal-shaped
/// query (rejecting a runtime-built `String`, e.g. from `format!`, as an
/// SQL-injection-auditing safeguard), so this is spliced into a `concat!`-
/// built literal at each call site rather than interpolated at runtime.
macro_rules! status_columns {
    () => {
        "id, actor_id, uri, url, content, visibility, sensitive, spoiler_text, in_reply_to_id, \
         in_reply_to_account_id, reblog_of_id, poll_id, language, reblogs_count, \
         favourites_count, replies_count, local, created_at, edited_at"
    };
}

/// Reconstructs a [`Status`] from a raw row.
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

/// This repository's own minimal, fail-closed visible-scope rule — see this
/// module's doc comment ("Visible-scope filtering without `VisibilityPolicy`")
/// for why it does not (yet) call a `VisibilityPolicy` component.
fn is_visible_to(status: &Status, viewer: Option<Id>) -> bool {
    match status.visibility {
        Visibility::Public | Visibility::Unlisted => true,
        Visibility::Private | Visibility::Direct => viewer == Some(status.actor_id),
    }
}

/// Fetches a `statuses` row by `id`, with no visibility filtering at all —
/// an internal building block for [`find_visible`]/[`ancestors`]/
/// [`descendants`] (which apply [`is_visible_to`] themselves after fetching)
/// and for [`delete_status`]/[`apply_edit`] (which need the current,
/// unfiltered row regardless of who is asking).
async fn fetch_raw<'e, E>(executor: E, id: Id) -> Result<Option<Status>, AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let row: Option<StatusRow> = sqlx::query_as(concat!(
        "SELECT ",
        status_columns!(),
        " FROM statuses WHERE id = $1"
    ))
    .bind(id.as_i64())
    .fetch_optional(executor)
    .await
    .map_err(map_server_error)?;

    Ok(row.map(row_to_status))
}

/// Fetches every `statuses` row directly replying to `parent_id`
/// (`in_reply_to_id = parent_id`), unfiltered by visibility — an internal
/// building block for [`descendants`].
async fn fetch_children(pool: &PgPool, parent_id: Id) -> Result<Vec<Status>, AppError> {
    let rows: Vec<StatusRow> = sqlx::query_as(concat!(
        "SELECT ",
        status_columns!(),
        " FROM statuses WHERE in_reply_to_id = $1 ORDER BY created_at, id"
    ))
    .bind(parent_id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows.into_iter().map(row_to_status).collect())
}

/// Persists `status` as a new `statuses` row (Requirements 3.1's insertion,
/// 3.5's reply relation columns, 3.6's language column — mention/tag/emoji
/// *extraction* itself is `StatusService`'s job, out of this repository's
/// boundary; this function only persists whatever `status` already carries).
///
/// A `statuses_uri_key` violation surfaces as a caller-facing (`ErrorKind::Client`)
/// `409 Conflict` rather than a generic 5xx — see [`map_insert_error`].
///
/// Generic over `executor` (this module's doc comment, "Executor genericity")
/// so `StatusService::create_status` can drive it against an open
/// `sqlx::Transaction` (`&mut *tx`); every pre-existing caller keeps passing
/// a bare `&PgPool` unchanged.
pub async fn insert_status<'e, E>(executor: E, status: &Status) -> Result<(), AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query(
        "INSERT INTO statuses ( \
             id, actor_id, uri, url, content, visibility, sensitive, spoiler_text, \
             in_reply_to_id, in_reply_to_account_id, reblog_of_id, poll_id, language, \
             reblogs_count, favourites_count, replies_count, local, created_at, edited_at \
         ) VALUES ( \
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19 \
         )",
    )
    .bind(status.id.as_i64())
    .bind(status.actor_id.as_i64())
    .bind(&status.uri)
    .bind(&status.url)
    .bind(&status.content)
    .bind(visibility_as_str(status.visibility))
    .bind(status.sensitive)
    .bind(&status.spoiler_text)
    .bind(status.in_reply_to_id.map(|id| id.as_i64()))
    .bind(status.in_reply_to_account_id.map(|id| id.as_i64()))
    .bind(status.reblog_of_id.map(|id| id.as_i64()))
    .bind(status.poll_id.map(|id| id.as_i64()))
    .bind(&status.language)
    .bind(status.reblogs_count)
    .bind(status.favourites_count)
    .bind(status.replies_count)
    .bind(status.local)
    .bind(status.created_at)
    .bind(status.edited_at)
    .execute(executor)
    .await
    .map_err(map_insert_error)?;

    Ok(())
}

/// Looks up the [`Status`] persisted under `id`, applying this repository's
/// visible-scope rule ([`is_visible_to`]) for `viewer` (Requirement 6.1).
///
/// Returns `Ok(None)` both when no row matches `id` at all and when a row
/// exists but is not visible to `viewer` — Requirement 6.1's "不可視・未存在
/// の投稿に対しては未検出を返す" deliberately does not distinguish the two
/// at this layer (a caller cannot learn "a private post exists here" from a
/// uniform 404-shaped response either way).
pub async fn find_visible(
    pool: &PgPool,
    id: Id,
    viewer: Option<Id>,
) -> Result<Option<Status>, AppError> {
    let status = fetch_raw(pool, id).await?;
    Ok(status.filter(|status| is_visible_to(status, viewer)))
}

/// Looks up the [`Status`] persisted under `id` with **no** visibility
/// filtering at all (a thin `pub` wrapper over [`fetch_raw`]).
///
/// Added by task 5.1 (`StatusService`): per this repository's own module doc
/// comment ("Visible-scope filtering without `VisibilityPolicy`"),
/// [`is_visible_to`] is a deliberately narrow, temporary stand-in for the
/// real policy (`visibility::is_visible`, task 3.1) — `StatusService`'s
/// `show`/`context`/`history`/`source`/delete/edit operations are the
/// "later task" that module doc comment names as the one meant to route
/// visibility decisions through the real policy instead. Doing that requires
/// the *unfiltered* row (so the real policy — which needs a resolved
/// `ViewerRelation` this data-layer module has no way to obtain, per that
/// same doc comment's reasoning — can decide, rather than having
/// [`is_visible_to`]'s narrower rule silently pre-reject something the real
/// policy would have admitted, e.g. a `private` post visible to the
/// author's follower). [`find_visible`]/[`ancestors`]/[`descendants`] above
/// are left entirely unmodified for any other/future caller that still
/// wants this repository's own bundled fail-closed filtering.
pub async fn find_by_id(pool: &PgPool, id: Id) -> Result<Option<Status>, AppError> {
    fetch_raw(pool, id).await
}

/// Looks up the [`Status`] persisted under ActivityPub object `uri`, with
/// **no** visibility filtering at all (a thin `pub` wrapper, mirroring
/// [`find_by_id`]'s identical "additive read-only helper" precedent).
///
/// Added by task 6.1 (`InboundHandlers`): inbound `Announce`/`Like`/`Delete`/
/// `Update`/`Undo` Activities, and a `Create(Note)`'s `inReplyTo`, all
/// reference their target by ActivityPub `uri` string, not by this crate's
/// internal [`Id`] — `statuses.uri`'s own `UNIQUE` constraint
/// (`migrations/0007_statuses.sql`) makes this lookup unambiguous. `Ok(None)`
/// (not an error) when no row matches `uri` — the caller (an inbound
/// handler) decides whether an unknown target is a safe no-op or a rejection.
pub async fn find_by_uri(pool: &PgPool, uri: &str) -> Result<Option<Status>, AppError> {
    let row: Option<StatusRow> = sqlx::query_as(concat!(
        "SELECT ",
        status_columns!(),
        " FROM statuses WHERE uri = $1"
    ))
    .bind(uri)
    .fetch_optional(pool)
    .await
    .map_err(map_server_error)?;

    Ok(row.map(row_to_status))
}

/// Returns the ancestor chain of `id` (the posts it replies to, transitively),
/// oldest (root) first, with **no** visibility filtering — the traversal
/// half of [`ancestors`], factored out so a caller needing the real
/// visibility policy (see [`find_by_id`]'s doc comment) can filter the raw
/// chain itself instead of going through [`is_visible_to`]. Added by task
/// 5.1; [`ancestors`] below is refactored to call this and then apply
/// [`is_visible_to`], an internal-only change that does not alter
/// [`ancestors`]'s own observable behavior or signature.
pub async fn ancestors_unfiltered(pool: &PgPool, id: Id) -> Result<Vec<Status>, AppError> {
    let mut chain = Vec::new();
    let mut visited: HashSet<Id> = HashSet::from([id]);

    let mut next = fetch_raw(pool, id).await?.and_then(|s| s.in_reply_to_id);
    while let Some(parent_id) = next {
        if !visited.insert(parent_id) {
            break; // cyclic in_reply_to_id chain guard
        }
        let Some(parent) = fetch_raw(pool, parent_id).await? else {
            break; // dangling logical reference: parent no longer exists
        };
        next = parent.in_reply_to_id;
        chain.push(parent);
    }

    chain.reverse(); // walked immediate-parent-first; root-first is wanted
    Ok(chain)
}

/// Returns the ancestor chain of `id` (the posts it replies to, transitively),
/// oldest (root) first, filtered to what `viewer` may see (Requirements 6.2,
/// 6.3) — see this module's doc comment ("Ordering of `ancestors`/
/// `descendants`") for the full ordering/traversal contract. Implemented as
/// [`ancestors_unfiltered`] plus this repository's own [`is_visible_to`]
/// filter — same observable behavior as before task 5.1's refactor.
pub async fn ancestors(pool: &PgPool, id: Id, viewer: Option<Id>) -> Result<Vec<Status>, AppError> {
    let chain = ancestors_unfiltered(pool, id).await?;
    Ok(chain
        .into_iter()
        .filter(|parent| is_visible_to(parent, viewer))
        .collect())
}

/// Returns the full descendant tree of `id` (every reply, transitively),
/// flattened and ordered `created_at`-then-`id` ascending, with **no**
/// visibility filtering — the traversal half of [`descendants`], factored
/// out for the same reason as [`ancestors_unfiltered`] (see [`find_by_id`]'s
/// doc comment). Added by task 5.1.
pub async fn descendants_unfiltered(pool: &PgPool, id: Id) -> Result<Vec<Status>, AppError> {
    let mut result = Vec::new();
    let mut visited: HashSet<Id> = HashSet::from([id]);
    let mut queue: VecDeque<Id> = VecDeque::from([id]);

    while let Some(current) = queue.pop_front() {
        for child in fetch_children(pool, current).await? {
            if !visited.insert(child.id) {
                continue; // cyclic in_reply_to_id chain guard
            }
            result.push(child.clone());
            queue.push_back(child.id);
        }
    }

    result.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    Ok(result)
}

/// Returns the full descendant tree of `id` (every reply, transitively),
/// flattened and ordered `created_at`-then-`id` ascending, filtered to what
/// `viewer` may see (Requirements 6.2, 6.3) — see this module's doc comment
/// ("Ordering of `ancestors`/`descendants`") for the full traversal
/// contract. Implemented as [`descendants_unfiltered`] plus this
/// repository's own [`is_visible_to`] filter — same observable behavior as
/// before task 5.1's refactor.
pub async fn descendants(
    pool: &PgPool,
    id: Id,
    viewer: Option<Id>,
) -> Result<Vec<Status>, AppError> {
    let all = descendants_unfiltered(pool, id).await?;
    Ok(all
        .into_iter()
        .filter(|child| is_visible_to(child, viewer))
        .collect())
}

/// Deletes the `statuses` row `id`, handling the two self-referential
/// cleanup steps a physical `ON DELETE CASCADE` cannot (Requirement 7.4) in
/// one transaction — see this module's doc comment ("Delete's two explicit
/// self-referential steps only") for exactly what is and is not in scope
/// here:
/// (a) any `statuses` row referencing `id` via `reblog_of_id` (a boost of
///     the deleted post) is itself deleted;
/// (b) if the deleted post was itself a reply (`in_reply_to_id` set), the
///     parent's `replies_count` is decremented by 1 (floored at 0).
///
/// Every other relation (`status_edits`/`status_media`/`favourites`/
/// `bookmarks`/`pins`/`polls`/`status_idempotency_keys`/`status_tags`) is
/// left to the real FK `ON DELETE CASCADE` `migrations/0007_statuses.sql`
/// already declares for it.
///
/// A no-op (`Ok(())`, no error) when `id` matches no row — mirrors this
/// crate's established "absence is not an error at this layer" convention
/// (e.g. `actor/repository.rs::update_state`).
pub async fn delete_status(pool: &PgPool, id: Id) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(map_server_error)?;

    let target: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT in_reply_to_id FROM statuses WHERE id = $1")
            .bind(id.as_i64())
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_server_error)?;

    let Some((in_reply_to_id,)) = target else {
        // Nothing to delete; roll back the (read-only so far) transaction
        // and report success, mirroring this layer's "absence is not an
        // error" convention.
        let _ = tx.rollback().await;
        return Ok(());
    };

    // (a) explicit cascade: delete boost rows referencing this status via
    // `reblog_of_id` (no physical FK covers this self-reference).
    sqlx::query("DELETE FROM statuses WHERE reblog_of_id = $1")
        .bind(id.as_i64())
        .execute(&mut *tx)
        .await
        .map_err(map_server_error)?;

    // Delete the target row itself — every same-spec FK referencing
    // `statuses(id)` carries `ON DELETE CASCADE`, so this alone also
    // removes this row's own `status_edits`/`status_media`/`favourites`/
    // `bookmarks`/`pins`/`polls`/`status_idempotency_keys`/`status_tags`
    // rows.
    //
    // The affected-row count is load-bearing, not incidental — see this
    // module's doc comment ("Closing the double-delete race") for why: it
    // distinguishes "this call actually removed the row" from "some
    // concurrent racer already removed it", which step (b) below needs to
    // avoid double-decrementing the parent's `replies_count`.
    let deleted = sqlx::query("DELETE FROM statuses WHERE id = $1")
        .bind(id.as_i64())
        .execute(&mut *tx)
        .await
        .map_err(map_server_error)?;

    // (b) explicit self-referential fixup: decrement the parent's
    // `replies_count` if the deleted post was itself a reply — but only if
    // this call actually removed the row (`deleted.rows_affected() > 0`).
    // Under READ COMMITTED, a concurrent racer that lost the DELETE above
    // (its `WHERE id = $1` matched zero rows once the winner's DELETE had
    // already committed and released the row lock) must not also decrement
    // the parent, or the same single deletion would be double-counted.
    if deleted.rows_affected() > 0
        && let Some(parent_id) = in_reply_to_id
    {
        sqlx::query(
            "UPDATE statuses SET replies_count = GREATEST(replies_count - 1, 0) WHERE id = $1",
        )
        .bind(parent_id)
        .execute(&mut *tx)
        .await
        .map_err(map_server_error)?;
    }

    tx.commit().await.map_err(map_server_error)?;
    Ok(())
}

/// Applies an edit to the `statuses` row `id`: archives what is currently
/// live into a new `status_edits` row, then overwrites the live row's
/// content/spoiler_text/sensitive and stamps `edited_at = now` (Requirements
/// 8.1, 8.2) — both in one transaction. See this module's doc comment
/// ("`apply_edit`'s `edit: &StatusEdit` argument") for exactly which of
/// `edit`'s fields feed which half of this operation.
///
/// Returns a caller-facing (`ErrorKind::Client`) `404 Not Found` if `id`
/// matches no row — unlike `delete_status`'s silent no-op, an edit request
/// against a nonexistent post is a genuine caller error the service layer
/// needs to distinguish (Requirement 8.5's "対象が要求アクターの所有でない
/// とき...未検出を返す" path ultimately routes through this).
pub async fn apply_edit(
    pool: &PgPool,
    id: Id,
    edit: &StatusEdit,
    now: OffsetDateTime,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(map_server_error)?;

    let Some(current) = fetch_raw(&mut *tx, id).await? else {
        let _ = tx.rollback().await;
        return Err(AppError::client(StatusCode::NOT_FOUND, "status not found"));
    };

    let archived_created_at = current.edited_at.unwrap_or(current.created_at);
    sqlx::query(
        "INSERT INTO status_edits (id, status_id, content, spoiler_text, sensitive, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(edit.id.as_i64())
    .bind(id.as_i64())
    .bind(&current.content)
    .bind(&current.spoiler_text)
    .bind(current.sensitive)
    .bind(archived_created_at)
    .execute(&mut *tx)
    .await
    .map_err(map_server_error)?;

    sqlx::query(
        "UPDATE statuses SET content = $1, spoiler_text = $2, sensitive = $3, edited_at = $4 \
         WHERE id = $5",
    )
    .bind(&edit.content)
    .bind(&edit.spoiler_text)
    .bind(edit.sensitive)
    .bind(now)
    .bind(id.as_i64())
    .execute(&mut *tx)
    .await
    .map_err(map_server_error)?;

    tx.commit().await.map_err(map_server_error)?;
    Ok(())
}

/// Returns every archived prior version of `id`'s content, oldest first
/// (Requirement 8.2's "履歴取得要求に対して各版...を返す"). Does not include
/// the currently-live version — that is available separately via
/// [`find_visible`]/[`fetch_raw`]; composing the two into a single
/// "full version list including current" view (if a caller wants that) is
/// `StatusService`'s job (task 5.x), out of this repository's boundary.
pub async fn list_edits(pool: &PgPool, id: Id) -> Result<Vec<StatusEdit>, AppError> {
    let rows: Vec<(i64, i64, String, String, bool, OffsetDateTime)> = sqlx::query_as(
        "SELECT id, status_id, content, spoiler_text, sensitive, created_at FROM status_edits \
         WHERE status_id = $1 ORDER BY created_at, id",
    )
    .bind(id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows
        .into_iter()
        .map(
            |(edit_id, status_id, content, spoiler_text, sensitive, created_at)| StatusEdit {
                id: Id::from_i64(edit_id),
                status_id: Id::from_i64(status_id),
                content,
                spoiler_text,
                sensitive,
                created_at,
            },
        )
        .collect())
}

/// Which `statuses` counter column [`adjust_counts`] targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountKind {
    Reblogs,
    Favourites,
    Replies,
}

/// Atomically adjusts `statuses.<kind's column>` by `delta` (positive or
/// negative) for `id`, floored at 0 so a race between two opposite-signed
/// adjustments (or an already-zero counter receiving a further decrement)
/// can never leave a negative count. `delta`/target column are combined in
/// a single `UPDATE ... SET col = GREATEST(col + $delta, 0)` — never a
/// read-modify-write pair of separate queries — so concurrent adjustments
/// serialize correctly at the database's own row-lock level rather than
/// racing at the application level.
///
/// One of three fixed, literal `UPDATE` statements is selected by matching
/// on the closed `CountKind` enum, rather than splicing a column name into
/// one dynamically-built query string: `sqlx`'s `SqlSafeStr` bound rejects a
/// runtime-built query string outright (see [`status_columns!`]'s doc
/// comment for the same constraint), and three static literals are no less
/// clear than one templated one for a 3-variant enum.
///
/// A no-op (`Ok(())`, no error) when `id` matches no row — same "absence is
/// not an error at this layer" convention as [`delete_status`].
///
/// Generic over `executor` (this module's doc comment, "Executor
/// genericity") so `StatusService`/`InteractionService` can drive it against
/// an open `sqlx::Transaction` (`&mut *tx`) alongside the row write whose
/// counter it adjusts; every pre-existing caller keeps passing a bare
/// `&PgPool` unchanged.
pub async fn adjust_counts<'e, E>(
    executor: E,
    id: Id,
    kind: CountKind,
    delta: i64,
) -> Result<(), AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let query = match kind {
        CountKind::Reblogs => {
            "UPDATE statuses SET reblogs_count = GREATEST(reblogs_count + $1, 0) WHERE id = $2"
        }
        CountKind::Favourites => {
            "UPDATE statuses SET favourites_count = GREATEST(favourites_count + $1, 0) \
             WHERE id = $2"
        }
        CountKind::Replies => {
            "UPDATE statuses SET replies_count = GREATEST(replies_count + $1, 0) WHERE id = $2"
        }
    };
    sqlx::query(query)
        .bind(delta)
        .bind(id.as_i64())
        .execute(executor)
        .await
        .map_err(map_server_error)?;

    Ok(())
}

// -- status_media (added by task 5.1, `StatusService`) ----------------------
//
// `migrations/0007_statuses.sql`'s own `status_media` table ("投稿への添付
// (media-pipelineのmediaを論理参照)") has never had a writer anywhere in
// this crate before task 5.1: task 2.1's own Service Interface (design.md
// lines 391-399) enumerates exactly six functions, none of them touching
// `status_media`, and no sibling repository claims it either. `StatusService
// ::create_status`/`edit_status` (Requirements 3.4, 8.1) is the first real
// need for persisting a post's attached media, so these three functions are
// added here — the natural home, alongside `insert_status`/`apply_edit`,
// since `status_media` rows share `statuses`' own lifecycle (its `ON DELETE
// CASCADE` is already declared against `statuses(id)`, so `delete_status`
// needs no further change).

/// Persists `status_id`'s attached media as new `status_media` rows, in
/// `media_ids`' given order (`position` 0-based) — Requirement 3.4. Callers
/// are responsible for having already verified each `media_id`'s ownership
/// (`media_repository::find_owned`, media-pipeline's own contract) before
/// calling this; this function only persists the association.
///
/// Generic over `executor` (this module's doc comment, "Executor
/// genericity"); every pre-existing caller keeps passing a bare `&PgPool`
/// unchanged. Unlike this module's single-statement writers the bound here is
/// [`sqlx::Acquire`], not `sqlx::PgExecutor`: a `PgExecutor` is consumed by
/// the single `execute` it drives, which cannot serve this function's
/// per-`media_id` loop. Acquiring once and reusing the borrowed connection
/// keeps the emitted statements byte-identical to the pool-driven loop this
/// replaced (this is deliberately *not* wrapped in a transaction of its own —
/// the previous pool-driven behavior had none, and a transactional caller
/// supplies the enclosing one).
///
/// Callers that already hold an open transaction use
/// [`attach_media_on_conn`] instead — see its own doc comment for why the
/// generic form cannot serve them.
pub async fn attach_media<'a, A>(
    executor: A,
    status_id: Id,
    media_ids: &[Id],
) -> Result<(), AppError>
where
    A: sqlx::Acquire<'a, Database = sqlx::Postgres>,
{
    let mut conn = executor.acquire().await.map_err(map_server_error)?;
    attach_media_on_conn(&mut conn, status_id, media_ids).await
}

/// [`attach_media`] against an already-acquired connection — the variant
/// `StatusService::create_status` uses to run this loop inside its own open
/// transaction. Identical statements, identical order; the only difference
/// is that the connection comes from the caller.
///
/// This concrete-`&mut PgConnection` variant exists because the generic
/// [`sqlx::Acquire`] form, while callable from a transaction, yields
/// a future that cannot be *proven* `Send`: the value it holds across awaits
/// has type `<A as Acquire>::Connection`, and when `A` is substituted with a
/// reborrow like `&mut *tx` the auto-trait leak check universalizes over that
/// borrow's region and reports "implementation of `sqlx::Acquire` is not
/// general enough". An axum handler requires a `Send` future, so
/// `create_status` could not otherwise call it (`insert_poll` is unaffected:
/// it holds a concrete `sqlx::Transaction`, not an associated type). Adding
/// this variant is purely additive — [`attach_media`]'s own signature,
/// statements, and every existing caller are untouched.
pub async fn attach_media_on_conn(
    conn: &mut sqlx::PgConnection,
    status_id: Id,
    media_ids: &[Id],
) -> Result<(), AppError> {
    for (position, media_id) in media_ids.iter().enumerate() {
        sqlx::query("INSERT INTO status_media (status_id, media_id, position) VALUES ($1, $2, $3)")
            .bind(status_id.as_i64())
            .bind(media_id.as_i64())
            .bind(position as i32)
            .execute(&mut *conn)
            .await
            .map_err(map_server_error)?;
    }
    Ok(())
}

/// Replaces `status_id`'s entire attached-media set with `media_ids`, in the
/// given order (Requirement 8.1's "メディア...の変更" on edit): deletes every
/// existing `status_media` row for `status_id`, then inserts `media_ids`
/// fresh, both in one transaction. An empty `media_ids` clears all
/// attachments.
pub async fn replace_media(pool: &PgPool, status_id: Id, media_ids: &[Id]) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(map_server_error)?;

    sqlx::query("DELETE FROM status_media WHERE status_id = $1")
        .bind(status_id.as_i64())
        .execute(&mut *tx)
        .await
        .map_err(map_server_error)?;

    for (position, media_id) in media_ids.iter().enumerate() {
        sqlx::query("INSERT INTO status_media (status_id, media_id, position) VALUES ($1, $2, $3)")
            .bind(status_id.as_i64())
            .bind(media_id.as_i64())
            .bind(position as i32)
            .execute(&mut *tx)
            .await
            .map_err(map_server_error)?;
    }

    tx.commit().await.map_err(map_server_error)?;
    Ok(())
}

/// Returns `status_id`'s attached media ids, in attachment order
/// (Requirement 3.4/8.1's read side).
pub async fn media_ids_for_status(pool: &PgPool, status_id: Id) -> Result<Vec<Id>, AppError> {
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT media_id FROM status_media WHERE status_id = $1 ORDER BY position")
            .bind(status_id.as_i64())
            .fetch_all(pool)
            .await
            .map_err(map_server_error)?;

    Ok(rows.into_iter().map(|(id,)| Id::from_i64(id)).collect())
}

/// The batched form of [`media_ids_for_status`]: resolves every id in
/// `status_ids` in one query instead of one query per status, so a list
/// endpoint's media lookups stop scaling with the number of statuses it
/// returns.
///
/// Equivalent to calling [`media_ids_for_status`] once per id, by
/// construction: same table, same `WHERE` scoping (`status_id` only — this
/// association carries no viewer/visibility dimension for either function to
/// disagree about), and the same `ORDER BY position` within each status. The
/// leading `status_id` in the `ORDER BY` only groups each status's rows
/// together; it cannot reorder rows *within* one status, which is the
/// ordering [`media_ids_for_status`] actually promises.
///
/// A status with no attached media has **no entry** in the returned map
/// rather than an empty `Vec` (the returned keys are the subset of
/// `status_ids` that have at least one attachment) — callers should read a
/// miss as the empty attachment list [`media_ids_for_status`] returns for
/// that same id. An empty `status_ids` returns an empty map without issuing
/// a query at all: `= ANY` on an empty array would match nothing anyway, so
/// the round trip would be pure cost.
pub async fn media_ids_for_statuses(
    pool: &PgPool,
    status_ids: &[Id],
) -> Result<HashMap<Id, Vec<Id>>, AppError> {
    if status_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let raw_ids: Vec<i64> = status_ids.iter().map(|id| id.as_i64()).collect();
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT status_id, media_id FROM status_media \
         WHERE status_id = ANY($1::bigint[]) ORDER BY status_id, position",
    )
    .bind(&raw_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    let mut by_status: HashMap<Id, Vec<Id>> = HashMap::new();
    for (status_id, media_id) in rows {
        by_status
            .entry(Id::from_i64(status_id))
            .or_default()
            .push(Id::from_i64(media_id));
    }

    Ok(by_status)
}

// -- status_mentions / status_remote_attachments (added by task 10.3,
// `InboundHandlers, StatusIngestService` — Requirement 14.2's "添付・メンショ
// ンを反映する", closing the gap task 6.1/6.2's own doc comments flagged) --
//
// `migrations/0011_status_mentions_and_remote_attachments.sql` (added by this
// same task) has never had a writer anywhere in this crate before now — see
// that migration's own doc comment for the full schema/scoping rationale
// (why `status_remote_attachments` stores plain metadata rather than a real
// media-pipeline `Media`/`status_media` row, and why `status_mentions` only
// ever holds locally-resolved actor ids). These four functions are added
// here, alongside `attach_media`/`replace_media`/`media_ids_for_status`
// above, since both new tables share `statuses`' own lifecycle (`ON DELETE
// CASCADE` against `statuses(id)` — `delete_status` needs no further
// change).

/// One reflected entry from a remote `Note`'s `attachment` property
/// (`inbound_handlers.rs::extract_attachments`), stored as lightweight,
/// statuses-core-owned metadata — see
/// `migrations/0011_status_mentions_and_remote_attachments.sql`'s own doc
/// comment for why this is a plain URL/type/description tuple rather than a
/// real media-pipeline `Media` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAttachment {
    pub url: String,
    pub media_type: Option<String>,
    pub description: Option<String>,
}

/// Persists `status_id`'s resolved-local mentioned actor ids as new
/// `status_mentions` rows (Requirement 14.2). `ON CONFLICT DO NOTHING`: a
/// `Note`'s `tag` array naming the same local actor twice (already
/// deduplicated by `extract_tag_mentions`, but defensively covered here too)
/// or a caller retrying this call is a safe no-op, matching this table's own
/// `(status_id, actor_id)` primary key.
pub async fn insert_mentions(
    pool: &PgPool,
    status_id: Id,
    actor_ids: &[Id],
) -> Result<(), AppError> {
    for actor_id in actor_ids {
        sqlx::query(
            "INSERT INTO status_mentions (status_id, actor_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(status_id.as_i64())
        .bind(actor_id.as_i64())
        .execute(pool)
        .await
        .map_err(map_server_error)?;
    }
    Ok(())
}

/// Returns `status_id`'s persisted, locally-resolved mentioned actor ids
/// (Requirement 14.2's read side), in ascending `actor_id` order (this
/// table's own primary-key order — no separate ordering column exists, and
/// `tag`-array order is not preserved by design, mirroring
/// `status_service.rs::extract_content_tokens`'s own "dedup by exact token
/// text" precedent for mentions being a set, not a sequence).
pub async fn mentioned_actor_ids(pool: &PgPool, status_id: Id) -> Result<Vec<Id>, AppError> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT actor_id FROM status_mentions WHERE status_id = $1 ORDER BY actor_id",
    )
    .bind(status_id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows.into_iter().map(|(id,)| Id::from_i64(id)).collect())
}

/// Persists `status_id`'s reflected remote attachments as new
/// `status_remote_attachments` rows, in `attachments`' given order
/// (`position` 0-based) — Requirement 14.2.
pub async fn insert_remote_attachments(
    pool: &PgPool,
    status_id: Id,
    attachments: &[RemoteAttachment],
) -> Result<(), AppError> {
    for (position, attachment) in attachments.iter().enumerate() {
        sqlx::query(
            "INSERT INTO status_remote_attachments \
             (status_id, position, url, media_type, description) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(status_id.as_i64())
        .bind(position as i32)
        .bind(&attachment.url)
        .bind(&attachment.media_type)
        .bind(&attachment.description)
        .execute(pool)
        .await
        .map_err(map_server_error)?;
    }
    Ok(())
}

/// Returns `status_id`'s reflected remote attachments, in attachment order
/// (Requirement 14.2's read side).
pub async fn remote_attachments_for_status(
    pool: &PgPool,
    status_id: Id,
) -> Result<Vec<RemoteAttachment>, AppError> {
    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT url, media_type, description FROM status_remote_attachments \
         WHERE status_id = $1 ORDER BY position",
    )
    .bind(status_id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows
        .into_iter()
        .map(|(url, media_type, description)| RemoteAttachment {
            url,
            media_type,
            description,
        })
        .collect())
}

// -- account-scoped listing (added by task 9.1, `AccountStatusesProviderImpl`) --
//
// accounts-and-instance's `AccountStatusesProvider`/`AccountCountsProvider`
// ports (`crate::accounts::ports`, task 1.3) need, respectively, "every post
// authored by one actor, newest first" and "how many posts has this actor
// authored, and when was the latest one" — neither existed anywhere in this
// module before this task (task 2.1's own six-function Service Interface
// list has no per-actor listing query; `statuses_actor_idx`,
// `migrations/0007_statuses.sql`, was created for exactly this future need
// per that migration's own column comment: "投稿者...一意インデックス...
// 支援 actor-scoped listing"). `list_by_actor` fetches the *unfiltered*
// candidate set (no `pinned`/`only_media`/`exclude_replies`/`exclude_reblogs`/
// visibility filtering at the SQL layer) — mirrors this module's/
// `interaction_repository.rs::list_bookmarks`'s own established "fetch the
// full matching set, filter/paginate in Rust" convention, since per-viewer
// visibility filtering (`crate::statuses::visibility::is_visible`) cannot be
// expressed as a `WHERE` clause at all (it needs a resolved
// `ViewerRelation`), so pushing the other four filters into SQL as well
// would only leave that one, most-important filter still applied
// application-side — `crate::statuses::account_provider` is the one caller,
// applying every filter (visibility included) uniformly, in Rust, over this
// function's result.

/// Every `statuses` row authored by `actor_id`, newest-id-first, entirely
/// unfiltered (no visibility/`pinned`/`only_media`/`exclude_replies`/
/// `exclude_reblogs` filtering — see this section's own doc comment for why
/// that is `crate::statuses::account_provider`'s job, not this repository's).
pub async fn list_by_actor(pool: &PgPool, actor_id: Id) -> Result<Vec<Status>, AppError> {
    let rows: Vec<StatusRow> = sqlx::query_as(concat!(
        "SELECT ",
        status_columns!(),
        " FROM statuses WHERE actor_id = $1 ORDER BY id DESC"
    ))
    .bind(actor_id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows.into_iter().map(row_to_status).collect())
}

/// The total number of `statuses` rows authored by `actor_id` — the
/// `AccountCountsProvider` port's `statuses_count` (design.md's Boundary
/// Commitments: "`AccountCountsProvider` へ `statuses_count`...を供給する"),
/// a raw, unfiltered total (not scoped to any particular viewer) — matching
/// `followers`/`following`'s own already-established "instance-wide raw
/// count" convention (`crate::accounts::ports::AccountCountsProvider`'s own
/// doc comment).
pub async fn count_for_actor(pool: &PgPool, actor_id: Id) -> Result<i64, AppError> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM statuses WHERE actor_id = $1")
        .bind(actor_id.as_i64())
        .fetch_one(pool)
        .await
        .map_err(map_server_error)?;

    Ok(count)
}

/// The most recent `created_at` among `actor_id`'s `statuses` rows, or
/// `None` when `actor_id` has authored none — the `AccountCountsProvider`
/// port's `last_status_at`.
pub async fn last_created_at_for_actor(
    pool: &PgPool,
    actor_id: Id,
) -> Result<Option<OffsetDateTime>, AppError> {
    let (last,): (Option<OffsetDateTime>,) =
        sqlx::query_as("SELECT MAX(created_at) FROM statuses WHERE actor_id = $1")
            .bind(actor_id.as_i64())
            .fetch_one(pool)
            .await
            .map_err(map_server_error)?;

    Ok(last)
}
