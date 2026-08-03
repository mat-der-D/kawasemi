//! `PgSearchBackend` (design.md "Search Port / 照合境界層" ->
//! `SearchBackend(ports) / PgSearchBackend`, Requirements 3.1, 3.4, 4.1,
//! 4.3, 4.4, 4.5, 4.6, 7.2; task 3.1, `Boundary: PgSearchBackend`): the
//! standard-PostgreSQL default [`crate::search::ports::SearchBackend`]
//! implementation — `search_accounts` (local/known-remote display_name/
//! username/acct partial match) and `search_statuses` (visibility-candidate
//! post body partial match, optional `account_id` scope), both via plain SQL
//! `ILIKE` (Requirement 4.4/8.1: no required PostgreSQL extension).
//!
//! Scope: this module owns exactly this task's own two methods on
//! [`PgSearchBackend`] — `search_accounts` and `search_statuses`. It does
//! not touch `src/search/ports.rs`, `src/search/model.rs`,
//! `src/search/hashtag_repository.rs`, or `src/search/hashtag_indexer.rs`.
//! `search_hashtags` (task 3.2's job, `HashtagIndexRepository` wiring) is a
//! documented placeholder here only so this struct compiles as a full
//! [`crate::search::ports::SearchBackend`] impl — see this module's own doc
//! comment further down for why.
//!
//! ## SQL query surface (design.md's own "`PgSearchBackend` が依存する
//! upstream カラム" table, authoritative over a broader reading of
//! Requirement 3.1's prose)
//! design.md's own upstream-column table lists exactly five columns this
//! module may reference: `account_profiles.display_name` (local accounts),
//! `remote_accounts.username`/`domain`/`display_name` (known remote
//! accounts), and `statuses.content` (post bodies) — explicitly closing with
//! "上記以外のカラムは `PgSearchBackend` の SQL 照合が参照しない". Requirement
//! 3.1's own prose ("表示名・ユーザー名・ハンドル（acct）に対する一致") could be
//! read as also wanting a *local* account's username/handle
//! (`local_actors.handle`, actor-model's own table) to participate in the
//! match, but design.md's table is this task's authoritative SQL contract
//! and does not name `local_actors` at all — a local account is therefore
//! matched here only by its `account_profiles.display_name`. Flagged as a
//! CONCERN in this task's status report for reviewer confirmation.
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
//! ## `search_hashtags` is an out-of-boundary placeholder (task 3.2's job)
//! [`crate::search::ports::SearchBackend`] requires all three methods for
//! `impl SearchBackend for PgSearchBackend` to compile at all, but this
//! task's own instructions are explicit that `search_hashtags`
//! (`HashtagIndexRepository` wiring) is task 3.2's job, strictly downstream
//! of and outside this task's boundary, and that this task must not
//! implement it beyond a documented placeholder. This module's
//! `search_hashtags` therefore `unimplemented!()`s with a doc comment
//! pointing at task 3.2 — it is never called by any test this task adds.
//! Flagged as a CONCERN in this task's status report per this task's own
//! brief.

use axum::http::StatusCode;
use sqlx::postgres::PgPool;

use crate::domain::{AccountRef, Id};
use crate::error::AppError;
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
}

impl PgSearchBackend {
    /// Builds a `PgSearchBackend` against `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl SearchBackend for PgSearchBackend {
    /// Matches local accounts (`account_profiles.display_name`) and known
    /// remote accounts (`remote_accounts.username`/`domain`/`display_name`,
    /// plus the synthesized `username@domain` acct form) whose relevant
    /// column(s) contain `q.term`, case-insensitively (`ILIKE`), returning
    /// bare [`AccountRef`]s with `limit`/`offset` applied to the combined
    /// result set — see this module's own doc comment for the full SQL
    /// query surface and dedup reasoning.
    async fn search_accounts(&self, q: &AccountQuery) -> Result<Vec<AccountRef>, AppError> {
        let pattern = format!("%{}%", q.term);

        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT 'local' AS kind, actor_id AS id \
               FROM account_profiles \
              WHERE display_name ILIKE $1 \
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

    /// Not implemented by this task — see this module's own doc comment
    /// ("`search_hashtags` is an out-of-boundary placeholder"). Task 3.2
    /// wires this method to `crate::search::hashtag_repository`.
    async fn search_hashtags(&self, _q: &HashtagQuery) -> Result<Vec<TagMatch>, AppError> {
        unimplemented!("PgSearchBackend::search_hashtags: task 3.2's job, not task 3.1's")
    }
}
