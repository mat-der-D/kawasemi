//! Unit tests for `NotificationSerializer` (task 2.1, Requirements 1.1-1.6),
//! per this task's own observable completion condition: "type が v1 種別の
//! みで、follow 系で status が null、投稿関連種別で status 埋め込み点が受
//! 信者 viewer となり、外殻ゴールデンが決定的に再現される状態". Mirrors
//! `src/statuses/serializer/tests.rs`'s/`src/accounts/serializer/tests.rs`'s
//! identical precedent: a pure serializer has nothing non-deterministic
//! upstream to inject a `RuntimeContext` boundary for — literal
//! `datetime!`/`Id::from_i64` fixtures already satisfy Requirement 1.6's
//! "決定的に再現可能".

use time::macros::datetime;

use super::*;
use crate::accounts::model::{AccountView, AccountViewFields};
use crate::accounts::serializer::account_to_json;
use crate::domain::AccountRef;
use crate::statuses::model::Status;
use crate::statuses::serializer::{StatusInteractionState, StatusRenderInput, status_to_json};

fn sample_notification(kind: NotificationType, status_id: Option<Id>) -> Notification {
    Notification {
        id: Id::from_i64(1),
        recipient_id: Id::from_i64(10),
        kind,
        origin: AccountRef::Local(Id::from_i64(20)),
        status_id,
        dismissed: false,
        created_at: datetime!(2026-07-24 00:00:00 UTC),
    }
}

/// A hand-built stand-in Account JSON, mirroring
/// `src/statuses/serializer/tests.rs::account_json`'s identical convention
/// for a cross-domain delegated field a pure serializer's own unit tests
/// don't need to re-resolve for every scenario.
fn account_json_stand_in() -> Value {
    serde_json::json!({
        "id": "20",
        "username": "alice",
        "acct": "alice",
        "display_name": "Alice",
    })
}

/// A hand-built stand-in Status JSON, same convention as
/// [`account_json_stand_in`].
fn status_json_stand_in() -> Value {
    serde_json::json!({
        "id": "99",
        "content": "<p>hello</p>",
    })
}

fn minimal_input(notification: &Notification) -> NotificationRenderInput<'_> {
    NotificationRenderInput {
        notification,
        account: account_json_stand_in(),
        status: if notification.status_id.is_some() {
            Some(status_json_stand_in())
        } else {
            None
        },
    }
}

// -- Requirement 1.1: outer-shell field presence -------------------------

#[test]
fn notification_to_json_emits_every_requirement_1_1_field() {
    let notification = sample_notification(NotificationType::Favourite, Some(Id::from_i64(99)));
    let json = notification_to_json(&minimal_input(&notification));

    for field in ["id", "type", "created_at", "account", "status"] {
        assert!(
            json.get(field).is_some(),
            "expected field {field:?} in Notification JSON, got {json:?}"
        );
    }
    assert_eq!(json["id"], "1");
}

// -- Requirement 1.5: type restricted to the v1 kind set -----------------

/// Exhaustively matches all eight `NotificationType` variants against
/// `kind_str`'s output — no wildcard arm, mirroring `model.rs`'s identical
/// exhaustive-match technique for proving a closed mapping at compile time.
#[test]
fn kind_str_maps_exactly_the_eight_v1_kinds_to_mastodons_own_strings() {
    fn expected(kind: NotificationType) -> &'static str {
        match kind {
            NotificationType::Mention => "mention",
            NotificationType::Follow => "follow",
            NotificationType::FollowRequest => "follow_request",
            NotificationType::Favourite => "favourite",
            NotificationType::Reblog => "reblog",
            NotificationType::Poll => "poll",
            NotificationType::Status => "status",
            NotificationType::Update => "update",
        }
    }

    for kind in [
        NotificationType::Mention,
        NotificationType::Follow,
        NotificationType::FollowRequest,
        NotificationType::Favourite,
        NotificationType::Reblog,
        NotificationType::Poll,
        NotificationType::Status,
        NotificationType::Update,
    ] {
        assert_eq!(kind_str(kind), expected(kind));
    }
}

