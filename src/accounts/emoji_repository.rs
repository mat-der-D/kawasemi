//! `CustomEmojiRepository` (design.md "Data / データ層" ->
//! `AccountProfileRepository / RemoteAccountRepository / CustomEmojiRepository /
//! InstanceSettingsRepository`, Requirements 1.4, 9.1, 9.3; task 2.3,
//! `Boundary: CustomEmojiRepository`): the custom-emoji **read-only** model's
//! persistence — visible-in-picker listing and shortcode resolution — against
//! `migrations/0006_accounts.sql`'s `custom_emojis` table (already applied,
//! unmodified by this task).
//!
//! Scope: this module owns exactly [`list_visible_emojis`]/[`resolve_emojis`],
//! design.md's Service Interface signatures for the `// emoji (read only)`
//! half of the `AccountProfileRepository`/`RemoteAccountRepository`/
//! `CustomEmojiRepository`/`InstanceSettingsRepository` component (the
//! `profile`/`remote`/`instance settings` Service Interface entries belong to
//! tasks 2.1/2.2/2.4, out of this task's boundary). It does not touch
//! `src/accounts/model.rs`, `src/accounts/ports.rs`,
//! `src/accounts/profile_repository.rs`, `src/accounts/remote_repository.rs`,
//! or any migration file. This module defines **no** `INSERT`/`UPDATE`/
//! `DELETE` against `custom_emojis` anywhere — Requirement 9.3 ("その登録・
//! アップロード・連合取り込み・管理は本 spec で行わない") is enforced
//! structurally by this module simply never containing such a statement, not
//! by a runtime check. Population of `custom_emojis` rows is a later spec's
//! (custom-federation's) responsibility; this repository's own integration
//! tests seed rows with raw `sqlx::query` `INSERT`s directly against the test
//! database, not through any function this module exposes.
//!
//! ## design.md prose vs. literal-signature judgment call
//! Mirroring task 2.1/2.2's own precedent (recorded in tasks.md's
//! "Implementation Notes": "design.md's Service Interface block is generally
//! the authoritative contract other tasks are built against"), design.md's
//! Service Interface code block for this component is taken as literal and
//! authoritative:
//! ```text
//! pub async fn list_visible_emojis(pool: &PgPool) -> Result<Vec<CustomEmojiView>, AppError>;
//! pub async fn resolve_emojis(pool: &PgPool, shortcodes: &[String]) -> Result<Vec<CustomEmojiView>, AppError>;
//! ```
//! Neither signature takes a `domain` parameter, even though the table's
//! primary key is the composite `(shortcode, domain)` — `domain = ''` meaning
//! local (`migrations/0006_accounts.sql`). **Both functions read this
//! absence the same way: no domain filter, matching rows across every
//! `domain`.** This is the "literal-signature-is-authoritative" principle
//! applied symmetrically to both functions:
//!
//! - **`list_visible_emojis` does not filter by domain** — it returns every
//!   `visible_in_picker = TRUE` row regardless of `domain`. Requirement 9.1's
//!   text ("ピッカーに表示可能なカスタム絵文字の一覧を...返す") names no domain
//!   restriction at all, and Mastodon's real `GET /api/v1/custom_emojis`
//!   endpoint (the "一次情報は Mastodon 実レスポンス" this spec's steering
//!   commits to) returns every emoji the instance knows about with
//!   `visible_in_picker: true` — local *and* any remote emoji the instance
//!   has learned about via federation — not local-only.
//! - **`resolve_emojis` also does not filter by domain** — it matches
//!   `shortcode = ANY($1)` across every `domain`. An earlier revision of this
//!   module restricted this function to `domain = ''` (local only), reasoned
//!   from Requirement 1.4's call site being a *local* account's own
//!   `display_name`/`note` text. That reasoning does not survive contact with
//!   the rest of design.md: the "accounts/:id 取得" flow diagram routes
//!   `Counts --> Emojis[resolve emojis from custom emoji read model]` for
//!   *both* the local and remote branches, not just local;
//!   `AccountSerializer::build_account_remote`'s literal Service Interface
//!   signature takes `emojis: &[CustomEmojiView]` exactly like
//!   `build_account_local`, implying its caller resolves those views through
//!   this same repository rather than receiving remote-supplied emoji data
//!   directly; Requirement 9.4 describes one read model for any Account's
//!   `emojis` construction with no local/remote carve-out; and
//!   `migrations/0006_accounts.sql`'s own comment documents per-remote-domain
//!   rows as an intended, anticipated case for this table, not an
//!   accident of the composite key. A local-only filter here was therefore a
//!   real design.md-vs-implementation gap, not a defensible narrow reading —
//!   `resolve_emojis` must be able to resolve shortcodes against
//!   remote-domain rows the same way `list_visible_emojis` already surfaces
//!   them, because both functions read from the same unified,
//!   federation-populated `custom_emojis` table.
//!
//! `resolve_emojis` also does not filter on `visible_in_picker`: an emoji
//! referenced by shortcode in an account's bio/display_name should still
//! render even if it has been hidden from the picker (`visible_in_picker`
//! only gates picker *listing*, not shortcode *resolvability* — this mirrors
//! real Mastodon's `CustomEmoji.from_text` resolution path, which is
//! independent of `visible_in_picker`).
//!
//! ## `fields`-style JSON handling not needed here
//! Unlike `profile_repository.rs`/`remote_repository.rs`, `custom_emojis` has
//! no JSONB column this module needs to hand-encode/decode — every column
//! [`CustomEmojiView`] carries maps directly to a scalar SQL column
//! (`TEXT`/`BOOLEAN`), so no `fields_to_json`/`fields_from_json`-shaped
//! helper exists in this file.

