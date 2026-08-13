//! `TagRepository` (design.md Boundary Commitments: "ハッシュタグの永続化
//! （`tags` / `status_tags` 関連テーブル）と、タグ関連付けの照会可能な読み取
//! り境界（タグ→投稿関連付け／投稿→タグの取得）"; Requirement 3.6; task 2.1,
//! `Boundary: StatusRepository, TagRepository`): hashtag persistence and its
//! read boundary against `tags` / `status_tags`
//! (`migrations/0007_statuses.sql`, already applied, unmodified by this
//! task).
//!
//! Scope: design.md's Service Interface block does not itemize a concrete
//! signature list for this component the way it does for `StatusRepository`
//! (it is described only in prose, in the Boundary Commitments section) —
//! this module's functions are that task's own construction, scoped
//! tightly to what that prose commits to and what this task's own
//! instruction asks for ("`tags` / `status_tags` へのタグ関連付け永続化と、
//! 照会可能な読み取り境界（タグ→投稿・投稿→タグ）"):
//! - [`upsert_tag`]: idempotent "insert or fetch existing" against
//!   `tags_name_unique`, so extracting the same hashtag from two different
//!   posts' content (a later task's job — hashtag *extraction* itself is
//!   `StatusService::create_status`'s responsibility, out of this
//!   repository's boundary) never creates two `tags` rows for the same
//!   normalized name.
//! - [`find_tag_by_name`]: a plain read lookup by normalized name.
//! - [`associate_tag`]: persists one `status_tags` edge, deduplicated by
//!   the table's own `(status_id, tag_id)` primary key.
//! - [`tags_for_status`]: the status -> tag read direction, plus its batched
//!   form [`tags_for_statuses`] (added later, so a list endpoint's tag
//!   lookups do not scale with the number of statuses it renders).
//! - [`status_ids_for_tag`]: the tag -> status read direction (downstream:
//!   timelines' tag timeline, search's hashtag index, per this task's own
//!   instruction). Returns `Id`s rather than full `Status` rows deliberately
//!   — a tag timeline/hashtag index still needs to run each candidate
//!   through its own visibility/pagination logic before rendering, so
//!   handing back bare ids keeps this repository from presuming how a
//!   downstream spec wants to hydrate/filter/paginate them (`StatusRepository::find_visible`
//!   already exists in the sibling module for that hydration step, one id
//!   at a time or batched by the caller).
//!
//! Like every other read in this crate's repositories, none of these apply
//! visibility filtering themselves (`is_visible_to`, in the sibling
//! `status_repository` module, is a `StatusRepository`-only concern) —
//! design.md's own Out-of-Boundary note ("タイムライン集約...の業務ロジック
//! を本 spec に持ち込まない") keeps that downstream-consumer responsibility
//! out of this module.

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::api::db::map_server_error;
use crate::domain::Id;
use crate::error::AppError;
use crate::statuses::model::Tag;

type TagRow = (i64, String, OffsetDateTime);

fn row_to_tag(row: TagRow) -> Tag {
    let (id, name, created_at) = row;
    Tag {
        id: Id::from_i64(id),
        name,
        created_at,
    }
}

/// Inserts `tag` as a new `tags` row, or — if a row with the same
/// (normalized) `tag.name` already exists (`tags_name_unique`) — returns
/// that existing row unchanged, `tag.id`/`tag.created_at` discarded.
///
/// Implemented as a single `INSERT ... ON CONFLICT (name) DO UPDATE ...
/// RETURNING` (a self-assigning, effectively no-op `UPDATE`), never a
/// separate `SELECT`-then-`INSERT` pair: that would race under concurrent
/// callers extracting the same hashtag from two different posts at once
/// (mirrors `accounts/remote_repository.rs::upsert_remote`'s identical
/// "`id` stability across re-upserts" discipline — the first upsert for a
/// given `name` establishes that row's `id` permanently, exactly like
/// `upsert_remote`'s `actor_uri`).
///
/// Generic over `executor` so `StatusService::create_status` can drive it
/// against an open `sqlx::Transaction` (`&mut *tx`) together with the post
/// insertion whose hashtags it registers; every pre-existing caller keeps
/// passing a bare `&PgPool` unchanged.
pub async fn upsert_tag<'e, E>(executor: E, tag: &Tag) -> Result<Tag, AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    let row: TagRow = sqlx::query_as(
        "INSERT INTO tags (id, name, created_at) VALUES ($1, $2, $3) \
         ON CONFLICT (name) DO UPDATE SET name = tags.name \
         RETURNING id, name, created_at",
    )
    .bind(tag.id.as_i64())
    .bind(&tag.name)
    .bind(tag.created_at)
    .fetch_one(executor)
    .await
    .map_err(map_server_error)?;

    Ok(row_to_tag(row))
}

