//! `AccountProfileRepository` (design.md "Data / データ層" ->
//! `AccountProfileRepository / RemoteAccountRepository / CustomEmojiRepository /
//! InstanceSettingsRepository`, Requirements 1.4, 2.2, 6.1, 6.5; task 2.1,
//! `Boundary: AccountProfileRepository`): the local-actor profile
//! extension's persistence — actor_id lookup and `update_credentials`'
//! partial upsert — against `migrations/0006_accounts.sql`'s
//! `account_profiles` table (already applied, unmodified by this task).
//!
//! Scope: this module owns exactly [`find_profile`]/[`upsert_profile`],
//! design.md's Service Interface signatures for this half of the
//! `AccountProfileRepository`/`RemoteAccountRepository`/
//! `CustomEmojiRepository`/`InstanceSettingsRepository` component (the
//! `remote`/`emoji`/`instance settings` Service Interface entries belong to
//! tasks 2.2/2.3/2.4, out of this task's boundary). It does not touch
//! `src/accounts/model.rs`, `src/accounts/ports.rs`, or any migration file.
//!
//! ## Reconciling the task text with design.md's literal signature
//! Task 2.1's own text says an actor with no profile row yet gets "a safe
//! default" (未作成アクターには安全な既定を返す), and design.md's
//! Responsibilities prose for this component repeats the same claim
//! verbatim ("プロフィール未作成のローカルアクターには安全な既定を返す").
//! But design.md's own Service Interface code block is explicit:
//! `find_profile(...) -> Result<Option<AccountProfile>, AppError>` — an
//! `Option`, not an infallible default-substituting accessor. Per this
//! task's own guidance ("design.md's Service Interface block is generally
//! the authoritative contract other tasks are built against"), [`find_profile`]
//! keeps that literal `Option<AccountProfile>` signature intact: `None`
//! means "no `account_profiles` row exists for this `actor_id` yet", full
//! stop, with no substitution performed inside this function. The "safe
//! default" the task text/design.md prose describes is instead provided as
//! [`AccountProfile::default_for`], a small constructor added here (an
//! `impl AccountProfile` block in this file — permitted under Rust's orphan
//! rules without editing `model.rs`, which this task must not touch) that
//! any caller (`AccountService`, task 5.x, out of this task's boundary) can
//! substitute when [`find_profile`] returns `None`. This mirrors
//! `account_profiles`'s own column `DEFAULT`s exactly (`display_name`/`note`
//! default to `''`, `locked`/`bot`/`discoverable`/`source_sensitive` default
//! to `FALSE`, `source_privacy` defaults to `'public'`, `fields` defaults to
//! `'[]'`) so the "safe default" a caller substitutes is identical in shape
//! to the row [`upsert_profile`] would create from an all-`None` patch.
//!
//! ## `fields` (JSONB) hand-rolled (de)serialization
//! [`crate::accounts::model::ProfileField`] deliberately carries no
//! `#[derive(Serialize, Deserialize)]` (task 1.2's own scope, not this
//! task's to add — this task must not edit `model.rs`). `account_profiles.fields`
//! is still a genuine `JSONB` column, so this module hand-builds/-parses the
//! JSON array itself ([`fields_to_json`]/[`fields_from_json`]) rather than
//! relying on a derived `Serialize`/`Deserialize` impl that does not exist.
//! `verified_at` is carried as a Unix timestamp (`i64`, via
//! [`time::OffsetDateTime::unix_timestamp`]/[`time::OffsetDateTime::from_unix_timestamp`])
//! rather than an RFC 3339 string, so no additional `time` crate formatting
//! feature is needed beyond what the crate already enables.
//! [`fields_from_json`] panics on a malformed array — mirrors
//! `media/media_repository.rs`'s `media_type_from_str`/`media_state_from_str`
//! precedent: a row this repository itself always writes via
//! [`fields_to_json`] should never come back malformed; a panic here would
//! only fire on genuine data corruption, not a normal error path.
//!
//! ## `upsert_profile`'s patch application (Requirement 6.1, 6.5)
//! A single `INSERT ... ON CONFLICT (actor_id) DO UPDATE` statement, never a
//! read-modify-write pair of separate queries (mirrors
//! `media/media_repository.rs::update_metadata`'s identical "atomic COALESCE
//! update, no lost-update race" discipline). Two different "patch item not
//! present" encodings are handled:
//! - Plain `Option<T>` fields (`display_name`/`note`/`fields`/`locked`/`bot`/
//!   `discoverable`/`source_privacy`/`source_sensitive`): `None` means
//!   "leave unchanged" on an `UPDATE` (`COALESCE($n, account_profiles.column)`)
//!   and "use the table's own safe default" on a fresh `INSERT`
//!   (`COALESCE($n, '<default literal>')`).
//! - Doubled `Option<Option<T>>` fields (`avatar_media`/`header_media`/
//!   `source_language`): a plain `COALESCE` cannot distinguish "leave
//!   unchanged" (outer `None`) from "explicitly clear to `NULL`" (outer
//!   `Some(None)`) — both would bind as SQL `NULL`. Each of these three
//!   fields therefore also binds a `bool` "touch" flag
//!   (`patch.<field>.is_some()`); the `UPDATE` branch uses
//!   `CASE WHEN $touch THEN $value ELSE account_profiles.<column> END`. On a
//!   fresh `INSERT` the touch flag does not matter — inserting the flattened
//!   value (`NULL` for both "leave unchanged, nothing to leave" and
//!   "explicitly clear" on a row that does not exist yet) is correct either
//!   way.
//!
//! `now` (Requirement 6.5: "時刻は `RuntimeContext`") stamps `updated_at` on
//! every upsert, supplied by the caller — this module performs no
//! wall-clock reads itself, mirroring every other repository in this crate.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;
use time::OffsetDateTime;