#[test]
fn notification_to_json_type_field_matches_kind_str() {
    let notification = sample_notification(NotificationType::Mention, Some(Id::from_i64(5)));
    let json = notification_to_json(&minimal_input(&notification));
    assert_eq!(json["type"], "mention");
}

// -- Requirement 1.4: null discipline for follow/follow_request ----------

#[test]
fn follow_notification_status_is_null() {
    let notification = sample_notification(NotificationType::Follow, None);
    let json = notification_to_json(&minimal_input(&notification));
    assert_eq!(json["status"], Value::Null);
}

#[test]
fn follow_request_notification_status_is_null() {
    let notification = sample_notification(NotificationType::FollowRequest, None);
    let json = notification_to_json(&minimal_input(&notification));
    assert_eq!(json["status"], Value::Null);
}

/// Requirement 1.4's null discipline is enforced by this module itself, not
/// merely documented as a caller invariant: even when a caller mistakenly
/// supplies a `Some(..)` status for a `Follow` notification, the emitted
/// JSON still nulls it out. See `serializer.rs`'s own doc comment, "Null
/// discipline is enforced here, not just documented".
#[test]
fn follow_notification_status_is_null_even_if_a_status_value_is_mistakenly_supplied() {
    let notification = sample_notification(NotificationType::Follow, None);
    let input = NotificationRenderInput {
        notification: &notification,
        account: account_json_stand_in(),
        status: Some(status_json_stand_in()),
    };
    let json = notification_to_json(&input);
    assert_eq!(
        json["status"],
        Value::Null,
        "status must be null for Follow regardless of what the caller passed"
    );
}

/// `status` is an explicit JSON `null`, not an omitted key — matching
/// `StatusJson`'s established "never `skip_serializing_if`" convention in
/// this crate (see `serializer.rs`'s own doc comment).
#[test]
fn follow_notification_status_key_is_present_with_an_explicit_null_not_omitted() {
    let notification = sample_notification(NotificationType::Follow, None);
    let json = notification_to_json(&minimal_input(&notification));
    let obj = json.as_object().expect("Notification JSON is an object");
    assert!(
        obj.contains_key("status"),
        "the status key must be present (as null), not omitted"
    );
    assert_eq!(obj["status"], Value::Null);
}

// -- Requirement 1.2: post-related kinds embed the delegated status -------

#[test]
fn favourite_notification_embeds_the_delegated_status_value_verbatim() {
    let notification = sample_notification(NotificationType::Favourite, Some(Id::from_i64(99)));
    let json = notification_to_json(&minimal_input(&notification));
    assert_eq!(json["status"], status_json_stand_in());
}

#[test]
fn every_post_related_kind_embeds_a_non_null_status() {
    for kind in [
        NotificationType::Mention,
        NotificationType::Favourite,
        NotificationType::Reblog,
        NotificationType::Poll,
        NotificationType::Status,
        NotificationType::Update,
    ] {
        let notification = sample_notification(kind, Some(Id::from_i64(99)));
        let json = notification_to_json(&minimal_input(&notification));
        assert_ne!(
            json["status"],
            Value::Null,
            "kind {kind:?} must embed a non-null status"
        );
    }
}

// -- Requirement 1.3: account is always the delegated origin account -----

#[test]
fn account_field_is_the_delegated_value_verbatim() {
    let notification = sample_notification(NotificationType::Reblog, Some(Id::from_i64(1)));
    let json = notification_to_json(&minimal_input(&notification));
    assert_eq!(json["account"], account_json_stand_in());
}

