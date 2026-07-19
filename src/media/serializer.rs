//! `MediaAttachmentSerializer` (design.md "Media API / エンドポイント層" ->
//! "MediaEndpoints / MediaAttachmentSerializer", Requirements 2.2, 7.2, 7.3,
//! 8.1, 8.2, 8.3, 8.4; task 4.2, `Boundary: MediaAttachmentSerializer`):
//! serializes a persisted [`Media`] into the Mastodon-compatible
//! MediaAttachment JSON contract (design.md's Data Contracts &
//! Integration: `id`/`type`/`url`/`preview_url`/`remote_url`/
//! `meta`(`original`,`small`,`focus`)/`description`/`blurhash`).
//!
//! Scope: this module owns exactly the pure serialization function
//! ([`to_media_attachment`]/[`to_json`]) and its typed JSON shape
//! ([`MediaAttachmentJson`]/[`MediaMetaJson`]/[`DimensionsJson`]/
//! [`FocusJson`]). It does not implement `MediaService` (task 4.1, already
//! implemented, consumed here only through its output type [`Media`]), does
//! not implement `ProcessingWorker` (task 4.3), and does not implement any
//! HTTP surface (`MediaEndpoints`, task 5.1) or runtime wiring (task 5.2) —
//! this module has no `axum`/router/`AppState` code at all. It takes an
//! already-resolved `&Media` and an already-resolved [`ForwardedOrigin`] as
//! plain inputs; extracting those from a live HTTP request is task 5.1's
//! job.
//!
//! ## Typed struct + `Serialize`, not a hand-built `serde_json::json!` value
//! No other entity serializer exists yet anywhere in this crate to follow a
//! precedent from (grepped for `*_serializer.rs`/`impl Serialize`/
//! `fn to_json` across `src/` — this is the first). Given a free choice,
//! this module defines [`MediaAttachmentJson`] (and its nested
//! [`MediaMetaJson`]/[`DimensionsJson`]/[`FocusJson`]) as plain
//! `#[derive(Serialize)]` structs mirroring design.md's Data Contracts
//! field list exactly, rather than building a `serde_json::Value` by hand
//! with the `json!` macro: the struct's field list *is* the contract (a
//! reviewer/future serializer can see the whole shape in one place, and the
//! compiler enforces every field is always emitted), whereas a `json!{...}`
//! literal would let a field silently go missing without any compile-time
//! signal. [`to_json`] is a thin `serde_json::to_value` wrapper over
//! [`to_media_attachment`], provided because
//! [`crate::contract::assert_golden`]'s signature is `(&str, &serde_json::Value)`.
//!
//! ## `url`/`preview_url` null discipline (Requirements 2.2, 8.1, 8.2)
//! `url` (the original object's URL) is `Some(...)` **only** when
//! `media.state == MediaState::Ready` — not merely "the original bytes
//! exist in the store". The original upload's bytes are already durably
//! stored via `MediaStore::put` at *upload* time
//! (`MediaService::accept_upload`, task 4.1, before the processing job even
//! runs), so "does the object physically exist" is never a reliable signal
//! for whether processing has completed — only [`Media::state`] is
//! (design.md: "メディア状態が処理進捗の真実源"). Requirement 8.2's "処理未
//! 完了であるとき...URL を未確定として表現" is read as "state is not yet
//! `Ready`" (covering both `Processing` and `Failed`, matching
//! `MediaEndpoints`'s own design — a `Failed` media's `GET` never even
//! reaches this serializer, returning a `422` error body directly per
//! design.md's API Contract table — so a `Failed` media reaching this
//! function at all is an edge case this module still handles safely rather
//! than panicking).
//!
//! `preview_url` uses a narrower, *data-driven* gate instead of repeating
//! the `state == Ready` check: `Some(...)` only when
//! `media.meta.as_ref().and_then(|meta| meta.small).is_some()` — i.e. only
//! once thumbnail dimensions are actually recorded. This is deliberately
//! not redundant with `url`'s gate: `Media::meta`/`blurhash` are only ever
//! populated on a successful processing completion (`model.rs`'s own
//! documented invariant: "they stay `None` while `state ==
//! MediaState::Processing` and permanently `None` if `state ==
//! MediaState::Failed`"), so gating on the *presence of small dimensions*
//! is self-consistently `None` during `Processing`/`Failed` without ever
//! having to name `MediaState` a second time, and stays correctly `None`
//! even in the (invariant-violating, but not type-excluded) case of a
//! `Ready` media whose worker never actually produced a thumbnail
//! (`MediaMeta::small: Option<Dimensions>` — see `model.rs`'s own doc
//! comment on why that field can be `None`).
//!
//! ## `meta.original`/`meta.small` are omitted (not null) until confirmed
//! (Requirement 2.2's "確定済みのメタデータのみを含める")
//! [`MediaMetaJson::original`]/[`MediaMetaJson::small`] use
//! `#[serde(skip_serializing_if = "Option::is_none")]` rather than emitting
//! an explicit JSON `null`, so a still-`Processing` media's `meta` object
//! contains only `focus` (always known, defaulting to center — Requirement
//! 7.2) — no fabricated/placeholder dimension fields at all, matching this
//! task's acceptance text literally ("確定済みのメタデータのみを含める").
//!
//! ## `remote_url` is always `null` (Requirement 8.1, design.md non-goal)
//! [`MediaAttachmentJson::remote_url`] is hard-coded `None` — there is no
//! remote-media-cache concept in this MVP spec at all (design.md's Out of
//! Boundary list), so this is not a field this function ever has real data
//! for.
//!
//! ## Golden fixtures (Requirements 8.3, 8.4)
//! This module's own unit tests (`serializer/tests.rs`) register both the
//! `Processing` and `Ready` variants of [`to_json`]'s output as goldens via
//! [`crate::contract::assert_golden`] (Requirement 8.3), built from
//! literal, hand-constructed [`Media`] fixtures rather than any live
//! clock/id/rng source — there is nothing non-deterministic upstream of
//! this module to inject a `RuntimeContext` boundary for in the first
//! place: [`to_media_attachment`] is a pure function of its `&Media`/
//! `&ForwardedOrigin` arguments (no `created_at` field is even part of the
//! MediaAttachment JSON contract), so literal fixture values already
//! satisfy Requirement 8.4's "reproducible golden" the same way
//! `model.rs`'s own tests' literal `Id`/`datetime!` fixtures do. The golden
//! JSON files themselves live under `tests/golden/media/` (relative to
//! `CARGO_MANIFEST_DIR`), following `crate::contract`'s own documented
//! convention ("golden files... the calling spec's own test authors own
//! and check in that file directly, e.g. `tests/golden/accounts/
//! show_public.json`"). A later task 6.3 (`Depends: 5.2, 4.2`) builds the
//! full `spawn_test_app`-backed contract test (`tests/
//! media_attachment_contract_it.rs`, design.md's File Structure Plan) that
//! exercises this same serializer end to end through a real `MediaService`/
//! `MediaStore`; this task's golden tests only prove the serializer's own
//! output shape is registrable and reproducible, standalone.

