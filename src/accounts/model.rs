//! Accounts domain types (`model` component, design.md "Accounts Domain /
//! ドメイン層" -> `model`, Requirements 1.1, 1.2, 1.3, 2.2, 5.2, 6.1, 7.2,
//! 8.1, 9.2; task 1.2, `Boundary: model`).
//!
//! Scope: this module owns exactly the domain value types design.md's
//! `model` component names — [`AccountView`], [`ProfileField`],
//! [`CredentialSource`], [`AccountProfile`], [`ProfilePatch`],
//! [`RemoteAccount`], [`CustomEmojiView`], [`RelationshipView`],
//! [`AccountCounts`], and [`InstanceSettings`] — plus [`Acct`], a small
//! helper type this module introduces to carry [`AccountView::acct`]'s
//! local/remote rendering discipline (see "Why `Acct` exists" below).
//! [`AccountRef`]/[`Visibility`] are *not* redefined here: both are
//! imported from `crate::domain` (core-runtime's canonical shared
//! primitives module), per design.md's model "Outbound" dependency list
//! and the task's explicit instruction.
//!
//! No persistence (`AccountProfileRepository` / `RemoteAccountRepository` /
//! `CustomEmojiRepository` / `InstanceSettingsRepository`, task 2.x), no
//! delegation ports (`AccountStatusesProvider` / `RelationshipStateProvider`
//! / `AccountCountsProvider`, task 1.3), no serialization to Mastodon JSON
//! (`AccountSerializer` / `RelationshipSerializer` / `InstanceSerializer` /
//! `CustomEmojiSerializer`, task 3.x), no business logic (`AccountService` /
//! `InstanceService` / `CustomEmojiService`), and no HTTP surface
//! (`AccountsEndpoints`) live here — those consume the types defined in this
//! module but are out of scope for task 1.2 (`Boundary: model`). In
//! particular, this module has no dependency on `actor-model`'s
//! `ResolvedActor`/`Handle` types: design.md's model component only lists
//! core-runtime's `Id`/time types and the `domain` module's canonical
//! shared types as Outbound dependencies — building an [`AccountView`] out
//! of a `ResolvedActor` (local) or a [`RemoteAccount`] (remote) is
//! `AccountSerializer`'s job (task 3.1), not this module's.
//!
//! ## Why `Acct` exists
//! Requirement 1.2/1.3 and design.md's model Responsibilities both require
//! that the local/remote `acct` string discipline (bare handle for local,
//! `username@domain` for remote) be expressed "by type"
//! (`型で区別可能にする`), not as "a runtime string-formatting afterthought"
//! (task brief). A plain `String` field fed by ad hoc `format!` calls at the
//! call site cannot express that distinction at all — nothing would stop a
//! caller from accidentally building a local account with a `@domain`
//! suffix, or a remote account with a bare handle. [`Acct`] closes that gap:
//! its two variants each carry exactly the data their own discipline needs
//! (`Local` holds only a handle; `Remote` holds `username` *and* `domain`
//! separately, not a pre-joined string), and [`Acct::as_str`] is the single
//! place the `user@domain` join happens, so every [`AccountView`] carries
//! its local/remote-ness as a type-level discriminant rather than an
//! incidental string shape.
//!
//! [`AccountView`] additionally carries an [`AccountRef`] (not just
//! [`Acct`]): [`AccountRef`] is the canonical, cross-spec local/remote
//! identity discriminator (an [`Id`] tagged `Local`/`Remote`, owned by
//! core-runtime's `domain` module and reused by every downstream spec's
//! delegation ports — see design.md's ports Service Interface, task 1.3);
//! [`Acct`] is the sibling *string-rendering* discipline for the same
//! local/remote distinction. Keeping both lets [`AccountView::account_ref`]
//! answer "which entity is this a view of, for cross-spec reference
//! purposes" while [`AccountView::acct`] independently answers "what string
//! does this render as in the Account JSON contract" — the two are
//! constructed together and are expected to always agree in variant
//! (enforced by [`AccountView::local`]/[`AccountView::remote`]'s
//! constructors, not by a runtime assertion elsewhere).

use time::OffsetDateTime;

use crate::domain::{AccountRef, Id, Visibility};

