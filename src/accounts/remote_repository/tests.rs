//! Integration tests for `RemoteAccountRepository` (Requirements 3.1, 3.2,
//! 7.2, 7.3), per task 2.2's observable completion condition: "同一
//! actor_uri の再 upsert が重複行を作らず最新値で更新される統合テストが
//! green".
//!
//! Mirrors `src/accounts/profile_repository/tests.rs`'s established
//! convention: reuses `crate::test_harness::spawn_test_app` for an isolated,
//! already-migrated schema and a deterministic `RuntimeContext`.
//! `remote_accounts` has no hard FK to any local-actor/owner row (it is a
//! standalone cache table, `migrations/0006_accounts.sql`), so unlike
//! `profile_repository/tests.rs` these tests need no `create_test_actor`
//! helper.

use time::Duration;
use time::macros::datetime;

use super::{find_remote_by_id, find_remote_by_uri, is_stale, upsert_remote};
use crate::accounts::model::{ProfileField, RemoteAccount};
use crate::domain::Id;
use crate::test_harness::spawn_test_app;

/// Builds a sample [`RemoteAccount`] for `actor_uri`, with `id` and
/// `fetched_at` supplied by the caller so tests can control both precisely.
fn sample_remote_account(
    id: Id,
    actor_uri: &str,
    fetched_at: time::OffsetDateTime,
) -> RemoteAccount {
    RemoteAccount {
        id,
        actor_uri: actor_uri.to_string(),
        username: "alice".to_string(),
        domain: "remote.example".to_string(),
        display_name: "Alice".to_string(),
        note: "Hello from remote.example.".to_string(),
        url: "https://remote.example/@alice".to_string(),
        avatar_url: Some("https://remote.example/avatars/alice.png".to_string()),
        header_url: None,
        fields: vec![ProfileField {
            name: "Pronouns".to_string(),
            value: "she/her".to_string(),
            verified_at: None,
        }],
        bot: false,
        locked: true,
        fetched_at,
    }
}

/// Requirements 3.1/3.2: an unknown `actor_uri`/`id` resolves to `None`, not
/// an error.
#[tokio::test]
async fn find_remote_returns_none_for_unknown_actor_uri_and_id() {
    let app = spawn_test_app().await;

    let by_uri = find_remote_by_uri(&app.pool, "https://remote.example/users/nobody")
        .await
        .expect("find_remote_by_uri must succeed even with no matching row");
    assert!(by_uri.is_none());

    let by_id = find_remote_by_id(&app.pool, Id::from_i64(999_999))
        .await
        .expect("find_remote_by_id must succeed even with no matching row");
    assert!(by_id.is_none());

    app.cleanup().await;
}

/// Requirements 3.1, 3.2, 7.2: after `upsert_remote`, the same account is
/// resolvable both by `actor_uri` and by internal `id`, with every
/// normalized field intact.
#[tokio::test]
async fn upsert_remote_is_findable_by_both_actor_uri_and_id() {
    let app = spawn_test_app().await;
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let account = sample_remote_account(id, "https://remote.example/users/alice", now);

    let upserted = upsert_remote(&app.pool, &account)
        .await
        .expect("upsert_remote must succeed for a fresh actor_uri");
    assert_eq!(upserted, account);

    let by_uri = find_remote_by_uri(&app.pool, "https://remote.example/users/alice")
        .await
        .expect("find_remote_by_uri must succeed")
        .expect("the row just upserted must be findable by actor_uri");
    assert_eq!(by_uri, account);

    let by_id = find_remote_by_id(&app.pool, id)
        .await
        .expect("find_remote_by_id must succeed")
        .expect("the row just upserted must be findable by id");
    assert_eq!(by_id, account);

    app.cleanup().await;
}

