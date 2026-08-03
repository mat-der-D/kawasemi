//! `AccountSerializer` (design.md "Service / サービス層" ->
//! "AccountSerializer"; Requirements 1.1, 1.2, 1.3, 1.4, 1.5, 2.2; task 3.1,
//! `Boundary: AccountSerializer`): maps a local actor
//! ([`ResolvedActor`] + [`AccountProfile`]) or a [`RemoteAccount`] onto the
//! single, unified Account/CredentialAccount JSON contract.
//!
//! Scope: this module owns exactly the mapping from already-resolved domain
//! values to Account/CredentialAccount JSON. It does not implement
//! `AccountService` (task 5.x, the eventual caller that will resolve a
//! `ResolvedActor`/`AccountProfile`/`RemoteAccount`/counts/emoji-candidate
//! set and hand them to this module), does not implement
//! `RelationshipSerializer`/`InstanceSerializer`/`CustomEmojiSerializer`
//! (tasks 3.2/3.3/3.4), and registers no contract-harness goldens (task
//! 3.5) — this module's own tests only prove determinism and the null/
//! domain-collision disciplines called out below.
//!
//! ## Typed structs + `Serialize`, not a hand-built `serde_json::json!` value
//! Follows `src/media/serializer.rs`'s established precedent (the first
//! entity serializer in this crate, and the only prior art for this
//! module): [`AccountJson`]/[`CredentialAccountJson`]/[`CustomEmojiJson`]/
//! [`AccountFieldJson`]/[`CredentialSourceJson`]/[`RoleJson`] are plain
//! `#[derive(Serialize)]` structs mirroring Requirement 1.1's/2.2's field
//! lists field-by-field, not a `json!{...}` literal a field could silently
//! go missing from. [`account_to_json`]/[`credential_account_to_json`] are
//! thin `serde_json::to_value` wrappers, matching `media/serializer.rs`'s
//! `to_json` convention.
//!
//! ## Deliberate deviations from design.md's literal Service Interface
//! design.md's Service Interface sketch is:
//! ```text
//! pub fn build_account_local(&self, actor: &ResolvedActor, profile: &AccountProfile, counts: &AccountCounts, emojis: &[CustomEmojiView]) -> serde_json::Value;
//! pub fn build_account_remote(&self, remote: &RemoteAccount, counts: &AccountCounts, emojis: &[CustomEmojiView]) -> serde_json::Value;
//! pub fn build_credential_account(&self, actor: &ResolvedActor, profile: &AccountProfile, counts: &AccountCounts, emojis: &[CustomEmojiView]) -> serde_json::Value;
//! ```
//! This module's actual signatures add three parameters beyond that sketch,
//! each a documented, narrow gap-fill rather than a silent guess:
//!
//! - **`created_at: OffsetDateTime`.** Requirement 1.1 mandates `created_at`
//!   on every Account, but neither `ResolvedActor` nor `AccountProfile`
//!   carries it: `crate::actor::model::ResolvedActor` is actor-model's own
//!   protocol-layer reference type, and its own design.md sketch (and this
//!   crate's actual implementation) deliberately excludes `created_at` —
//!   `crate::actor::directory::local_actor_to_resolved` names and discards
//!   `LocalActor::created_at` (`created_at: _`) the same structural way it
//!   discards `owner_id`. That is actor-model's own (already-reviewed, own-
//!   spec) design, not a bug this task can fix by editing `src/actor/`
//!   (a different feature spec entirely, well outside this task's — and
//!   this spec's — boundary). Rather than silently inventing a value (e.g.
//!   `OffsetDateTime::now_utc()`, which would make every render of the same
//!   account produce a *different* `created_at` and violate this task's own
//!   "同一入力で決定的 JSON" completion condition) this module takes
//!   `created_at` as an explicit caller-supplied parameter — the same "pure
//!   function, pre-resolved inputs" contract `counts: &AccountCounts`
//!   already establishes for the delegated counts. See this task's status
//!   report CONCERNS: wiring a real value into this parameter (task 5.x,
//!   `AccountService`) will hit the same wall and needs its own resolution
//!   (most plausibly a small, additive `ResolvedActor`/`ActorDirectory`
//!   revalidation in actor-model — not something this task decides
//!   unilaterally).
//! - **`store: &impl MediaStore, origin: &ForwardedOrigin`.** design.md's
//!   Responsibilities prose names `MediaStore.public_url` as exactly how a
//!   local avatar/header media reference becomes a URL, but its literal
//!   Service Interface sketch has no parameter to carry a `MediaStore`/
//!   `ForwardedOrigin` through. `media/store.rs`'s own doc comment already
//!   establishes the precedent for this exact class of deviation (design.md
//!   sketch vs. the actually-implemented, proxy-aware `MediaStore::
//!   public_url(&self, key: &ObjectKey, origin: &ForwardedOrigin)`
//!   signature) — this module reuses that already-accepted primitive
//!   directly, generic over `impl MediaStore` exactly like
//!   `media::serializer::to_media_attachment` does, so no DB/HTTP call
//!   happens inside this module (`MediaStore::public_url` is a pure,
//!   synchronous string-formatting function, not an I/O call — see
//!   `store.rs`'s own trait doc comment).
//!
//! Every other parameter matches design.md's literal sketch exactly
//! (`actor`/`profile`/`remote`/`counts`/`emojis`), and no repository or
//! `PgPool` is threaded through anywhere in this module — matching
//! `media/serializer.rs`'s "no DB/HTTP calls inside the serializer itself"
//! discipline this task's brief points to as this crate's established
//! pattern.
//!
//! ## Emoji domain-collision safety (Requirement 1.4; the flagged concern)
//! tasks.md's Implementation Notes flag a real gap this task must resolve
//! deliberately, not silently: `CustomEmojiRepository::resolve_emojis(pool,
//! shortcodes)` (task 2.3, already implemented and reviewed) is
//! *intentionally* domain-blind by design.md's literal signature — it
//! matches `shortcode = ANY($1)` across every `domain`, so if the same
//! shortcode string exists as a `custom_emojis` row in more than one domain
//! (e.g. a local emoji and a same-named remote-federated one), calling it
//! returns *both* rows. `CustomEmojiView` (task 1.2, `model.rs`) carries no
//! `domain` field at all, so **no caller anywhere — this module, a future
//! `AccountService`, or anything else — can tell those rows apart from a
//! `Vec<CustomEmojiView>` alone.** This is not a layering problem solvable
//! by moving code around; it is a lost-information problem: the SQL row's
//! `domain` column is never carried into the returned Rust value in the
//! first place. Fixing that would require either adding `domain` to
//! `CustomEmojiView` (task 1.2's boundary) or adding a domain-scoped query
//! to `CustomEmojiRepository` (task 2.3's boundary) — both outside this
//! task's own boundary (`Boundary: AccountSerializer`), and the exact "初回
//! 実装は resolve_emojis を domain='' に絞る判断をしたがレビューで REJECTED"
//! precedent already recorded in tasks.md's own Implementation Notes for
//! task 2.3 warns against unilaterally changing that file's already-
//! reviewed, domain-blind behavior from a different task.
//!
//! Given that, this module does not attempt to guess which domain a
//! colliding shortcode belongs to. [`match_referenced_emojis`] takes
//! whatever `emojis: &[CustomEmojiView]` candidate slice its caller
//! supplies (design.md's literal parameter, unchanged) exactly as given,
//! and is deliberately defensive at the one point it actually matters —
//! where a shortcode referenced in `display_name`/`note` gets attached to
//! the output: it groups candidates by `shortcode`, and only emits an entry
//! when every candidate sharing that shortcode is a *literal, structural
//! duplicate* (same url/static_url/visible_in_picker/category — i.e. there
//! is genuinely only one distinct emoji under that shortcode, regardless of
//! how many identical rows happened to produce it). When two or more
//! *distinct* candidates share a shortcode (the actual domain-collision
//! case), that shortcode is **omitted from the account's `emojis` array**
//! and a `tracing::warn!` is emitted naming the shortcode and how many
//! distinct candidates collided — never an arbitrary pick, so this
//! serializer can never attach the *wrong* domain's emoji to an account.
//! This is "correct" in the sense that matters most: it never mismatches
//! (Requirement 1.4's whole purpose — emoji rendering — would be actively
//! misleading if wrong), even though it cannot always be *complete*, since
//! the domain information needed for completeness genuinely does not exist
//! in `CustomEmojiView` today. See this task's status report CONCERNS for
//! the same "who fixes model.rs/emoji_repository.rs" note as the
//! `created_at` gap above.
//!
//! ## `avatar_static`/`header_static`: no distinct derivative modeled yet
//! Real Mastodon's `avatar_static`/`header_static` differ from `avatar`/
//! `header` only for animated (GIF) avatars/headers, where `_static` points
//! at a non-animated preview frame. Neither `AccountProfile` (local:
//! `avatar_media`/`header_media` are a single `Option<Id>` each, resolved
//! through `MediaStore::public_url`'s single `ObjectKey::original`
//! variant — media-pipeline's `ObjectKey`/`MediaStore` has no "static
//! preview of an avatar" concept, only `Original`/`Small`) nor
//! `RemoteAccount` (`avatar_url`/`header_url` are each a single
//! `Option<String>`) carries a second, distinct still-frame URL. This
//! module therefore renders `avatar_static`/`header_static` identically to
//! `avatar`/`header` (and the same default URL when unset) — never null
//! either way (Requirement 1.5) — rather than fabricating a second URL this
//! crate has no data for.
//!
//! ## `group`/remote `discoverable`: no source field exists yet
//! `group` is hard-coded `false` for both local and remote accounts:
//! `crate::actor::model::ActorType` only distinguishes `Person`/`Service`
//! (BOT), and `RemoteAccount` (Requirement 7.2's explicit normalized-field
//! list: `acct`/`display_name`/`note`/`url`/`uri`/avatar/header/`fields`/
//! `bot`/`locked` — no `group`) carries no group-actor concept either, so
//! this MVP has no source data for it under either branch. Remote
//! `discoverable` is likewise hard-coded `false` (a safe, conservative
//! default, never claiming discoverability this crate has no data for) —
//! local `discoverable` does come from a real field
//! ([`AccountProfile::discoverable`]).
//!
//! ## Local `url`/`uri`: both render the same [`ActorUrls::actor_url`]
//! Requirement 1.2 only asks that both be "当該ローカルアクターの公開URL"
//! (the local actor's public URL) — this crate has no separate user-facing
//! web-profile route (e.g. a Mastodon-style `/@handle` page) distinct from
//! the ActivityPub actor URI `federation-core`'s [`ActorUrls::actor_url`]
//! already builds (`https://{domain}/users/{handle}`), so both `url` and
//! `uri` render that same value for a local account. A remote account's
//! `url`/`uri` are not ambiguous this way: [`RemoteAccount`] already carries
//! both a normalized web `url` and the ActivityPub `actor_uri` as two
//! distinct, already-resolved fields (Requirement 7.2), so those map
//! straight across with no default-URL construction needed.
//!
//! ## `role`: a fixed MVP placeholder (Requirement 2.2)
//! Requirement 2.2 only says CredentialAccount must include "少なくとも...
//! `role`", with no field list and no admin-role/permission model defined
//! anywhere in this codebase yet (this is a single-owner instance —
//! steering's "一人鯖" — with no roles/RBAC spec). Mirroring this same
//! spec's own already-written precedent for an equally underspecified,
//! structurally-required field
//! (`InstanceSerializer`'s `usage.users.active_month`, design.md: "MVP では
//! 固定値プレースホルダとする"), [`owner_role`] returns one fixed, Mastodon-
//! Role-shaped value (`id`/`name`/`color`/`permissions`/`highlighted`) —
//! deterministic, not a guess at a real permission model this codebase does
//! not have yet.

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::accounts::model::{
    AccountCounts, AccountProfile, AccountView, AccountViewFields, CustomEmojiView, ProfileField,
    RemoteAccount,
};
use crate::actor::model::ResolvedActor;
use crate::api::pagination::ForwardedOrigin;
use crate::domain::{Id, Visibility};
use crate::federation::urls::ActorUrls;
use crate::media::store::{MediaStore, ObjectKey};