/// The Account JSON contract's `acct` field, with the local/remote
/// rendering discipline expressed as a type-level discriminant rather than
/// a runtime string-formatting afterthought (Requirements 1.2, 1.3). See
/// this module's doc comment ("Why `Acct` exists") for the full rationale.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Acct {
    /// A local account's `acct`: a bare handle, no domain part (Requirement
    /// 1.2).
    Local(String),
    /// A remote account's `acct`: `username`/`domain` held separately (not
    /// pre-joined), so the `user@domain` join happens in exactly one place
    /// ([`Acct::as_str`]) rather than being reconstructed ad hoc at each
    /// call site (Requirement 1.3).
    Remote { username: String, domain: String },
}

impl Acct {
    /// Builds a local `acct` from a bare handle.
    pub fn local(handle: impl Into<String>) -> Self {
        Acct::Local(handle.into())
    }

    /// Builds a remote `acct` from its `username`/`domain` parts.
    pub fn remote(username: impl Into<String>, domain: impl Into<String>) -> Self {
        Acct::Remote {
            username: username.into(),
            domain: domain.into(),
        }
    }

    /// Renders this `acct` per Mastodon's local/remote discipline: the bare
    /// handle for [`Acct::Local`], `username@domain` for [`Acct::Remote`]
    /// (Requirements 1.2, 1.3).
    pub fn as_str(&self) -> String {
        match self {
            Acct::Local(handle) => handle.clone(),
            Acct::Remote { username, domain } => format!("{username}@{domain}"),
        }
    }

    /// True for [`Acct::Local`].
    pub fn is_local(&self) -> bool {
        matches!(self, Acct::Local(_))
    }

    /// True for [`Acct::Remote`].
    pub fn is_remote(&self) -> bool {
        matches!(self, Acct::Remote { .. })
    }
}

/// One entry of an [`AccountProfile`]'s or [`CredentialSource`]'s `fields`
/// array (design.md model excerpt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileField {
    pub name: String,
    pub value: String,
    pub verified_at: Option<OffsetDateTime>,
}

/// The `source` object CredentialAccount adds on top of every Account field
/// (Requirement 2.2): "少なくとも `privacy` / `sensitive` / `language` /
/// `note` / `fields` / `follow_requests_count`". `privacy` reuses
/// core-runtime's canonical [`Visibility`] rather than redefining a parallel
/// enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSource {
    pub privacy: Visibility,
    pub sensitive: bool,
    pub language: Option<String>,
    pub note: String,
    pub fields: Vec<ProfileField>,
    pub follow_requests_count: i64,
}

/// A local actor's profile extension — `display_name`/`note` (Account/
/// CredentialAccount's same-named fields' supply source, Requirement 1.1,
/// 2.2), avatar/header media references, profile fields, `locked`/`bot`/
/// `discoverable`, and the [`CredentialSource`] defaults (Requirement 6.1).
///
/// `avatar_media`/`header_media` are `Option<Id>` (a logical, non-`REFERENCES`
/// reference to media-pipeline's `media.id`, mirroring
/// `migrations/0006_accounts.sql`'s `account_profiles.avatar_media_id`/
/// `header_media_id` columns) — `None` means "no avatar/header set", which
/// [`AccountSerializer`] (task 3.1, out of this task's boundary) is
/// responsible for resolving to a non-null default image URL (Requirement
/// 1.5). This module does not resolve that default itself: doing so would
/// require a `MediaStore`/default-URL dependency this model layer does not
/// have (design.md's model Outbound dependency list is core-runtime `Id`/
/// time types and the `domain` canonical types only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountProfile {
    pub actor_id: Id,
    pub display_name: String,
    pub note: String,
    pub avatar_media: Option<Id>,
    pub header_media: Option<Id>,
    pub fields: Vec<ProfileField>,
    pub locked: bool,
    pub bot: bool,
    pub discoverable: bool,
    pub source: CredentialSource,
}