/// The crux of task 2.2's own observable completion condition: a second
/// `upsert_remote` against the same `actor_uri` does not create a duplicate
/// row (asserted via a direct `COUNT(*)`, not just the returned struct) and
/// the latest values win. It also proves `id` stability: the second upsert
/// deliberately carries a *different* `id` in its input `RemoteAccount`
/// (simulating a caller that, e.g., re-minted an id by mistake), and the
/// repository must still keep the row's original `id` rather than trusting
/// the incoming one — see `remote_repository.rs`'s own doc comment ("`id`
/// stability across re-upserts of the same `actor_uri`").
#[tokio::test]
async fn upsert_remote_is_idempotent_and_keeps_the_original_id() {
    let app = spawn_test_app().await;
    let actor_uri = "https://remote.example/users/bob";
    let original_id = app.runtime.ids.next_id();
    let first_fetched_at = app.runtime.clock.now();

    let first = sample_remote_account(original_id, actor_uri, first_fetched_at);
    let after_first = upsert_remote(&app.pool, &first)
        .await
        .expect("first upsert_remote must succeed");
    assert_eq!(after_first.id, original_id);
    assert_eq!(after_first.display_name, "Alice");

    // Second upsert: same actor_uri, different id, and materially different
    // values (simulating a re-normalized document with fresher content).
    let different_id = app.runtime.ids.next_id();
    assert_ne!(different_id, original_id);
    let second_fetched_at = first_fetched_at + Duration::hours(1);
    let mut second = sample_remote_account(different_id, actor_uri, second_fetched_at);
    second.display_name = "Alice Updated".to_string();
    second.note = "Updated bio.".to_string();
    second.bot = true;
    second.locked = false;
    second.avatar_url = None;
    second.header_url = Some("https://remote.example/headers/alice.png".to_string());
    second.fields = vec![ProfileField {
        name: "Website".to_string(),
        value: "https://alice.example".to_string(),
        verified_at: Some(second_fetched_at),
    }];

    let after_second = upsert_remote(&app.pool, &second)
        .await
        .expect("second upsert_remote must succeed");

    // Latest values win...
    assert_eq!(after_second.display_name, "Alice Updated");
    assert_eq!(after_second.note, "Updated bio.");
    assert!(after_second.bot);
    assert!(!after_second.locked);
    assert!(after_second.avatar_url.is_none());
    assert_eq!(
        after_second.header_url.as_deref(),
        Some("https://remote.example/headers/alice.png")
    );
    assert_eq!(after_second.fields, second.fields);
    assert_eq!(
        after_second.fetched_at.unix_timestamp(),
        second_fetched_at.unix_timestamp()
    );
    // ...but the row's identity (id) is the original one, not the second
    // upsert's input id.
    assert_eq!(after_second.id, original_id);
    assert_ne!(after_second.id, different_id);

    // No duplicate row was created: exactly one row for this actor_uri,
    // and it carries the original id.
    let row_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM remote_accounts WHERE actor_uri = $1")
            .bind(actor_uri)
            .fetch_one(&app.pool)
            .await
            .expect("counting rows must succeed");
    assert_eq!(row_count.0, 1);

    let persisted_id: (i64,) =
        sqlx::query_as("SELECT id FROM remote_accounts WHERE actor_uri = $1")
            .bind(actor_uri)
            .fetch_one(&app.pool)
            .await
            .expect("selecting the persisted id must succeed");
    assert_eq!(persisted_id.0, original_id.as_i64());

    // Both find_remote_by_uri and find_remote_by_id (under the *original*
    // id) resolve to the latest values.
    let reloaded_by_uri = find_remote_by_uri(&app.pool, actor_uri)
        .await
        .expect("find_remote_by_uri must succeed")
        .expect("row must exist");
    assert_eq!(reloaded_by_uri, after_second);

    let reloaded_by_id = find_remote_by_id(&app.pool, original_id)
        .await
        .expect("find_remote_by_id must succeed")
        .expect("row must exist under the original id");
    assert_eq!(reloaded_by_id, after_second);

    // The different_id from the second upsert's input never became a real
    // row of its own.
    let by_different_id = find_remote_by_id(&app.pool, different_id)
        .await
        .expect("find_remote_by_id must succeed even for an id that was never persisted");
    assert!(by_different_id.is_none());

    app.cleanup().await;
}

/// Requirement 7.3: `is_stale` is a pure, threshold-parameterized check —
/// fresh (elapsed < ttl) is not stale, and elapsed >= ttl is stale.
#[test]
fn is_stale_distinguishes_fresh_from_stale_relative_to_an_explicit_ttl() {
    let fetched_at = datetime!(2026-01-01 00:00:00 UTC);
    let ttl = Duration::hours(24);

    // Fresh: well within the ttl window.
    let still_fresh = fetched_at + Duration::hours(1);
    assert!(!is_stale(fetched_at, still_fresh, ttl));

    // Exactly at the boundary: inclusive, counts as stale (see is_stale's
    // own doc comment for why the boundary is inclusive).
    let exactly_at_ttl = fetched_at + ttl;
    assert!(is_stale(fetched_at, exactly_at_ttl, ttl));

    // Comfortably past the ttl.
    let well_past = fetched_at + Duration::hours(48);
    assert!(is_stale(fetched_at, well_past, ttl));

    // A `now` at or before `fetched_at` (skewed/mocked clock) is never
    // stale.
    assert!(!is_stale(fetched_at, fetched_at, ttl));
    let before = fetched_at - Duration::minutes(5);
    assert!(!is_stale(fetched_at, before, ttl));
}
