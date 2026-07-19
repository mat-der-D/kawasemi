//! Unit tests for `MediaAttachmentSerializer` (task 4.2, Requirements 2.2,
//! 7.2, 7.3, 8.1, 8.2, 8.3, 8.4), per this task's observable completion
//! condition: "処理中で url=null・完了で実体/プレビュー URL・focus 既定中央
//! が出力されることを単体テストで確認できる".
//!
//! All fixtures below are literal, hand-constructed `Media` values (no
//! `RuntimeContext`/clock/id involved) — see `serializer.rs`'s own doc
//! comment ("Golden fixtures") for why that alone already satisfies
//! Requirement 8.4's determinism: [`to_media_attachment`]/[`to_json`] are
//! pure functions of their `&Media`/`&ForwardedOrigin` arguments, and
//! `created_at` (the one field a real `Clock` would supply) is not even
//! part of the MediaAttachment JSON contract.

use std::path::PathBuf;

use serde_json::{Value, json};
use time::macros::datetime;

use super::*;
use crate::contract::assert_golden;
use crate::domain::Id;
use crate::media::local_fs::LocalFsStore;
use crate::media::model::{Dimensions, MediaMeta};

/// A fixed proxy-resolved origin (Requirement 5.4) every test in this file
/// resolves `url`/`preview_url` against.
fn origin() -> ForwardedOrigin {
    ForwardedOrigin::resolve(
        "http",
        "127.0.0.1:8080",
        Some("https"),
        Some("example.social"),
    )
}

/// A `LocalFsStore` rooted at a path that is never actually touched:
/// `MediaStore::public_url` (the only method these tests exercise) never
/// reads or writes the filesystem — see `local_fs.rs`'s `public_url` impl —
/// so no real temp directory / cleanup guard is needed here, unlike
/// `local_fs/tests.rs`'s `put`/`get`/`delete` tests.
fn store() -> LocalFsStore {
    LocalFsStore::new(PathBuf::from(
        "/nonexistent-kawasemi-media-serializer-test-root",
    ))
}

/// A still-processing upload: description recorded, focus left at its
/// default, no derived metadata confirmed yet (Requirement 1.1, 2.1).
fn processing_media() -> Media {
    Media {
        id: Id::from_i64(1001),
        actor_id: Id::from_i64(42),
        media_type: MediaType::Image,
        state: MediaState::Processing,
        description: Some("a golden retriever mid-fetch".to_string()),
        focus: Focus::default(),
        meta: None,
        blurhash: None,
        created_at: datetime!(2026-01-01 00:00:00 UTC),
    }
}

/// A fully processed upload: derived original/small dimensions, BlurHash,
/// and an explicitly recorded (non-center) focus (Requirement 4.3, 6.1-6.3).
fn ready_media() -> Media {
    Media {
        id: Id::from_i64(2002),
        actor_id: Id::from_i64(42),
        media_type: MediaType::Image,
        state: MediaState::Ready,
        description: Some("a golden retriever mid-fetch".to_string()),
        focus: Focus::new(0.25, -0.5).unwrap(),
        meta: Some(MediaMeta {
            original: Dimensions {
                width: 1920,
                height: 1080,
                aspect: 1920.0 / 1080.0,
            },
            small: Some(Dimensions {
                width: 400,
                height: 225,
                aspect: 400.0 / 225.0,
            }),
        }),
        blurhash: Some("LKO2?U%2Tw=w]~RBVZRi};RPxuwH".to_string()),
        created_at: datetime!(2026-01-01 00:00:00 UTC),
    }
}

// ---- Requirement 2.2, 8.2: processing -> url=null, only confirmed meta ----

#[test]
fn processing_state_has_null_url_and_preview_url_and_only_confirmed_metadata() {
    let json = to_json(&processing_media(), &store(), &origin());

    assert_eq!(json["url"], Value::Null);
    assert_eq!(json["preview_url"], Value::Null);
    assert_eq!(json["remote_url"], Value::Null);
    // "original"/"small" are omitted entirely, not present as null.
    assert!(
        json["meta"].get("original").is_none(),
        "unconfirmed original dimensions must not appear at all: {json}"
    );
    assert!(
        json["meta"].get("small").is_none(),
        "unconfirmed small dimensions must not appear at all: {json}"
    );
    assert_eq!(json["meta"]["focus"], json!({"x": 0.0, "y": 0.0}));
    assert_eq!(json["id"], "1001");
    assert_eq!(json["type"], "image");
    assert_eq!(json["description"], "a golden retriever mid-fetch");
    assert_eq!(json["blurhash"], Value::Null);
}

// ---- Requirement 8.1, 8.2: ready -> full url/preview_url/meta ----

