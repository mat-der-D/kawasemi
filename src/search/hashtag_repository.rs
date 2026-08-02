//! `HashtagIndexRepository` (design.md "Data / データ層" ->
//! `HashtagIndexRepository / HashtagIndexer`, Requirements 5.1, 5.2, 5.5,
//! 8.2; task 2.1, `Boundary: HashtagIndexRepository`): read/upsert access to
//! this spec's own hashtag read index (`search_tags` / `search_status_tags`)
//! and the `HashtagIndexer` derivation cursor (`search_index_watermark`),
//! `migrations/0013_search.sql` (already applied, unmodified by this task).
//!
//! Scope: this module owns exactly the four functions design.md's own
//! Service Interface excerpt (lines ~378-383) and this task's own
//! instruction name — [`match_hashtags`], [`upsert_tag_usage`],
//! [`load_watermark`], [`save_watermark`] — against `search_tags` /
//! `search_status_tags` / `search_index_watermark` only. It never reads or
//! writes `statuses`/`tags`/`status_tags` (statuses-core's own tables) —
//! deriving hashtags from upstream posts read-only is `HashtagIndexer`'s job
//! (task 2.2, `src/search/hashtag_indexer.rs`, strictly downstream of and
//! outside this module's boundary), and `PgSearchBackend`'s account/status
//! matching is task 3.1's job. This module also does not build `SearchBackend`
//! `TagMatch` values or wire into `ports.rs`'s `SearchBackend` trait (task
//! 3.2's job, `pg_backend.rs`) — it returns [`TagView`] directly, per
//! design.md's own literal `match_hashtags` signature.
//!
//! ## `upsert_tag_usage`'s extra `new_tag_id: Id` parameter (deviates from
//! design.md's literal 4-parameter signature)
//! design.md's Service Interface excerpt writes `upsert_tag_usage(pool: &PgPool,
//! name: &str, status_id: Id, now: OffsetDateTime) -> Result<(), AppError>`
//! — no id parameter. But this crate's established convention for every
//! sibling repository that upserts a row whose primary key is app-minted
//! (never a DB default) is that the *caller* mints the candidate `Id` via
//! the core-runtime `IdGenerator` DI boundary and hands it to the repository
//! already set on the domain value being upserted — see
//! `crate::statuses::tag_repository::upsert_tag(pool, tag: &Tag)` (`tag.id`
//! pre-minted) and `crate::accounts::remote_repository::upsert_remote(pool,
//! account: &RemoteAccount)` (`account.id` pre-minted), both of whose own doc
//! comments document "id stability across re-upserts": the *first* upsert
//! for a given unique key establishes that row's `id` permanently, and every
//! later re-upsert for the same key keeps that original `id` regardless of
//! what candidate `id` the caller supplies. `search_tags.id` is `BIGINT
//! PRIMARY KEY` with no `SERIAL`/`IDENTITY` default
//! (`migrations/0013_search.sql`'s own doc comment: "identifiers are always
//! minted by the application's own core-runtime `IdGenerator` boundary,
//! never by the database"), so [`upsert_tag_usage`] needs *some* way to
//! supply a fresh `id` for the brand-new-tag-row case. Rather than reaching
//! for `crate::runtime::IdGenerator` inside this repository function itself
//! (no sibling repository does that — the DI boundary is always threaded in
//! by the *caller*, not resolved internally), this function takes an extra
//! `new_tag_id: Id` parameter: the candidate id to use *only if* `name`
//! turns out to be new. If `name` already has a `search_tags` row, the
//! supplied `new_tag_id` is silently discarded and the existing row's id is
//! kept (identical discipline to `upsert_tag`/`upsert_remote`). Flagged as a
//! CONCERN in this task's status report for reviewer confirmation, mirroring
//! `accounts/remote_repository.rs`'s own precedent for documenting a
//! literal-signature-vs-established-convention resolution inline.
//!
//! ## `match_hashtags`'s prefix-match predicate (`LIKE term || '%'`)
//! design.md's prose calls this "名前の前方/部分一致" (prefix/partial name
//! match). `migrations/0013_search.sql`'s own doc comment ties the concrete
//! mechanism directly to `search_tags_name_idx`, a standard-PostgreSQL
//! `text_pattern_ops` btree index that only accelerates `LIKE 'prefix%'`
//! (Requirement 8.1/8.4's "拡張不要" prefix-match mechanism) — a bare
//! substring `LIKE '%term%'` cannot use that index at all. This module
//! therefore implements the "前方一致" (prefix) half literally, via `name
//! LIKE $1` with `$1` built as `lower(term) || '%'`; `search_tags.name` is
//! itself stored pre-normalized/lowercased (migration doc: "正規化ハッシュ
//! タグ名（小文字化等）"), so lowercasing the incoming `term` before binding
//! keeps the comparison case-insensitive without defeating the index (a SQL
//! `LOWER(name) LIKE ...` wrapper would prevent the plain btree index from
//! being used at all).
//!
//! ## `TagView::url`: a domain-relative path, not an absolute URL
//! design.md's Physical Data Model describes `search_tags.name` as "url 構築
//! 元" (the source material a tag URL is built from), but this module's own
//! `match_hashtags` signature (design.md's literal Service Interface) takes
//! no server-domain/origin parameter — every other place in this crate that
//! builds a `/tags/{name}` URL (`statuses/endpoints.rs`,
//! `notifications/service.rs`, `timelines/hydrator.rs`) does so from a
//! `ForwardedOrigin`/`ActorUrls` value supplied by a request-handling layer
//! that actually has the scheme/host, which this repository function does
//! not have access to. This module therefore returns the same `/tags/{name}`
//! path segment those call sites use, without a scheme/host prefix — see
//! [`tag_relative_url`]. Making that a full absolute URL (design.md's
//! `TagSerializer` component note: "url は ActorUrls/サーバードメイン由来の
//! タグ URL") is left to whichever downstream layer actually has origin
//! context (task 4.1/4.2, outside this task's boundary). Flagged as a
//! CONCERN in this task's status report for reviewer confirmation.
//!
//! ## `TagView::history` starts empty (design.md's explicit "または空")
//! `search_tags` stores only a single running `last_status_at`/
//! `statuses_count` pair, not a per-day/per-account breakdown — there is no
//! data in this spec's own tables from which a [`TagHistoryEntry`]'s
//! `accounts` field (a *distinct account* count) could be honestly derived.
//! design.md's own Data Model note is explicit that history is allowed to
//! "最小集計（または空）で開始" (start as a minimal aggregate *or empty*), so
//! this module returns an empty `history` rather than fabricating an
//! `accounts` figure this schema cannot actually support.
//!
//! ## `upsert_tag_usage`'s aggregate update only fires on a genuinely new
//! association
//! Requirement 8.2 / this task's own completion condition require that
//! re-scanning the same post's tags never double-counts (`(tag_id,
//! status_id)` dedup). [`upsert_tag_usage`] upserts the `search_tags` row
//! first (by `name`, `id` stable per the section above), then attempts the
//! `search_status_tags` association via `ON CONFLICT (tag_id, status_id) DO
//! NOTHING` and only increments `search_tags.statuses_count`/advances
//! `last_status_at`/`updated_at` when that insert's `rows_affected() > 0` —
//! i.e. only when this call actually created a new association, never on a
//! duplicate re-association of the same (tag, status) pair. Both statements
//! run inside one transaction so a crash between them can never leave the
//! aggregate out of sync with the association row.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::domain::Id;
use crate::error::AppError;
use crate::search::model::TagView;

