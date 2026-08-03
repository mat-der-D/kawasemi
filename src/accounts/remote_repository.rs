//! `RemoteAccountRepository` (design.md "Data / データ層" ->
//! `AccountProfileRepository / RemoteAccountRepository / CustomEmojiRepository /
//! InstanceSettingsRepository`, Requirements 3.1, 3.2, 7.2, 7.3; task 2.2,
//! `Boundary: RemoteAccountRepository`): the normalized-remote-account
//! cache's persistence — actor_uri/internal-id lookup and the normalized
//! result's upsert — against `migrations/0006_accounts.sql`'s
//! `remote_accounts` table (already applied, unmodified by this task).
//!
//! Scope: this module owns exactly [`find_remote_by_uri`]/
//! [`find_remote_by_id`]/[`upsert_remote`], design.md's Service Interface
//! signatures for this half of the `AccountProfileRepository`/
//! `RemoteAccountRepository`/`CustomEmojiRepository`/
//! `InstanceSettingsRepository` component (the `profile`/`emoji`/`instance
//! settings` Service Interface entries belong to tasks 2.1/2.3/2.4, out of
//! this task's boundary), plus [`is_stale`], a small pure staleness-check
//! helper (see "Reconciling `fetched_at` 陳腐化判定 with the literal Service
//! Interface" below). It does not touch `src/accounts/model.rs`,
//! `src/accounts/ports.rs`, `src/accounts/profile_repository.rs`, or any
//! migration file.
//!
//! ## Reconciling `fetched_at` 陳腐化判定 with the literal Service Interface
//! Design.md's prose for this component says the repository provides
//! "`fetched_at` で陳腐化判定" (staleness determination via `fetched_at`),
//! but design.md's own Service Interface code block for this component only
//! lists three functions — [`find_remote_by_uri`], [`find_remote_by_id`],
//! [`upsert_remote`] — none of which take or return a staleness verdict.
//! Mirroring task 2.1's own resolution of the identical
//! literal-signature-vs-prose tension (recorded in tasks.md's
//! "Implementation Notes": "design.md's Service Interface block is generally
//! the authoritative contract other tasks are built against"), this module
//! keeps the three signatures exactly as design.md's code block states them
//! and adds [`is_stale`] alongside them as a small, pure, DB-free helper
//! rather than folding a TTL policy into any of the three literal functions.
//! `is_stale` is deliberately parameterized by an explicit `ttl` the
//! *caller* supplies: neither requirements.md nor design.md names a concrete
//! TTL value anywhere (Requirement 7.3 only says "正規化済みリモートアカウント
//! が有効に保持されている間" — while it remains validly held — without a
//! number), and per this task's own brief this repository does not own
//! cache-policy decisions — `RemoteAccountFetcher` (task 4, out of this
//! task's boundary) is what will actually decide "is this account's cache
//! still fresh enough to skip a re-fetch" and pick/own the concrete TTL
//! constant.
//!
//! ## `fields` (JSONB) hand-rolled (de)serialization
//! Mirrors `profile_repository.rs`'s identical precedent (see that module's
//! own doc comment, "`fields` (JSONB) hand-rolled (de)serialization"):
//! [`crate::accounts::model::ProfileField`] carries no `#[derive(Serialize,
//! Deserialize)]`, so this module hand-builds/-parses the JSON array itself
//! ([`fields_to_json`]/[`fields_from_json`]) rather than relying on a
//! derived impl that does not exist. These two functions are not imported
//! from `profile_repository.rs` (that module keeps them private, and this
//! task must not edit that file to make them `pub(crate)`) — they are
//! duplicated verbatim here, the same "small, self-contained, no
//! cross-module coupling" tradeoff `profile_repository.rs` itself made
//! relative to `media/media_repository.rs`'s analogous helpers.
//!
//! ## `upsert_remote` is a full overwrite, not a partial patch
//! Unlike `AccountProfileRepository::upsert_profile` (task 2.1), which takes
//! a `ProfilePatch` of item-by-item optional changes, design.md's
//! `upsert_remote` signature takes a complete `&RemoteAccount` value — the
//! task text calls it "正規化結果の upsert" (the upsert of a normalization
//! *result*, i.e. always a fresh, complete normalization of the remote actor
//! document, never a partial field-level edit). So every column except `id`
//! is unconditionally overwritten with the incoming value on conflict (a
//! plain `ON CONFLICT (actor_uri) DO UPDATE SET col = EXCLUDED.col`, no
//! `COALESCE`).
//!
//! ## `id` stability across re-upserts of the same `actor_uri`
//! `remote_accounts.id` is `BIGINT PRIMARY KEY` (app-minted by
//! `IdGenerator`, never a DB default — `migrations/0006_accounts.sql`'s own
//! comment), while `actor_uri` carries the separate `UNIQUE` constraint this
//! upsert conflicts on. `find_remote_by_id` is one of this module's own
//! lookup paths, so once a caller has learned an `Id` for a given remote
//! account (e.g. cached it as an `AccountRef::Remote(id)` elsewhere), that
//! `Id` must keep resolving to the same logical account across any number of
//! re-normalizations. [`upsert_remote`]'s `ON CONFLICT ... DO UPDATE`
//! therefore deliberately excludes `id` from its `SET` list: the first
//! upsert for a given `actor_uri` establishes that row's `id` permanently,
//! and every later re-upsert for the same `actor_uri` keeps that original
//! `id` no matter what `id` the caller's `RemoteAccount` value carries (the
//! returned `RemoteAccount` reflects the row's actual, possibly-different,
//! persisted `id` — see [`upsert_remote_is_idempotent_and_keeps_the_original_id`]
//! in this module's test suite for the caller-passes-a-different-id case
//! this guards against).

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::{Duration, OffsetDateTime};

