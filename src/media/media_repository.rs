//! `MediaRepository` (design.md "Data / データ層" -> `MediaRepository`,
//! Requirements 1.1, 1.2, 2.2, 2.3, 2.4, 3.1, 3.3, 4.3; task 3.1,
//! `Boundary: MediaRepository`): the persistence for a [`Media`] attachment
//! row (`migrations/0005_media.sql`'s `media` table, already applied,
//! unmodified by this task) — insertion, owner-scoped lookup, description/
//! focus update, and state+derived-metadata reflection.
//!
//! Scope: this module owns exactly the five operations design.md's Service
//! Interface sketches for this component (with two documented signature
//! extensions — see below) against a plain `&PgPool`. It does not implement
//! `ProcessingJobQueue` (task 3.2), `MediaService` (task 4.1), or any HTTP
//! surface (task 5.1) — those are separate boundaries, out of scope here.
//! It does not modify `src/media/model.rs` (task 2.1, already reviewed).
//!
//! ## `insert_media` gains `object_key`/`content_type` parameters (documented
//! discrepancy resolution)
//! design.md's Service Interface sketch is
//! `insert_media(pool: &PgPool, media: &Media) -> Result<(), AppError>` — a
//! single `&Media` argument. But `media.object_key` and `media.content_type`
//! (`migrations/0005_media.sql`) are both `NOT NULL`, and
//! [`crate::media::model::Media`] (task 2.1, already reviewed, not to be
//! modified by this task) has no `object_key`/`thumb_key`/`content_type`
//! fields at all — task 2.1's own doc comment is explicit that storage-layer
//! concerns like these deliberately do not live on the domain type (see
//! `model.rs`'s module doc comment: no `MediaStore`/storage concepts belong
//! there, that is task 2.2's `MediaStore`/`ObjectKey` boundary). A `&Media`
//! value therefore cannot, by itself, supply the two `NOT NULL` columns this
//! `INSERT` must populate.
//!
//! This mirrors two precedents already accepted in this exact spec/crate:
//! `code_repository.rs`'s `insert_code`/`consume_code` both gained a
//! `token_hash_key: &TokenHashKey` parameter design.md's sketch omitted
//! because it was unavoidably needed to compute a column value, and task
//! 2.2's `MediaStore::public_url` took `&ForwardedOrigin` instead of
//! design.md's sketched (but unusable) `&RequestUriContext`. Following the
//! same "extend the sketch in the direction the schema/later tasks actually
//! need, document why" convention, [`insert_media`] takes two additional
//! parameters: `object_key: &str` (the original upload's already-decided
//! `ObjectKey::original(media.id).as_str()`, task 2.2) and
//! `content_type: &str` (the upload's validated MIME type, known to whatever
//! caller — `MediaService::accept_upload`, task 4.1 — is about to call
//! `insert_media`). `thumb_key` is left `NULL` at insert time: no thumbnail
//! exists yet for a just-accepted, still-`processing` upload (Requirement
//! 1.1); [`set_ready`] is what later fills it in once a derivative is
//! generated (see that function's own doc comment for its own, symmetric
//! extension). This resolution keeps storage-layer identifiers as
//! repository/storage-layer concerns, never smuggled onto the `Media`
//! domain struct itself — recorded as a new entry in
//! `.kiro/specs/media-pipeline/tasks.md`'s `## Implementation Notes` (task
//! 3.1) per this task's assignment.
//!
//! ## `update_metadata`/`set_ready`/`set_failed` gain a `now: OffsetDateTime`
//! parameter (documented signature extension)
//! design.md's sketches for these three functions take no timestamp
//! parameter at all, but `media.updated_at` (`NOT NULL`) must be stamped on
//! every one of these writes, and Requirement statement "識別子は
//! core-runtime の ID 境界、時刻は時刻境界から取得する" (this task's own
//! acceptance text) requires that timestamp come from the `Clock` boundary,
//! never `NOW()`/wall-clock reads inside this module. This mirrors
//! `actor/repository.rs::update_state`'s identical, already-reviewed
//! precedent (`now: OffsetDateTime` supplied by the caller, sourced from
//! `RuntimeContext.clock`) — the same convention is applied here rather than
//! invented fresh.
//!
//! ## `set_ready` also gains a `thumb_key: &str` parameter
//! design.md's sketch is `set_ready(pool, media_id, meta, blurhash) ->
//! Result<(), AppError>`. `media.thumb_key` (nullable at insert, per the
//! note above) is exactly the column a successful processing run resolves —
//! without a `thumb_key` parameter, `set_ready` could never actually fill in
//! the one storage-layer field the schema reserves for it, leaving
//! `thumb_key` permanently `NULL` even for `ready` media (breaking
//! Requirement 2.2's "メディア実体 URL・プレビュー URL... を含む完成した
//! メディア表現" downstream, since a `MediaAttachmentSerializer`, task 4.2,
//! has no `thumb_key` to resolve a preview URL from). `set_ready` therefore
//! takes `thumb_key: &str` (the just-stored small/thumbnail derivative's
//! `ObjectKey::small(media_id).as_str()`, task 2.2) alongside `meta`/
//! `blurhash`.
//!
//! ## Owner-scoped lookup ([`find_owned`]) postcondition (Requirement 2.4)
//! [`find_owned`] scopes its `SELECT` by `WHERE id = $1 AND actor_id = $2`
//! in a single query — never a separate "does this media exist" lookup
//! followed by an application-level ownership comparison. A `media_id` that
//! exists but belongs to a different actor therefore returns exactly the
//! same `Ok(None)` an unknown `media_id` returns: this repository layer
//! makes "not found" and "found but not yours" indistinguishable by
//! construction (mirroring `oauth::code_repository::consume_code`'s own
//! "never leak which case a caller hit" discipline for atomicity, applied
//! here for confidentiality instead) — a later `MediaEndpoints` layer (task
//! 5.1) is responsible for turning that single `None` into Requirement
//! 2.3/2.4's "未検出または権限エラー" response, per design.md's Error
//! Strategy.
//!
//! ## `update_metadata`'s patch semantics
//! `description`/`focus` are each `Option`: `None` means "leave this field
//! unchanged" (Requirement 3.1 permits updating either field independently
//! — a caller who only wants to change the focal point must not
//! accidentally blank out the description, and vice versa), `Some(_)` means
//! "set to this value". The `UPDATE` binds both as SQL `NULL`/non-`NULL`
//! and uses `COALESCE(bound_value, existing_column)` so an unset field falls
//! back to its current column value in the same atomic statement — never a
//! read-modify-write pair of separate queries, which would open a lost-update
//! race between two concurrent metadata updates for the same media. `focus`
//! is passed through as a single `Option<Focus>` (not independently optional
//! x/y) so its two coordinates always move together, atomically, from one
//! already-range-validated [`Focus`] value — never applying a new `x` from
//! one caller alongside a stale `y` left over from a previous state.
//! Requirement 3.4 ("処理中でも...更新を受け付ける") is satisfied by this
//! `UPDATE` never filtering on `state` at all.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::domain::Id;
use crate::error::AppError;
use crate::media::model::{Dimensions, Focus, Media, MediaMeta, MediaState, MediaType};

