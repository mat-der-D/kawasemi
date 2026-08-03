//! `PgSearchBackend` (design.md "Search Port / 照合境界層" ->
//! `SearchBackend(ports) / PgSearchBackend`, Requirements 3.1, 3.4, 4.1,
//! 4.3, 4.4, 4.5, 4.6, 5.1, 5.3, 5.5, 7.2; tasks 3.1 and 3.2, `Boundary:
//! PgSearchBackend`): the standard-PostgreSQL default
//! [`crate::search::ports::SearchBackend`] implementation —
//! `search_accounts` (local/known-remote display_name/username/acct
//! partial match), `search_statuses` (visibility-candidate post body
//! partial match, optional `account_id` scope), both via plain SQL `ILIKE`
//! (Requirement 4.4/8.1: no required PostgreSQL extension), and
//! `search_hashtags` (task 3.2: on-demand `HashtagIndexer` catch-up scan
//! followed by `HashtagIndexRepository` name matching, see this module's
//! own doc comment "`search_hashtags`: on-demand catch-up then read-index
//! match" further down).
//!
//! Scope: task 3.1 owns [`PgSearchBackend`]'s `search_accounts` and
//! `search_statuses` methods; task 3.2 (this task) owns exactly its
//! `search_hashtags` method, wiring it to
//! [`crate::search::hashtag_indexer::catch_up_from_watermark`] and
//! [`crate::search::hashtag_repository::match_hashtags`] — both already
//! implemented/committed by earlier tasks and not modified here. This
//! module does not touch `src/search/ports.rs`, `src/search/model.rs`,
//! `src/search/hashtag_repository.rs`, or `src/search/hashtag_indexer.rs`.
//!
//! ## SQL query surface (design.md's own "`PgSearchBackend` が依存する
//! upstream カラム" table)
//! design.md's own upstream-column table lists the columns this module may
//! reference: `account_profiles.display_name` and `local_actors.handle`
//! (local accounts), `remote_accounts.username`/`domain`/`display_name`
//! (known remote accounts), and `statuses.content` (post bodies).
//! Requirement 3.1's prose ("表示名・ユーザー名・ハンドル（acct）に対する一致")
//! requires a *local* account's handle (`local_actors.handle`, actor-model's
//! own table, `UNIQUE`-constrained) to participate in the match the same way
//! a known remote account's `username` does — task 3.1's original
//! implementation narrowed local-account matching to
//! `account_profiles.display_name` only, per design.md's table at the time,
//! and flagged this as a CONCERN (see `.kiro/specs/search/tasks.md`
//! Implementation Notes, task 3.1 and its follow-up correction entry). A
//! feature-level validation pass determined the narrowing was a genuine
//! functional gap against requirements.md 3.1 (a local user could not be
//! found by their own `@handle` unless it happened to also appear in their
//! display name), so `search_accounts` below now additionally matches
//! `local_actors.handle` via a `LEFT JOIN` from `account_profiles` (`LEFT`,
//! not inner, so an `account_profiles` row with no corresponding
//! `local_actors` row — as some of this module's own tests fixture directly
//! — still matches on `display_name` alone; `account_profiles.actor_id` and
//! `local_actors.id` are both primary keys, so the join can never multiply a
//! single account into more than one output row). design.md's table has
//! been updated accordingly.
//!
//! `remote_accounts` carries no single `acct` column (`username`/`domain`
//! are stored separately) — design.md's own note says so explicitly
//! ("`remote_accounts` に `acct` という単一カラムは存在しない... 両カラムを
//! 直接参照する") — so [`search_accounts`](PgSearchBackend::search_accounts)
//! additionally matches the synthesized `username || '@' || domain` form
//! against the query term, letting a query like `"bob@example.social"` match
//! a known remote account whose `username`/`domain` individually would not
//! each contain the whole term.
//!
//! ## Combined local+remote result: one SQL round trip, no post-hoc
//! `Vec` merge
//! [`search_accounts`](PgSearchBackend::search_accounts) issues a single
//! `UNION ALL` query (a local-account subquery over `account_profiles`
//! unioned with a remote-account subquery over `remote_accounts`) so that
//! `limit`/`offset` (Requirement 3.4) apply to the *combined* candidate set
//! in one SQL statement, rather than fetching each side separately and
//! re-paginating in Rust (which would need to over-fetch both sides by an
//! unbounded amount to guarantee a correct combined page). Deduplication
//! (Requirement 3.5, "同一アカウントが重複して現れないよう...一意化") needs no
//! extra `DISTINCT`/dedup step here: each subquery's `OR`-of-ILIKE
//! predicates is evaluated once per row (never a join), so a single account
//! row can contribute at most one output row on its own side, and a local
//! account (`account_profiles`) and a remote account (`remote_accounts`) are
//! never the same [`AccountRef`] value (different enum variant) even if
//! their underlying numeric ids happen to coincide.
//!
//! ## `search_statuses`: no hand-rolled visibility SQL (Requirements 4.1,
//! 4.2, 7.2)
//! `crate::search::ports::StatusQuery`'s own doc comment is explicit that
//! `viewer` is carried through only as an optional optimization hint a
//! backend *may* use to prefilter to visibility *candidates*, with final
//! visibility enforcement re-applied downstream by `SearchHydrator`
//! (strictly outside this task's boundary, a later task). design.md's own
//! `PgSearchBackend::search_statuses` Responsibilities note says the exact
//! same thing ("最終可視性は Hydrator が `VisibilityPolicy` で再適用"). This
//! implementation therefore does not filter by `statuses.visibility` at all
//! — it matches on `content ILIKE` plus the optional `account_id` scope
//! only, deferring every visibility judgment to `SearchHydrator`. Flagged as
//! a CONCERN in this task's status report for reviewer confirmation against
//! requirements.md 4.1/4.2/7.2's "candidate" wording, per this task's own
//! brief.
//!
//! ## Overfetch convention (design.md's own "オーバーフェッチ規約")
//! design.md's prose gives two illustrative formulas for the SQL-level
//! overfetch amount — `limit * 2` or `limit + a fixed margin` — and asks the
//! implementation to pick and document one, using whichever of the two is
//! larger for a given `limit`. See [`STATUS_OVERFETCH_MARGIN`] and
//! [`overfetch_limit`] for the concrete constant/formula this module
//! commits to. `OFFSET` is always the requested `offset`, unscaled (design.md:
//! "`OFFSET` は要求 `offset` をそのまま用いる（オーバーフェッチはページ境界=
//! `offset` を動かさない）") — truncating the resulting (possibly
//! `limit`-exceeding) candidate set down to the requested `limit` is
//! `SearchHydrator`'s job, strictly outside this task's boundary (design.md:
//! "返却件数はオーバーフェッチ件数以下で `limit` を超えうる（切り詰めは Hydrator
//! 側の責務）").
//!
//! ## `search_hashtags`: on-demand catch-up then read-index match (task 3.2)
//! design.md's "ハッシュタグ照合と読み取りインデックス導出" flow diagram
//! places "on demand catch up scan from watermark" *before* "hashtag index
//! repository name match", inside the same request — not a separately
//! scheduled background job. [`PgSearchBackend::search_hashtags`] therefore
//! always calls
//! [`crate::search::hashtag_indexer::catch_up_from_watermark`] first, every
//! call (unconditionally — this task's own instruction is explicit the
//! catch-up runs "検索直前までのタグ状態に追いつかせ", i.e. immediately
//! before matching, not merely on a first request), so that a post inserted
//! upstream after the read index's watermark was last advanced is still
//! found by this same call, then matches `q.term` against the now
//! caught-up index via
//! [`crate::search::hashtag_repository::match_hashtags`] (Requirements 5.1,
//! 5.3), with `q.limit`/`q.offset` applied in that function's own SQL
//! (Requirement 5.5). `match_hashtags` returns
//! [`crate::search::model::TagView`] (`name`/`url`/`history`) — this method
//! maps each down to the bare [`TagMatch`] (`name` only) this port's return
//! type requires (Requirement 7.2); `url`/`history` are dropped here as
//! `TagSerializer`'s (task 4.1's) concern, not this port's.