fn map_server_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Builds the domain-relative `/tags/{name}` path this module uses as
/// [`TagView::url`] — see this module's doc comment ("`TagView::url`: a
/// domain-relative path, not an absolute URL") for why no scheme/host is
/// included here.
fn tag_relative_url(name: &str) -> String {
    format!("/tags/{name}")
}

/// Matches `search_tags` rows whose (pre-normalized, lowercased) `name`
/// starts with `term` (case-insensitively; Requirement 5.1), applying
/// `limit`/`offset` (Requirement 5.5) and a stable `name ASC` order.
/// `history` is always empty — see this module's doc comment
/// ("`TagView::history` starts empty"). References only `search_tags`
/// (Requirement 8.2's "本 spec 所有テーブルのみを参照する").
pub async fn match_hashtags(
    pool: &PgPool,
    term: &str,
    limit: u32,
    offset: u32,
) -> Result<Vec<TagView>, AppError> {
    let pattern = format!("{}%", term.to_lowercase());

    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM search_tags WHERE name LIKE $1 ORDER BY name ASC LIMIT $2 OFFSET $3",
    )
    .bind(pattern)
    .bind(i64::from(limit))
    .bind(i64::from(offset))
    .fetch_all(pool)
    .await
    .map_err(map_server_error)?;

    Ok(rows
        .into_iter()
        .map(|(name,)| TagView {
            url: tag_relative_url(&name),
            name,
            history: Vec::new(),
        })
        .collect())
}