#[test]
fn account_field_is_present_and_non_null_for_every_kind() {
    for kind in [
        NotificationType::Mention,
        NotificationType::Follow,
        NotificationType::FollowRequest,
        NotificationType::Favourite,
        NotificationType::Reblog,
        NotificationType::Poll,
        NotificationType::Status,
        NotificationType::Update,
    ] {
        let status_id = if is_status_less_kind(kind) {
            None
        } else {
            Some(Id::from_i64(99))
        };
        let notification = sample_notification(kind, status_id);
        let json = notification_to_json(&minimal_input(&notification));
        assert!(
            json["account"].is_object(),
            "kind {kind:?} must always embed a non-null account object"
        );
    }
}

// -- Determinism -----------------------------------------------------------

#[test]
fn notification_to_json_is_deterministic_for_the_same_input() {
    let notification = sample_notification(NotificationType::Poll, Some(Id::from_i64(7)));
    let input = minimal_input(&notification);
    let first = notification_to_json(&input);
    let second = notification_to_json(&input);
    assert_eq!(first, second);
}

// -- Genuine end-to-end delegation (not just a hand-built stand-in) -------
//
// Requirement 1.2/1.3: proves `NotificationRenderInput::account`/`::status`
// really do embed output *produced by* `crate::accounts::serializer::
// account_to_json`/`crate::statuses::serializer::status_to_json` — not
// merely documented as expected, since the rest of this file's scenarios
// (matching sibling serializers' own precedent) use hand-built stand-in
// Values for speed/simplicity.

fn real_origin_account_view() -> AccountView {
    AccountView::local(
        Id::from_i64(20),
        "alice",
        AccountViewFields {
            username: "alice".to_string(),
            display_name: "Alice".to_string(),
            locked: false,
            bot: false,
            discoverable: true,
            group: false,
            created_at: datetime!(2026-01-01 00:00:00 UTC),
            note: "hello".to_string(),
            url: "https://kawasemi.example/@alice".to_string(),
            uri: "https://kawasemi.example/actors/alice".to_string(),
            avatar: "https://kawasemi.example/avatars/missing.png".to_string(),
            avatar_static: "https://kawasemi.example/avatars/missing.png".to_string(),
            header: "https://kawasemi.example/headers/missing.png".to_string(),
            header_static: "https://kawasemi.example/headers/missing.png".to_string(),
            followers_count: 0,
            following_count: 0,
            statuses_count: 0,
            last_status_at: None,
            emojis: Vec::new(),
            fields: Vec::new(),
        },
    )
}

fn real_status(id: i64) -> Status {
    Status {
        id: Id::from_i64(id),
        actor_id: Id::from_i64(10),
        uri: format!("https://kawasemi.example/statuses/{id}"),
        url: Some(format!("https://kawasemi.example/@bob/{id}")),
        content: "<p>a real, fully-rendered post</p>".to_string(),
        visibility: crate::domain::Visibility::Public,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: datetime!(2026-07-20 12:00:00 UTC),
        edited_at: None,
    }
}

#[test]
fn account_embed_is_produced_by_a_real_call_into_accounts_serializer() {
    let real_account_json = account_to_json(&real_origin_account_view());

    let notification = sample_notification(NotificationType::Follow, None);
    let input = NotificationRenderInput {
        notification: &notification,
        account: real_account_json.clone(),
        status: None,
    };
    let json = notification_to_json(&input);

    assert_eq!(json["account"], real_account_json);
    assert_eq!(json["account"]["username"], "alice");
    assert_eq!(json["account"]["acct"], "alice");
}