#[test]
fn ready_state_has_full_url_preview_url_and_meta() {
    let media = ready_media();
    let json = to_json(&media, &store(), &origin());

    assert_eq!(json["url"], "https://example.social/media/2002/original");
    assert_eq!(
        json["preview_url"],
        "https://example.social/media/2002/small"
    );
    assert_eq!(json["remote_url"], Value::Null);
    assert_eq!(
        json["meta"]["original"],
        json!({"width": 1920, "height": 1080, "aspect": 1920.0f32 / 1080.0f32})
    );
    assert_eq!(
        json["meta"]["small"],
        json!({"width": 400, "height": 225, "aspect": 400.0f32 / 225.0f32})
    );
    assert_eq!(json["meta"]["focus"], json!({"x": 0.25, "y": -0.5}));
    assert_eq!(json["id"], "2002");
    assert_eq!(json["type"], "image");
    assert_eq!(json["description"], "a golden retriever mid-fetch");
    assert_eq!(json["blurhash"], "LKO2?U%2Tw=w]~RBVZRi};RPxuwH");
}

// ---- Requirement 7.2: unset focus defaults to center ----

#[test]
fn unset_focus_defaults_to_the_center_in_the_output() {
    let mut media = ready_media();
    media.focus = Focus::default();
    let json = to_json(&media, &store(), &origin());
    assert_eq!(json["meta"]["focus"], json!({"x": 0.0, "y": 0.0}));
}

// ---- Requirement 7.3: a set focus is reflected as recorded ----

#[test]
fn a_recorded_focus_is_reflected_exactly_in_the_output() {
    let mut media = ready_media();
    // -0.75/0.5 are both exactly representable in binary floating point, so
    // the expected `json!` literal (parsed as `f64`) and the actual `f32`
    // field (widened to `f64` by serialization) compare bit-for-bit equal —
    // unlike a value such as `0.9`, whose nearest `f32` and nearest `f64`
    // representations differ once widened.
    media.focus = Focus::new(-0.75, 0.5).unwrap();
    let json = to_json(&media, &store(), &origin());
    assert_eq!(json["meta"]["focus"], json!({"x": -0.75, "y": 0.5}));
}

// ---- Requirement 8.1: remote_url is always null, in every state ----

#[test]
fn remote_url_is_always_null_regardless_of_state() {
    let mut processing = processing_media();
    processing.state = MediaState::Processing;
    let mut ready = ready_media();
    ready.state = MediaState::Ready;
    let mut failed = processing_media();
    failed.state = MediaState::Failed;

    for media in [processing, ready, failed] {
        let json = to_json(&media, &store(), &origin());
        assert_eq!(
            json["remote_url"],
            Value::Null,
            "state {:?} must still have remote_url=null: {json}",
            media.state
        );
    }
}

// ---- media_type -> Mastodon wire string mapping, all variants ----

#[test]
fn media_type_maps_to_the_expected_mastodon_wire_string_for_every_variant() {
    let cases = [
        (MediaType::Image, "image"),
        (MediaType::Gifv, "gifv"),
        (MediaType::Video, "video"),
        (MediaType::Audio, "audio"),
        (MediaType::Unknown, "unknown"),
    ];
    for (media_type, expected) in cases {
        let mut media = processing_media();
        media.media_type = media_type;
        let json = to_json(&media, &store(), &origin());
        assert_eq!(json["type"], expected, "media_type {media_type:?}");
    }
}

// ---- Failed state: same null discipline as processing (never reaches this
// serializer in the real HTTP flow — MediaEndpoints, task 5.1, returns a 422
// error body instead — but this function must not panic if it is called on
// one anyway). ----

#[test]
fn failed_state_has_null_url_and_preview_url_like_processing() {
    let mut media = processing_media();
    media.state = MediaState::Failed;
    let json = to_json(&media, &store(), &origin());
    assert_eq!(json["url"], Value::Null);
    assert_eq!(json["preview_url"], Value::Null);
    assert!(json["meta"].get("original").is_none());
    assert!(json["meta"].get("small").is_none());
}

// ---- Edge case: `Ready` but no thumbnail dimensions recorded yet (an
// invariant-violating but type-permitted `Media` value) must still leave
// `preview_url`/`meta.small` null/omitted rather than fabricating a URL for
// a derivative that was never actually produced. ----

#[test]
fn preview_url_stays_null_when_ready_but_no_thumbnail_dimensions_are_recorded() {
    let mut media = ready_media();
    media.meta = Some(MediaMeta {
        original: Dimensions {
            width: 1920,
            height: 1080,
            aspect: 1920.0 / 1080.0,
        },
        small: None,
    });
    let json = to_json(&media, &store(), &origin());
    assert_eq!(json["url"], "https://example.social/media/2002/original");
    assert_eq!(json["preview_url"], Value::Null);
    assert!(json["meta"].get("small").is_none());
}

// ---- Requirements 8.3, 8.4: golden registration ----

#[test]
fn processing_variant_matches_the_registered_contract_golden() {
    let json = to_json(&processing_media(), &store(), &origin());
    assert_golden("tests/golden/media/media_attachment_processing.json", &json);
}

#[test]
fn ready_variant_matches_the_registered_contract_golden() {
    let json = to_json(&ready_media(), &store(), &origin());
    assert_golden("tests/golden/media/media_attachment_ready.json", &json);
}