/// JSON shape of one `fields`/`source.fields` entry (Requirement 1.1, 2.2).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountFieldJson {
    pub name: String,
    pub value: String,
    pub verified_at: Option<String>,
}

/// JSON shape of one `emojis` entry (Requirement 1.1, 9.2/9.4 — the same
/// representation `CustomEmojiSerializer`, task 3.4, will use for
/// `custom_emojis`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomEmojiJson {
    pub shortcode: String,
    pub url: String,
    pub static_url: String,
    pub visible_in_picker: bool,
    pub category: Option<String>,
}

/// The Mastodon-compatible `Role` shape CredentialAccount's `role` field
/// carries (Requirement 2.2). See this module's doc comment ("`role`: a
/// fixed MVP placeholder") for why every instance of this type is
/// identical, fixed content.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoleJson {
    pub id: String,
    pub name: String,
    pub color: String,
    pub permissions: String,
    pub highlighted: bool,
}

/// CredentialAccount's `source` object (Requirement 2.2: "少なくとも
/// `privacy` / `sensitive` / `language` / `note` / `fields` /
/// `follow_requests_count`").
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CredentialSourceJson {
    pub privacy: Visibility,
    pub sensitive: bool,
    pub language: Option<String>,
    pub note: String,
    pub fields: Vec<AccountFieldJson>,
    pub follow_requests_count: i64,
}