/// Maps a [`MediaType`] to its `media.media_type` `TEXT` column
/// representation (`migrations/0005_media.sql`'s column comment:
/// `image/gifv/video/audio/unknown`).
fn media_type_as_str(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Image => "image",
        MediaType::Gifv => "gifv",
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Unknown => "unknown",
    }
}

/// Reconstructs a [`MediaType`] from an already-persisted `media.media_type`
/// column value. Panics on any other value — such a row could only exist if
/// something wrote outside this repository's own [`media_type_as_str`]
/// mapping, a data-corruption invariant violation, not a normal error path
/// (mirrors `actor/repository.rs::actor_type_from_str`'s identical
/// precedent).
fn media_type_from_str(raw: &str) -> MediaType {
    match raw {
        "image" => MediaType::Image,
        "gifv" => MediaType::Gifv,
        "video" => MediaType::Video,
        "audio" => MediaType::Audio,
        "unknown" => MediaType::Unknown,
        other => panic!(
            "media.media_type contained unexpected value {other:?}; expected one of \
             'image'/'gifv'/'video'/'audio'/'unknown'"
        ),
    }
}

/// Maps a [`MediaState`] to its `media.state` `TEXT` column representation
/// (`migrations/0005_media.sql`'s column comment:
/// `processing/ready/failed`).
fn media_state_as_str(state: MediaState) -> &'static str {
    match state {
        MediaState::Processing => "processing",
        MediaState::Ready => "ready",
        MediaState::Failed => "failed",
    }
}

