//! Statuses domain types (`model` component, design.md "Status Domain /
//! ドメイン層" -> `model`, Requirements 1.1, 2.1, 3.1, 7.1, 8.1, 9.1, 10.1,
//! 11.1, 12.1, 13.1, 15.1; task 1.2, `Boundary: model`).
//!
//! Scope: this module owns exactly the domain value types this task's own
//! instruction enumerates — [`Status`], [`StatusEdit`], [`Poll`],
//! [`PollOption`], [`PollVote`], [`IdempotencyRecord`], and [`Tag`] — built
//! on core-runtime's [`Id`] and `time::OffsetDateTime`. [`Visibility`] is
//! *not* redefined here: it is imported from `crate::domain` (core-runtime's
//! canonical shared primitives module, mirroring `src/accounts/model.rs`'s
//! identical precedent), per this task's own explicit instruction and
//! design.md's model excerpt (`use core_runtime::domain_primitives::
//! {Visibility, AccountRef};`, design.md line 357 — `core_runtime::
//! domain_primitives` is that document's illustrative name for what this
//! crate exposes as `crate::domain`).
//!
//! `AccountRef` is deliberately *not* imported here even though design.md's
//! model excerpt's `use` line names it alongside `Visibility`: none of this
//! task's seven types needs a local/remote actor discriminant on any field.
//! Every actor reference in this task's boundary (`Status::actor_id`,
//! `Status::in_reply_to_account_id`, `PollVote::actor_id`,
//! `IdempotencyRecord::actor_id`) is a plain, logical-only [`Id`] reference
//! to actor-model's `local_actors.id` — matching `migrations/
//! 0007_statuses.sql`'s own naming-note precedent ("`actor_id` is a
//! logical-only reference to actor-model's `local_actors.id` (no
//! `REFERENCES`...)") and design.md's own [`Status`] type sketch, which
//! types `actor_id`/`in_reply_to_account_id` as plain `Id`, not `AccountRef`.
//! A post's own local/remote-ness is instead already carried by
//! [`Status::local`] (Requirement 4.x's "ローカル/リモート共通モデル"), so
//! there is no field here that would need `AccountRef`'s Local/Remote
//! discriminant; adding an unused import would just be dead weight. (This
//! does not contradict the "do not redefine `AccountRef`" instruction —
//! there is simply no field in this task's boundary that consumes it. It is
//! consumed downstream, e.g. by notifications' `NotificationEvent::origin`,
//! design.md line 66 — out of this task's boundary.)
//!
//! No persistence (`StatusRepository` / `InteractionRepository` /
//! `PollRepository` / `IdempotencyStore`, task 2.x), no delegation ports
//! (`RelationshipQuery`, task 1.3), no visibility/addressing logic
//! (`VisibilityPolicy` / `Addressing`, task 3.x), no Activity generation
//! (`StatusActivityBuilder`), no serialization to Mastodon JSON
//! (`StatusSerializer` / `PollSerializer`), no business logic
//! (`StatusService` / `InteractionService` / `PollService`), no inbound
//! handlers, and no HTTP surface live here — those consume the types
//! defined in this module but are out of scope for task 1.2
//! (`Boundary: model`).
//!
//! Per-actor interaction records (favourite/bookmark/pin) and the reblog
//! relationship deliberately have **no** dedicated domain struct in this
//! module: `migrations/0007_statuses.sql`'s own naming-note documents that a
//! reblog is represented as its own dedicated `statuses` row (via
//! [`Status::reblog_of_id`]), not a separate interaction table/type, and
//! design.md's `InteractionRepository` Service Interface (design.md lines
//! 419-424) operates directly on `(actor_id, status_id)` pairs — it never
//! names a `Favourite`/`Bookmark`/`Pin` domain type. Introducing one here
//! would be scope creep beyond this task's own explicit type list ("Status /
//! StatusEdit / Poll / PollOption / PollVote / IdempotencyRecord / Tag 等").
//!
//! ## Dialect isolation (Requirement 15.1)
//! [`Status`] holds no quote-post or emoji-reaction (custom-federation
//! dialect) fields. This is verified structurally, not just by omission: this
//! module's tests exhaustively destructure a [`Status`] value (no `..`
//! rest-pattern), which fails to compile the moment any field — dialect or
//! otherwise — is added without updating the test, mirroring `src/media/
//! model.rs`'s (`Media::actor_id`) and `src/accounts/model.rs`'s
//! (`AccountView`) identical precedent for proving a required-or-absent
//! field set at the type level rather than via a runtime check.

use time::OffsetDateTime;

use crate::domain::{Id, Visibility};