#[cfg(test)]
mod tests;

use serde::Serialize;
use serde_json::Value;

use crate::api::pagination::ForwardedOrigin;
use crate::domain::Id;
use crate::media::model::{Dimensions, Focus, Media, MediaState, MediaType};
use crate::media::store::{MediaStore, ObjectKey};

/// JSON shape of a [`Dimensions`] value within [`MediaMetaJson`]
/// (`meta.original`/`meta.small`, Requirement 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DimensionsJson {
    pub width: u32,
    pub height: u32,
    pub aspect: f32,
}

impl From<Dimensions> for DimensionsJson {
    fn from(dimensions: Dimensions) -> Self {
        DimensionsJson {
            width: dimensions.width,
            height: dimensions.height,
            aspect: dimensions.aspect,
        }
    }
}

/// JSON shape of a [`Focus`] value within [`MediaMetaJson`] (`meta.focus`,
/// Requirements 7.1, 7.2, 7.3).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FocusJson {
    pub x: f32,
    pub y: f32,
}

impl From<Focus> for FocusJson {
    fn from(focus: Focus) -> Self {
        FocusJson {
            x: focus.x(),
            y: focus.y(),
        }
    }
}

/// JSON shape of [`MediaAttachmentJson::meta`] (Requirement 8.1's
/// `meta(original/small/focus)`). `original`/`small` are omitted entirely
/// (not emitted as `null`) until confirmed — see this module's doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MediaMetaJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original: Option<DimensionsJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small: Option<DimensionsJson>,
    pub focus: FocusJson,
}