/// Reconstructs a [`MediaState`] from an already-persisted `media.state`
/// column value. Panics on any other value — see [`media_type_from_str`]'s
/// doc comment for why that is the right behavior here.
fn media_state_from_str(raw: &str) -> MediaState {
    match raw {
        "processing" => MediaState::Processing,
        "ready" => MediaState::Ready,
        "failed" => MediaState::Failed,
        other => panic!(
            "media.state contained unexpected value {other:?}; expected one of \
             'processing'/'ready'/'failed'"
        ),
    }
}

/// A `media` row's `Media`-reconstructible columns, as read directly off the
/// wire (shared shape between [`find_owned`]'s `SELECT` and
/// [`update_metadata`]'s `UPDATE ... RETURNING`). Deliberately excludes
/// `object_key`/`thumb_key`/`content_type` — those stay repository/
/// storage-layer concerns, never surfacing on the reconstructed [`Media`]
/// domain value (see this module's doc comment).
type MediaRow = (
    i64,
    i64,
    String,
    String,
    Option<String>,
    f32,
    f32,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<String>,
    OffsetDateTime,
);

/// The column list [`find_owned`]/[`update_metadata`] share, matching
/// [`MediaRow`]'s tuple shape exactly. A `macro_rules!`-based textual
/// constant (not a `const &str`) specifically so [`find_owned`]/
/// [`update_metadata`] can splice it into a `concat!`-built literal `&'static
/// str` at compile time via `media_row_columns!()`, keeping the full query
/// text a genuine string literal — sqlx's `query_as` requires a
/// `'static`-literal-shaped query (its `SqlSafeStr` bound rejects a
/// runtime-built `String`, e.g. from `format!`, as an SQL-injection-auditing
/// safeguard) even though nothing here is ever untrusted input.
macro_rules! media_row_columns {
    () => {
        "id, actor_id, media_type, state, description, focus_x, focus_y, \
         orig_width, orig_height, small_width, small_height, blurhash, created_at"
    };
}

/// Reconstructs a [`Media`] from a raw row tuple.
///
/// Uses `Focus::new(...).expect(...)` on the stored `focus_x`/`focus_y`
/// pair: a value already persisted through this repository's own
/// `insert_media`/`update_metadata` (both of which only ever accept an
/// already-validated [`Focus`]) is by definition already in range, so
/// re-validating it here would only ever fail on the same kind of
/// external data-corruption `media_type_from_str`/`media_state_from_str`
/// panic on.
fn row_to_media(row: MediaRow) -> Media {
    let (
        id,
        actor_id,
        media_type,
        state,
        description,
        focus_x,
        focus_y,
        orig_width,
        orig_height,
        small_width,
        small_height,
        blurhash,
        created_at,
    ) = row;

    let meta = orig_width.zip(orig_height).map(|(width, height)| {
        let width = width as u32;
        let height = height as u32;
        let original = Dimensions {
            width,
            height,
            aspect: width as f32 / height as f32,
        };
        let small = small_width.zip(small_height).map(|(width, height)| {
            let width = width as u32;
            let height = height as u32;
            Dimensions {
                width,
                height,
                aspect: width as f32 / height as f32,
            }
        });
        MediaMeta { original, small }
    });

    Media {
        id: Id::from_i64(id),
        actor_id: Id::from_i64(actor_id),
        media_type: media_type_from_str(&media_type),
        state: media_state_from_str(&state),
        description,
        focus: Focus::new(focus_x, focus_y)
            .expect("focus persisted in media must already be within the valid range"),
        meta,
        blurhash,
        created_at,
    }
}

