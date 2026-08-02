//! `HashtagIndexer` (design.md "Data / データ層" -> `HashtagIndexRepository /
//! HashtagIndexer`, Requirement 5.3; task 2.2, `Boundary: HashtagIndexer`):
//! the watermark-cursor-based derivation/catch-up scan that keeps this
//! spec's own hashtag read index (`search_tags`/`search_status_tags`, task
//! 2.1's `hashtag_repository`) synchronized with statuses-core's own,
//! already-extracted per-post hashtag associations (`tags`/`status_tags`,
//! `migrations/0007_statuses.sql`, task 2.1 in that spec's own
//! `statuses::tag_repository`).
//!
//! Scope: this module owns exactly the one function task 2.2's own
//! instruction and design.md's Service Interface excerpt (line ~383) name —
//! [`catch_up_from_watermark`] — read-only against `statuses`/`tags`/
//! `status_tags` (never `INSERT`/`UPDATE`/`DELETE` against any of the
//! three), driving [`crate::search::hashtag_repository`]'s own
//! `load_watermark`/`save_watermark`/`upsert_tag_usage` (task 2.1, already
//! implemented/reviewed/committed — not modified by this task). Wiring this
//! scan to run on-demand at the front of a hashtag search request (design.md
//! prose: "ハッシュタグ検索リクエストの先頭で本キャッチアップスキャンをオン
//! デマンド実行") is task 3.2's job (`pg_backend.rs`'s `search_hashtags`),
//! strictly downstream of and outside this task's boundary — this module
//! only implements the scan itself, callable by anything that wants "catch
//! the index up to the current state of `statuses`".
//!
//! ## What "抽出済みハッシュタグ" (already-extracted hashtags) means here
//! statuses-core's own `crate::statuses::tag_repository` doc comment is
//! explicit that hashtag *extraction* (parsing a post's content into
//! `#tag`-shaped tokens) is `StatusService::create_status`'s job, already
//! done and persisted (`tags`/`status_tags`) by the time any post row this
//! module reads even exists. This indexer therefore never re-parses post
//! *content* at all — [`catch_up_from_watermark`] reads each newly-in-scope
//! `statuses` row only for its `id`/`created_at` (the watermark cursor's own
//! fields), then looks up that status's already-associated tags via
//! `crate::statuses::tag_repository::tags_for_status` (the status -> tag
//! read direction that module's own doc comment names as exactly the
//! function downstream specs' hashtag indexes are meant to consume), and
//! upserts each returned [`crate::statuses::model::Tag::name`] into this
//! spec's own index via [`crate::search::hashtag_repository::upsert_tag_usage`].
//!
//! ## Signature: `runtime: &RuntimeContext`, not design.md's literal
//! `now: OffsetDateTime` (CONCERN — documented judgment call, mirrors task
//! 2.1's own precedent)
//! design.md's Service Interface excerpt (line ~383) writes
//! `catch_up_from_watermark(pool: &PgPool, now: OffsetDateTime) ->
//! Result<u64, AppError>` — a single already-resolved timestamp, no id
//! source. But this task's own instruction is explicit that "時刻/ID は
//! `RuntimeContext` を用いる" (time *and id* come from `RuntimeContext`), and
//! the actual work this function does needs both: `now` for
//! `upsert_tag_usage`'s `now: OffsetDateTime` parameter (as design.md's
//! literal signature already anticipates), *and* a fresh candidate
//! [`Id`](crate::domain::Id) for `upsert_tag_usage`'s own `new_tag_id: Id`
//! parameter (task 2.1's own documented signature deviation from *its*
//! design.md excerpt — see `hashtag_repository.rs`'s doc comment,
//! "`upsert_tag_usage`'s extra `new_tag_id: Id` parameter") every time this
//! scan encounters a tag `name` that might be brand new to `search_tags`. A
//! single scalar `now: OffsetDateTime` parameter has no way to also supply
//! that id-minting capability, and per this crate's established convention
//! (documented in that same `hashtag_repository.rs` doc comment: the
//! `IdGenerator` DI boundary is always threaded in by the *caller*, never
//! resolved internally by a repository/indexer function reaching for
//! `crate::runtime::ids` on its own), this function cannot mint ids without
//! being handed the capability to do so. This module therefore widens
//! design.md's single `now: OffsetDateTime` parameter to `runtime:
//! &RuntimeContext`, reading `runtime.clock.now()` once at the start of the
//! scan (a single, stable `now` value shared by every `upsert_tag_usage`
//! call within one `catch_up_from_watermark` invocation, exactly matching
//! what a single design.md-literal `now: OffsetDateTime` parameter would
//! have provided) and `runtime.ids.next_id()` once per tag association
//! attempted. Flagged as a CONCERN in this task's status report for reviewer
//! confirmation, mirroring `hashtag_repository.rs`'s own precedent for
//! documenting a literal-signature-vs-actual-need resolution inline.
//!
//! ## Batch/transaction shape: incremental watermark advancement per batch,
//! not one all-encompassing transaction (design decision, documented per
//! this task's own instruction)
//! Neither design.md nor task 2.1's own precedent mandates a specific
//! transaction shape for the *scan* (task 2.1's `upsert_tag_usage` is
//! already its own single-row transaction; nothing upstream of it commits to
//! how *many* rows a single `catch_up_from_watermark` call may process
//! atomically). Two shapes were considered:
//! - **One giant transaction for the entire scan** (every status since the
//!   watermark, however many, processed and the watermark advanced in one
//!   commit): simplest to reason about, but risks an unbounded-duration,
//!   unbounded-lock-footprint transaction on a large backfill (the "初回は
//!   全件走査" case design.md itself calls out) or a long catch-up gap, and
//!   a failure partway through (e.g. a transient connection error on
//!   status #50,000) loses *all* progress, forcing the entire scan to
//!   restart from the original watermark.
//! - **Batched, with the watermark advanced only after each batch's upserts
//!   have all succeeded** (this module's choice): processes statuses in
//!   fixed-size batches ([`CATCH_UP_BATCH_SIZE`]), and only calls
//!   `save_watermark` once every status in a given batch has been
//!   successfully upserted into `search_tags`/`search_status_tags`. A
//!   mid-batch failure (an upsert erroring out) propagates the error via
//!   `?` *before* that batch's `save_watermark` call runs, so the watermark
//!   never advances past a status whose derivation did not actually
//!   complete — a retry naturally resumes from the last successfully
//!   completed batch's watermark, at worst re-deriving the statuses in the
//!   batch that failed (safe: `upsert_tag_usage` is itself idempotent per
//!   `(name, status_id)`, task 2.1's own dedup guarantee). This is the more
//!   conservative option per this task's own instruction ("advance the
//!   watermark only after successfully upserting a batch, so a mid-scan
//!   failure doesn't lose or skip statuses"), so it is what this module
//!   implements; flagged as a CONCERN in this task's status report for
//!   reviewer confirmation since neither document forces this choice.
//!
//! ## A status contributing zero hashtags still advances the watermark
//! [`catch_up_from_watermark`]'s watermark cursor tracks "which `statuses`
//! rows have been considered", not "which tags were derived" — every status
//! read in a batch (`tags_for_status` returning an empty `Vec` included) is
//! part of that batch's scan range, and the watermark is advanced to the
//! *last status read in the batch* regardless of whether it, or any other
//! status in the batch, actually produced a tag association. This is what
//! guarantees a run of untagged posts can never stall the cursor (this
//! task's own explicit acceptance note).
//!
//! ## Return value: `u64` count of statuses processed (design.md's literal
//! `Result<u64, AppError>`, this module's own resolution of what the `u64`
//! counts)
//! design.md's Service Interface fixes the `Result<u64, AppError>` return
//! type but does not spell out what is being counted. This module returns
//! the number of `statuses` rows the scan actually read and derived tags
//! from (not the number of tag associations created, which could
//! legitimately be zero for a status with no hashtags, or more than one per
//! status) — the natural unit design.md's own prose ("watermark より新しい
//! `statuses` 行を...走査") describes the scan as operating over, and the
//! same unit this task's own completion condition's "再実行で...watermark
//! 以降の新規投稿のみが処理され" phrasing measures ("posts processed").

