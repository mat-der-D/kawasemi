//! `InstanceSettingsRepository` (design.md "Data / データ層" ->
//! `AccountProfileRepository / RemoteAccountRepository / CustomEmojiRepository /
//! InstanceSettingsRepository`, Requirements 8.1, 8.2, 8.3; task 2.4,
//! `Boundary: InstanceSettingsRepository`): the operational
//! instance-settings singleton's **read + application-level default
//! merge** — against `migrations/0006_accounts.sql`'s `instance_settings`
//! table (already applied, unmodified by this task).
//!
//! Scope: this module owns exactly [`load_instance_settings`], design.md's
//! Service Interface signature for the `// instance settings (read +
//! default merge)` half of the `AccountProfileRepository`/
//! `RemoteAccountRepository`/`CustomEmojiRepository`/
//! `InstanceSettingsRepository` component (the `profile`/`remote`/`emoji`
//! Service Interface entries belong to tasks 2.1/2.2/2.3, out of this
//! task's boundary). It does not touch `src/accounts/model.rs`,
//! `src/accounts/ports.rs`, `src/accounts/profile_repository.rs`,
//! `src/accounts/remote_repository.rs`, `src/accounts/emoji_repository.rs`,
//! or any migration file.
//!
//! ## Read-only, by construction (Requirement 8.2/8.3; design.md's Out of
//! Boundary: "運用設定の書き込み/管理画面...本 spec は読み取りと初期既定のみ")
//! This module defines **no** `INSERT`/`UPDATE`/`UPSERT` against
//! `instance_settings` anywhere. Writing/managing operational settings
//! belongs to admin-frontend (design.md "Boundary Commitments" -> "Out of
//! Boundary": "既定: `instance_settings` は未投入でも `load_instance_settings`
//! が既定マージで全項目を返す（8.3）。書き込みは admin-frontend"). There is
//! deliberately no seed/bootstrap row created anywhere in this crate (no
//! migration inserts `id = 1`, and this repository must not "self-heal" a
//! missing row by inserting one) — on a fresh database the `id = 1` row
//! simply does not exist, and this module handles that case entirely in
//! application code (see [`default_instance_settings`]).
//!
//! ## Default-merge strategy
//! Two distinct cases both need to produce an always-fully-populated
//! [`InstanceSettings`] (Requirement 8.3: "運用設定に値が未設定の項目がある間、
//! ...安全な初期既定値を用いて応答する"):
//! - **No row at all** (fresh database, `id = 1` never inserted):
//!   [`load_instance_settings`] returns [`default_instance_settings`]
//!   verbatim — every field at the same safe default
//!   `migrations/0006_accounts.sql`'s own column `DEFAULT`s specify (empty
//!   strings, empty JSONB arrays, `false` booleans, `NULL`/`None` for the
//!   nullable columns, explicitly including `thumbnail` (default `null`,
//!   Requirement 8.1) and `languages` (default `[]`, Requirement 8.1)).
//! - **Row present, some columns left at their own table default**: no
//!   extra application-level merge step is needed here, because Postgres
//!   itself already applied each column's `DEFAULT` at `INSERT` time for
//!   any column the (admin-frontend-owned) writer did not specify — reading
//!   the row's columns directly already yields the same
//!   default-for-unset-items behavior this repository does not itself
//!   perform any writes to produce. [`row_to_settings`] is therefore a
//!   direct, un-merged column mapping; the only place this module
//!   synthesizes a default value itself is the "no row" branch above.
//!
//! ## `rules`/`languages` (JSONB `TEXT[]`) hand-rolled deserialization
//! Mirroring `profile_repository.rs`'s `fields`/`remote_repository.rs`'s
//! `fields` precedent: [`crate::accounts::model::InstanceSettings`]
//! deliberately carries no `#[derive(Serialize, Deserialize)]` (task 1.2's
//! own scope, not this task's to add — this task must not edit
//! `model.rs`), so this module hand-parses `rules`/`languages`' shared
//! `JSONB` array-of-strings shape itself ([`string_array_from_json`])
//! rather than relying on a derived `Deserialize` impl that does not
//! exist. No corresponding `_to_json` encoder exists here — see "Read-only,
//! by construction" above; this module never writes either column.
//! [`string_array_from_json`] panics on a malformed array, mirroring
//! `profile_repository.rs::fields_from_json`'s identical precedent: a row
//! this repository never writes but only ever reads back from a table this
//! crate's own migration defines with a `JSONB NOT NULL DEFAULT '[]'`
//! column should never be malformed under normal operation; a panic here
//! only fires on genuine data corruption or an out-of-band write by
//! something outside this crate's own schema discipline, not a normal
//! error path.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;

