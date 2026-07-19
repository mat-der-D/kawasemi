//! `MediaService` (design.md "Media Service / サービス層" -> `MediaService`,
//! Requirements 1.1, 1.3, 1.4, 1.5, 1.6, 2.1, 2.2, 3.1, 3.2, 7.4; task 4.1,
//! `Boundary: MediaService`): the media business-service layer aggregating
//! upload acceptance (format/size validation -> original storage -> media
//! insertion in `processing` state -> processing-job enqueue), owner-scoped
//! status lookup, and description/focus metadata update (accepted even
//! while still `processing`, out-of-range focus rejected).
//!
//! Scope: this module owns exactly the three operations design.md's Service
//! Interface sketches for this component — [`MediaService::accept_upload`],
//! [`MediaService::show_media`], [`MediaService::update_metadata`] —
//! orchestrating `media_repository`/`job_queue`/`store` (tasks 3.1/3.2/2.2,
//! already implemented) against a plain `&PgPool`, an injected
//! [`RuntimeContext`], and a caller-supplied [`MediaStore`] implementation.
//! It does not implement the HTTP surface (`MediaEndpoints`, task 5.1), does
//! not wire itself into `AppState`/`bootstrap.rs`/the router (task 5.2), and
//! does not implement `MediaAttachmentSerializer` (task 4.2) or
//! `ProcessingWorker` (task 4.3) — those are separate boundaries.
//!
//! ## `MediaService<S: MediaStore>` held as a generic type parameter, not
//! `Arc<dyn MediaStore>`
//! `MediaStore` (`store.rs`) is `#[allow(async_fn_in_trait)]`-based and not
//! `dyn`-object-safe (its own doc comment says as much) — mirroring this
//! crate's established precedent for every other non-object-safe async port
//! consumed by a concrete service (`DeliveryWorker<Q, H>`,
//! `InboxService<V, B, D>`, `SignatureNegotiator<H: FederationHttpClient>`,
//! etc. under `src/federation/`), `MediaService` takes its `MediaStore`
//! implementation as a generic type parameter `S`, held by value (not
//! `Arc<dyn MediaStore>`). A later task 5.2 that needs to store a
//! `MediaService` behind `AppState` is responsible for choosing a concrete
//! `S` (`LocalFsStore`, task 2.2) at that call site — this task does not
//! pre-decide that.
//!
//! ## `UploadInput`/`MetadataPatch` shapes (judgment call, not in design.md's
//! excerpt)
//! design.md's Service Interface sketch names `UploadInput`/`MetadataPatch`
//! as parameter types but does not define their fields anywhere in the
//! excerpted document. Both are defined here, minimally, to carry exactly
//! what this task's acceptance criteria require:
//! - [`UploadInput`]: `bytes`/`content_type` (validated against
//!   `MediaConfig::supported_formats`/`max_upload_size_bytes`, Requirements
//!   1.3, 1.4), an optional `description` (Requirement 1.5), and an
//!   optional raw `focus: Option<(f32, f32)>` coordinate pair — *not*
//!   `Option<Focus>` — so this service, not the caller, owns converting it
//!   through the fallible [`Focus::new`] and reporting a range violation as
//!   an [`AppError`] (Requirement 7.4's "その指定を受理せず...拒否する"
//!   applies identically at upload time per this task's own instructions:
//!   "focus out-of-range must be rejected the same way as at update time").
//!   A caller handing this service an already-`Focus`-typed value would
//!   have had to perform that same validation itself first, defeating the
//!   point of centralizing it in this business-service boundary.
//! - [`MetadataPatch`]: `description: Option<String>` and
//!   `focus: Option<(f32, f32)>`, deliberately mirroring
//!   `media_repository::update_metadata`'s own exact patch semantics
//!   (`None` = leave unchanged, `Some` = set to this value) rather than a
//!   `description: Option<Option<String>>` "explicit clear" shape: the
//!   repository layer this service calls has no way to explicitly reset
//!   `description` back to SQL `NULL` at all (its `COALESCE(bound_value,
//!   existing_column)` update means a bound SQL `NULL` is indistinguishable
//!   from "leave unchanged", see `media_repository.rs`'s own doc comment,
//!   "`update_metadata`'s patch semantics") — offering a richer
//!   `Option<Option<String>>` shape here would silently promise a
//!   capability this service cannot actually deliver.
//!
//! ## Focus validation is shared, symmetric logic (Requirement 7.4 applies
//! identically at upload and update time)
//! [`validate_focus`] is the single place either `(f32, f32)` coordinate
//! pair — from [`UploadInput::focus`] or [`MetadataPatch::focus`] — is
//! turned into a validated [`Focus`] or rejected, via [`Focus::new`]
//! (task 2.1's fallible constructor) mapped to an [`AppError`]
//! (`422 Unprocessable Entity`, mirroring this crate's established
//! `AppError::client(StatusCode::UNPROCESSABLE_ENTITY, ...)` convention for
//! a rejected-but-well-formed input, e.g. `image_processor.rs`,
//! `oauth/scope.rs`). Both [`MediaService::accept_upload`] and
//! [`MediaService::update_metadata`] validate focus *before* performing any
//! storage/database write (fail-fast, matching this task's own acceptance
//! text: "未対応形式・上限超過・フォーカル範囲外を検証エラーとして拒否").
//!
//! ## Validation ordering in `accept_upload` (fail-fast, Requirements 1.3,
//! 1.4)
//! Format, then size, then focus are all checked — and any failure returns
//! immediately — before [`MediaStore::put`], `media_repository::insert_media`,
//! or `job_queue::enqueue` ever run (design.md's sequence flow: "validate
//! format and size" precedes "put original object"). Requirements 1.3/1.4's
//! "メディアを保管せず...拒否する" is satisfied structurally: there is no
//! code path from a validation failure to any of those three side-effecting
//! calls.
//!
//! ## `media_type` is inferred from `content_type`, not caller-supplied
//! `UploadInput` carries no explicit `media_type` field: design.md's model
//! sketch/Requirement 10.3 fix the MVP's media-type surface to images only
//! (`supported_formats` defaults to four raster MIME types), so
//! [`media_type_for_content_type`] maps any `image/*` content type accepted
//! by format validation to [`MediaType::Image`] — the only variant this
//! MVP's `ProcessingWorker`/`PureRustImageProcessor` (task 2.3, already
//! implemented) actually processes end to end. A non-`image/*` content type
//! reaching this function at all would mean an operator configured
//! `supported_formats` outside the MVP's documented scope (Requirement
//! 10.3) — [`MediaType::Unknown`] is returned rather than panicking, so such
//! a misconfiguration degrades to an inert, never-processed attachment
//! instead of a crash.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;