/// The Mastodon-compatible Account JSON contract (Requirement 1.1). Field
/// order matches Requirement 1.1's own listing. `avatar`/`avatar_static`/
/// `header`/`header_static` are plain `String` (never `Option`), mirroring
/// [`AccountView`]'s own non-optional avatar/header fields — Requirement
/// 1.5's "never null" invariant is enforced at the type level, not by a
/// runtime check here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountJson {
    pub id: Id,
    pub username: String,
    pub acct: String,
    pub display_name: String,
    pub locked: bool,
    pub bot: bool,
    pub discoverable: bool,
    pub group: bool,
    pub created_at: String,
    pub note: String,
    pub url: String,
    pub uri: String,
    pub avatar: String,
    pub avatar_static: String,
    pub header: String,
    pub header_static: String,
    pub followers_count: i64,
    pub following_count: i64,
    pub statuses_count: i64,
    pub last_status_at: Option<String>,
    pub emojis: Vec<CustomEmojiJson>,
    pub fields: Vec<AccountFieldJson>,
}

/// The Mastodon-compatible CredentialAccount JSON contract (Requirement
/// 2.2): every Account field (`#[serde(flatten)]`) plus `source` and
/// `role`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CredentialAccountJson {
    #[serde(flatten)]
    pub account: AccountJson,
    pub source: CredentialSourceJson,
    pub role: RoleJson,
}