/// The Mastodon-compatible MediaAttachment JSON contract (design.md's Data
/// Contracts & Integration, Requirement 8.1). See this module's doc comment
/// for the null-discipline rules governing `url`/`preview_url`/`meta`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MediaAttachmentJson {
    pub id: Id,
    /// Mastodon's `type` field (`image`/`gifv`/`video`/`audio`/`unknown`,
    /// see [`media_type_str`]). Named `r#type` (a raw identifier) rather
    /// than `#[serde(rename = "type")]` on a differently-named field, since
    /// `type` is otherwise a reserved word — `serde`'s derive strips the
    /// `r#` prefix on its own, so this already serializes as the bare
    /// `"type"` key with no extra attribute needed.
    pub r#type: &'static str,
    pub url: Option<String>,
    pub preview_url: Option<String>,
    /// Always `None` for MVP — see this module's doc comment
    /// ("`remote_url` is always `null`").
    pub remote_url: Option<String>,
    pub meta: MediaMetaJson,
    pub description: Option<String>,
    pub blurhash: Option<String>,
}

/// Maps a [`MediaType`] to Mastodon's exact wire string (design.md's model
/// sketch comment: `image/gifv/video/audio/unknown`). No mapping table
/// elsewhere in the crate to reuse (`config.rs`'s `MediaConfig` only lists
/// accepted upload MIME types, never Mastodon `type` strings) — this is the
/// first and only place this mapping is defined.
fn media_type_str(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Image => "image",
        MediaType::Gifv => "gifv",
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Unknown => "unknown",
    }
}

/// Serializes `media` into the MediaAttachment JSON contract (Requirements
/// 2.2, 7.2, 7.3, 8.1, 8.2), resolving `url`/`preview_url` through `store`'s
/// proxy-aware [`MediaStore::public_url`] (Requirement 5.4, via `origin`)
/// only once the corresponding derivative is actually confirmed ready —
/// see this module's doc comment for the exact null-discipline rules.
pub fn to_media_attachment(
    media: &Media,
    store: &impl MediaStore,
    origin: &ForwardedOrigin,
) -> MediaAttachmentJson {
    let is_ready = media.state == MediaState::Ready;
    let small_dimensions = media.meta.as_ref().and_then(|meta| meta.small);

    let url = is_ready.then(|| store.public_url(&ObjectKey::original(media.id), origin));
    let preview_url =
        small_dimensions.map(|_| store.public_url(&ObjectKey::small(media.id), origin));

    let meta = MediaMetaJson {
        original: media.meta.as_ref().map(|meta| meta.original.into()),
        small: small_dimensions.map(Into::into),
        focus: media.focus.into(),
    };

    MediaAttachmentJson {
        id: media.id,
        r#type: media_type_str(media.media_type),
        url,
        preview_url,
        remote_url: None,
        meta,
        description: media.description.clone(),
        blurhash: media.blurhash.clone(),
    }
}

/// [`to_media_attachment`], converted to a plain [`serde_json::Value`] —
/// the shape [`crate::contract::assert_golden`] compares against
/// (Requirement 8.3).
pub fn to_json(media: &Media, store: &impl MediaStore, origin: &ForwardedOrigin) -> Value {
    serde_json::to_value(to_media_attachment(media, store, origin))
        .expect("MediaAttachmentJson always serializes to JSON")
}
