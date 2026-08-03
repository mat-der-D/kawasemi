//! Integration tests for `PgSearchBackend::search_accounts` (search spec
//! task 3.1, `Boundary: PgSearchBackend`; Requirements 3.1, 3.4, 7.2),
//! design.md's `search_accounts_it.rs` ("アカウント検索（ローカル/既知リモート
//! 一致・following 絞り・一意化・limit/offset）（統合, SearchBackend 実体）").
//!
//! `following_of`-scoping is *not* exercised here: design.md's own
//! `PgSearchBackend::search_accounts` Responsibilities note is explicit that
//! the default backend does not filter by `following` at the matching
//! stage ("following は照合段では候補抽出に留め、フォロー限定は
//! Hydrator/上流関係に委譲") — that behavior belongs to a later task
//! (`SearchHydrator`), strictly outside task 3.1's boundary.
//!
//! Fixtures are inserted directly via raw SQL against `account_profiles`/
//! `remote_accounts` rather than through the full `ActorService`/
//! `AccountService` creation path: both tables' primary keys have no
//! physical FK to `local_actors`/`owners` (`migrations/0006_accounts.sql`'s
//! own doc comment — "1:1 論理参照... no REFERENCES"), so a bare row is
//! sufficient to exercise `PgSearchBackend`'s own SQL in isolation, mirroring
//! `tests/search_migrations_it.rs`'s own "raw SQL against this spec's own
//! table" convention for a repository test that does not need the full
//! upstream object graph.

use kawasemi::domain::{AccountRef, Id};
use kawasemi::search::pg_backend::PgSearchBackend;
use kawasemi::search::ports::{AccountQuery, SearchBackend};
use kawasemi::test_harness::{TestApp, spawn_test_app};

/// Inserts a minimal `account_profiles` row (a local account), returning the
/// `actor_id` used.
async fn insert_local_account(app: &TestApp, display_name: &str) -> Id {
    let actor_id = app.runtime.ids.next_id();
    sqlx::query(
        "INSERT INTO account_profiles (actor_id, display_name, updated_at) VALUES ($1, $2, $3)",
    )
    .bind(actor_id.as_i64())
    .bind(display_name)
    .bind(app.runtime.clock.now())
    .execute(&app.pool)
    .await
    .expect("inserting a fixture account_profiles row must succeed");
    actor_id
}

/// Inserts a minimal `remote_accounts` row (a known remote account),
/// returning the `id` used.
async fn insert_remote_account(
    app: &TestApp,
    username: &str,
    domain: &str,
    display_name: &str,
) -> Id {
    let id = app.runtime.ids.next_id();
    sqlx::query(
        "INSERT INTO remote_accounts (id, actor_uri, username, domain, display_name, url, \
         fetched_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id.as_i64())
    .bind(format!("https://{domain}/users/{username}"))
    .bind(username)
    .bind(domain)
    .bind(display_name)
    .bind(format!("https://{domain}/@{username}"))
    .bind(app.runtime.clock.now())
    .execute(&app.pool)
    .await
    .expect("inserting a fixture remote_accounts row must succeed");
    id
}

fn query(term: &str, limit: u32, offset: u32) -> AccountQuery {
    AccountQuery {
        term: term.to_string(),
        following_of: None,
        limit,
        offset,
    }
}

/// A local account is matched by a partial, case-insensitive `display_name`
/// substring (Requirement 3.1), returning a bare `AccountRef::Local`
/// (Requirement 7.2 — identifiers only).
#[tokio::test]
async fn search_accounts_matches_local_account_by_display_name_substring() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone());
    let alice = insert_local_account(&app, "Alice Wonderland").await;

    let matches = backend
        .search_accounts(&query("wonder", 50, 0))
        .await
        .expect("search_accounts must succeed");

    assert_eq!(matches, vec![AccountRef::Local(alice)]);

    app.cleanup().await;
}

/// A known remote account is matched by a partial, case-insensitive
/// `username` substring (Requirement 3.1), returning a bare
/// `AccountRef::Remote`.
#[tokio::test]
async fn search_accounts_matches_remote_account_by_username_substring() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone());
    let bob = insert_remote_account(&app, "bobby", "example.social", "Bob Marley").await;

    let matches = backend
        .search_accounts(&query("BOB", 50, 0))
        .await
        .expect("search_accounts must succeed");

    assert_eq!(matches, vec![AccountRef::Remote(bob)]);

    app.cleanup().await;
}

/// A known remote account is matched by its synthesized `username@domain`
/// acct form even when neither `username` nor `domain` alone contains the
/// full query term (Requirement 3.1's "ハンドル（acct）に対する一致").
#[tokio::test]
async fn search_accounts_matches_remote_account_by_synthesized_acct() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone());
    let carol = insert_remote_account(&app, "carol", "remote.example", "Carol Danvers").await;

    let matches = backend
        .search_accounts(&query("carol@remote", 50, 0))
        .await
        .expect("search_accounts must succeed");

    assert_eq!(matches, vec![AccountRef::Remote(carol)]);

    app.cleanup().await;
}

/// A term matching neither a local `display_name` nor any remote
/// username/domain/display_name/acct field matches nothing on that side —
/// only accounts that actually match appear in the combined result
/// (proving local and remote matching are independently applied, not
/// cross-contaminated).
#[tokio::test]
async fn search_accounts_only_returns_accounts_that_actually_match() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone());
    let alice = insert_local_account(&app, "Alice Wonderland").await;
    insert_remote_account(&app, "bobby", "example.social", "Bob Marley").await;

    let matches = backend
        .search_accounts(&query("wonderland", 50, 0))
        .await
        .expect("search_accounts must succeed");

    assert_eq!(matches, vec![AccountRef::Local(alice)]);

    app.cleanup().await;
}

/// `limit`/`offset` are applied to the combined local+remote result
/// (Requirement 3.4).
#[tokio::test]
async fn search_accounts_applies_limit_and_offset() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone());
    let first = insert_local_account(&app, "Tagalpha").await;
    let second = insert_local_account(&app, "Tagbeta").await;
    let third = insert_remote_account(&app, "taggamma", "example.social", "Tag Gamma").await;

    let page1 = backend
        .search_accounts(&query("tag", 1, 0))
        .await
        .expect("search_accounts page 1 must succeed");
    assert_eq!(page1, vec![AccountRef::Local(first)]);

    let page2 = backend
        .search_accounts(&query("tag", 1, 1))
        .await
        .expect("search_accounts page 2 must succeed");
    assert_eq!(page2, vec![AccountRef::Local(second)]);

    let page3 = backend
        .search_accounts(&query("tag", 1, 2))
        .await
        .expect("search_accounts page 3 must succeed");
    assert_eq!(page3, vec![AccountRef::Remote(third)]);

    let beyond = backend
        .search_accounts(&query("tag", 50, 100))
        .await
        .expect("search_accounts offset-beyond-end must succeed");
    assert!(beyond.is_empty());

    app.cleanup().await;
}

/// A term matching nothing at all returns an empty `Vec`, not an error.
#[tokio::test]
async fn search_accounts_returns_empty_for_no_match() {
    let app = spawn_test_app().await;
    let backend = PgSearchBackend::new(app.pool.clone());
    insert_local_account(&app, "Alice Wonderland").await;

    let matches = backend
        .search_accounts(&query("nonexistentterm", 50, 0))
        .await
        .expect("search_accounts must succeed even with no matches");
    assert!(matches.is_empty());

    app.cleanup().await;
}