/// Renders `when` as an RFC 3339 timestamp string (Requirement 1.1's
/// `created_at`/`last_status_at`, Mastodon's own ISO 8601 convention).
fn format_time(when: OffsetDateTime) -> String {
    when.format(&Rfc3339)
        .expect("a valid OffsetDateTime always formats as RFC 3339")
}

fn field_to_json(field: &ProfileField) -> AccountFieldJson {
    AccountFieldJson {
        name: field.name.clone(),
        value: field.value.clone(),
        verified_at: field.verified_at.map(format_time),
    }
}

fn emoji_to_json(emoji: &CustomEmojiView) -> CustomEmojiJson {
    CustomEmojiJson {
        shortcode: emoji.shortcode.clone(),
        url: emoji.url.clone(),
        static_url: emoji.static_url.clone(),
        visible_in_picker: emoji.visible_in_picker,
        category: emoji.category.clone(),
    }
}

/// Projects an [`AccountView`] (already local/remote-unified, Requirements
/// 1.2, 1.3) into [`AccountJson`] — a pure, total mapping with no branching
/// on local-vs-remote left to do (that branching already happened when the
/// [`AccountView`] was constructed, by [`AccountSerializer::view_local`]/
/// [`AccountSerializer::view_remote`]).
pub fn to_account_json(view: &AccountView) -> AccountJson {
    AccountJson {
        id: view.id(),
        username: view.username.clone(),
        acct: view.acct.as_str(),
        display_name: view.display_name.clone(),
        locked: view.locked,
        bot: view.bot,
        discoverable: view.discoverable,
        group: view.group,
        created_at: format_time(view.created_at),
        note: view.note.clone(),
        url: view.url.clone(),
        uri: view.uri.clone(),
        avatar: view.avatar.clone(),
        avatar_static: view.avatar_static.clone(),
        header: view.header.clone(),
        header_static: view.header_static.clone(),
        followers_count: view.followers_count,
        following_count: view.following_count,
        statuses_count: view.statuses_count,
        last_status_at: view.last_status_at.map(format_time),
        emojis: view.emojis.iter().map(emoji_to_json).collect(),
        fields: view.fields.iter().map(field_to_json).collect(),
    }
}

