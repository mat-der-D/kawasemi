//! Unit tests for `AccountSerializer` (task 3.1, Requirements 1.1-1.5, 2.2),
//! per this task's observable completion condition: "同一入力で決定的 JSON
//! を生成し、avatar/header が常に非 null になる単体テストが green". Also
//! covers this task's flagged concern: an emoji shortcode colliding across
//! domains must never be mis-attached (see `serializer.rs`'s doc comment,
//! "Emoji domain-collision safety").

use std::path::PathBuf;

use serde_json::Value;
use time::macros::datetime;

use super::*;
use crate::accounts::model::{AccountProfile, CredentialSource, RemoteAccount};
use crate::actor::model::{ActorState, ActorType, Handle, ResolvedActor};
use crate::media::local_fs::LocalFsStore;

fn origin() -> ForwardedOrigin {
    ForwardedOrigin::resolve(
        "http",
        "127.0.0.1:8080",
        Some("https"),
        Some("kawasemi.example"),
    )
}

/// A `LocalFsStore` never actually touched: `MediaStore::public_url` (the
/// only method these tests exercise) never reads/writes the filesystem —
/// mirrors `media/serializer/tests.rs`'s identical precedent.
fn store() -> LocalFsStore {
    LocalFsStore::new(PathBuf::from(
        "/nonexistent-kawasemi-account-serializer-test-root",
    ))
}

fn serializer() -> AccountSerializer {
    AccountSerializer::new("kawasemi.example")
}

fn resolved_actor(id: i64, handle: &str) -> ResolvedActor {
    ResolvedActor {
        id: Id::from_i64(id),
        handle: Handle::new(handle).unwrap(),
        actor_type: ActorType::Person,
        display_name: "unused by AccountSerializer".to_string(),
        summary: "unused by AccountSerializer".to_string(),
        state: ActorState::Active,
    }
}

fn empty_source() -> CredentialSource {
    CredentialSource {
        privacy: Visibility::Public,
        sensitive: false,
        language: None,
        note: String::new(),
        fields: Vec::new(),
        follow_requests_count: 0,
    }
}

fn bare_profile(actor_id: i64) -> AccountProfile {
    AccountProfile {
        actor_id: Id::from_i64(actor_id),
        display_name: "Alice".to_string(),
        note: "hello world".to_string(),
        avatar_media: None,
        header_media: None,
        fields: Vec::new(),
        locked: false,
        bot: false,
        discoverable: true,
        source: empty_source(),
    }
}

fn zero_counts() -> AccountCounts {
    AccountCounts {
        followers: 0,
        following: 0,
        statuses: 0,
        last_status_at: None,
    }
}

fn remote(id: i64, username: &str, domain: &str) -> RemoteAccount {
    RemoteAccount {
        id: Id::from_i64(id),
        actor_uri: format!("https://{domain}/users/{username}"),
        username: username.to_string(),
        domain: domain.to_string(),
        display_name: "Remote Alice".to_string(),
        note: "hi from afar".to_string(),
        url: format!("https://{domain}/@{username}"),
        avatar_url: None,
        header_url: None,
        fields: Vec::new(),
        bot: false,
        locked: false,
        fetched_at: datetime!(2026-01-01 00:00:00 UTC),
    }
}

fn emoji(shortcode: &str, url: &str) -> CustomEmojiView {
    CustomEmojiView {
        shortcode: shortcode.to_string(),
        url: url.to_string(),
        static_url: url.to_string(),
        visible_in_picker: true,
        category: None,
    }
}

// ---- Requirement 1.1, determinism: same input -> identical JSON ----

#[test]
fn build_account_local_is_deterministic_for_the_same_input() {
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let profile = bare_profile(1);
    let counts = zero_counts();
    let created_at = datetime!(2025-06-01 12:00:00 UTC);

    let first = ser.build_account_local(
        &actor,
        &profile,
        created_at,
        &counts,
        &store(),
        &origin(),
        &[],
    );
    let second = ser.build_account_local(
        &actor,
        &profile,
        created_at,
        &counts,
        &store(),
        &origin(),
        &[],
    );

    assert_eq!(first, second);
}