use crate::config::MediaConfig;
use crate::domain::Id;
use crate::error::AppError;
use crate::media::job_queue;
use crate::media::media_repository;
use crate::media::model::{Focus, FocusRangeError, Media, MediaState, MediaType};
use crate::media::store::{MediaStore, ObjectKey};
use crate::runtime::RuntimeContext;

/// Input to [`MediaService::accept_upload`] (Requirements 1.1, 1.2, 1.3,
/// 1.4, 1.5, 1.6). See this module's doc comment ("`UploadInput`/
/// `MetadataPatch` shapes") for why `focus` is a raw coordinate pair, not
/// an already-validated [`Focus`].
#[derive(Debug, Clone, PartialEq)]
pub struct UploadInput {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub description: Option<String>,
    pub focus: Option<(f32, f32)>,
}

/// Input to [`MediaService::update_metadata`] (Requirements 3.1, 3.2, 3.4,
/// 7.4). `None` on either field means "leave unchanged" — see this module's
/// doc comment for why `description` has no separate "explicit clear"
/// variant.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetadataPatch {
    pub description: Option<String>,
    pub focus: Option<(f32, f32)>,
}

/// Maps a [`FocusRangeError`] to a caller-facing [`AppError`] (Requirement
/// 7.4), mirroring this crate's established
/// `AppError::client(StatusCode::UNPROCESSABLE_ENTITY, ...)` convention for
/// a rejected-but-well-formed input.
fn focus_range_error_to_app_error(err: FocusRangeError) -> AppError {
    AppError::client(StatusCode::UNPROCESSABLE_ENTITY, err.to_string())
}

/// Validates an optional raw focus coordinate pair, converting it to a
/// [`Focus`] via the fallible [`Focus::new`] (task 2.1) or rejecting it as
/// an [`AppError`] (Requirement 7.4) — shared by
/// [`MediaService::accept_upload`] and [`MediaService::update_metadata`],
/// see this module's doc comment ("Focus validation is shared, symmetric
/// logic").
fn validate_focus(coords: Option<(f32, f32)>) -> Result<Option<Focus>, AppError> {
    coords
        .map(|(x, y)| Focus::new(x, y).map_err(focus_range_error_to_app_error))
        .transpose()
}

/// Rejects `content_type` unless it exactly matches one of
/// `config.supported_formats` (Requirement 1.3).
fn validate_format(config: &MediaConfig, content_type: &str) -> Result<(), AppError> {
    if config
        .supported_formats
        .iter()
        .any(|supported| supported == content_type)
    {
        Ok(())
    } else {
        Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("unsupported media format: {content_type}"),
        ))
    }
}