/// [`to_account_json`], converted to a plain [`serde_json::Value`] (matching
/// `media/serializer.rs::to_json`'s convention).
pub fn account_to_json(view: &AccountView) -> Value {
    serde_json::to_value(to_account_json(view)).expect("AccountJson always serializes to JSON")
}

/// The fixed MVP `role` value every CredentialAccount carries. See this
/// module's doc comment ("`role`: a fixed MVP placeholder").
fn owner_role() -> RoleJson {
    RoleJson {
        id: "owner".to_string(),
        name: "Owner".to_string(),
        color: String::new(),
        permissions: "65536".to_string(),
        highlighted: true,
    }
}

/// Extracts every distinct `:shortcode:`-delimited token referenced in
/// `display_name`/`note` (Requirement 1.4), in first-appearance order, each
/// appearing once even if referenced multiple times. A shortcode's allowed
/// characters mirror the conventional ActivityPub/Mastodon custom-emoji
/// charset (ASCII letters, digits, underscore, plus, hyphen); no crate in
/// this workspace provides a shortcode grammar to reuse, so this is a small,
/// self-contained scanner rather than a regex dependency this crate does not
/// otherwise need.
fn extract_shortcodes(display_name: &str, note: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for text in [display_name, note] {
        for shortcode in shortcodes_in(text) {
            if seen.insert(shortcode.clone()) {
                result.push(shortcode);
            }
        }
    }
    result
}

fn is_shortcode_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '+' || c == '-'
}

/// Scans `text` for `:shortcode:`-delimited tokens via a sliding window over
/// every `:` position: a candidate span between two consecutive `:`
/// positions is accepted (and the scan jumps past both delimiters) only when
/// it is non-empty and entirely [`is_shortcode_char`]; otherwise the window
/// slides forward by one `:` position so a later, valid pair is not missed
/// (e.g. `"cost: $5 :blob:"` must still find `"blob"` despite the earlier,
/// unrelated `:` after `"cost"`). Back-to-back shortcodes sharing a `:`
/// (`":a::b:"`) are handled correctly by this same window, since the shared
/// middle `:` naturally serves as both the first token's closing delimiter
/// and the second token's opening delimiter.
fn shortcodes_in(text: &str) -> Vec<String> {
    let colon_positions: Vec<usize> = text
        .char_indices()
        .filter(|(_, c)| *c == ':')
        .map(|(i, _)| i)
        .collect();

    let mut result = Vec::new();
    let mut i = 0;
    while i + 1 < colon_positions.len() {
        let start = colon_positions[i];
        let end = colon_positions[i + 1];
        let inner = &text[start + 1..end];
        if !inner.is_empty() && inner.chars().all(is_shortcode_char) {
            result.push(inner.to_string());
            i += 2;
        } else {
            i += 1;
        }
    }
    result
}

/// Matches `display_name`/`note`'s referenced shortcodes against `candidates`
/// (Requirement 1.4), never emitting a shortcode whose candidates disagree
/// across domains. See this module's doc comment ("Emoji domain-collision
/// safety") for the full reasoning; this is where that guard actually lives.
fn match_referenced_emojis(
    display_name: &str,
    note: &str,
    candidates: &[CustomEmojiView],
) -> Vec<CustomEmojiView> {
    let referenced = extract_shortcodes(display_name, note);
    if referenced.is_empty() {
        return Vec::new();
    }

    let mut grouped: HashMap<&str, Vec<&CustomEmojiView>> = HashMap::new();
    for candidate in candidates {
        grouped
            .entry(candidate.shortcode.as_str())
            .or_default()
            .push(candidate);
    }

    referenced
        .into_iter()
        .filter_map(|shortcode| {
            let group = grouped.get(shortcode.as_str())?;
            let mut rows = group.iter();
            let first = *rows.next()?;
            if rows.all(|other| *other == first) {
                Some(first.clone())
            } else {
                tracing::warn!(
                    shortcode = %shortcode,
                    candidate_count = group.len(),
                    "custom emoji shortcode resolved to more than one distinct row (likely a \
                     local/remote-domain collision); CustomEmojiView carries no domain field to \
                     disambiguate, so this shortcode is omitted from the account's emojis rather \
                     than guessing which row belongs to this account"
                );
                None
            }
        })
        .collect()
}

