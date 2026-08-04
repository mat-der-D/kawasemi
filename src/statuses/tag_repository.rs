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
//! this module's five functions are this task's own construction, scoped
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
//! - [`tags_for_status`]: the status -> tag read direction.
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
pub async fn upsert_tag(pool: &PgPool, tag: &Tag) -> Result<Tag, AppError> {
    let row: TagRow = sqlx::query_as(
        "INSERT INTO tags (id, name, created_at) VALUES ($1, $2, $3) \
         ON CONFLICT (name) DO UPDATE SET name = tags.name \
         RETURNING id, name, created_at",
    )
    .bind(tag.id.as_i64())
    .bind(&tag.name)
    .bind(tag.created_at)
    .fetch_one(pool)
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
pub async fn associate_tag(pool: &PgPool, status_id: Id, tag_id: Id) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO status_tags (status_id, tag_id) VALUES ($1, $2) \
         ON CONFLICT (status_id, tag_id) DO NOTHING",
    )
    .bind(status_id.as_i64())
    .bind(tag_id.as_i64())
    .execute(pool)
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
