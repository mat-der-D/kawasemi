//! `CandidateRepository` (design.md "Data / データ層" -> `CandidateRepository`,
//! Requirements 1.1, 2.1, 2.3, 2.4, 2.5, 3.1, 3.4, 4.1, 4.4, 4.5, 7.1, 7.4;
//! task 2.1, `Boundary: CandidateRepository`).
//!
//! Scope: this module owns exactly [`fetch_candidates`] — a read-only
//! candidate-post fetch against `statuses` (plus `status_media` for
//! `only_media`, `tags`/`status_tags` for tag-timeline matching), translating
//! a [`TimelineQuerySpec`]'s per-kind condition (mirroring, never
//! re-deriving, [`crate::timelines::kind_rules::TimelineKindRules`]'s
//! structural condition — design.md's "条件の二重定義禁止") plus its cursor
//! range and request-level `local`/`remote`/`only_media`/tag narrowing into
//! SQL. No visibility (`VisibilityPolicy`) or relationship
//! (blocked/blocked_by/muted/`show_reblogs`) filtering is applied here —
//! that is `TimelineFilter`'s job (task 3.1, out of this task's boundary).
//! No writes: every query issued by this module is a `SELECT`.
//!
//! ## Design decision: reconciling design.md's `fetch_candidates` sketch with
//! home's extra following-set parameter
//!
//! design.md's Service Interface for `CandidateRepository` sketches:
//! ```ignore
//! pub async fn fetch_candidates(pool: &PgPool, spec: &TimelineQuerySpec, batch_limit: u32) -> Result<Vec<Status>, AppError>;
//! ```
//! but its own prose (`CandidateRepository`'s Responsibilities & Constraints,
//! "フォロー集合の受け渡し（MVP 方針）") requires the home kind's
//! `author_id = ANY($n)` condition to be bound from a `following_set` id
//! array the caller (`TimelineService`, task 4.2) already holds from
//! social-graph's `FilterQuery` — this repository must not fetch that set
//! itself (out of this task's boundary, per this task's own instruction).
//! Since [`TimelineQuerySpec`] carries no id-array field (`src/timelines/model.rs`,
//! task 1.1) and design.md gives no second config-struct sketch to route it
//! through, the most conservative resolution — closest to design.md's own
//! three-parameter sketch, adding exactly the one slot home structurally
//! needs — is to give [`fetch_candidates`] a fourth parameter,
//! `following_and_self: &[Id]`, inserted before `batch_limit`. It is read
//! only when `spec.kind == TimelineKind::Home` (bound as
//! `actor_id = ANY($1)`, already including the viewer's own id per design.md's
//! "自分自身の id を含めた配列") and is otherwise ignored — every non-home
//! call site may simply pass `&[]`. This keeps `fetch_candidates` the single
//! public dispatch entry point (mirroring `TimelineKindRules::matches`'s own
//! single-dispatch-point precedent, `src/timelines/kind_rules.rs`) rather
//! than splintering into a per-kind function family, while still expressing
//! home's genuinely extra input explicitly in the signature instead of
//! smuggling it through a mutable/global side channel.
//!
//! ## Row shape: `Vec<Status>`, not a new candidate type
//! design.md's approved Service Interface returns `Result<Vec<Status>, AppError>`
//! using `crate::statuses::model::Status` — this module reconstructs `Status`
//! values directly from `statuses` rows, mirroring
//! `crate::statuses::status_repository`'s own row-mapping conventions
//! (`StatusRow`, `status_columns!`, `row_to_status`, `visibility_from_str`,
//! `map_server_error`) exactly rather than reinventing them, since none of
//! those items are `pub` there. `kind_rules.rs`'s `TimelineCandidate` (task
//! 1.2) is a separate, deliberately DB-free unit-test fixture type for that
//! module's own tests — not this function's return shape (see this crate's
//! tasks.md "Implementation Notes" for task 1.2, which documents this same
//! distinction).
//!
//! ## Cursor semantics (Requirement 7.1, 7.4)
//! Per `crate::api::pagination`'s documented convention: `max_id` bounds `id`
//! exclusive-above (`id < max_id`), `since_id` and `min_id` both bound `id`
//! exclusive-below (`id > cursor`) — they differ only in which end of the
//! window `TimelineService`'s later page-selection step
//! ([`crate::api::pagination::paginate`]) anchors on, not in the inequality
//! direction itself. This repository only narrows the *candidate batch* via
//! straightforward SQL bounds (both lower bounds applied together, via `AND`,
//! when both happen to be present) — the anchor-direction selection logic
//! that distinguishes `since_id` from `min_id` lives downstream in
//! `paginate`, not here. Results are always ordered `id DESC` (newest
//! first), and `batch_limit` is bound straight into `LIMIT` — no silent cap
//! to some other default (Requirement 7.4's "`limit` より多めのバッチ取得に
//! 対応する").
//!
//! ## Local/remote/only_media narrowing
//! `statuses.local` already carries the author-locality flag directly on the
//! candidate row (`migrations/0007_statuses.sql`) — no join against an
//! actor table is needed. `params.only_media` is expressed as an `EXISTS`
//! against `status_media` (existence check only; media *shape* is
//! `StatusHydrator`'s concern, a later task).
//!
//! ## Tag matching
//! `tags.name` is already normalized (lower-cased) by statuses-core at
//! write time (`migrations/0007_statuses.sql`'s own column comment), but
//! this module still case-folds the *filter's* own tag strings before
//! binding (mirrors `crate::timelines::kind_rules::fold_tag`'s trim+lowercase
//! exactly, duplicated here privately since that function is not `pub`) so
//! the comparison is correct even if a caller passes non-normalized input.
//! `primary` must match; `any` (if non-empty) requires at least one match;
//! `all` requires every listed tag to match; `none` requires none to match —
//! mirrors `crate::timelines::kind_rules::tag_matches`'s semantics exactly,
//! expressed as SQL `EXISTS`/`NOT EXISTS` conditions instead of a Rust
//! predicate (the *condition* is kept equivalent; the *code* is necessarily
//! separate, SQL WHERE vs. Rust match — see this crate's own tasks.md
//! Implementation Notes / this task's CONCERNS for why that duplication is
//! expected here, not a violation of "条件の二重定義禁止").