/// Resolves `media_id` to its public URL via `store`/`origin`, or `default`
/// when unset (Requirement 1.5: never null). Returns the same URL for both
/// the primary and `_static` fields — see this module's doc comment
/// (`avatar_static`/`header_static`) for why.
fn local_media_urls(
    media_id: Option<Id>,
    default: &str,
    store: &impl MediaStore,
    origin: &ForwardedOrigin,
) -> (String, String) {
    let resolved = media_id
        .map(|id| store.public_url(&ObjectKey::original(id), origin))
        .unwrap_or_else(|| default.to_string());
    (resolved.clone(), resolved)
}

/// Resolves an already-normalized remote avatar/header URL, or `default`
/// when unset (Requirement 1.5). Same `_static` duplication rationale as
/// [`local_media_urls`].
fn remote_media_urls(url: Option<&str>, default: &str) -> (String, String) {
    let resolved = url
        .map(str::to_string)
        .unwrap_or_else(|| default.to_string());
    (resolved.clone(), resolved)
}

/// Maps a local actor (`ResolvedActor` + `AccountProfile`) and a remote
/// account (`RemoteAccount`) onto the single, unified Account/
/// CredentialAccount JSON contract (Requirements 1.1-1.5, 2.2). See this
/// module's doc comment for the full reasoning behind every deviation from
/// design.md's literal Service Interface sketch.
#[derive(Debug, Clone)]
pub struct AccountSerializer {
    urls: ActorUrls,
    default_avatar_url: String,
    default_header_url: String,
}

impl AccountSerializer {
    /// Builds a serializer for `domain` (this instance's own bare server
    /// domain, e.g. `"example.social"` — matching [`ActorUrls::new`]'s own
    /// shape). `default_avatar_url`/`default_header_url` (Requirement 1.5)
    /// are derived from the same domain, conventionally shaped
    /// (`/avatars/original/missing.png` / `/headers/original/missing.png`)
    /// — no prior art for a default-image URL exists anywhere else in this
    /// crate to reuse.
    pub fn new(domain: impl Into<String>) -> Self {
        let domain = domain.into();
        let default_avatar_url = format!("https://{domain}/avatars/original/missing.png");
        let default_header_url = format!("https://{domain}/headers/original/missing.png");
        AccountSerializer {
            urls: ActorUrls::new(domain),
            default_avatar_url,
            default_header_url,
        }
    }

    /// Builds the unified [`AccountView`] for a local actor (Requirement
    /// 1.2). `emojis` is the caller-supplied emoji-candidate slice (design.md's
    /// literal parameter) — see this module's doc comment ("Emoji
    /// domain-collision safety") for how referenced shortcodes are matched
    /// against it safely.
    #[allow(clippy::too_many_arguments)]
    pub fn view_local(
        &self,
        actor: &ResolvedActor,
        profile: &AccountProfile,
        created_at: OffsetDateTime,
        counts: &AccountCounts,
        store: &impl MediaStore,
        origin: &ForwardedOrigin,
        emojis: &[CustomEmojiView],
    ) -> AccountView {
        let (avatar, avatar_static) = local_media_urls(
            profile.avatar_media,
            &self.default_avatar_url,
            store,
            origin,
        );
        let (header, header_static) = local_media_urls(
            profile.header_media,
            &self.default_header_url,
            store,
            origin,
        );
        let matched_emojis = match_referenced_emojis(&profile.display_name, &profile.note, emojis);
        let handle = actor.handle.as_str().to_string();
        let actor_url = self.urls.actor_url(&actor.handle);

        AccountView::local(
            actor.id,
            handle.clone(),
            AccountViewFields {
                username: handle,
                display_name: profile.display_name.clone(),
                locked: profile.locked,
                bot: profile.bot,
                discoverable: profile.discoverable,
                // No group-actor concept exists in actor-model yet — see
                // this module's doc comment ("`group`/remote
                // `discoverable`").
                group: false,
                created_at,
                note: profile.note.clone(),
                // See this module's doc comment ("Local `url`/`uri`").
                url: actor_url.clone(),
                uri: actor_url,
                avatar,
                avatar_static,
                header,
                header_static,
                followers_count: counts.followers,
                following_count: counts.following,
                statuses_count: counts.statuses,
                last_status_at: counts.last_status_at,
                emojis: matched_emojis,
                fields: profile.fields.clone(),
            },
        )
    }