/// Records one use of hashtag `name` by post `status_id` (Requirement 5.1's
/// derivation target; Requirement 8.2's own-tables-only boundary): upserts
/// the `search_tags` row for `name` (minting `new_tag_id` only if `name` is
/// new — see this module's doc comment, "`upsert_tag_usage`'s extra
/// `new_tag_id: Id` parameter"), then idempotently associates `status_id`
/// with that tag in `search_status_tags` (`(tag_id, status_id)` PK dedup),
/// bumping `search_tags.statuses_count`/`last_status_at`/`updated_at` only
/// when this call actually created a new association — see this module's
/// doc comment ("`upsert_tag_usage`'s aggregate update only fires on a
/// genuinely new association").
pub async fn upsert_tag_usage(
    pool: &PgPool,
    name: &str,
    new_tag_id: Id,
    status_id: Id,
    now: OffsetDateTime,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(map_server_error)?;

    let (tag_id,): (i64,) = sqlx::query_as(
        "INSERT INTO search_tags (id, name, last_status_at, statuses_count, updated_at) \
         VALUES ($1, $2, NULL, 0, $3) \
         ON CONFLICT (name) DO UPDATE SET name = search_tags.name \
         RETURNING id",
    )
    .bind(new_tag_id.as_i64())
    .bind(name)
    .bind(now)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_server_error)?;

    let association = sqlx::query(
        "INSERT INTO search_status_tags (tag_id, status_id, created_at) VALUES ($1, $2, $3) \
         ON CONFLICT (tag_id, status_id) DO NOTHING",
    )
    .bind(tag_id)
    .bind(status_id.as_i64())
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(map_server_error)?;

    if association.rows_affected() > 0 {
        sqlx::query(
            "UPDATE search_tags SET statuses_count = statuses_count + 1, last_status_at = $2, \
             updated_at = $2 WHERE id = $1",
        )
        .bind(tag_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_server_error)?;
    }

    tx.commit().await.map_err(map_server_error)?;

    Ok(())
}

/// Reads the `HashtagIndexer`'s derivation cursor: the last processed
/// `statuses.created_at`/`id` pair (Requirement 5.3's watermark, consumed by
/// task 2.2's `catch_up_from_watermark`). Returns `Ok(None)` — not an error —
/// both when the singleton `search_index_watermark` row does not exist yet
/// (the table starts empty; `migrations/0013_search.sql` seeds no initial
/// row) and when it exists but has never been advanced
/// (`status_created_at`/`status_id` still `NULL`, the "no watermark yet"
/// state that same migration's doc comment calls out).
pub async fn load_watermark(pool: &PgPool) -> Result<Option<(OffsetDateTime, Id)>, AppError> {
    let row: Option<(Option<OffsetDateTime>, Option<i64>)> = sqlx::query_as(
        "SELECT status_created_at, status_id FROM search_index_watermark WHERE id = TRUE",
    )
    .fetch_optional(pool)
    .await
    .map_err(map_server_error)?;

    Ok(
        row.and_then(|(created_at, status_id)| match (created_at, status_id) {
            (Some(created_at), Some(status_id)) => Some((created_at, Id::from_i64(status_id))),
            _ => None,
        }),
    )
}

/// Advances the `HashtagIndexer`'s derivation cursor to `(created_at,
/// status_id)` (Requirement 5.3), upserting the singleton
/// `search_index_watermark` row (`id = TRUE`) whether or not it already
/// exists (the migration seeds no initial row, so the very first
/// `save_watermark` call must insert it).
pub async fn save_watermark(
    pool: &PgPool,
    created_at: OffsetDateTime,
    status_id: Id,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO search_index_watermark (id, status_created_at, status_id, updated_at) \
         VALUES (TRUE, $1, $2, $1) \
         ON CONFLICT (id) DO UPDATE SET \
             status_created_at = EXCLUDED.status_created_at, \
             status_id = EXCLUDED.status_id, \
             updated_at = EXCLUDED.updated_at",
    )
    .bind(created_at)
    .bind(status_id.as_i64())
    .execute(pool)
    .await
    .map_err(map_server_error)?;

    Ok(())
}