use axum::http::StatusCode;
use sqlx::{PgPool, Postgres, QueryBuilder};
use time::OffsetDateTime;

use crate::domain::{Id, Visibility};
use crate::error::AppError;
use crate::statuses::model::Status;

use super::model::{TagFilter, TimelineKind, TimelineQuerySpec};

/// Maps a raw `sqlx::Error` the same way
/// `crate::statuses::status_repository::map_server_error` does (mirrored
/// here, not imported — that function is private to its own module): every
/// failure from this read-only repository is a 5xx `AppError`, never a new
/// error type (steering: エラーは `AppError` に集約).
fn map_server_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Reconstructs a [`Visibility`] from an already-persisted
/// `statuses.visibility` column value — mirrors
/// `crate::statuses::status_repository::visibility_from_str` exactly
/// (duplicated here since that function is private to its own module).
/// Panics on any other value: such a row could only exist if something wrote
/// outside statuses-core's own `visibility_as_str` mapping, a data-corruption
/// invariant violation, not a normal error path.
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

/// Case-folds a tag name for case-insensitive comparison — mirrors
/// `crate::timelines::kind_rules::fold_tag` exactly (duplicated here since
/// that function is private to its own module): trim + lowercase.
fn fold_tag(tag: &str) -> String {
    tag.trim().to_lowercase()
}

/// A `statuses` row as read directly off the wire, before reconstructing its
/// typed [`Status`] form — mirrors
/// `crate::statuses::status_repository::StatusRow` field-for-field (that
/// struct is private to its own module, so this module defines its own
/// identically-shaped copy per this module's own doc comment on why row
/// mapping is mirrored, not imported).
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

/// The column list this module's single `SELECT` uses, matching
/// [`StatusRow`]'s field set exactly — mirrors
/// `crate::statuses::status_repository::status_columns!`'s identical
/// convention/rationale (duplicated here, private to its own module).
macro_rules! status_columns {
    () => {
        "s.id, s.actor_id, s.uri, s.url, s.content, s.visibility, s.sensitive, s.spoiler_text, \
         s.in_reply_to_id, s.in_reply_to_account_id, s.reblog_of_id, s.poll_id, s.language, \
         s.reblogs_count, s.favourites_count, s.replies_count, s.local, s.created_at, \
         s.edited_at"
    };
}

/// Reconstructs a [`Status`] from a raw row — mirrors
/// `crate::statuses::status_repository::row_to_status` exactly.
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

/// Appends the tag-timeline `AND` conditions for `filter` onto `qb` (`primary`
/// required, `any`/`all`/`none` per Requirement 4.4) — see this module's own
/// doc comment ("Tag matching") for the exact semantics mirrored from
/// `crate::timelines::kind_rules::tag_matches`.
fn push_tag_conditions(qb: &mut QueryBuilder<Postgres>, filter: &TagFilter) {
    let primary = fold_tag(&filter.primary);
    qb.push(
        " AND EXISTS (SELECT 1 FROM status_tags st_primary \
         JOIN tags t_primary ON t_primary.id = st_primary.tag_id \
         WHERE st_primary.status_id = s.id AND t_primary.name = ",
    );
    qb.push_bind(primary);
    qb.push(")");

    if !filter.any.is_empty() {
        let any_tags: Vec<String> = filter.any.iter().map(|t| fold_tag(t)).collect();
        qb.push(
            " AND EXISTS (SELECT 1 FROM status_tags st_any \
             JOIN tags t_any ON t_any.id = st_any.tag_id \
             WHERE st_any.status_id = s.id AND t_any.name = ANY(",
        );
        qb.push_bind(any_tags);
        qb.push("))");
    }

    for tag in &filter.all {
        let folded = fold_tag(tag);
        qb.push(
            " AND EXISTS (SELECT 1 FROM status_tags st_all \
             JOIN tags t_all ON t_all.id = st_all.tag_id \
             WHERE st_all.status_id = s.id AND t_all.name = ",
        );
        qb.push_bind(folded);
        qb.push(")");
    }

    if !filter.none.is_empty() {
        let none_tags: Vec<String> = filter.none.iter().map(|t| fold_tag(t)).collect();
        qb.push(
            " AND NOT EXISTS (SELECT 1 FROM status_tags st_none \
             JOIN tags t_none ON t_none.id = st_none.tag_id \
             WHERE st_none.status_id = s.id AND t_none.name = ANY(",
        );
        qb.push_bind(none_tags);
        qb.push("))");
    }
}