use crate::accounts::model::{AccountProfile, CredentialSource, ProfileField, ProfilePatch};
use crate::domain::{Id, Visibility};
use crate::error::AppError;

fn map_query_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Maps a [`Visibility`] to its `account_profiles.source_privacy` `TEXT`
/// column representation. Matches [`Visibility`]'s own
/// `#[serde(rename_all = "snake_case")]` rendering
/// (`crate::domain::primitives`) so a value written here reads back
/// identically whether inspected via SQL or via this column's JSON-facing
/// sibling.
fn visibility_as_str(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Public => "public",
        Visibility::Unlisted => "unlisted",
        Visibility::Private => "private",
        Visibility::Direct => "direct",
    }
}

/// Reconstructs a [`Visibility`] from an already-persisted
/// `account_profiles.source_privacy` column value. Panics on any other
/// value — such a row could only exist if something wrote outside this
/// module's own [`visibility_as_str`] mapping, a data-corruption invariant
/// violation, not a normal error path (mirrors
/// `media/media_repository.rs::media_type_from_str`'s identical precedent).
fn visibility_from_str(raw: &str) -> Visibility {
    match raw {
        "public" => Visibility::Public,
        "unlisted" => Visibility::Unlisted,
        "private" => Visibility::Private,
        "direct" => Visibility::Direct,
        other => panic!(
            "account_profiles.source_privacy contained unexpected value {other:?}; expected one \
             of 'public'/'unlisted'/'private'/'direct'"
        ),
    }
}

/// Builds `account_profiles.fields`' JSONB array representation from a
/// [`ProfileField`] slice. See this module's doc comment ("`fields` (JSONB)
/// hand-rolled (de)serialization") for why this is hand-built rather than
/// derived.
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