#[test]
fn build_account_remote_is_deterministic_for_the_same_input() {
    let ser = serializer();
    let remote_account = remote(9, "bob", "remote.example");
    let counts = zero_counts();

    let first = ser.build_account_remote(&remote_account, &counts, &[]);
    let second = ser.build_account_remote(&remote_account, &counts, &[]);

    assert_eq!(first, second);
}

// ---- Requirement 1.5: avatar/header default to non-null URLs ----

#[test]
fn local_account_with_no_avatar_or_header_media_gets_non_null_default_urls() {
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let profile = bare_profile(1);
    let counts = zero_counts();

    let json = ser.build_account_local(
        &actor,
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &counts,
        &store(),
        &origin(),
        &[],
    );

    assert_ne!(json["avatar"], Value::Null);
    assert_ne!(json["avatar_static"], Value::Null);
    assert_ne!(json["header"], Value::Null);
    assert_ne!(json["header_static"], Value::Null);
    assert_eq!(
        json["avatar"],
        "https://kawasemi.example/avatars/original/missing.png"
    );
    assert_eq!(
        json["header"],
        "https://kawasemi.example/headers/original/missing.png"
    );
}

#[test]
fn local_account_with_avatar_media_resolves_through_the_media_store() {
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let mut profile = bare_profile(1);
    profile.avatar_media = Some(Id::from_i64(500));
    let counts = zero_counts();

    let json = ser.build_account_local(
        &actor,
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &counts,
        &store(),
        &origin(),
        &[],
    );

    assert_eq!(
        json["avatar"],
        "https://kawasemi.example/media/500/original"
    );
    assert_eq!(json["avatar"], json["avatar_static"]);
    assert_ne!(json["avatar"], Value::Null);
}

#[test]
fn remote_account_with_no_avatar_or_header_url_gets_non_null_default_urls() {
    let ser = serializer();
    let remote_account = remote(9, "bob", "remote.example");
    let counts = zero_counts();

    let json = ser.build_account_remote(&remote_account, &counts, &[]);

    assert_ne!(json["avatar"], Value::Null);
    assert_ne!(json["header"], Value::Null);
    assert_eq!(
        json["avatar"],
        "https://kawasemi.example/avatars/original/missing.png"
    );
}

#[test]
fn remote_account_with_avatar_url_uses_the_normalized_value() {
    let ser = serializer();
    let mut remote_account = remote(9, "bob", "remote.example");
    remote_account.avatar_url = Some("https://remote.example/avatars/bob.png".to_string());
    let counts = zero_counts();

    let json = ser.build_account_remote(&remote_account, &counts, &[]);

    assert_eq!(json["avatar"], "https://remote.example/avatars/bob.png");
    assert_eq!(json["avatar"], json["avatar_static"]);
}

// ---- Requirements 1.2, 1.3: acct/url/uri discipline ----

#[test]
fn local_account_acct_is_a_bare_handle_and_url_uri_are_the_actor_url() {
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let profile = bare_profile(1);
    let counts = zero_counts();

    let json = ser.build_account_local(
        &actor,
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &counts,
        &store(),
        &origin(),
        &[],
    );

    assert_eq!(json["username"], "alice");
    assert_eq!(json["acct"], "alice");
    assert_eq!(json["url"], "https://kawasemi.example/users/alice");
    assert_eq!(json["uri"], "https://kawasemi.example/users/alice");
    assert_eq!(json["id"], "1");
}

#[test]
fn remote_account_acct_is_username_at_domain_and_url_uri_differ() {
    let ser = serializer();
    let remote_account = remote(9, "bob", "remote.example");
    let counts = zero_counts();

    let json = ser.build_account_remote(&remote_account, &counts, &[]);

    assert_eq!(json["username"], "bob");
    assert_eq!(json["acct"], "bob@remote.example");
    assert_eq!(json["url"], "https://remote.example/@bob");
    assert_eq!(json["uri"], "https://remote.example/users/bob");
    assert_ne!(json["url"], json["uri"]);
}

#[test]
fn local_and_remote_accounts_with_the_same_username_render_different_acct() {
    let ser = serializer();
    let local_json = ser.build_account_local(
        &resolved_actor(1, "alice"),
        &bare_profile(1),
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &[],
    );
    let remote_json =
        ser.build_account_remote(&remote(2, "alice", "remote.example"), &zero_counts(), &[]);

    assert_eq!(local_json["acct"], "alice");
    assert_eq!(remote_json["acct"], "alice@remote.example");
    assert_ne!(local_json["acct"], remote_json["acct"]);
}