use axum::http::StatusCode;
use sqlx::postgres::PgPool;

use crate::domain::{AccountRef, Id};
use crate::error::AppError;
use crate::runtime::RuntimeContext;
use crate::search::hashtag_indexer::catch_up_from_watermark;
use crate::search::hashtag_repository::match_hashtags;
use crate::search::model::TagMatch;
use crate::search::ports::{AccountQuery, HashtagQuery, SearchBackend, StatusQuery};

fn map_server_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// The fixed margin half of this module's overfetch convention (see this
/// module's own doc comment, "Overfetch convention"): [`overfetch_limit`]
/// uses `max(limit * 2, limit + STATUS_OVERFETCH_MARGIN)`, so a small
/// requested `limit` (where `limit * 2` would barely exceed it) still gets a
/// meaningful absolute cushion of extra candidate rows for
/// `SearchHydrator`'s downstream visibility filter to draw from.
const STATUS_OVERFETCH_MARGIN: u32 = 20;

/// Computes the SQL-level `LIMIT` [`PgSearchBackend::search_statuses`] uses
/// for a requested `limit` (design.md's own "オーバーフェッチ規約"): the
/// larger of `limit * 2` and `limit + `[`STATUS_OVERFETCH_MARGIN`]`,
/// saturating rather than overflowing for a pathologically large `limit`.
fn overfetch_limit(limit: u32) -> u32 {
    let doubled = limit.saturating_mul(2);
    let margin_based = limit.saturating_add(STATUS_OVERFETCH_MARGIN);
    doubled.max(margin_based)
}

/// The standard-PostgreSQL default [`SearchBackend`] implementation
/// (Requirement 7.3): holds a plain [`PgPool`], mirroring
/// `src/media/local_fs.rs`'s `LocalFsStore` precedent for a struct
/// implementing an `&self` async port (see this module's own doc comment).
pub struct PgSearchBackend {
    pool: PgPool,
    runtime: RuntimeContext,
}