/// A single post — local or remote, in the same model (design.md's model
/// doc: "ローカル/リモート共通モデル"; Requirements 1.1, 3.1, 4.1).
///
/// `reblog_of_id`/`in_reply_to_id`/`poll_id` are `Option<Id>` *logical*
/// relations (Requirement 4.1's boost/reply/poll associations, mirroring
/// `migrations/0007_statuses.sql`'s nullable, FK-less `reblog_of_id`/
/// `in_reply_to_id`/`poll_id` columns): `None` means "this post is not a
/// boost" / "not a reply" / "has no attached poll" respectively, and a
/// boost's `reblog_of_id` deliberately points at another `statuses` row
/// rather than a separate reblog entity (see this module's doc comment).
/// `visibility` reuses core-runtime's canonical [`Visibility`] rather than
/// redefining a parallel enum (Requirement 4.1's four-value visibility).
///
/// Holds no quote-post or emoji-reaction (custom-federation dialect) field —
/// see this module's doc comment, "Dialect isolation" (Requirement 15.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub id: Id,
    pub actor_id: Id,
    pub uri: String,
    pub url: Option<String>,
    pub content: String,
    pub visibility: Visibility,
    pub sensitive: bool,
    pub spoiler_text: String,
    pub in_reply_to_id: Option<Id>,
    pub in_reply_to_account_id: Option<Id>,
    pub reblog_of_id: Option<Id>,
    pub poll_id: Option<Id>,
    pub language: Option<String>,
    pub reblogs_count: i64,
    pub favourites_count: i64,
    pub replies_count: i64,
    pub local: bool,
    pub created_at: OffsetDateTime,
    pub edited_at: Option<OffsetDateTime>,
}

/// One prior version of a [`Status`]'s editable fields (Requirement 8.2:
/// edit history retains the pre-edit body/CW/sensitive per version), one row
/// per version stored in `status_edits` (`migrations/0007_statuses.sql`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEdit {
    pub status_id: Id,
    pub content: String,
    pub spoiler_text: String,
    pub sensitive: bool,
    pub created_at: OffsetDateTime,
}

/// A poll attached to a [`Status`] via [`Status::poll_id`] (Requirement
/// 13.1). `status_id` is the inverse direction of that same relation
/// (`polls.status_id REFERENCES statuses(id)`, `migrations/
/// 0007_statuses.sql`'s naming-note: "the relationship is already covered
/// from the other direction").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poll {
    pub id: Id,
    pub status_id: Id,
    pub expires_at: Option<OffsetDateTime>,
    pub multiple: bool,
}

/// One selectable option of a [`Poll`], addressed by `(poll_id, idx)`
/// (`migrations/0007_statuses.sql`'s `poll_options` composite primary key;
/// Requirement 13.1). `votes_count` is this option's running tally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollOption {
    pub poll_id: Id,
    pub idx: i32,
    pub title: String,
    pub votes_count: i64,
}

/// One actor's vote for one [`PollOption`] of a [`Poll`] (Requirement 13.2,
/// 13.5). A single-choice poll produces exactly one [`PollVote`] row per
/// voter; a multiple-choice poll produces one row per selected `choice` —
/// mirroring `migrations/0007_statuses.sql`'s `poll_votes` primary key
/// `(poll_id, actor_id, choice)`, which is this task's own explicit
/// instruction ("vote は (poll_id, actor_id, choice) 一意") and rejects a
/// repeated `choice` by the same actor as a duplicate-key violation
/// (Requirement 13.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollVote {
    pub poll_id: Id,
    pub actor_id: Id,
    pub choice: i32,
    pub created_at: OffsetDateTime,
}

/// The `Idempotency-Key` ledger entry binding one `(actor_id, key)` pair to
/// the [`Status`] created by the first request that used it (Requirements
/// 5.1, 5.2): a resend of the same key by the same actor resolves to
/// `status_id` instead of creating a second post. Field shape matches
/// design.md's model excerpt (design.md line 369) exactly, including the
/// `key` field name (the underlying `status_idempotency_keys` column is
/// named `idempotency_key`; this struct keeps design.md's shorter Rust-side
/// name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRecord {
    pub actor_id: Id,
    pub key: String,
    pub status_id: Id,
    pub created_at: OffsetDateTime,
}

