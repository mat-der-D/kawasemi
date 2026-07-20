//! Unit tests for `CustomEmojiSerializer` (task 3.4, Requirements 9.2, 9.4),
//! per this task's observable completion condition: "shortcode/url/
//! static_url/visible_in_picker/category を持つ JSON を生成する単体テストが
//! green" plus this task's own explicit "同一表現を共有" requirement (9.4),
//! verified directly against `AccountSerializer`'s already-implemented
//! `emojis` output.

use time::macros::datetime;

use super::*;
use crate::accounts::model::{AccountView, CustomEmojiView};
use crate::accounts::model::{AccountViewFields, ProfileField};
use crate::accounts::serializer::to_account_json;

fn blobcat() -> CustomEmojiView {
    CustomEmojiView {
        shortcode: "blobcat".to_string(),
        url: "https://kawasemi.example/emoji/blobcat.png".to_string(),
        static_url: "https://kawasemi.example/emoji/blobcat_static.png".to_string(),
        visible_in_picker: true,
        category: Some("cats".to_string()),
    }
}

/// A second emoji with every field set to something different from
/// [`blobcat`], and `category: None`, to prove the mapping is field-by-field
/// rather than coincidentally matching on one fixture alone.
fn no_category_emoji() -> CustomEmojiView {
    CustomEmojiView {
        shortcode: "heart_eyes".to_string(),
        url: "https://kawasemi.example/emoji/heart_eyes.png".to_string(),
        static_url: "https://kawasemi.example/emoji/heart_eyes_static.png".to_string(),
        visible_in_picker: false,
        category: None,
    }
}

#[test]
fn every_requirement_9_2_field_is_present_with_correct_type_and_value() {
    let view = blobcat();

    let json = custom_emoji_to_json(&view);
    let obj = json.as_object().expect("CustomEmoji JSON is an object");

    for field in [
        "shortcode",
        "url",
        "static_url",
        "visible_in_picker",
        "category",
    ] {
        assert!(obj.contains_key(field), "missing field: {field}");
    }

    assert_eq!(obj["shortcode"], "blobcat");
    assert_eq!(obj["url"], "https://kawasemi.example/emoji/blobcat.png");
    assert_eq!(
        obj["static_url"],
        "https://kawasemi.example/emoji/blobcat_static.png"
    );
    assert!(obj["visible_in_picker"].is_boolean());
    assert_eq!(obj["visible_in_picker"], true);
    assert_eq!(obj["category"], "cats");
}

#[test]
fn category_none_serializes_as_json_null_not_omitted() {
    let view = no_category_emoji();

    let json = custom_emoji_to_json(&view);
    let obj = json.as_object().expect("CustomEmoji JSON is an object");

    assert!(
        obj.contains_key("category"),
        "category must still be present"
    );
    assert!(obj["category"].is_null());
    assert_eq!(obj["visible_in_picker"], false);
}

#[test]
fn same_input_produces_the_same_json_deterministically() {
    let view = blobcat();

    let first = custom_emoji_to_json(&view);
    let second = custom_emoji_to_json(&view);

    assert_eq!(first, second);
}

#[test]
fn build_custom_emoji_on_the_serializer_matches_the_free_function() {
    let view = blobcat();
    let serializer = CustomEmojiSerializer::new();

    assert_eq!(
        serializer.build_custom_emoji(&view),
        custom_emoji_to_json(&view)
    );
}

#[test]
fn to_custom_emoji_json_maps_every_field_straight_across() {
    let view = no_category_emoji();

    let json = to_custom_emoji_json(&view);

    assert_eq!(json.shortcode, view.shortcode);
    assert_eq!(json.url, view.url);
    assert_eq!(json.static_url, view.static_url);
    assert_eq!(json.visible_in_picker, view.visible_in_picker);
    assert_eq!(json.category, view.category);
}

// ---- Requirement 9.4: shared representation with Account's `emojis` ----
//
// These tests prove — not just assert in a doc comment — that this module's
// output is the exact same representation `AccountSerializer` (task 3.1)
// already produces for each entry of an account's `emojis` array. Both go
// through `crate::accounts::serializer::CustomEmojiJson`, the one shared
// type; if a future edit ever gave the two modules diverging field sets or
// diverging value mappings, these tests would catch it immediately.

#[test]
fn custom_emoji_serializer_output_is_identical_to_the_account_emojis_entry_for_the_same_view() {
    let emoji = blobcat();

    // Build a minimal local AccountView whose only referenced-emoji
    // candidate is `emoji`, referenced from `display_name` so
    // `AccountSerializer`'s shortcode-matching includes it in `emojis`
    // (mirrors `serializer/tests.rs`'s own emoji-attachment fixtures).
    let view = AccountView::local(
        crate::domain::Id::from_i64(1),
        "alice",
        AccountViewFields {
            username: "alice".to_string(),
            display_name: "hi :blobcat:".to_string(),
            locked: false,
            bot: false,
            discoverable: true,
            group: false,
            created_at: datetime!(2024-01-01 00:00:00 UTC),
            note: String::new(),
            url: "https://kawasemi.example/users/alice".to_string(),
            uri: "https://kawasemi.example/users/alice".to_string(),
            avatar: "https://kawasemi.example/avatars/original/missing.png".to_string(),
            avatar_static: "https://kawasemi.example/avatars/original/missing.png".to_string(),
            header: "https://kawasemi.example/headers/original/missing.png".to_string(),
            header_static: "https://kawasemi.example/headers/original/missing.png".to_string(),
            followers_count: 0,
            following_count: 0,
            statuses_count: 0,
            last_status_at: None,
            emojis: vec![emoji.clone()],
            fields: Vec::<ProfileField>::new(),
        },
    );

    let account_json = to_account_json(&view);
    let account_emoji_entry = account_json.emojis.first().expect("emoji was attached");

    // Same type (`CustomEmojiJson`), same field values, for the same
    // underlying `CustomEmojiView` — this is Requirement 9.4's "同一表現を
    // 共有" made concrete.
    assert_eq!(*account_emoji_entry, to_custom_emoji_json(&emoji));
    assert_eq!(
        serde_json::to_value(account_emoji_entry).unwrap(),
        custom_emoji_to_json(&emoji)
    );
}