impl PgSearchBackend {
    /// Builds a `PgSearchBackend` against `pool`, using `runtime` (time/id
    /// sourcing, this crate's established DI boundary) for
    /// `search_hashtags`'s on-demand `catch_up_from_watermark` scan.
    pub fn new(pool: PgPool, runtime: RuntimeContext) -> Self {
        Self { pool, runtime }
    }
}

impl SearchBackend for PgSearchBackend {
    /// Matches local accounts (`account_profiles.display_name` or
    /// `local_actors.handle`) and known remote accounts
    /// (`remote_accounts.username`/`domain`/`display_name`, plus the
    /// synthesized `username@domain` acct form) whose relevant column(s)
    /// contain `q.term`, case-insensitively (`ILIKE`), returning bare
    /// [`AccountRef`]s with `limit`/`offset` applied to the combined result
    /// set — see this module's own doc comment for the full SQL query
    /// surface and dedup reasoning.
    async fn search_accounts(&self, q: &AccountQuery) -> Result<Vec<AccountRef>, AppError> {
        let pattern = format!("%{}%", q.term);

        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT 'local' AS kind, account_profiles.actor_id AS id \
               FROM account_profiles \
               LEFT JOIN local_actors ON local_actors.id = account_profiles.actor_id \
              WHERE account_profiles.display_name ILIKE $1 \
                 OR local_actors.handle ILIKE $1 \
             UNION ALL \
             SELECT 'remote' AS kind, id \
               FROM remote_accounts \
              WHERE username ILIKE $1 \
                 OR domain ILIKE $1 \
                 OR display_name ILIKE $1 \
                 OR (username || '@' || domain) ILIKE $1 \
             ORDER BY kind, id \
             LIMIT $2 OFFSET $3",
        )
        .bind(&pattern)
        .bind(i64::from(q.limit))
        .bind(i64::from(q.offset))
        .fetch_all(&self.pool)
        .await
        .map_err(map_server_error)?;

        Ok(rows
            .into_iter()
            .map(|(kind, id)| {
                let id = Id::from_i64(id);
                if kind == "local" {
                    AccountRef::Local(id)
                } else {
                    AccountRef::Remote(id)
                }
            })
            .collect())
    }

    /// Matches posts (`statuses.content`) whose body contains `q.term`,
    /// case-insensitively (`ILIKE`), optionally scoped to `q.account_id`
    /// (matched against `statuses.actor_id`), ordered `created_at DESC, id
    /// DESC` (newest first, deterministic tiebreak), with the SQL `LIMIT`
    /// set to this module's overfetch amount ([`overfetch_limit`]) rather
    /// than `q.limit` directly, and `OFFSET` set to `q.offset` unchanged —
    /// see this module's own doc comment ("Overfetch convention",
    /// "`search_statuses`: no hand-rolled visibility SQL") for why.
    async fn search_statuses(&self, q: &StatusQuery) -> Result<Vec<Id>, AppError> {
        let pattern = format!("%{}%", q.term);
        let account_id = q.account_id.map(|id| id.as_i64());
        let sql_limit = overfetch_limit(q.limit);

        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT id \
               FROM statuses \
              WHERE content ILIKE $1 \
                AND ($2::bigint IS NULL OR actor_id = $2) \
              ORDER BY created_at DESC, id DESC \
              LIMIT $3 OFFSET $4",
        )
        .bind(&pattern)
        .bind(account_id)
        .bind(i64::from(sql_limit))
        .bind(i64::from(q.offset))
        .fetch_all(&self.pool)
        .await
        .map_err(map_server_error)?;

        Ok(rows.into_iter().map(|(id,)| Id::from_i64(id)).collect())
    }

    /// Runs the on-demand `HashtagIndexer` catch-up scan
    /// ([`crate::search::hashtag_indexer::catch_up_from_watermark`]) first,
    /// bringing this spec's own read index (`search_tags`/
    /// `search_status_tags`) up to date with every `statuses` row newer
    /// than its watermark, then matches `q.term` against that freshly
    /// caught-up index via
    /// [`crate::search::hashtag_repository::match_hashtags`] (Requirements
    /// 5.1, 5.3), applying `q.limit`/`q.offset` in SQL (Requirement 5.5).
    /// The richer [`crate::search::model::TagView`] `match_hashtags`
    /// returns is mapped down to the bare [`TagMatch`] identifier this
    /// port's return type requires (Requirement 7.2) — `url`/`history` are
    /// dropped here, not this task's concern (see design.md's flow, "on
    /// demand catch up scan from watermark" happens before "hashtag index
    /// repository name match", inside this single request, every call —
    /// never a separate background job).
    async fn search_hashtags(&self, q: &HashtagQuery) -> Result<Vec<TagMatch>, AppError> {
        catch_up_from_watermark(&self.pool, &self.runtime).await?;

        let views = match_hashtags(&self.pool, &q.term, q.limit, q.offset).await?;

        Ok(views
            .into_iter()
            .map(|view| TagMatch { name: view.name })
            .collect())
    }
}