use crate::accounts::model::{ProfileField, RemoteAccount};
use crate::domain::Id;
use crate::error::AppError;

fn map_query_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Builds `remote_accounts.fields`' JSONB array representation from a
/// [`ProfileField`] slice. See this module's doc comment ("`fields` (JSONB)
/// hand-rolled (de)serialization") for why this is hand-built rather than
/// derived, and why it is duplicated from `profile_repository.rs` rather
/// than imported.
fn fields_to_json(fields: &[ProfileField]) -> serde_json::Value {
    serde_json::Value::Array(
        fields
            .iter()
            .map(|field| {
                serde_json::json!({
                    "name": field.name,
                    "value": field.value,
                    "verified_at": field.verified_at.map(|t| t.unix_timestamp()),
                })
            })
            .collect(),
    )
}

/// Parses `remote_accounts.fields`' JSONB array representation back into
/// [`ProfileField`]s. Panics on a malformed array — a row this repository
/// itself always writes via [`fields_to_json`] should never come back
/// malformed; a panic here would only fire on genuine data corruption, not a
/// normal error path (mirrors `profile_repository.rs::fields_from_json`'s
/// identical precedent).
fn fields_from_json(value: &serde_json::Value) -> Vec<ProfileField> {
    let items = value
        .as_array()
        .unwrap_or_else(|| panic!("remote_accounts.fields must be a JSON array, got {value:?}"));

    items
        .iter()
        .map(|item| {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    panic!("remote_accounts.fields item missing string 'name': {item:?}")
                })
                .to_string();
            let value_field = item
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    panic!("remote_accounts.fields item missing string 'value': {item:?}")
                })
                .to_string();
            let verified_at = item.get("verified_at").and_then(|v| v.as_i64()).map(|ts| {
                OffsetDateTime::from_unix_timestamp(ts).expect(
                    "persisted remote_accounts.fields verified_at must be a valid unix timestamp",
                )
            });
            ProfileField {
                name,
                value: value_field,
                verified_at,
            }
        })
        .collect()
}

/// A `remote_accounts` row's columns, as read directly off the wire (shared
/// shape between [`find_remote_by_uri`]/[`find_remote_by_id`]'s `SELECT`s and
/// [`upsert_remote`]'s `INSERT ... RETURNING`).
type RemoteRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    serde_json::Value,
    bool,
    bool,
    OffsetDateTime,
);

/// The column list [`find_remote_by_uri`]/[`find_remote_by_id`]/
/// [`upsert_remote`] share, matching [`RemoteRow`]'s tuple shape exactly. A
/// `macro_rules!`-based textual constant (not a `const &str`), mirroring
/// `profile_repository.rs::profile_columns!`'s identical precedent: sqlx's
/// `query_as` requires a `'static`-literal-shaped query, so this is spliced
/// into a `concat!`-built literal rather than interpolated at runtime.
macro_rules! remote_columns {
    () => {
        "id, actor_uri, username, domain, display_name, note, url, avatar_url, header_url, \
         fields, bot, locked, fetched_at"
    };
}

/// Reconstructs a [`RemoteAccount`] from a raw row tuple.
fn row_to_remote_account(row: RemoteRow) -> RemoteAccount {
    let (
        id,
        actor_uri,
        username,
        domain,
        display_name,
        note,
        url,
        avatar_url,
        header_url,
        fields,
        bot,
        locked,
        fetched_at,
    ) = row;

    RemoteAccount {
        id: Id::from_i64(id),
        actor_uri,
        username,
        domain,
        display_name,
        note,
        url,
        avatar_url,
        header_url,
        fields: fields_from_json(&fields),
        bot,
        locked,
        fetched_at,
    }
}