// ---- Requirement 1.4: emojis resolved from display_name/note shortcodes ----

#[test]
fn emojis_referenced_in_display_name_and_note_are_attached() {
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let mut profile = bare_profile(1);
    profile.display_name = "Alice :blobcat:".to_string();
    profile.note = "loves :heart_eyes: cats".to_string();
    let candidates = [
        emoji("blobcat", "https://kawasemi.example/emoji/blobcat.png"),
        emoji(
            "heart_eyes",
            "https://kawasemi.example/emoji/heart_eyes.png",
        ),
        emoji("unused", "https://kawasemi.example/emoji/unused.png"),
    ];

    let json = ser.build_account_local(
        &actor,
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &candidates,
    );

    let emojis = json["emojis"].as_array().unwrap();
    let shortcodes: Vec<&str> = emojis
        .iter()
        .map(|e| e["shortcode"].as_str().unwrap())
        .collect();
    assert_eq!(shortcodes, vec!["blobcat", "heart_eyes"]);
}

#[test]
fn no_referenced_shortcodes_yields_an_empty_emojis_array_not_null() {
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let profile = bare_profile(1); // display_name/note contain no ":shortcode:"
    let candidates = [emoji(
        "blobcat",
        "https://kawasemi.example/emoji/blobcat.png",
    )];

    let json = ser.build_account_local(
        &actor,
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &candidates,
    );

    assert_eq!(json["emojis"], serde_json::json!([]));
}

// ---- The flagged concern: emoji shortcode colliding across domains must
// never be mis-attached (serializer.rs's "Emoji domain-collision safety"). ----

#[test]
fn a_shortcode_with_two_distinct_candidate_rows_is_omitted_not_guessed() {
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let mut profile = bare_profile(1);
    profile.display_name = "Alice :blobcat:".to_string();
    profile.note = String::new();
    // Two distinct rows for the same shortcode -- exactly what
    // `CustomEmojiRepository::resolve_emojis` returns when the shortcode
    // exists in more than one `custom_emojis.domain` (task 2.3's documented,
    // deliberately domain-blind behavior). `CustomEmojiView` carries no
    // domain field, so nothing here can tell which one belongs to this
    // account.
    let candidates = [
        emoji(
            "blobcat",
            "https://kawasemi.example/emoji/blobcat-local.png",
        ),
        emoji("blobcat", "https://remote.example/emoji/blobcat-remote.png"),
    ];

    let json = ser.build_account_local(
        &actor,
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &candidates,
    );

    // Never the wrong one, and never both: the ambiguous shortcode is
    // omitted entirely.
    assert_eq!(json["emojis"], serde_json::json!([]));
}

#[test]
fn duplicate_identical_candidate_rows_for_one_shortcode_are_not_treated_as_a_collision() {
    // Two structurally-identical rows (same url/static_url/etc.) for one
    // shortcode is not a real collision -- there is genuinely only one
    // distinct emoji, so it must still be attached.
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let mut profile = bare_profile(1);
    profile.display_name = "Alice :blobcat:".to_string();
    profile.note = String::new();
    let candidates = [
        emoji("blobcat", "https://kawasemi.example/emoji/blobcat.png"),
        emoji("blobcat", "https://kawasemi.example/emoji/blobcat.png"),
    ];

    let json = ser.build_account_local(
        &actor,
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &candidates,
    );

    let emojis = json["emojis"].as_array().unwrap();
    assert_eq!(emojis.len(), 1);
    assert_eq!(emojis[0]["shortcode"], "blobcat");
}

#[test]
fn a_collision_on_one_shortcode_does_not_suppress_an_unambiguous_sibling_shortcode() {
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let mut profile = bare_profile(1);
    profile.display_name = "Alice :blobcat: :verified:".to_string();
    profile.note = String::new();
    let candidates = [
        emoji(
            "blobcat",
            "https://kawasemi.example/emoji/blobcat-local.png",
        ),
        emoji("blobcat", "https://remote.example/emoji/blobcat-remote.png"),
        emoji("verified", "https://kawasemi.example/emoji/verified.png"),
    ];

    let json = ser.build_account_local(
        &actor,
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &candidates,
    );

    let emojis = json["emojis"].as_array().unwrap();
    let shortcodes: Vec<&str> = emojis
        .iter()
        .map(|e| e["shortcode"].as_str().unwrap())
        .collect();
    assert_eq!(shortcodes, vec!["verified"]);
}