#[test]
fn status_embed_is_produced_by_a_real_call_into_statuses_serializer_from_the_recipient_viewpoint() {
    let recipient_account_json = account_to_json(&real_origin_account_view());
    let status = real_status(42);
    let render_input = StatusRenderInput {
        status: &status,
        account: recipient_account_json,
        media_attachments: Vec::new(),
        mentions: Vec::new(),
        tags: Vec::new(),
        emojis: Vec::new(),
        poll: None,
        // Recipient-viewpoint interaction state (Requirement 1.2: "受信者
        // 視点で"): the recipient has favourited the mentioning post.
        interactions: StatusInteractionState {
            favourited: true,
            ..StatusInteractionState::default()
        },
        reblog: None,
    };
    let real_status_json = status_to_json(&render_input);

    let notification = sample_notification(NotificationType::Mention, Some(status.id));
    let input = NotificationRenderInput {
        notification: &notification,
        account: account_json_stand_in(),
        status: Some(real_status_json.clone()),
    };
    let json = notification_to_json(&input);

    assert_eq!(json["status"], real_status_json);
    assert_eq!(json["status"]["id"], "42");
    assert_eq!(
        json["status"]["favourited"], true,
        "the embedded status must reflect the recipient's own viewpoint, not a default"
    );
}

// ---- Requirement 1.6: contract-harness golden registration ----
//
// Registers one Notification outer-shell JSON per representative kind as a
// golden via `crate::contract::assert_golden`, mirroring `src/statuses/
// serializer/tests.rs`'s/`src/accounts/serializer/tests.rs`'s identical
// precedent: a pure serializer has nothing non-deterministic upstream to
// inject a `RuntimeContext` boundary for -- literal `datetime!`/
// `Id::from_i64` fixtures already satisfy Requirement 1.6's "決定的に再現
// 可能".

#[test]
fn mention_notification_json_matches_the_registered_contract_golden() {
    let notification = sample_notification(NotificationType::Mention, Some(Id::from_i64(99)));
    let json = notification_to_json(&minimal_input(&notification));
    crate::contract::assert_golden(
        "tests/golden/notifications/notification_mention.json",
        &json,
    );
}

#[test]
fn follow_notification_json_matches_the_registered_contract_golden() {
    let notification = sample_notification(NotificationType::Follow, None);
    let json = notification_to_json(&minimal_input(&notification));
    crate::contract::assert_golden("tests/golden/notifications/notification_follow.json", &json);
}

#[test]
fn follow_request_notification_json_matches_the_registered_contract_golden() {
    let notification = sample_notification(NotificationType::FollowRequest, None);
    let json = notification_to_json(&minimal_input(&notification));
    crate::contract::assert_golden(
        "tests/golden/notifications/notification_follow_request.json",
        &json,
    );
}

#[test]
fn favourite_notification_json_matches_the_registered_contract_golden() {
    let notification = sample_notification(NotificationType::Favourite, Some(Id::from_i64(99)));
    let json = notification_to_json(&minimal_input(&notification));
    crate::contract::assert_golden(
        "tests/golden/notifications/notification_favourite.json",
        &json,
    );
}

#[test]
fn reblog_notification_json_matches_the_registered_contract_golden() {
    let notification = sample_notification(NotificationType::Reblog, Some(Id::from_i64(99)));
    let json = notification_to_json(&minimal_input(&notification));
    crate::contract::assert_golden("tests/golden/notifications/notification_reblog.json", &json);
}

#[test]
fn poll_notification_json_matches_the_registered_contract_golden() {
    let notification = sample_notification(NotificationType::Poll, Some(Id::from_i64(99)));
    let json = notification_to_json(&minimal_input(&notification));
    crate::contract::assert_golden("tests/golden/notifications/notification_poll.json", &json);
}

#[test]
fn status_notification_json_matches_the_registered_contract_golden() {
    let notification = sample_notification(NotificationType::Status, Some(Id::from_i64(99)));
    let json = notification_to_json(&minimal_input(&notification));
    crate::contract::assert_golden("tests/golden/notifications/notification_status.json", &json);
}

#[test]
fn update_notification_json_matches_the_registered_contract_golden() {
    let notification = sample_notification(NotificationType::Update, Some(Id::from_i64(99)));
    let json = notification_to_json(&minimal_input(&notification));
    crate::contract::assert_golden("tests/golden/notifications/notification_update.json", &json);
}