#[cfg(test)]
mod tests;

use axum::http::StatusCode;
use sqlx::postgres::PgPool;

use crate::accounts::model::CustomEmojiView;
use crate::error::AppError;

fn map_query_error(source: sqlx::Error) -> AppError {
    AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source)
}

/// A `custom_emojis` row's columns, as read directly off the wire (shared
/// shape between [`list_visible_emojis`]'s and [`resolve_emojis`]'s
/// `SELECT`s). Excludes `domain`/`updated_at` — [`CustomEmojiView`] does not
/// carry either (design.md model excerpt: `shortcode`/`url`/`static_url`/
/// `visible_in_picker`/`category` only).
type EmojiRow = (String, String, String, bool, Option<String>);

/// The column list [`list_visible_emojis`]/[`resolve_emojis`] share, matching
/// [`EmojiRow`]'s tuple shape exactly. A `macro_rules!`-based textual
/// constant (not a `const &str`), mirroring `profile_repository.rs::
/// profile_columns!`/`remote_repository.rs::remote_columns!`'s identical
/// precedent: sqlx's `query_as` requires a `'static`-literal-shaped query, so
/// this is spliced into a `concat!`-built literal rather than interpolated at
/// runtime.
macro_rules! emoji_columns {
    () => {
        "shortcode, url, static_url, visible_in_picker, category"
    };
}

/// Reconstructs a [`CustomEmojiView`] from a raw row tuple.
fn row_to_emoji(row: EmojiRow) -> CustomEmojiView {
    let (shortcode, url, static_url, visible_in_picker, category) = row;
    CustomEmojiView {
        shortcode,
        url,
        static_url,
        visible_in_picker,
        category,
    }
}

/// Lists every `custom_emojis` row with `visible_in_picker = TRUE`
/// (Requirement 9.1, 9.2). Not filtered by `domain` — see this module's doc
/// comment ("design.md prose vs. literal-signature judgment call") for
/// why. Ordered by `(domain, shortcode)` for a deterministic result across
/// calls (this repository performs no application-level sort itself
/// elsewhere, so ordering is pushed into SQL).
pub async fn list_visible_emojis(pool: &PgPool) -> Result<Vec<CustomEmojiView>, AppError> {
    let rows: Vec<EmojiRow> = sqlx::query_as(concat!(
        "SELECT ",
        emoji_columns!(),
        " FROM custom_emojis WHERE visible_in_picker = TRUE ORDER BY domain, shortcode"
    ))
    .fetch_all(pool)
    .await
    .map_err(map_query_error)?;

    Ok(rows.into_iter().map(row_to_emoji).collect())
}

/// Resolves `shortcodes` against `custom_emojis` rows in **any** `domain`
/// (Requirement 1.4, 9.1, 9.4) — not filtered to local (`domain = ''`); see
/// this module's doc comment ("design.md prose vs. literal-signature
/// judgment call") for why. This is symmetric with [`list_visible_emojis`]'s
/// "no domain filter" behavior. Shortcodes with no matching row in any domain
/// are silently skipped (never an error): a display_name/note may reference a
/// shortcode that does not (or no longer) exist as a real emoji, and that
/// should not fail Account serialization, just omit that entry from
/// `emojis`. Not filtered by `visible_in_picker` — see this module's doc
/// comment for why shortcode resolvability is independent of picker
/// visibility. An empty `shortcodes` slice resolves to an empty result
/// without an error.
pub async fn resolve_emojis(
    pool: &PgPool,
    shortcodes: &[String],
) -> Result<Vec<CustomEmojiView>, AppError> {
    let rows: Vec<EmojiRow> = sqlx::query_as(concat!(
        "SELECT ",
        emoji_columns!(),
        " FROM custom_emojis WHERE shortcode = ANY($1) ORDER BY shortcode"
    ))
    .bind(shortcodes)
    .fetch_all(pool)
    .await
    .map_err(map_query_error)?;

    Ok(rows.into_iter().map(row_to_emoji).collect())
}
