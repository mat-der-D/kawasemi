//! Integration tests for `CustomEmojiService` (task 5.6), per this task's own
//! observable completion condition: "visible 絵文字一覧を CustomEmoji 配列で
//! 返すサービステストが green".
//!
//! Mirrors `src/accounts/emoji_repository/tests.rs`'s established
//! convention: `spawn_test_app` for an isolated, already-migrated schema,
//! seeding `custom_emojis` directly with a raw `sqlx::query` `INSERT` (this
//! crate never seeds that table through any function it exposes — see
//! `emoji_repository.rs`'s own doc comment, Requirement 9.3's "read only"
//! scoping). Also mirrors `src/accounts/instance_service/tests.rs`'s own
//! small `service(pool)` helper convention.

use super::CustomEmojiService;
use crate::accounts::custom_emoji_serializer::CustomEmojiSerializer;
use crate::test_harness::{TestApp, spawn_test_app};

fn service(pool: sqlx::PgPool) -> CustomEmojiService {
    CustomEmojiService::new(pool, CustomEmojiSerializer::new())
}

/// Seeds one `custom_emojis` row via a raw `INSERT` (mirrors
/// `emoji_repository/tests.rs::seed_emoji` exactly — this crate exposes no
/// write API for this table, Requirement 9.3).
async fn seed_emoji(
    app: &TestApp,
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

/// Requirement 9.1: `list_custom_emojis` returns only `visible_in_picker =
/// TRUE` rows as a CustomEmoji JSON array, end to end through the service —
/// a `visible_in_picker = FALSE` row must be excluded from the result.
#[tokio::test]
async fn list_custom_emojis_returns_only_visible_emojis() {
    let app = spawn_test_app().await;

    seed_emoji(&app, "blobcat", "", true, Some("cats")).await;
    seed_emoji(&app, "hidden_emoji", "", false, None).await;

    let svc = service(app.pool.clone());
    let json = svc
        .list_custom_emojis()
        .await
        .expect("list_custom_emojis must succeed");

    let array = json.as_array().expect("result must be a JSON array");
    let shortcodes: Vec<&str> = array
        .iter()
        .map(|entry| {
            entry["shortcode"]
                .as_str()
                .expect("shortcode must be a string")
        })
        .collect();
    assert!(shortcodes.contains(&"blobcat"));
    assert!(
        !shortcodes.contains(&"hidden_emoji"),
        "a visible_in_picker = FALSE row must not appear in the service result"
    );

    app.cleanup().await;
}

/// Requirement 9.2: each returned CustomEmoji JSON entry carries the
/// `shortcode`/`url`/`static_url`/`visible_in_picker`/`category` shape,
/// proven end to end through the service (not just through the serializer in
/// isolation, task 3.4's own tests already cover that unit).
#[tokio::test]
async fn list_custom_emojis_entries_have_the_expected_shape() {
    let app = spawn_test_app().await;

    seed_emoji(&app, "blobcat", "", true, Some("cats")).await;

    let svc = service(app.pool.clone());
    let json = svc
        .list_custom_emojis()
        .await
        .expect("list_custom_emojis must succeed");

    let array = json.as_array().expect("result must be a JSON array");
    let blobcat = array
        .iter()
        .find(|entry| entry["shortcode"] == serde_json::json!("blobcat"))
        .expect("blobcat must be present");

    assert_eq!(
        blobcat["url"],
        serde_json::json!("https://example.test/emoji/blobcat.png")
    );
    assert_eq!(
        blobcat["static_url"],
        serde_json::json!("https://example.test/emoji/blobcat.png")
    );
    assert_eq!(blobcat["visible_in_picker"], serde_json::json!(true));
    assert_eq!(blobcat["category"], serde_json::json!("cats"));

    app.cleanup().await;
}

/// With zero `custom_emojis` rows in the database, `list_custom_emojis` must
/// still succeed and return an empty JSON array, never an error.
#[tokio::test]
async fn list_custom_emojis_with_no_rows_returns_an_empty_array() {
    let app = spawn_test_app().await;

    let row_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM custom_emojis")
        .fetch_one(&app.pool)
        .await
        .expect("counting rows must succeed");
    assert_eq!(
        row_count.0, 0,
        "the test database must start with no custom_emojis row"
    );

    let svc = service(app.pool.clone());
    let json = svc
        .list_custom_emojis()
        .await
        .expect("list_custom_emojis must succeed even with zero rows");

    assert_eq!(json, serde_json::json!([]));

    app.cleanup().await;
}