#[cfg(test)]
mod tests;

use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::domain::Id;
use crate::error::AppError;
use crate::runtime::RuntimeContext;
use crate::search::hashtag_repository::{load_watermark, save_watermark, upsert_tag_usage};
use crate::statuses::tag_repository::tags_for_status;

fn map_server_error(source: sqlx::Error) -> AppError {
    AppError::server(axum::http::StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// The maximum number of `statuses` rows [`catch_up_from_watermark`] reads
/// and derives per batch before advancing the watermark and (if more rows
/// remain) starting the next batch — see this module's doc comment
/// ("Batch/transaction shape") for why batching, rather than one
/// all-encompassing transaction, was chosen.
const CATCH_UP_BATCH_SIZE: i64 = 500;

/// Reads up to `limit` `statuses` rows' `(id, created_at)` strictly newer
/// than `cursor` (`None` meaning "no watermark yet", i.e. every row is
/// in scope — the backfill case), ordered ascending by `(created_at, id)`
/// (Requirement 5.3's own cursor basis; this crate's established keyset-
/// pagination convention — see this module's own doc comment for why a
/// bare `id`-only cursor is insufficient here: this crate's test harness's
/// deterministic `FixedClock` makes `created_at` collide across many rows,
/// so the tie-break on `id` is load-bearing even though production's
/// `SystemClock` rarely collides in practice).
///
/// Read-only: issues exactly one `SELECT` against `statuses`, never a write
/// — this task's own "upstream テーブルを変更しない" constraint.
async fn fetch_statuses_since(
    pool: &PgPool,
    cursor: Option<(OffsetDateTime, Id)>,
    limit: i64,
) -> Result<Vec<(Id, OffsetDateTime)>, AppError> {
    let rows: Vec<(i64, OffsetDateTime)> = match cursor {
        Some((created_at, id)) => sqlx::query_as(
            "SELECT id, created_at FROM statuses WHERE (created_at, id) > ($1, $2) \
             ORDER BY created_at ASC, id ASC LIMIT $3",
        )
        .bind(created_at)
        .bind(id.as_i64())
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_server_error)?,
        None => sqlx::query_as(
            "SELECT id, created_at FROM statuses ORDER BY created_at ASC, id ASC LIMIT $1",
        )
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(map_server_error)?,
    };

    Ok(rows
        .into_iter()
        .map(|(id, created_at)| (Id::from_i64(id), created_at))
        .collect())
}

/// Watermark-cursor-based on-demand catch-up scan (design.md's "Data /
/// データ層" -> `HashtagIndexer`, Requirement 5.3): reads
/// [`crate::search::hashtag_repository::load_watermark`]'s cursor (`None` =
/// no watermark yet = backfill every existing `statuses` row), then walks
/// `statuses` rows strictly newer than that cursor, in ascending
/// `(created_at, id)` batches of [`CATCH_UP_BATCH_SIZE`]. For each status in
/// a batch, looks up its already-extracted hashtags via
/// [`crate::statuses::tag_repository::tags_for_status`] (read-only against
/// statuses-core's own `tags`/`status_tags`) and upserts each one into this
/// spec's own index via
/// [`crate::search::hashtag_repository::upsert_tag_usage`] — see this
/// module's own doc comment ("What ... means here") for why this never
/// re-parses post content. Once every status in a batch has been derived,
/// advances the watermark to that batch's last `(created_at, id)` via
/// [`crate::search::hashtag_repository::save_watermark`] before starting the
/// next batch (see this module's doc comment, "Batch/transaction shape",
/// for why watermark advancement is per-batch rather than per-scan or
/// per-status).
///
/// `runtime.clock.now()` is read once, at the start of the scan, and reused
/// as every `upsert_tag_usage` call's `now` within this single invocation
/// (see this module's doc comment, "Signature", for why `runtime:
/// &RuntimeContext` replaces design.md's literal single `now:
/// OffsetDateTime` parameter). Time/id are always sourced from `runtime`,
/// never `OffsetDateTime::now_utc()`/an ad hoc id (this task's own explicit
/// instruction).
///
/// Returns the number of `statuses` rows read and derived across every
/// batch (see this module's doc comment, "Return value", for why this is
/// the `u64` design.md's Service Interface names). Never writes to
/// `statuses`/`tags`/`status_tags` — every access against those three
/// tables in this function is a `SELECT` (this task's own "upstream テーブ
/// ルを変更しない" constraint, verified directly by this module's own
/// `catch_up_from_watermark_never_writes_upstream_tables` integration
/// test).
pub async fn catch_up_from_watermark(
    pool: &PgPool,
    runtime: &RuntimeContext,
) -> Result<u64, AppError> {
    let now = runtime.clock.now();
    let mut cursor = load_watermark(pool).await?;
    let mut processed: u64 = 0;

    loop {
        let batch = fetch_statuses_since(pool, cursor, CATCH_UP_BATCH_SIZE).await?;
        let Some(&(last_id, last_created_at)) = batch.last() else {
            break;
        };

        for &(status_id, _status_created_at) in &batch {
            for tag in tags_for_status(pool, status_id).await? {
                let new_tag_id = runtime.ids.next_id();
                upsert_tag_usage(pool, &tag.name, new_tag_id, status_id, now).await?;
            }
        }

        // Watermark advances only after every status in this batch has been
        // fully derived above (see this module's doc comment,
        // "Batch/transaction shape") — a failure inside the loop above
        // propagates via `?` before this call, so a retried scan resumes
        // from the last batch that fully completed.
        save_watermark(pool, last_created_at, last_id).await?;
        processed += batch.len() as u64;
        cursor = Some((last_created_at, last_id));

        if (batch.len() as i64) < CATCH_UP_BATCH_SIZE {
            break;
        }
    }

    Ok(processed)
}