/// Rejects an upload whose byte length exceeds
/// `config.max_upload_size_bytes` (Requirement 1.4).
fn validate_size(config: &MediaConfig, len: usize) -> Result<(), AppError> {
    if (len as u64) <= config.max_upload_size_bytes {
        Ok(())
    } else {
        Err(AppError::client(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "upload of {len} bytes exceeds the maximum accepted size of {} bytes",
                config.max_upload_size_bytes
            ),
        ))
    }
}

/// Maps an accepted upload `content_type` to a [`MediaType`] (see this
/// module's doc comment, "`media_type` is inferred from `content_type`").
fn media_type_for_content_type(content_type: &str) -> MediaType {
    if content_type.starts_with("image/") {
        MediaType::Image
    } else {
        MediaType::Unknown
    }
}

/// The media business-service layer (design.md's `MediaService`, task 4.1).
/// See this module's doc comment for why `S: MediaStore` is a generic type
/// parameter rather than `Arc<dyn MediaStore>`.
pub struct MediaService<S: MediaStore> {
    pool: PgPool,
    runtime: RuntimeContext,
    config: MediaConfig,
    store: S,
}

impl<S: MediaStore> MediaService<S> {
    /// Builds a service bound to `pool` (passed through to
    /// `media_repository`/`job_queue`), `runtime` (the injected clock/id
    /// boundaries — identifiers and timestamps are never drawn from
    /// `OffsetDateTime::now_utc()`/ad hoc generation), `config`
    /// (`AppConfig.media`, this task's validation source of truth), and
    /// `store` (a [`MediaStore`] implementation the original upload's bytes
    /// are persisted through).
    pub fn new(pool: PgPool, runtime: RuntimeContext, config: MediaConfig, store: S) -> Self {
        Self {
            pool,
            runtime,
            config,
            store,
        }
    }

    /// Accepts a new upload (Requirements 1.1, 1.2, 1.3, 1.4, 1.5, 1.6):
    /// validates `input.content_type`/`input.bytes.len()`/`input.focus`
    /// (in that order, all before any write — see this module's doc
    /// comment, "Validation ordering"), stores the original bytes via
    /// [`MediaStore::put`], inserts the resulting [`Media`] in
    /// [`MediaState::Processing`] bound to `actor_id` (Requirement 1.2),
    /// and enqueues its processing job (Requirement 1.6). Returns the
    /// freshly-inserted, still-`processing` [`Media`].
    pub async fn accept_upload(&self, actor_id: Id, input: UploadInput) -> Result<Media, AppError> {
        validate_format(&self.config, &input.content_type)?;
        validate_size(&self.config, input.bytes.len())?;
        let focus = validate_focus(input.focus)?.unwrap_or_default();

        let id = self.runtime.ids.next_id();
        let now = self.runtime.clock.now();

        let object_key = ObjectKey::original(id);
        self.store
            .put(&object_key, &input.bytes, &input.content_type)
            .await?;

        let media = Media {
            id,
            actor_id,
            media_type: media_type_for_content_type(&input.content_type),
            state: MediaState::Processing,
            description: input.description,
            focus,
            meta: None,
            blurhash: None,
            created_at: now,
        };

        media_repository::insert_media(
            &self.pool,
            &media,
            object_key.as_str(),
            &input.content_type,
        )
        .await?;

        job_queue::enqueue(&self.pool, self.runtime.ids.as_ref(), id, now).await?;

        Ok(media)
    }

    /// Owner-scoped status lookup (Requirements 2.1, 2.2): returns `Ok(None)`
    /// both when `media_id` does not exist and when it exists but is not
    /// owned by `actor_id` (delegating to `media_repository::find_owned`'s
    /// identical, already-reviewed "indistinguishable" contract — mapping
    /// that to a 404/206/200 HTTP response is `MediaEndpoints`'s job, task
    /// 5.1, out of this task's boundary).
    pub async fn show_media(&self, actor_id: Id, media_id: Id) -> Result<Option<Media>, AppError> {
        media_repository::find_owned(&self.pool, media_id, actor_id).await
    }

    /// Owner-scoped description/focus update (Requirements 3.1, 3.2, 3.4,
    /// 7.4): validates `patch.focus` before ever calling
    /// `media_repository::update_metadata` (Requirement 7.4's "受理せず"),
    /// never filters on media state, so an update against a still-
    /// `processing` media succeeds (Requirement 3.4, matched exactly by
    /// `media_repository::update_metadata`'s own "never filters on state"
    /// behavior). Returns `Ok(None)` for the same not-found-or-not-owned
    /// case `show_media` does.
    pub async fn update_metadata(
        &self,
        actor_id: Id,
        media_id: Id,
        patch: MetadataPatch,
    ) -> Result<Option<Media>, AppError> {
        let focus = validate_focus(patch.focus)?;
        let now = self.runtime.clock.now();

        media_repository::update_metadata(
            &self.pool,
            media_id,
            actor_id,
            patch.description.as_deref(),
            focus,
            now,
        )
        .await
    }
}