/// Looks up the [`RemoteAccount`] cached for `actor_uri` (Requirements 3.2,
/// 7.2). Returns `Ok(None)` — not an error — when no `remote_accounts` row
/// exists yet for this `actor_uri`.
pub async fn find_remote_by_uri(
    pool: &PgPool,
    actor_uri: &str,
) -> Result<Option<RemoteAccount>, AppError> {
    let row: Option<RemoteRow> = sqlx::query_as(concat!(
        "SELECT ",
        remote_columns!(),
        " FROM remote_accounts WHERE actor_uri = $1"
    ))
    .bind(actor_uri)
    .fetch_optional(pool)
    .await
    .map_err(map_query_error)?;

    Ok(row.map(row_to_remote_account))
}

/// Looks up the [`RemoteAccount`] cached under internal identifier `id`
/// (Requirement 3.1). Returns `Ok(None)` — not an error — when no
/// `remote_accounts` row exists for this `id`.
pub async fn find_remote_by_id(pool: &PgPool, id: Id) -> Result<Option<RemoteAccount>, AppError> {
    let row: Option<RemoteRow> = sqlx::query_as(concat!(
        "SELECT ",
        remote_columns!(),
        " FROM remote_accounts WHERE id = $1"
    ))
    .bind(id.as_i64())
    .fetch_optional(pool)
    .await
    .map_err(map_query_error)?;

    Ok(row.map(row_to_remote_account))
}

/// Inserts or overwrites the cached normalization result for
/// `account.actor_uri` (Requirement 7.2), keyed on the table's `actor_uri
/// UNIQUE` constraint so a second upsert for the same `actor_uri` never
/// creates a duplicate row and always leaves the latest normalized values in
/// place (Requirement 7.3's staleness re-check ultimately re-runs through
/// this same path). Every column except `id` is unconditionally overwritten
/// with `account`'s value — see this module's doc comment ("`upsert_remote`
/// is a full overwrite, not a partial patch") for why that differs from
/// `AccountProfileRepository::upsert_profile`'s `COALESCE`-based partial
/// patch, and ("`id` stability across re-upserts of the same `actor_uri`")
/// for why `id` itself is deliberately excluded from the `UPDATE` branch.
pub async fn upsert_remote(
    pool: &PgPool,
    account: &RemoteAccount,
) -> Result<RemoteAccount, AppError> {
    let fields = fields_to_json(&account.fields);

    let row: RemoteRow = sqlx::query_as(concat!(
        "INSERT INTO remote_accounts ( \
             id, actor_uri, username, domain, display_name, note, url, avatar_url, header_url, \
             fields, bot, locked, fetched_at \
         ) VALUES ( \
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13 \
         ) \
         ON CONFLICT (actor_uri) DO UPDATE SET \
             username = EXCLUDED.username, \
             domain = EXCLUDED.domain, \
             display_name = EXCLUDED.display_name, \
             note = EXCLUDED.note, \
             url = EXCLUDED.url, \
             avatar_url = EXCLUDED.avatar_url, \
             header_url = EXCLUDED.header_url, \
             fields = EXCLUDED.fields, \
             bot = EXCLUDED.bot, \
             locked = EXCLUDED.locked, \
             fetched_at = EXCLUDED.fetched_at \
         RETURNING ",
        remote_columns!()
    ))
    .bind(account.id.as_i64())
    .bind(&account.actor_uri)
    .bind(&account.username)
    .bind(&account.domain)
    .bind(&account.display_name)
    .bind(&account.note)
    .bind(&account.url)
    .bind(&account.avatar_url)
    .bind(&account.header_url)
    .bind(fields)
    .bind(account.bot)
    .bind(account.locked)
    .bind(account.fetched_at)
    .fetch_one(pool)
    .await
    .map_err(map_query_error)?;

    Ok(row_to_remote_account(row))
}

/// A pure, DB-free staleness check for a normalized remote account
/// (Requirement 7.3's "陳腐化判定"): `true` once at least `ttl` has elapsed
/// between `fetched_at` and `now`. `ttl` is supplied by the caller — see
/// this module's doc comment ("Reconciling `fetched_at` 陳腐化判定...") for
/// why this repository does not hardcode a TTL constant itself.
///
/// The boundary is inclusive: an elapsed duration exactly equal to `ttl`
/// counts as stale (a cache is considered to have "reached" its TTL, not to
/// still have an instant of freshness left at the exact boundary). A
/// `fetched_at` at or after `now` (e.g. supplied by a caller with a skewed
/// or mocked clock) is never stale, since no time has elapsed at all.
pub fn is_stale(fetched_at: OffsetDateTime, now: OffsetDateTime, ttl: Duration) -> bool {
    if now <= fetched_at {
        return false;
    }
    (now - fetched_at) >= ttl
}
