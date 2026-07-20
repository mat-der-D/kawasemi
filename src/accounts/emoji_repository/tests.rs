//! Integration tests for `CustomEmojiRepository` (Requirements 1.4, 9.1,
//! 9.3), per task 2.3's observable completion condition: "投入済み絵文字に対
//! し一覧取得とショートコード解決が期待値を返す統合テストが green".
//!
//! Mirrors `src/accounts/profile_repository/tests.rs`/`src/accounts/
//! remote_repository/tests.rs`'s established convention: reuses
//! `crate::test_harness::spawn_test_app` for an isolated, already-migrated
//! schema and a deterministic `RuntimeContext`. `custom_emojis` has no write
//! API in this crate (Requirement 9.3 — this repository is read-only), so
//! these tests seed rows with a raw `sqlx::query` `INSERT` directly, not
//! through any function `emoji_repository.rs` exposes.

use super::{list_visible_emojis, resolve_emojis};
use crate::test_harness::spawn_test_app;

/// Seeds one `custom_emojis` row via a raw `INSERT` (this repository exposes
/// no write API of its own — Requirement 9.3).
async fn seed_emoji(
    app: &crate::test_harness::TestApp,
    shortcode: &str,
    domain: &str,
    visible_in_picker: bool,
    category: Option<&str>,
) {
    let now = app.runtime.clock.now();
    let url = format!("https://example.test/emoji/{shortcode}.png");
    sqlx::query(
        "INSERT INTO custom_emojis \
             (shortcode, domain, url, static_url, visible_in_picker, category, updated_at) \
         VALUES ($1, $2, $3, $3, $4, $5, $6)",
    )
    .bind(shortcode)
    .bind(domain)
    .bind(&url)
    .bind(visible_in_picker)
    .bind(category)
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding a custom_emojis row must succeed");
}

/// Requirement 9.1/9.2: `list_visible_emojis` returns only
/// `visible_in_picker = TRUE` rows, with every field correctly mapped
/// (including `category` for both `Some` and `None`), and excludes a
/// `visible_in_picker = FALSE` row.
#[tokio::test]
async fn list_visible_emojis_returns_only_picker_visible_rows_with_correct_fields() {
    let app = spawn_test_app().await;

    seed_emoji(&app, "blobcat", "", true, Some("cats")).await;
    seed_emoji(&app, "hidden_emoji", "", false, None).await;
    seed_emoji(&app, "remote_cat", "remote.example", true, Some("cats")).await;

    let visible = list_visible_emojis(&app.pool)
        .await
        .expect("list_visible_emojis must succeed");

    let shortcodes: Vec<&str> = visible.iter().map(|e| e.shortcode.as_str()).collect();
    assert!(shortcodes.contains(&"blobcat"));
    assert!(shortcodes.contains(&"remote_cat"));
    assert!(
        !shortcodes.contains(&"hidden_emoji"),
        "a visible_in_picker = FALSE row must not be listed"
    );

    let blobcat = visible
        .iter()
        .find(|e| e.shortcode == "blobcat")
        .expect("blobcat must be present");
    assert_eq!(blobcat.url, "https://example.test/emoji/blobcat.png");
    assert_eq!(blobcat.static_url, "https://example.test/emoji/blobcat.png");
    assert!(blobcat.visible_in_picker);
    assert_eq!(blobcat.category.as_deref(), Some("cats"));

    app.cleanup().await;
}

/// Requirement 9.1: a `custom_emojis` row with no `category` maps to
/// `CustomEmojiView.category == None`, not an empty string.
#[tokio::test]
async fn list_visible_emojis_maps_a_missing_category_to_none() {
    let app = spawn_test_app().await;

    seed_emoji(&app, "no_category", "", true, None).await;

    let visible = list_visible_emojis(&app.pool)
        .await
        .expect("list_visible_emojis must succeed");
    let found = visible
        .iter()
        .find(|e| e.shortcode == "no_category")
        .expect("no_category must be present");
    assert!(found.category.is_none());

    app.cleanup().await;
}

/// Requirement 1.4/9.1/9.4: `resolve_emojis` given a set of shortcodes
/// returns exactly the matching [`crate::accounts::model::CustomEmojiView`]s
/// across *any* `domain` — local and remote-domain rows both resolve, since
/// `resolve_emojis` reads from the same unified `custom_emojis` table as
/// `list_visible_emojis` with no domain filter — while a shortcode that does
/// not exist at all (in any domain) is silently skipped without erroring.
#[tokio::test]
async fn resolve_emojis_returns_exactly_the_matching_emojis_across_any_domain() {
    let app = spawn_test_app().await;

    seed_emoji(&app, "blobcat", "", true, Some("cats")).await;
    seed_emoji(&app, "parrot", "", true, None).await;
    // Registered under a remote domain — must now resolve, symmetric with
    // list_visible_emojis's no-domain-filter behavior (Requirement 9.4).
    seed_emoji(&app, "remote_only", "remote.example", true, None).await;
    // A local row that is not requested — must not leak into the result.
    seed_emoji(&app, "unrequested", "", true, None).await;

    let requested = vec![
        "blobcat".to_string(),
        "parrot".to_string(),
        "remote_only".to_string(),
        "does_not_exist".to_string(),
    ];
    let resolved = resolve_emojis(&app.pool, &requested)
        .await
        .expect("resolve_emojis must succeed even with unmatched shortcodes");

    let shortcodes: Vec<&str> = resolved.iter().map(|e| e.shortcode.as_str()).collect();
    assert_eq!(
        resolved.len(),
        3,
        "the two local matches and the remote-domain match all resolve"
    );
    assert!(shortcodes.contains(&"blobcat"));
    assert!(shortcodes.contains(&"parrot"));
    assert!(
        shortcodes.contains(&"remote_only"),
        "a remote-domain shortcode must resolve, same as any other domain"
    );
    assert!(!shortcodes.contains(&"unrequested"));
    assert!(
        !shortcodes.contains(&"does_not_exist"),
        "a shortcode that does not exist in any domain must still be skipped"
    );

    app.cleanup().await;
}

/// Requirement 1.4: `resolve_emojis` does not filter on `visible_in_picker` —
/// a shortcode referenced in bio text must still resolve even when the emoji
/// has been hidden from the picker.
#[tokio::test]
async fn resolve_emojis_resolves_regardless_of_visible_in_picker() {
    let app = spawn_test_app().await;

    seed_emoji(&app, "hidden_but_referenceable", "", false, None).await;

    let resolved = resolve_emojis(&app.pool, &["hidden_but_referenceable".to_string()])
        .await
        .expect("resolve_emojis must succeed");

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].shortcode, "hidden_but_referenceable");
    assert!(!resolved[0].visible_in_picker);

    app.cleanup().await;
}

/// An empty `shortcodes` slice resolves to an empty result, not an error.
#[tokio::test]
async fn resolve_emojis_with_no_requested_shortcodes_returns_empty() {
    let app = spawn_test_app().await;

    seed_emoji(&app, "blobcat", "", true, None).await;

    let resolved = resolve_emojis(&app.pool, &[])
        .await
        .expect("resolve_emojis with an empty request must succeed");
    assert!(resolved.is_empty());

    app.cleanup().await;
}