    /// Builds the unified [`AccountView`] for a [`RemoteAccount`]
    /// (Requirement 1.3). Same `emojis`-matching discipline as
    /// [`Self::view_local`].
    pub fn view_remote(
        &self,
        remote: &RemoteAccount,
        counts: &AccountCounts,
        emojis: &[CustomEmojiView],
    ) -> AccountView {
        let (avatar, avatar_static) =
            remote_media_urls(remote.avatar_url.as_deref(), &self.default_avatar_url);
        let (header, header_static) =
            remote_media_urls(remote.header_url.as_deref(), &self.default_header_url);
        let matched_emojis = match_referenced_emojis(&remote.display_name, &remote.note, emojis);

        AccountView::remote(
            remote.id,
            remote.domain.clone(),
            AccountViewFields {
                username: remote.username.clone(),
                display_name: remote.display_name.clone(),
                locked: remote.locked,
                bot: remote.bot,
                // No `discoverable` field on `RemoteAccount` — see this
                // module's doc comment ("`group`/remote `discoverable`").
                discoverable: false,
                group: false,
                created_at: remote.fetched_at,
                note: remote.note.clone(),
                url: remote.url.clone(),
                uri: remote.actor_uri.clone(),
                avatar,
                avatar_static,
                header,
                header_static,
                followers_count: counts.followers,
                following_count: counts.following,
                statuses_count: counts.statuses,
                last_status_at: counts.last_status_at,
                emojis: matched_emojis,
                fields: remote.fields.clone(),
            },
        )
    }

    /// Builds the Account JSON for a local actor (Requirement 1.1, 1.2, 1.5).
    #[allow(clippy::too_many_arguments)]
    pub fn build_account_local(
        &self,
        actor: &ResolvedActor,
        profile: &AccountProfile,
        created_at: OffsetDateTime,
        counts: &AccountCounts,
        store: &impl MediaStore,
        origin: &ForwardedOrigin,
        emojis: &[CustomEmojiView],
    ) -> Value {
        account_to_json(&self.view_local(actor, profile, created_at, counts, store, origin, emojis))
    }

    /// Builds the Account JSON for a remote account (Requirement 1.1, 1.3,
    /// 1.5).
    pub fn build_account_remote(
        &self,
        remote: &RemoteAccount,
        counts: &AccountCounts,
        emojis: &[CustomEmojiView],
    ) -> Value {
        account_to_json(&self.view_remote(remote, counts, emojis))
    }

    /// Builds the CredentialAccount JSON for the authenticated local actor
    /// (Requirement 2.2): every Account field plus `source`/`role`.
    /// CredentialAccount is local-only (design.md: "CredentialAccount は
    /// ローカルのみで生成") — there is no `build_credential_account_remote`.
    #[allow(clippy::too_many_arguments)]
    pub fn build_credential_account(
        &self,
        actor: &ResolvedActor,
        profile: &AccountProfile,
        created_at: OffsetDateTime,
        counts: &AccountCounts,
        store: &impl MediaStore,
        origin: &ForwardedOrigin,
        emojis: &[CustomEmojiView],
    ) -> Value {
        let view = self.view_local(actor, profile, created_at, counts, store, origin, emojis);
        let account = to_account_json(&view);
        let source = CredentialSourceJson {
            privacy: profile.source.privacy,
            sensitive: profile.source.sensitive,
            language: profile.source.language.clone(),
            note: profile.source.note.clone(),
            fields: profile.source.fields.iter().map(field_to_json).collect(),
            follow_requests_count: profile.source.follow_requests_count,
        };
        let credential = CredentialAccountJson {
            account,
            source,
            role: owner_role(),
        };
        serde_json::to_value(credential).expect("CredentialAccountJson always serializes to JSON")
    }
}