/// Parses `account_profiles.fields`' JSONB array representation back into
/// [`ProfileField`]s. Panics on a malformed array — see this module's doc
/// comment for why that is the right behavior here.
fn fields_from_json(value: &serde_json::Value) -> Vec<ProfileField> {
    let items = value
        .as_array()
        .unwrap_or_else(|| panic!("account_profiles.fields must be a JSON array, got {value:?}"));

    items
        .iter()
        .map(|item| {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    panic!("account_profiles.fields item missing string 'name': {item:?}")
                })
                .to_string();
            let value_field = item
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    panic!("account_profiles.fields item missing string 'value': {item:?}")
                })
                .to_string();
            let verified_at = item.get("verified_at").and_then(|v| v.as_i64()).map(|ts| {
                OffsetDateTime::from_unix_timestamp(ts).expect(
                    "persisted account_profiles.fields verified_at must be a valid unix timestamp",
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

/// A `account_profiles` row's columns, as read directly off the wire
/// (shared shape between [`find_profile`]'s `SELECT` and [`upsert_profile`]'s
/// `INSERT ... RETURNING`).
type ProfileRow = (
    i64,
    String,
    String,
    Option<i64>,
    Option<i64>,
    serde_json::Value,
    bool,
    bool,
    bool,
    String,
    bool,
    Option<String>,
    OffsetDateTime,
);

/// The column list [`find_profile`]/[`upsert_profile`] share, matching
/// [`ProfileRow`]'s tuple shape exactly. A `macro_rules!`-based textual
/// constant (not a `const &str`), mirroring
/// `media/media_repository.rs::media_row_columns!`'s identical precedent:
/// sqlx's `query_as` requires a `'static`-literal-shaped query (its
/// `SqlSafeStr` bound rejects a runtime-built `String`, e.g. from `format!`,
/// as an SQL-injection-auditing safeguard), so this is spliced into a
/// `concat!`-built literal rather than interpolated at runtime.
macro_rules! profile_columns {
    () => {
        "actor_id, display_name, note, avatar_media_id, header_media_id, fields, locked, bot, \
         discoverable, source_privacy, source_sensitive, source_language, updated_at"
    };
}

/// Reconstructs an [`AccountProfile`] from a raw row tuple. `source.note`/
/// `source.fields` mirror the profile's own `note`/`fields` (this spec keeps
/// no separate raw-vs-rendered note representation); `source.follow_requests_count`
/// is always `0` here — that count is not an `account_profiles` column at
/// all (it is social-graph's own delegated concern, out of this repository's
/// boundary), so this repository can only ever report the persisted
/// baseline of `0`, leaving any real substitution to whichever later
/// caller/provider owns that count.
fn row_to_profile(row: ProfileRow) -> AccountProfile {
    let (
        actor_id,
        display_name,
        note,
        avatar_media_id,
        header_media_id,
        fields,
        locked,
        bot,
        discoverable,
        source_privacy,
        source_sensitive,
        source_language,
        _updated_at,
    ) = row;

    let fields = fields_from_json(&fields);

    AccountProfile {
        actor_id: Id::from_i64(actor_id),
        display_name,
        note: note.clone(),
        avatar_media: avatar_media_id.map(Id::from_i64),
        header_media: header_media_id.map(Id::from_i64),
        fields: fields.clone(),
        locked,
        bot,
        discoverable,
        source: CredentialSource {
            privacy: visibility_from_str(&source_privacy),
            sensitive: source_sensitive,
            language: source_language,
            note,
            fields,
            follow_requests_count: 0,
        },
    }
}

/// A safe default-shaped [`AccountProfile`] for a local actor that has not
/// yet created an `account_profiles` row. See this module's doc comment
/// ("Reconciling the task text with design.md's literal signature") for why
/// this exists alongside [`find_profile`] returning `Option<AccountProfile>`
/// rather than substituting a default itself.
impl AccountProfile {
    pub fn default_for(actor_id: Id) -> Self {
        AccountProfile {
            actor_id,
            display_name: String::new(),
            note: String::new(),
            avatar_media: None,
            header_media: None,
            fields: Vec::new(),
            locked: false,
            bot: false,
            discoverable: false,
            source: CredentialSource {
                privacy: Visibility::Public,
                sensitive: false,
                language: None,
                note: String::new(),
                fields: Vec::new(),
                follow_requests_count: 0,
            },
        }
    }
}

/// Looks up the [`AccountProfile`] persisted for `actor_id` (Requirement
/// 1.4, 2.2). Returns `Ok(None)` — not an error, not a substituted default —
/// when no `account_profiles` row exists yet for this actor; see this
/// module's doc comment for why `find_profile` keeps design.md's literal
/// `Option<AccountProfile>` signature rather than substituting
/// [`AccountProfile::default_for`] itself.
pub async fn find_profile(pool: &PgPool, actor_id: Id) -> Result<Option<AccountProfile>, AppError> {
    let row: Option<ProfileRow> = sqlx::query_as(concat!(
        "SELECT ",
        profile_columns!(),
        " FROM account_profiles WHERE actor_id = $1"
    ))
    .bind(actor_id.as_i64())
    .fetch_optional(pool)
    .await
    .map_err(map_query_error)?;

    Ok(row.map(row_to_profile))
}

/// Applies `patch` to `actor_id`'s `account_profiles` row, creating it first
/// (with every un-patched column at its safe table default) if it does not
/// exist yet, and returns the resulting [`AccountProfile`] (Requirement 6.1,
/// 6.5). A patch item left `None` never changes that column's already-stored
/// value; see this module's doc comment ("`upsert_profile`'s patch
/// application") for the exact atomic-`UPSERT` mechanics, including how the
/// doubled `Option<Option<_>>` fields' "leave unchanged" vs. "explicitly
/// clear" distinction is preserved. `now` stamps `updated_at`
/// (Requirement 6.5: "時刻は `RuntimeContext`" — supplied by the caller, this
/// function performs no wall-clock read itself).
pub async fn upsert_profile(
    pool: &PgPool,
    actor_id: Id,
    patch: ProfilePatch,
    now: OffsetDateTime,
) -> Result<AccountProfile, AppError> {
    let display_name = patch.display_name.as_deref();
    let note = patch.note.as_deref();

    let avatar_media_id = patch.avatar_media.flatten().map(|id| id.as_i64());
    let avatar_touch = patch.avatar_media.is_some();
    let header_media_id = patch.header_media.flatten().map(|id| id.as_i64());
    let header_touch = patch.header_media.is_some();

    let fields = patch.fields.as_deref().map(fields_to_json);

    let locked = patch.locked;
    let bot = patch.bot;
    let discoverable = patch.discoverable;

    let source_privacy = patch.source_privacy.map(visibility_as_str);
    let source_sensitive = patch.source_sensitive;

    let source_language = patch.source_language.clone().flatten();
    let source_language_touch = patch.source_language.is_some();

    let row: ProfileRow = sqlx::query_as(concat!(
        "INSERT INTO account_profiles ( \
             actor_id, display_name, note, avatar_media_id, header_media_id, fields, \
             locked, bot, discoverable, source_privacy, source_sensitive, source_language, \
             updated_at \
         ) VALUES ( \
             $1, COALESCE($2, ''), COALESCE($3, ''), $4, $6, COALESCE($8, '[]'::jsonb), \
             COALESCE($9, FALSE), COALESCE($10, FALSE), COALESCE($11, FALSE), \
             COALESCE($12, 'public'), COALESCE($13, FALSE), $14, $16 \
         ) \
         ON CONFLICT (actor_id) DO UPDATE SET \
             display_name = COALESCE($2, account_profiles.display_name), \
             note = COALESCE($3, account_profiles.note), \
             avatar_media_id = CASE WHEN $5 THEN $4 ELSE account_profiles.avatar_media_id END, \
             header_media_id = CASE WHEN $7 THEN $6 ELSE account_profiles.header_media_id END, \
             fields = COALESCE($8, account_profiles.fields), \
             locked = COALESCE($9, account_profiles.locked), \
             bot = COALESCE($10, account_profiles.bot), \
             discoverable = COALESCE($11, account_profiles.discoverable), \
             source_privacy = COALESCE($12, account_profiles.source_privacy), \
             source_sensitive = COALESCE($13, account_profiles.source_sensitive), \
             source_language = CASE WHEN $15 THEN $14 ELSE account_profiles.source_language END, \
             updated_at = $16 \
         RETURNING ",
        profile_columns!()
    ))
    .bind(actor_id.as_i64())
    .bind(display_name)
    .bind(note)
    .bind(avatar_media_id)
    .bind(avatar_touch)
    .bind(header_media_id)
    .bind(header_touch)
    .bind(fields)
    .bind(locked)
    .bind(bot)
    .bind(discoverable)
    .bind(source_privacy)
    .bind(source_sensitive)
    .bind(source_language)
    .bind(source_language_touch)
    .bind(now)
    .fetch_one(pool)
    .await
    .map_err(map_query_error)?;

    Ok(row_to_profile(row))
}