/// `update_credentials`' item-by-item partial-update input (task text:
/// "項目別部分更新入力、`None` は変更なし"; Requirements 6.1, 6.5).
///
/// Every field is `Option<T>` (or, for `avatar_media`/`header_media`/
/// `source_language` — themselves already `Option<Id>`/`Option<String>`
/// fields on [`AccountProfile`]/[`CredentialSource`] — the doubled
/// `Option<Option<T>>`): the *outer* `None` means "this item was not
/// present in the update request, leave it unchanged"; for the doubled
/// fields, an outer `Some` carrying an inner `None` means "clear this item
/// to unset" (e.g. remove the avatar), distinct from "leave unchanged".
/// [`ProfilePatch`] carries no direct application logic itself (applying a
/// patch to a stored [`AccountProfile`] is `AccountProfileRepository::
/// upsert_profile`'s job, task 2.1, out of this task's boundary) — this
/// type is deliberately only the input shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfilePatch {
    pub display_name: Option<String>,
    pub note: Option<String>,
    pub avatar_media: Option<Option<Id>>,
    pub header_media: Option<Option<Id>>,
    pub fields: Option<Vec<ProfileField>>,
    pub locked: Option<bool>,
    pub bot: Option<bool>,
    pub discoverable: Option<bool>,
    pub source_privacy: Option<Visibility>,
    pub source_sensitive: Option<bool>,
    pub source_language: Option<Option<String>>,
}

impl ProfilePatch {
    /// True when every field is `None` — i.e. this patch, if applied, would
    /// change nothing (task text: "`None` は変更なし"). `#[derive(Default)]`
    /// already gives an all-`None` value via [`ProfilePatch::default`]; this
    /// method is the readable predicate downstream callers (e.g.
    /// `AccountService::update_credentials`, task 3.x, out of this task's
    /// boundary) can use instead of comparing against a freshly constructed
    /// default value.
    pub fn changes_nothing(&self) -> bool {
        self == &ProfilePatch::default()
    }
}

/// A normalized, cached remote account (Requirement 7.2): the fields an
/// ActivityPub actor document is normalized into. Field shapes mirror
/// `migrations/0006_accounts.sql`'s `remote_accounts` table exactly
/// (`id`/`actor_uri`/`username`/`domain`/`display_name`/`note`/`url`/
/// `avatar_url`/`header_url`/`fields`/`bot`/`locked`/`fetched_at`).
///
/// `username`/`domain` are held separately (matching [`Acct::Remote`]'s own
/// shape) rather than as a pre-joined `acct` string, so building an
/// [`Acct::remote`] from a [`RemoteAccount`] never needs to split a string
/// back apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAccount {
    pub id: Id,
    pub actor_uri: String,
    pub username: String,
    pub domain: String,
    pub display_name: String,
    pub note: String,
    pub url: String,
    pub avatar_url: Option<String>,
    pub header_url: Option<String>,
    pub fields: Vec<ProfileField>,
    pub bot: bool,
    pub locked: bool,
    pub fetched_at: OffsetDateTime,
}

/// The read-only custom-emoji entity (Requirement 9.2: "少なくとも
/// `shortcode` / `url` / `static_url` / `visible_in_picker` / `category`"),
/// shared verbatim between `custom_emojis`(read) and an [`AccountView`]'s
/// `emojis` array (Requirement 9.4 — one representation, not two).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomEmojiView {
    pub shortcode: String,
    pub url: String,
    pub static_url: String,
    pub visible_in_picker: bool,
    pub category: Option<String>,
}

/// The `relationships` entity (Requirement 5.2's full flag list). The
/// "関係なし" default (Requirement 5.4: all booleans `false`, all counts
/// `0`, `note` empty) is not encoded as a `Default` impl here — the default
/// *value* also needs a caller-supplied `id` (the target account's id,
/// which has no sensible default), so building that default is the
/// delegation port layer's job (`NoRelationshipProvider`, task 1.3, out of
/// this task's boundary), not this type's own responsibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipView {
    pub id: Id,
    pub following: bool,
    pub showing_reblogs: bool,
    pub notifying: bool,
    pub languages: Vec<String>,
    pub followed_by: bool,
    pub blocking: bool,
    pub blocked_by: bool,
    pub muting: bool,
    pub muting_notifications: bool,
    pub requested: bool,
    pub requested_by: bool,
    pub domain_blocking: bool,
    pub endorsed: bool,
    pub note: String,
}

/// The counts an [`AccountView`] needs (`followers_count`/`following_count`/
/// `statuses_count`/`last_status_at`), sourced from `AccountCountsProvider`
/// (task 1.3, out of this task's boundary) — this type is just the value
/// shape those counts flow through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountCounts {
    pub followers: i64,
    pub following: i64,
    pub statuses: i64,
    pub last_status_at: Option<OffsetDateTime>,
}