/// Looks up the [`Tag`] persisted under the exact (normalized) `name`, if
/// any. Returns `Ok(None)` (not an error) when no row matches — mirrors
/// `actor/repository.rs::find_by_handle`'s "does this exist" contract.
pub async fn find_tag_by_name(pool: &PgPool, name: &str) -> Result<Option<Tag>, AppError> {
    let row: Option<TagRow> =
        sqlx::query_as("SELECT id, name, created_at FROM tags WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await
            .map_err(map_server_error)?;

    Ok(row.map(row_to_tag))
}

/// Persists one `status_tags` association between `status_id` and `tag_id`.
/// Idempotent: associating the same pair twice is a silent no-op
/// (`ON CONFLICT (status_id, tag_id) DO NOTHING`), matching `status_tags`'
/// own composite primary key's dedup guarantee.
///
/// Generic over `executor` for the same reason as [`upsert_tag`] — every
/// pre-existing caller keeps passing a bare `&PgPool` unchanged.
pub async fn associate_tag<'e, E>(executor: E, status_id: Id, tag_id: Id) -> Result<(), AppError>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query(
        "INSERT INTO status_tags (status_id, tag_id) VALUES ($1, $2) \
         ON CONFLICT (status_id, tag_id) DO NOTHING",
    )
    .bind(status_id.as_i64())
    .bind(tag_id.as_i64())
    .execute(executor)
    .await
    .map_err(map_server_error)?;

    Ok(())
}

/// Returns every [`Tag`] associated with `status_id` (the status -> tag read
/// direction), ordered by `tags.id` for a stable, deterministic result.
pub async fn tags_for_status(pool: &PgPool, status_id: Id) -> Result<Vec<Tag>, AppError> {
    let rows: Vec<TagRow> = sqlx::query_as(
        "SELECT tags.id, tags.name, tags.created_at FROM tags \
         INNER JOIN status_tags ON status_tags.tag_id = tags.id \
         WHERE status_tags.status_id = $1 ORDER BY tags.id",
    )
    .bind(status_id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows.into_iter().map(row_to_tag).collect())
}

/// The batched form of [`tags_for_status`]: resolves every id in
/// `status_ids` in one query instead
/// of one query per status, so a list endpoint's tag lookups stop scaling
/// with the number of statuses it returns.
///
/// Equivalent to calling [`tags_for_status`] once per id, by construction:
/// same join, same `WHERE` scoping (`status_tags.status_id` only — this
/// association carries no viewer/visibility dimension for either function to
/// disagree about), and the same `ORDER BY tags.id` within each status. The
/// leading `status_tags.status_id` in the `ORDER BY` only groups each
/// status's rows together; it cannot reorder rows *within* one status, which
/// is the ordering [`tags_for_status`] actually promises.
///
/// A status with no associated tags has **no entry** in the returned map
/// rather than an empty `Vec` (the returned keys are the subset of
/// `status_ids` carrying at least one tag) — callers should read a miss as
/// the empty tag list [`tags_for_status`] returns for that same id, which
/// also means an id with no `statuses` row at all is not distinguished from
/// an existing but untagged one. An empty `status_ids` returns an empty map
/// without issuing a query at all: `= ANY` on an empty array would match
/// nothing anyway, so the round trip would be pure cost.
pub async fn tags_for_statuses(
    pool: &PgPool,
    status_ids: &[Id],
) -> Result<HashMap<Id, Vec<Tag>>, AppError> {
    if status_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let raw_ids: Vec<i64> = status_ids.iter().map(|id| id.as_i64()).collect();
    let rows: Vec<(i64, i64, String, OffsetDateTime)> = sqlx::query_as(
        "SELECT status_tags.status_id, tags.id, tags.name, tags.created_at FROM tags \
         INNER JOIN status_tags ON status_tags.tag_id = tags.id \
         WHERE status_tags.status_id = ANY($1::bigint[]) \
         ORDER BY status_tags.status_id, tags.id",
    )
    .bind(&raw_ids)
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    let mut by_status: HashMap<Id, Vec<Tag>> = HashMap::new();
    for (status_id, id, name, created_at) in rows {
        by_status
            .entry(Id::from_i64(status_id))
            .or_default()
            .push(row_to_tag((id, name, created_at)));
    }

    Ok(by_status)
}

/// Returns the `Id` of every status associated with `tag_id` (the tag ->
/// status read direction consumed by timelines' tag timeline / search's
/// hashtag index, per this task's own instruction), newest first
/// (`status_id DESC` — status ids are monotonically increasing generation-
/// time `Id`s, so this is also newest-first chronologically, the
/// conventional tag-timeline order).
pub async fn status_ids_for_tag(pool: &PgPool, tag_id: Id) -> Result<Vec<Id>, AppError> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT status_id FROM status_tags WHERE tag_id = $1 ORDER BY status_id DESC",
    )
    .bind(tag_id.as_i64())
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows.into_iter().map(|(id,)| Id::from_i64(id)).collect())
}