// ---- Requirement 2.2: CredentialAccount adds source/role on top of Account ----

#[test]
fn credential_account_includes_every_account_field_plus_source_and_role() {
    let ser = serializer();
    let actor = resolved_actor(1, "alice");
    let mut profile = bare_profile(1);
    profile.source = CredentialSource {
        privacy: Visibility::Unlisted,
        sensitive: true,
        language: Some("en".to_string()),
        note: "bio for editing".to_string(),
        fields: Vec::new(),
        follow_requests_count: 3,
    };

    let json = ser.build_credential_account(
        &actor,
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &[],
    );

    // Every Account field is present (flattened).
    assert_eq!(json["acct"], "alice");
    assert_eq!(json["username"], "alice");
    assert_ne!(json["avatar"], Value::Null);
    // Plus source/role.
    assert_eq!(json["source"]["privacy"], "unlisted");
    assert_eq!(json["source"]["sensitive"], true);
    assert_eq!(json["source"]["language"], "en");
    assert_eq!(json["source"]["note"], "bio for editing");
    assert_eq!(json["source"]["follow_requests_count"], 3);
    assert!(json["role"].is_object());
    assert!(json["role"]["id"].is_string());
}

// ---- Requirement 1.1: counts/last_status_at flow through untouched ----

#[test]
fn counts_and_last_status_at_are_reflected_as_given() {
    let ser = serializer();
    let counts = AccountCounts {
        followers: 12,
        following: 3,
        statuses: 44,
        last_status_at: Some(datetime!(2026-02-01 00:00:00 UTC)),
    };

    let json = ser.build_account_local(
        &resolved_actor(1, "alice"),
        &bare_profile(1),
        datetime!(2025-01-01 00:00:00 UTC),
        &counts,
        &store(),
        &origin(),
        &[],
    );

    assert_eq!(json["followers_count"], 12);
    assert_eq!(json["following_count"], 3);
    assert_eq!(json["statuses_count"], 44);
    assert_ne!(json["last_status_at"], Value::Null);
}

#[test]
fn last_status_at_is_null_when_absent() {
    let ser = serializer();

    let json = ser.build_account_local(
        &resolved_actor(1, "alice"),
        &bare_profile(1),
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &[],
    );

    assert_eq!(json["last_status_at"], Value::Null);
}

// ---- fields propagate with verified_at formatting ----

#[test]
fn profile_fields_propagate_including_verified_at() {
    let ser = serializer();
    let mut profile = bare_profile(1);
    profile.fields = vec![
        ProfileField {
            name: "Pronouns".to_string(),
            value: "she/her".to_string(),
            verified_at: None,
        },
        ProfileField {
            name: "Website".to_string(),
            value: "https://alice.example".to_string(),
            verified_at: Some(datetime!(2026-01-01 00:00:00 UTC)),
        },
    ];

    let json = ser.build_account_local(
        &resolved_actor(1, "alice"),
        &profile,
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &[],
    );

    let fields = json["fields"].as_array().unwrap();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0]["name"], "Pronouns");
    assert_eq!(fields[0]["verified_at"], Value::Null);
    assert_eq!(fields[1]["name"], "Website");
    assert_ne!(fields[1]["verified_at"], Value::Null);
}

// ---- group is always false (no group-actor concept exists yet) ----

#[test]
fn group_is_always_false_for_local_and_remote() {
    let ser = serializer();
    let local_json = ser.build_account_local(
        &resolved_actor(1, "alice"),
        &bare_profile(1),
        datetime!(2025-01-01 00:00:00 UTC),
        &zero_counts(),
        &store(),
        &origin(),
        &[],
    );
    let remote_json =
        ser.build_account_remote(&remote(2, "bob", "remote.example"), &zero_counts(), &[]);

    assert_eq!(local_json["group"], false);
    assert_eq!(remote_json["group"], false);
}