/// Instance v2's operationally-variable settings (Requirement 8.1, 8.2):
/// `title`/`description`/`contact_email`/`contact_account_id`/`rules`/
/// `registrations_*`/`thumbnail`/`languages`. Deliberately excludes
/// `version`/`source_url`/`usage` — design.md's model doc is explicit that
/// those are supplied by `InstanceSerializer` from build-time constants/a
/// fixed placeholder, never persisted here (Requirement 8.1's fields not
/// backed by `instance_settings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSettings {
    pub title: String,
    pub description: String,
    pub contact_email: String,
    pub contact_account_id: Option<Id>,
    pub rules: Vec<String>,
    pub registrations_enabled: bool,
    pub registrations_approval_required: bool,
    pub registrations_message: Option<String>,
    pub thumbnail: Option<String>,
    pub languages: Vec<String>,
}

/// The Account JSON contract's logical representation (design.md model
/// Responsibilities: "Account 契約の論理表現（ローカル/リモート両入力の正規化
/// 先）") — the normalization target both a local actor (`ResolvedActor` +
/// [`AccountProfile`]) and a [`RemoteAccount`] converge onto before
/// `AccountSerializer` (task 3.1, out of this task's boundary) turns one
/// into Mastodon JSON.
///
/// Carries every field Requirement 1.1 lists as required on the Account
/// entity: `id`/`username`/`acct`/`display_name`/`locked`/`bot`/
/// `discoverable`/`group`/`created_at`/`note`/`url`/`uri`/`avatar`/
/// `avatar_static`/`header`/`header_static`/`followers_count`/
/// `following_count`/`statuses_count`/`last_status_at`/`emojis`/`fields`.
///
/// `avatar`/`avatar_static`/`header`/`header_static` are plain `String`
/// (not `Option<String>`): Requirement 1.5 requires these never be null in
/// the JSON contract, and by the time an [`AccountView`] exists that
/// invariant must already hold (a default image URL substituted upstream,
/// by `AccountSerializer`) — making the field itself non-optional means an
/// `AccountView` with a null avatar/header cannot even be constructed,
/// rather than relying on a runtime check to catch it later.
///
/// `id` is not a separate field: it is always recoverable from
/// [`AccountView::account_ref`] (an [`AccountRef`] already wraps exactly one
/// [`Id`]), so storing it twice would just be two sources of truth for the
/// same value — see [`AccountView::id`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountView {
    pub account_ref: AccountRef,
    pub username: String,
    pub acct: Acct,
    pub display_name: String,
    pub locked: bool,
    pub bot: bool,
    pub discoverable: bool,
    pub group: bool,
    pub created_at: OffsetDateTime,
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
    pub last_status_at: Option<OffsetDateTime>,
    pub emojis: Vec<CustomEmojiView>,
    pub fields: Vec<ProfileField>,
}

/// Everything [`AccountView`] needs beyond its own `account_ref`/`acct`
/// (which [`AccountView::local`]/[`AccountView::remote`] supply themselves,
/// keeping the two always in agreement — see this module's doc comment).
/// Grouped into one struct rather than ~18 individual constructor
/// parameters purely for readability at call sites; it carries no behavior
/// of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountViewFields {
    pub username: String,
    pub display_name: String,
    pub locked: bool,
    pub bot: bool,
    pub discoverable: bool,
    pub group: bool,
    pub created_at: OffsetDateTime,
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
    pub last_status_at: Option<OffsetDateTime>,
    pub emojis: Vec<CustomEmojiView>,
    pub fields: Vec<ProfileField>,
}

impl AccountView {
    /// Builds a local [`AccountView`]: `account_ref` is
    /// `AccountRef::Local(id)` and `acct` is `Acct::Local(handle)` — the two
    /// local/remote discriminators are set together by construction, so an
    /// `AccountView` combining a local `account_ref` with a remote-shaped
    /// `acct` (or vice versa) cannot arise from this constructor (Requirement
    /// 1.2).
    pub fn local(id: Id, handle: impl Into<String>, fields: AccountViewFields) -> Self {
        Self::build(AccountRef::Local(id), Acct::local(handle), fields)
    }

    /// Builds a remote [`AccountView`]: `account_ref` is
    /// `AccountRef::Remote(id)` and `acct` is `Acct::Remote{ username,
    /// domain }`, built from the same `username` this view's own `username`
    /// field carries — matching [`RemoteAccount`]'s "`username`/`domain`
    /// held separately" shape (Requirement 1.3).
    pub fn remote(id: Id, domain: impl Into<String>, fields: AccountViewFields) -> Self {
        let acct = Acct::remote(fields.username.clone(), domain);
        Self::build(AccountRef::Remote(id), acct, fields)
    }