use crate::accounts::model::InstanceSettings;
use crate::domain::Id;
use crate::error::AppError;

fn map_query_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// Parses a `JSONB` array-of-strings column value (shared shape between
/// `instance_settings.rules` and `instance_settings.languages`) into a
/// `Vec<String>`. Panics on a malformed array — see this module's doc
/// comment ("`rules`/`languages` (JSONB `TEXT[]`) hand-rolled
/// deserialization") for why that is the right behavior here.
fn string_array_from_json(value: &serde_json::Value) -> Vec<String> {
    let items = value.as_array().unwrap_or_else(|| {
        panic!("instance_settings JSONB string-array column must be a JSON array, got {value:?}")
    });

    items
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| {
                    panic!("instance_settings JSONB string-array column item must be a string, got {item:?}")
                })
                .to_string()
        })
        .collect()
}

/// The safe, fully-populated [`InstanceSettings`] this repository returns
/// when no `instance_settings` row exists yet (Requirement 8.1, 8.3):
/// every field at the same default `migrations/0006_accounts.sql`'s own
/// column `DEFAULT`s specify — `thumbnail: None` (default `null`) and
/// `languages: vec![]` (default `[]`) explicitly included, per Requirement
/// 8.1 and this task's own named observable completion condition.
fn default_instance_settings() -> InstanceSettings {
    InstanceSettings {
        title: String::new(),
        description: String::new(),
        contact_email: String::new(),
        contact_account_id: None,
        rules: Vec::new(),
        registrations_enabled: false,
        registrations_approval_required: false,
        registrations_message: None,
        thumbnail: None,
        languages: Vec::new(),
    }
}

/// A `instance_settings` row's columns, as read directly off the wire
/// (excludes `id`/`updated_at` — [`InstanceSettings`] does not carry
/// either; design.md's model excerpt is explicit that `InstanceSettings`
/// holds only the operationally-variable fields).
type SettingsRow = (
    String,
    String,
    String,
    Option<i64>,
    serde_json::Value,
    bool,
    bool,
    Option<String>,
    Option<String>,
    serde_json::Value,
);

/// The column list [`load_instance_settings`]'s `SELECT` uses, matching
/// [`SettingsRow`]'s tuple shape exactly. A `macro_rules!`-based textual
/// constant (not a `const &str`), mirroring `profile_repository.rs::
/// profile_columns!`/`remote_repository.rs::remote_columns!`/
/// `emoji_repository.rs::emoji_columns!`'s identical precedent: sqlx's
/// `query_as` requires a `'static`-literal-shaped query, so this is spliced
/// into a `concat!`-built literal rather than interpolated at runtime.
macro_rules! settings_columns {
    () => {
        "title, description, contact_email, contact_account_id, rules, registrations_enabled, \
         registrations_approval_required, registrations_message, thumbnail, languages"
    };
}

/// Reconstructs an [`InstanceSettings`] from a raw row tuple. A direct
/// column mapping with no further default substitution — see this module's
/// doc comment ("Default-merge strategy") for why a present row needs no
/// extra merge step here.
fn row_to_settings(row: SettingsRow) -> InstanceSettings {
    let (
        title,
        description,
        contact_email,
        contact_account_id,
        rules,
        registrations_enabled,
        registrations_approval_required,
        registrations_message,
        thumbnail,
        languages,
    ) = row;

    InstanceSettings {
        title,
        description,
        contact_email,
        contact_account_id: contact_account_id.map(Id::from_i64),
        rules: string_array_from_json(&rules),
        registrations_enabled,
        registrations_approval_required,
        registrations_message,
        thumbnail,
        languages: string_array_from_json(&languages),
    }
}

/// Loads the singleton `instance_settings` row (`id = 1`) and merges any
/// unset item to its safe default, always returning every
/// [`InstanceSettings`] field filled in (Requirements 8.1, 8.2, 8.3). When
/// no row exists yet — a fresh database, since nothing in this crate seeds
/// one (see this module's doc comment, "Read-only, by construction") —
/// returns [`default_instance_settings`] verbatim, including `thumbnail:
/// None` and `languages: vec![]`. Never writes to `instance_settings`
/// (read-only per this repository's boundary).
pub async fn load_instance_settings(pool: &PgPool) -> Result<InstanceSettings, AppError> {
    let row: Option<SettingsRow> = sqlx::query_as(concat!(
        "SELECT ",
        settings_columns!(),
        " FROM instance_settings WHERE id = 1"
    ))
    .fetch_optional(pool)
    .await
    .map_err(map_query_error)?;

    Ok(row
        .map(row_to_settings)
        .unwrap_or_else(default_instance_settings))
}