/// Maps a failed `INSERT INTO media` to an [`AppError`]. A primary-key
/// collision on `id` should not occur in practice (`IdGenerator`'s
/// uniqueness contract), so any failure here is treated as an unexpected
/// `Server` (5xx) error, mirroring `oauth/code_repository.rs::map_insert_error`'s
/// identical reasoning.
fn map_insert_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

fn map_query_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Persists `media` as a new `media` row (Requirement 1.1, 1.2): `actor_id`
/// is required by [`Media`]'s own type (task 2.1 makes an ownerless `Media`
/// unconstructable), so no separate runtime check is needed for "所有
/// アクター必須" here. `object_key`/`content_type` fill the two `NOT NULL`
/// storage columns [`Media`] itself cannot supply — see this module's doc
/// comment ("`insert_media` gains `object_key`/`content_type` parameters")
/// for why. `thumb_key` is always persisted as `NULL` (no thumbnail exists
/// yet at insert time); [`set_ready`] is what fills it in later.
///
/// `media.created_at` is used for both the `created_at` and `updated_at`
/// columns at insert time (`Media` carries no separate `updated_at` field of
/// its own) — the two are only expected to diverge once a later
/// [`update_metadata`]/[`set_ready`]/[`set_failed`] call stamps a fresh
/// `now`.
pub async fn insert_media(
    pool: &PgPool,
    media: &Media,
    object_key: &str,
    content_type: &str,
) -> Result<(), AppError> {
    let (orig_width, orig_height, small_width, small_height) = match &media.meta {
        Some(meta) => (
            Some(meta.original.width as i32),
            Some(meta.original.height as i32),
            meta.small.map(|d| d.width as i32),
            meta.small.map(|d| d.height as i32),
        ),
        None => (None, None, None, None),
    };

    sqlx::query(
        "INSERT INTO media \
            (id, actor_id, media_type, state, description, focus_x, focus_y, \
             orig_width, orig_height, small_width, small_height, blurhash, \
             object_key, thumb_key, content_type, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
    )
    .bind(media.id.as_i64())
    .bind(media.actor_id.as_i64())
    .bind(media_type_as_str(media.media_type))
    .bind(media_state_as_str(media.state))
    .bind(&media.description)
    .bind(media.focus.x())
    .bind(media.focus.y())
    .bind(orig_width)
    .bind(orig_height)
    .bind(small_width)
    .bind(small_height)
    .bind(&media.blurhash)
    .bind(object_key)
    .bind(Option::<&str>::None)
    .bind(content_type)
    .bind(media.created_at)
    .bind(media.created_at)
    .execute(pool)
    .await
    .map_err(map_insert_error)?;

    Ok(())
}

/// Looks up the [`Media`] persisted under `media_id`, scoped to `actor_id`
/// (Requirements 2.2, 2.3, 2.4, 3.3): returns `Ok(None)` — not an error —
/// both when no row matches `media_id` at all, and when it does but is
/// owned by a different actor. See this module's doc comment ("Owner-scoped
/// lookup postcondition") for why those two cases are made
/// indistinguishable by construction here.
pub async fn find_owned(
    pool: &PgPool,
    media_id: Id,
    actor_id: Id,
) -> Result<Option<Media>, AppError> {
    let row: Option<MediaRow> = sqlx::query_as(concat!(
        "SELECT ",
        media_row_columns!(),
        " FROM media WHERE id = $1 AND actor_id = $2"
    ))
    .bind(media_id.as_i64())
    .bind(actor_id.as_i64())
    .fetch_optional(pool)
    .await
    .map_err(map_query_error)?;

    Ok(row.map(row_to_media))
}