    fn build(account_ref: AccountRef, acct: Acct, fields: AccountViewFields) -> Self {
        let AccountViewFields {
            username,
            display_name,
            locked,
            bot,
            discoverable,
            group,
            created_at,
            note,
            url,
            uri,
            avatar,
            avatar_static,
            header,
            header_static,
            followers_count,
            following_count,
            statuses_count,
            last_status_at,
            emojis,
            fields,
        } = fields;
        AccountView {
            account_ref,
            username,
            acct,
            display_name,
            locked,
            bot,
            discoverable,
            group,
            created_at,
            note,
            url,
            uri,
            avatar,
            avatar_static,
            header,
            header_static,
            followers_count,
            following_count,
            statuses_count,
            last_status_at,
            emojis,
            fields,
        }
    }

    /// Recovers this view's [`Id`] from [`AccountView::account_ref`] (see
    /// this type's own doc comment for why `id` is not stored as a separate
    /// field).
    pub fn id(&self) -> Id {
        match self.account_ref {
            AccountRef::Local(id) => id,
            AccountRef::Remote(id) => id,
        }
    }

    /// True when this view was built by [`AccountView::local`] — i.e. both
    /// `account_ref` and `acct` agree on being local.
    pub fn is_local(&self) -> bool {
        matches!(self.account_ref, AccountRef::Local(_))
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    fn sample_fields(username: &str) -> AccountViewFields {
        AccountViewFields {
            username: username.to_string(),
            display_name: "Sample".to_string(),
            locked: false,
            bot: false,
            discoverable: true,
            group: false,
            created_at: datetime!(2026-01-01 00:00:00 UTC),
            note: "hello".to_string(),
            url: "https://example.test/@sample".to_string(),
            uri: "https://example.test/actors/sample".to_string(),
            avatar: "https://example.test/avatars/default.png".to_string(),
            avatar_static: "https://example.test/avatars/default.png".to_string(),
            header: "https://example.test/headers/default.png".to_string(),
            header_static: "https://example.test/headers/default.png".to_string(),
            followers_count: 0,
            following_count: 0,
            statuses_count: 0,
            last_status_at: None,
            emojis: Vec::new(),
            fields: Vec::new(),
        }
    }

    #[test]
    fn acct_local_renders_as_a_bare_handle() {
        let acct = Acct::local("alice");
        assert_eq!(acct.as_str(), "alice");
        assert!(acct.is_local());
        assert!(!acct.is_remote());
    }

    #[test]
    fn acct_remote_renders_as_username_at_domain() {
        let acct = Acct::remote("alice", "remote.example");
        assert_eq!(acct.as_str(), "alice@remote.example");
        assert!(acct.is_remote());
        assert!(!acct.is_local());
    }

    #[test]
    fn acct_local_and_remote_with_the_same_username_render_differently() {
        // The crux of Requirements 1.2/1.3: identical usernames must not
        // collapse to the same `acct` string once one is local and the
        // other remote.
        let local = Acct::local("alice");
        let remote = Acct::remote("alice", "remote.example");
        assert_ne!(local.as_str(), remote.as_str());
        assert!(!local.as_str().contains('@'));
        assert!(remote.as_str().contains('@'));
    }

    #[test]
    fn account_view_local_and_remote_acct_discipline_differs_for_the_same_username() {
        let local = AccountView::local(Id::from_i64(1), "alice", sample_fields("alice"));
        let remote = AccountView::remote(Id::from_i64(2), "remote.example", sample_fields("alice"));

        assert_eq!(local.acct.as_str(), "alice");
        assert_eq!(remote.acct.as_str(), "alice@remote.example");
        assert_ne!(local.acct, remote.acct);
        assert!(local.is_local());
        assert!(!remote.is_local());
    }

    #[test]
    fn account_view_account_ref_and_acct_variant_always_agree() {
        let local = AccountView::local(Id::from_i64(10), "bob", sample_fields("bob"));
        assert!(matches!(local.account_ref, AccountRef::Local(_)));
        assert!(local.acct.is_local());

        let remote = AccountView::remote(Id::from_i64(11), "remote.example", sample_fields("bob"));
        assert!(matches!(remote.account_ref, AccountRef::Remote(_)));
        assert!(remote.acct.is_remote());
    }

    #[test]
    fn account_view_id_is_recovered_from_account_ref_not_duplicated() {
        let id = Id::from_i64(99);
        let view = AccountView::local(id, "carol", sample_fields("carol"));
        assert_eq!(view.id(), id);
    }

    #[test]
    fn account_view_carries_every_required_account_field() {
        // Exhaustive destructuring (no `..`): fails to compile if a
        // Requirement-1.1-mandated field were ever removed from
        // `AccountView`, structurally proving the required-field set at the
        // type level (mirrors `src/media/model.rs`'s identical precedent
        // for `Media::actor_id`).
        let view = AccountView::local(Id::from_i64(1), "dave", sample_fields("dave"));
        let AccountView {
            account_ref: _,
            username,
            acct: _,
            display_name,
            locked,
            bot,
            discoverable,
            group,
            created_at: _,
            note,
            url,
            uri,
            avatar,
            avatar_static,
            header,
            header_static,
            followers_count,
            following_count,
            statuses_count,
            last_status_at,
            emojis,
            fields,
        } = view;
        assert_eq!(username, "dave");
        assert_eq!(display_name, "Sample");
        assert!(!locked);
        assert!(!bot);
        assert!(discoverable);
        assert!(!group);
        assert_eq!(note, "hello");
        assert!(url.starts_with("https://"));
        assert!(uri.starts_with("https://"));
        assert!(!avatar.is_empty());
        assert!(!avatar_static.is_empty());
        assert!(!header.is_empty());
        assert!(!header_static.is_empty());
        assert_eq!(followers_count, 0);
        assert_eq!(following_count, 0);
        assert_eq!(statuses_count, 0);
        assert!(last_status_at.is_none());
        assert!(emojis.is_empty());
        assert!(fields.is_empty());
    }

    #[test]
    fn profile_patch_default_changes_nothing() {
        let patch = ProfilePatch::default();
        assert!(patch.changes_nothing());
        assert!(patch.display_name.is_none());
        assert!(patch.note.is_none());
        assert!(patch.avatar_media.is_none());
        assert!(patch.header_media.is_none());
        assert!(patch.fields.is_none());
        assert!(patch.locked.is_none());
        assert!(patch.bot.is_none());
        assert!(patch.discoverable.is_none());
        assert!(patch.source_privacy.is_none());
        assert!(patch.source_sensitive.is_none());
        assert!(patch.source_language.is_none());
    }

    #[test]
    fn profile_patch_with_any_field_set_no_longer_changes_nothing() {
        let mut patch = ProfilePatch::default();
        assert!(patch.changes_nothing());
        patch.display_name = Some("New Name".to_string());
        assert!(!patch.changes_nothing());
    }

    #[test]
    fn profile_patch_double_option_distinguishes_unset_from_clear() {
        // `avatar_media: None` (outer) means "leave unchanged"; `Some(None)`
        // means "clear it" — distinct states the doubled `Option<Option<_>>`
        // shape exists specifically to express (Requirements 6.1, 6.5).
        let leave_unchanged = ProfilePatch {
            avatar_media: None,
            ..ProfilePatch::default()
        };
        let clear_it = ProfilePatch {
            avatar_media: Some(None),
            ..ProfilePatch::default()
        };
        let set_it = ProfilePatch {
            avatar_media: Some(Some(Id::from_i64(5))),
            ..ProfilePatch::default()
        };
        assert_ne!(leave_unchanged, clear_it);
        assert_ne!(clear_it, set_it);
        assert!(leave_unchanged.avatar_media.is_none());
        assert_eq!(clear_it.avatar_media, Some(None));
        assert_eq!(set_it.avatar_media, Some(Some(Id::from_i64(5))));
    }

    #[test]
    fn profile_field_holds_name_value_and_verified_at() {
        let field = ProfileField {
            name: "Pronouns".to_string(),
            value: "she/her".to_string(),
            verified_at: Some(datetime!(2026-02-01 12:00:00 UTC)),
        };
        assert_eq!(field.name, "Pronouns");
        assert_eq!(field.value, "she/her");
        assert_eq!(field.verified_at, Some(datetime!(2026-02-01 12:00:00 UTC)));
    }

    #[test]
    fn account_profile_holds_all_required_fields() {
        let profile = AccountProfile {
            actor_id: Id::from_i64(42),
            display_name: "Erin".to_string(),
            note: "Runs a single-user instance.".to_string(),
            avatar_media: Some(Id::from_i64(101)),
            header_media: None,
            fields: vec![ProfileField {
                name: "Website".to_string(),
                value: "https://erin.example".to_string(),
                verified_at: None,
            }],
            locked: true,
            bot: false,
            discoverable: true,
            source: CredentialSource {
                privacy: Visibility::Private,
                sensitive: false,
                language: Some("en".to_string()),
                note: "Runs a single-user instance.".to_string(),
                fields: Vec::new(),
                follow_requests_count: 2,
            },
        };
        assert_eq!(profile.actor_id, Id::from_i64(42));
        assert_eq!(profile.display_name, "Erin");
        assert_eq!(profile.note, "Runs a single-user instance.");
        assert!(profile.locked);
        assert!(!profile.bot);
        assert!(profile.discoverable);
        assert_eq!(profile.source.privacy, Visibility::Private);
    }

    #[test]
    fn credential_source_reuses_the_canonical_visibility_type() {
        let source = CredentialSource {
            privacy: Visibility::Unlisted,
            sensitive: true,
            language: Some("en".to_string()),
            note: "bio".to_string(),
            fields: Vec::new(),
            follow_requests_count: 3,
        };
        assert_eq!(source.privacy, Visibility::Unlisted);
        assert_eq!(source.follow_requests_count, 3);
    }

    #[test]
    fn remote_account_holds_username_and_domain_separately_matching_acct_remote() {
        let remote = RemoteAccount {
            id: Id::from_i64(7),
            actor_uri: "https://remote.example/users/alice".to_string(),
            username: "alice".to_string(),
            domain: "remote.example".to_string(),
            display_name: "Alice".to_string(),
            note: String::new(),
            url: "https://remote.example/@alice".to_string(),
            avatar_url: None,
            header_url: None,
            fields: Vec::new(),
            bot: false,
            locked: false,
            fetched_at: datetime!(2026-01-01 00:00:00 UTC),
        };
        let acct = Acct::remote(remote.username.clone(), remote.domain.clone());
        assert_eq!(acct.as_str(), "alice@remote.example");
    }

    #[test]
    fn custom_emoji_view_holds_the_required_fields() {
        let emoji = CustomEmojiView {
            shortcode: "blobcat".to_string(),
            url: "https://example.test/emoji/blobcat.png".to_string(),
            static_url: "https://example.test/emoji/blobcat.png".to_string(),
            visible_in_picker: true,
            category: Some("cats".to_string()),
        };
        assert!(emoji.visible_in_picker);
        assert_eq!(emoji.category.as_deref(), Some("cats"));
    }

    #[test]
    fn relationship_view_no_relationship_default_shape_is_expressible() {
        // This module does not own the "no relationship" default *value*
        // (that is the delegation port layer's job, task 1.3) but the type
        // must be able to represent it.
        let view = RelationshipView {
            id: Id::from_i64(1),
            following: false,
            showing_reblogs: false,
            notifying: false,
            languages: Vec::new(),
            followed_by: false,
            blocking: false,
            blocked_by: false,
            muting: false,
            muting_notifications: false,
            requested: false,
            requested_by: false,
            domain_blocking: false,
            endorsed: false,
            note: String::new(),
        };
        assert!(!view.following);
        assert!(view.note.is_empty());
    }

    #[test]
    fn account_counts_zero_default_shape_is_expressible() {
        let counts = AccountCounts {
            followers: 0,
            following: 0,
            statuses: 0,
            last_status_at: None,
        };
        assert_eq!(counts.followers, 0);
        assert!(counts.last_status_at.is_none());
    }

    #[test]
    fn instance_settings_excludes_version_source_url_and_usage() {
        // Structural check that InstanceSettings only carries the
        // DB-backed operational fields design.md's model doc assigns it —
        // `version`/`source_url`/`usage` are deliberately not fields here.
        let settings = InstanceSettings {
            title: "My Instance".to_string(),
            description: String::new(),
            contact_email: String::new(),
            contact_account_id: None,
            rules: Vec::new(),
            registrations_enabled: false,
            registrations_approval_required: false,
            registrations_message: None,
            thumbnail: None,
            languages: Vec::new(),
        };
        assert_eq!(settings.title, "My Instance");
        assert!(settings.thumbnail.is_none());
        assert!(settings.languages.is_empty());
    }
}