/// A persisted, normalized hashtag (Requirement 3.6's extraction target;
/// `migrations/0007_statuses.sql`'s `tags` table). `name` is expected to
/// already be normalized (lower-cased) by the extraction logic that
/// produces a [`Tag`] (a later task's `StatusService::create_status`
/// responsibility, out of this task's boundary) — this module does not
/// itself normalize or validate `name`'s case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: Id,
    pub name: String,
    pub created_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn sample_status(
        reblog_of_id: Option<Id>,
        in_reply_to_id: Option<Id>,
        poll_id: Option<Id>,
    ) -> Status {
        Status {
            id: Id::from_i64(1),
            actor_id: Id::from_i64(10),
            uri: "https://example.test/statuses/1".to_string(),
            url: Some("https://example.test/@alice/1".to_string()),
            content: "hello".to_string(),
            visibility: Visibility::Public,
            sensitive: false,
            spoiler_text: String::new(),
            in_reply_to_id,
            in_reply_to_account_id: in_reply_to_id.map(|_| Id::from_i64(11)),
            reblog_of_id,
            poll_id,
            language: Some("en".to_string()),
            reblogs_count: 0,
            favourites_count: 0,
            replies_count: 0,
            local: true,
            created_at: datetime!(2026-07-24 00:00:00 UTC),
            edited_at: None,
        }
    }

    // -- Visibility: exactly 4 variants (Requirement 4.1) --------------

    /// Exhaustive match with no wildcard arm: this function fails to
    /// compile the moment a fifth `Visibility` variant is added (or one of
    /// the four is removed/renamed), structurally proving "exactly 4
    /// values" at the type level rather than via a runtime enumeration
    /// alone.
    fn visibility_ordinal(v: Visibility) -> u8 {
        match v {
            Visibility::Public => 0,
            Visibility::Unlisted => 1,
            Visibility::Private => 2,
            Visibility::Direct => 3,
        }
    }

    #[test]
    fn visibility_has_exactly_four_distinct_variants() {
        let all = [
            Visibility::Public,
            Visibility::Unlisted,
            Visibility::Private,
            Visibility::Direct,
        ];
        assert_eq!(all.len(), 4);
        let ordinals: Vec<u8> = all.iter().copied().map(visibility_ordinal).collect();
        // Every ordinal distinct -> no two variants collapse to the same
        // case, and the exhaustive match above already guarantees no fifth
        // variant can exist without a compile error.
        let mut sorted = ordinals.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "expected 4 distinct Visibility variants");
        assert_eq!(ordinals, vec![0, 1, 2, 3]);
    }

    #[test]
    fn visibility_is_imported_from_core_runtime_domain_not_redefined() {
        // Type-level proof: `Status::visibility` is `crate::domain::
        // Visibility` (imported above), the same type
        // `src/domain/primitives.rs` canonically owns — this line would
        // fail to compile if `Status` held a locally-redefined, structurally
        // different `Visibility` type instead.
        let status = sample_status(None, None, None);
        let _: Visibility = status.visibility;
    }

    // -- Status relations: reblog / reply / poll are typed Option<Id> ---

    #[test]
    fn status_can_be_constructed_with_reblog_reply_and_poll_relations_populated() {
        let reblog_of = Id::from_i64(100);
        let in_reply_to = Id::from_i64(200);
        let poll = Id::from_i64(300);
        let status = sample_status(Some(reblog_of), Some(in_reply_to), Some(poll));

        assert_eq!(status.reblog_of_id, Some(reblog_of));
        assert_eq!(status.in_reply_to_id, Some(in_reply_to));
        assert_eq!(status.poll_id, Some(poll));
        // A reply also carries the replied-to account (Requirement 3.5).
        assert!(status.in_reply_to_account_id.is_some());
    }

    #[test]
    fn status_can_be_constructed_with_reblog_reply_and_poll_relations_absent() {
        let status = sample_status(None, None, None);

        assert_eq!(status.reblog_of_id, None);
        assert_eq!(status.in_reply_to_id, None);
        assert_eq!(status.poll_id, None);
        assert_eq!(status.in_reply_to_account_id, None);
    }

    #[test]
    fn status_reblog_reply_and_poll_relations_are_independent_of_each_other() {
        // A plain original post (no boost, no reply) that still carries a
        // poll: proves the three Option<Id> relations vary independently
        // rather than being coupled to one another.
        let status = sample_status(None, None, Some(Id::from_i64(9)));
        assert_eq!(status.reblog_of_id, None);
        assert_eq!(status.in_reply_to_id, None);
        assert_eq!(status.poll_id, Some(Id::from_i64(9)));
    }

    #[test]
    fn status_holds_no_dialect_fields_beyond_the_core_field_set() {
        // Exhaustive destructuring (no `..`): fails to compile if a field
        // were ever added to `Status` (dialect or otherwise) without this
        // test being updated, structurally proving Requirement 15.1's "コア
        // 状態モデルに連合方言フィールドを含めない" at the type level rather
        // than via a runtime check or mere omission. Mirrors `src/media/
        // model.rs`'s `Media::actor_id` and `src/accounts/model.rs`'s
        // `AccountView` precedent for the same technique.
        let status = sample_status(Some(Id::from_i64(1)), None, None);
        let Status {
            id: _,
            actor_id: _,
            uri: _,
            url: _,
            content: _,
            visibility: _,
            sensitive: _,
            spoiler_text: _,
            in_reply_to_id: _,
            in_reply_to_account_id: _,
            reblog_of_id,
            poll_id: _,
            language: _,
            reblogs_count: _,
            favourites_count: _,
            replies_count: _,
            local: _,
            created_at: _,
            edited_at: _,
        } = status;
        // No `quote_id`/`quoted_status_id` field exists to destructure above
        // (a quote-post dialect relation); no `emoji_reactions`/
        // `reaction_counts` field exists either (an emoji-reaction dialect
        // aggregate) — both are simply absent from the struct, which the
        // exhaustive pattern above would fail to compile against if either
        // were ever added without updating this test.
        assert!(reblog_of_id.is_some());
    }

    // -- StatusEdit --------------------------------------------------------

    #[test]
    fn status_edit_holds_one_prior_version_of_the_editable_fields() {
        let edit = StatusEdit {
            status_id: Id::from_i64(1),
            content: "previous content".to_string(),
            spoiler_text: "previous cw".to_string(),
            sensitive: true,
            created_at: datetime!(2026-07-23 12:00:00 UTC),
        };
        assert_eq!(edit.status_id, Id::from_i64(1));
        assert_eq!(edit.content, "previous content");
        assert!(edit.sensitive);
    }

    // -- Poll / PollOption / PollVote --------------------------------------

    #[test]
    fn poll_links_back_to_its_status_and_may_have_no_expiry() {
        let poll = Poll {
            id: Id::from_i64(1),
            status_id: Id::from_i64(2),
            expires_at: None,
            multiple: false,
        };
        assert_eq!(poll.status_id, Id::from_i64(2));
        assert!(poll.expires_at.is_none());
        assert!(!poll.multiple);
    }

    #[test]
    fn poll_option_is_addressed_by_poll_id_and_idx() {
        let option = PollOption {
            poll_id: Id::from_i64(1),
            idx: 0,
            title: "Yes".to_string(),
            votes_count: 5,
        };
        assert_eq!(option.poll_id, Id::from_i64(1));
        assert_eq!(option.idx, 0);
        assert_eq!(option.votes_count, 5);
    }

    #[test]
    fn poll_vote_single_choice_is_one_row_per_voter() {
        let vote = PollVote {
            poll_id: Id::from_i64(1),
            actor_id: Id::from_i64(42),
            choice: 0,
            created_at: datetime!(2026-07-24 00:00:00 UTC),
        };
        assert_eq!(vote.actor_id, Id::from_i64(42));
        assert_eq!(vote.choice, 0);
    }

    #[test]
    fn poll_vote_multiple_choice_is_distinguished_by_choice_index() {
        // Requirement 13.5's `(poll_id, actor_id, choice)` uniqueness means
        // a multi-choice vote is *distinct rows* differing only by
        // `choice`, not one row holding a `Vec<i32>`.
        let voter = Id::from_i64(42);
        let poll_id = Id::from_i64(1);
        let now = datetime!(2026-07-24 00:00:00 UTC);
        let first = PollVote {
            poll_id,
            actor_id: voter,
            choice: 0,
            created_at: now,
        };
        let second = PollVote {
            poll_id,
            actor_id: voter,
            choice: 1,
            created_at: now,
        };
        assert_ne!(first, second);
        assert_eq!(first.poll_id, second.poll_id);
        assert_eq!(first.actor_id, second.actor_id);
    }

    // -- IdempotencyRecord --------------------------------------------------

    #[test]
    fn idempotency_record_binds_actor_and_key_to_the_created_status() {
        let record = IdempotencyRecord {
            actor_id: Id::from_i64(42),
            key: "client-generated-key-1".to_string(),
            status_id: Id::from_i64(1),
            created_at: datetime!(2026-07-24 00:00:00 UTC),
        };
        assert_eq!(record.actor_id, Id::from_i64(42));
        assert_eq!(record.key, "client-generated-key-1");
        assert_eq!(record.status_id, Id::from_i64(1));
    }

    // -- Tag ------------------------------------------------------------

    #[test]
    fn tag_holds_a_normalized_name_and_its_own_id() {
        let tag = Tag {
            id: Id::from_i64(1),
            name: "rustlang".to_string(),
            created_at: datetime!(2026-07-24 00:00:00 UTC),
        };
        assert_eq!(tag.id, Id::from_i64(1));
        assert_eq!(tag.name, "rustlang");
    }
}