/// Fetches up to `batch_limit` candidate [`Status`] rows satisfying `spec`'s
/// kind-level structural condition (mirroring, never re-deriving,
/// [`crate::timelines::kind_rules::TimelineKindRules`]) plus its
/// `local`/`remote`/`only_media`/tag request-level narrowing and cursor range,
/// in descending `id` order (newest first), stable.
///
/// `following_and_self` is read only for [`TimelineKind::Home`] (the
/// pre-loaded `following ∪ self` id array a caller such as `TimelineService`
/// already holds from social-graph's `FilterQuery`, per design.md's MVP
/// `author_id = ANY($n)` binding approach — see this module's own doc
/// comment for the full reasoning); pass `&[]` for every other kind.
///
/// Postcondition (design.md): returns candidates satisfying only the
/// *kind-level SQL condition* — visibility/relationship filtering is **not**
/// applied here (that is `TimelineFilter`'s job, task 3.1, out of this task's
/// boundary). Issues exactly one `SELECT`; never writes to `statuses` /
/// `status_media` / `tags` / `status_tags`.
pub async fn fetch_candidates(
    pool: &PgPool,
    spec: &TimelineQuerySpec,
    following_and_self: &[Id],
    batch_limit: u32,
) -> Result<Vec<Status>, AppError> {
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(concat!(
        "SELECT ",
        status_columns!(),
        " FROM statuses s WHERE "
    ));

    match spec.kind {
        TimelineKind::Home => {
            // Requirement 1.1: author in `following ∪ self`; Requirement
            // 1.3: `direct` excluded. Boosts are structurally included (no
            // `reblog_of_id IS NULL` condition) — mirrors
            // `TimelineKindRules::matches_home` exactly.
            let ids: Vec<i64> = following_and_self.iter().map(|id| id.as_i64()).collect();
            qb.push("s.actor_id = ANY(");
            qb.push_bind(ids);
            qb.push(") AND s.visibility <> 'direct'");
        }
        TimelineKind::Public => {
            // Requirements 2.1, 2.2: `public`-only, boosts excluded.
            qb.push("s.visibility = 'public' AND s.reblog_of_id IS NULL");
        }
        TimelineKind::Local => {
            // Requirements 3.1, 3.2: same as Public, plus local-author only.
            qb.push("s.visibility = 'public' AND s.reblog_of_id IS NULL AND s.local = TRUE");
        }
        TimelineKind::Tag => {
            // Requirements 4.1, 4.3: same as Public, plus tag matching.
            qb.push("s.visibility = 'public' AND s.reblog_of_id IS NULL");
            match &spec.params.tag {
                Some(filter) => push_tag_conditions(&mut qb, filter),
                None => {
                    // Mirrors `TimelineKindRules::matches_tag`'s own
                    // conservative "no filter attached -> matches nothing"
                    // rule (`src/timelines/kind_rules.rs`).
                    qb.push(" AND FALSE");
                }
            }
        }
    }

    // Request-level narrowing (Requirements 2.3, 2.4, 2.5, 3.4, 4.5).
    if spec.params.local {
        qb.push(" AND s.local = TRUE");
    }
    if spec.params.remote {
        qb.push(" AND s.local = FALSE");
    }
    if spec.params.only_media {
        qb.push(" AND EXISTS (SELECT 1 FROM status_media sm WHERE sm.status_id = s.id)");
    }

    // Cursor range (Requirement 7.1, 7.4) — see this module's own doc
    // comment ("Cursor semantics") for the exact inequality directions.
    if let Some(max_id) = spec.max_id {
        qb.push(" AND s.id < ");
        qb.push_bind(max_id.as_i64());
    }
    if let Some(since_id) = spec.since_id {
        qb.push(" AND s.id > ");
        qb.push_bind(since_id.as_i64());
    }
    if let Some(min_id) = spec.min_id {
        qb.push(" AND s.id > ");
        qb.push_bind(min_id.as_i64());
    }

    qb.push(" ORDER BY s.id DESC LIMIT ");
    qb.push_bind(i64::from(batch_limit));

    let rows: Vec<StatusRow> = qb
        .build_query_as()
        .fetch_all(pool)
        .await
        .map_err(map_server_error)?;

    Ok(rows.into_iter().map(row_to_status).collect())
}