/// Updates the description and/or focal point of the [`Media`] persisted
/// under `media_id`, scoped to `actor_id` (Requirements 3.1, 3.3, 3.4):
/// returns the updated [`Media`] on success, or `Ok(None)` when `media_id`
/// does not exist or is not owned by `actor_id` (same "not found or not
/// yours, indistinguishable" contract as [`find_owned`]). `description`/
/// `focus` are independently optional patch fields — see this module's doc
/// comment ("`update_metadata`'s patch semantics") for the exact atomic
/// COALESCE-based update this performs and why. Never filters on `state`,
/// so an update against a still-`processing` media succeeds (Requirement
/// 3.4). `now` stamps `updated_at` — see this module's doc comment for why
/// this parameter is an intentional extension of design.md's sketch.
pub async fn update_metadata(
    pool: &PgPool,
    media_id: Id,
    actor_id: Id,
    description: Option<&str>,
    focus: Option<Focus>,
    now: OffsetDateTime,
) -> Result<Option<Media>, AppError> {
    let focus_x = focus.map(|f| f.x());
    let focus_y = focus.map(|f| f.y());

    let row: Option<MediaRow> = sqlx::query_as(concat!(
        "UPDATE media SET \
            description = COALESCE($1, description), \
            focus_x = COALESCE($2, focus_x), \
            focus_y = COALESCE($3, focus_y), \
            updated_at = $4 \
         WHERE id = $5 AND actor_id = $6 \
         RETURNING ",
        media_row_columns!()
    ))
    .bind(description)
    .bind(focus_x)
    .bind(focus_y)
    .bind(now)
    .bind(media_id.as_i64())
    .bind(actor_id.as_i64())
    .fetch_optional(pool)
    .await
    .map_err(map_query_error)?;

    Ok(row.map(row_to_media))
}

/// Transitions the [`Media`] persisted under `media_id` to
/// [`MediaState::Ready`] and reflects the derived metadata a successful
/// processing job produced (Requirement 4.3): original/small dimensions
/// (from `meta`), `blurhash`, and the small/thumbnail derivative's storage
/// key (`thumb_key` — see this module's doc comment for why this parameter
/// is an intentional extension of design.md's sketch). `now` stamps
/// `updated_at` (same extension rationale as [`update_metadata`]'s `now`).
///
/// Does not itself verify `media_id` exists (mirrors
/// `actor/repository.rs::update_state`'s "no error for absence" convention
/// at this data layer) — an unknown `media_id` simply updates zero rows,
/// no error. The caller (`ProcessingWorker`, task 4.3) is expected to only
/// ever call this for a `media_id` it just claimed a real job for.
pub async fn set_ready(
    pool: &PgPool,
    media_id: Id,
    meta: &MediaMeta,
    blurhash: &str,
    thumb_key: &str,
    now: OffsetDateTime,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE media SET \
            state = 'ready', \
            orig_width = $1, orig_height = $2, \
            small_width = $3, small_height = $4, \
            blurhash = $5, thumb_key = $6, updated_at = $7 \
         WHERE id = $8",
    )
    .bind(meta.original.width as i32)
    .bind(meta.original.height as i32)
    .bind(meta.small.map(|d| d.width as i32))
    .bind(meta.small.map(|d| d.height as i32))
    .bind(blurhash)
    .bind(thumb_key)
    .bind(now)
    .bind(media_id.as_i64())
    .execute(pool)
    .await
    .map_err(map_query_error)?;

    Ok(())
}

/// Transitions the [`Media`] persisted under `media_id` to
/// [`MediaState::Failed`] (Requirement 4.5's terminal-failure state
/// reflection; retry-budget/attempt accounting itself is
/// `ProcessingJobQueue`'s concern, task 3.2 — this function only performs
/// the resulting media-state transition). `now` stamps `updated_at` (same
/// extension rationale as [`update_metadata`]'s `now`). Same "no error for
/// an unknown `media_id`" contract as [`set_ready`].
pub async fn set_failed(pool: &PgPool, media_id: Id, now: OffsetDateTime) -> Result<(), AppError> {
    sqlx::query("UPDATE media SET state = 'failed', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(media_id.as_i64())
        .execute(pool)
        .await
        .map_err(map_query_error)?;

    Ok(())
}
