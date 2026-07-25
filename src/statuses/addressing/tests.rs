//! Unit tests for [`derive_addressing`]/[`derive_recipients`] (task 3.2's
//! own completion criteria): deterministic `to`/`cc`/recipients for each of
//! the 4 [`Visibility`] kinds, `direct` limited to mentions, and empty vs.
//! non-empty `followers` behavior matching the default
//! [`crate::statuses::visibility::NoRelationshipQuery`] (empty) and a real
//! `RelationshipQuery::followers_of` resolution (non-empty).

use time::macros::datetime;

use super::*;
use crate::actor::Handle;
use crate::domain::Id;

const FOLLOWERS_URI: &str = "https://example.test/users/alice/followers";

fn sample_status(visibility: Visibility) -> Status {
    Status {
        id: Id::from_i64(1),
        actor_id: Id::from_i64(10),
        uri: "https://example.test/statuses/1".to_string(),
        url: Some("https://example.test/@alice/1".to_string()),
        content: "hello".to_string(),
        visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id: None,
        language: Some("en".to_string()),
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local: true,
        created_at: datetime!(2026-07-24 00:00:00 UTC),
        edited_at: None,
    }
}

fn local_mention(handle: &str, uri: &str) -> ActorRef {
    ActorRef {
        uri: uri.to_string(),
        recipient: Recipient::Local(Handle::new(handle).unwrap()),
    }
}

fn remote_mention(uri: &str, inbox: &str) -> ActorRef {
    ActorRef {
        uri: uri.to_string(),
        recipient: Recipient::Remote {
            inbox: inbox.to_string(),
            shared_inbox: None,
        },
    }
}

fn local_recipient(handle: &str) -> Recipient {
    Recipient::Local(Handle::new(handle).unwrap())
}

// -- derive_addressing: per-visibility to/cc shape ----------------------

#[test]
fn public_puts_public_collection_in_to_and_followers_plus_mentions_in_cc() {
    let status = sample_status(Visibility::Public);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];

    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);

    assert_eq!(addressing.to, vec![PUBLIC_COLLECTION_URI.to_string()]);
    assert_eq!(
        addressing.cc,
        vec![
            FOLLOWERS_URI.to_string(),
            "https://example.test/users/bob".to_string()
        ]
    );
}

#[test]
fn unlisted_puts_followers_plus_mentions_in_to_and_public_collection_in_cc() {
    let status = sample_status(Visibility::Unlisted);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];

    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);

    assert_eq!(
        addressing.to,
        vec![
            FOLLOWERS_URI.to_string(),
            "https://example.test/users/bob".to_string()
        ]
    );
    assert_eq!(addressing.cc, vec![PUBLIC_COLLECTION_URI.to_string()]);
}

#[test]
fn private_puts_followers_plus_mentions_in_to_and_never_references_public_collection() {
    let status = sample_status(Visibility::Private);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];

    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);

    assert_eq!(
        addressing.to,
        vec![
            FOLLOWERS_URI.to_string(),
            "https://example.test/users/bob".to_string()
        ]
    );
    assert!(addressing.cc.is_empty());
    assert!(!addressing.to.contains(&PUBLIC_COLLECTION_URI.to_string()));
    assert!(!addressing.cc.contains(&PUBLIC_COLLECTION_URI.to_string()));
}

#[test]
fn direct_has_no_collection_reference_only_mentioned_actors() {
    let status = sample_status(Visibility::Direct);
    let mentions = vec![
        local_mention("bob", "https://example.test/users/bob"),
        remote_mention(
            "https://remote.test/users/carol",
            "https://remote.test/inbox",
        ),
    ];

    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);

    assert_eq!(
        addressing.to,
        vec![
            "https://example.test/users/bob".to_string(),
            "https://remote.test/users/carol".to_string()
        ]
    );
    assert!(addressing.cc.is_empty());
    assert!(!addressing.to.contains(&FOLLOWERS_URI.to_string()));
    assert!(!addressing.to.contains(&PUBLIC_COLLECTION_URI.to_string()));
}

#[test]
fn direct_with_no_mentions_has_empty_to_and_cc() {
    let status = sample_status(Visibility::Direct);

    let addressing = derive_addressing(&status, &[], FOLLOWERS_URI);

    assert!(addressing.to.is_empty());
    assert!(addressing.cc.is_empty());
}

// -- derive_addressing: determinism --------------------------------------

#[test]
fn derive_addressing_is_deterministic_across_repeated_calls() {
    let status = sample_status(Visibility::Public);
    let mentions = vec![
        local_mention("bob", "https://example.test/users/bob"),
        remote_mention(
            "https://remote.test/users/carol",
            "https://remote.test/inbox",
        ),
    ];

    let first = derive_addressing(&status, &mentions, FOLLOWERS_URI);
    let second = derive_addressing(&status, &mentions, FOLLOWERS_URI);

    assert_eq!(first, second);
}

// -- derive_recipients: public/unlisted/private with empty followers ----

#[test]
fn public_recipients_with_empty_followers_is_mentions_only() {
    let status = sample_status(Visibility::Public);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];
    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);

    let recipients = derive_recipients(&addressing, &mentions, &[]);

    assert_eq!(recipients, vec![local_recipient("bob")]);
}

#[test]
fn unlisted_recipients_with_empty_followers_is_mentions_only() {
    let status = sample_status(Visibility::Unlisted);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];
    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);

    let recipients = derive_recipients(&addressing, &mentions, &[]);

    assert_eq!(recipients, vec![local_recipient("bob")]);
}

#[test]
fn private_recipients_with_empty_followers_is_mentions_only() {
    let status = sample_status(Visibility::Private);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];
    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);

    let recipients = derive_recipients(&addressing, &mentions, &[]);

    assert_eq!(recipients, vec![local_recipient("bob")]);
}

#[test]
fn private_recipients_with_empty_followers_and_no_mentions_is_empty() {
    let status = sample_status(Visibility::Private);
    let addressing = derive_addressing(&status, &[], FOLLOWERS_URI);

    let recipients = derive_recipients(&addressing, &[], &[]);

    assert!(recipients.is_empty());
}

// -- derive_recipients: public/unlisted/private with non-empty followers

#[test]
fn public_recipients_with_followers_includes_followers_then_mentions() {
    let status = sample_status(Visibility::Public);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];
    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);
    let followers = vec![local_recipient("dave"), local_recipient("erin")];

    let recipients = derive_recipients(&addressing, &mentions, &followers);

    assert_eq!(
        recipients,
        vec![
            local_recipient("dave"),
            local_recipient("erin"),
            local_recipient("bob"),
        ]
    );
}

#[test]
fn unlisted_recipients_with_followers_includes_followers_then_mentions() {
    let status = sample_status(Visibility::Unlisted);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];
    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);
    let followers = vec![local_recipient("dave")];

    let recipients = derive_recipients(&addressing, &mentions, &followers);

    assert_eq!(
        recipients,
        vec![local_recipient("dave"), local_recipient("bob")]
    );
}

#[test]
fn private_recipients_with_followers_includes_followers_then_mentions() {
    let status = sample_status(Visibility::Private);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];
    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);
    let followers = vec![local_recipient("dave")];

    let recipients = derive_recipients(&addressing, &mentions, &followers);

    assert_eq!(
        recipients,
        vec![local_recipient("dave"), local_recipient("bob")]
    );
}

// -- derive_recipients: direct never includes followers ------------------

#[test]
fn direct_recipients_come_only_from_mentions_never_from_followers() {
    let status = sample_status(Visibility::Direct);
    let mentions = vec![local_mention("bob", "https://example.test/users/bob")];
    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);
    // Even if a caller mistakenly passes a non-empty followers list for a
    // `direct` post, it must never leak into direct's recipients.
    let followers = vec![local_recipient("dave"), local_recipient("erin")];

    let recipients = derive_recipients(&addressing, &mentions, &followers);

    assert_eq!(recipients, vec![local_recipient("bob")]);
}

#[test]
fn direct_recipients_with_no_mentions_is_empty() {
    let status = sample_status(Visibility::Direct);
    let addressing = derive_addressing(&status, &[], FOLLOWERS_URI);

    let recipients = derive_recipients(&addressing, &[], &[local_recipient("dave")]);

    assert!(recipients.is_empty());
}

// -- derive_recipients: determinism --------------------------------------

#[test]
fn derive_recipients_is_deterministic_across_repeated_calls() {
    let status = sample_status(Visibility::Public);
    let mentions = vec![
        local_mention("bob", "https://example.test/users/bob"),
        remote_mention(
            "https://remote.test/users/carol",
            "https://remote.test/inbox",
        ),
    ];
    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);
    let followers = vec![local_recipient("dave"), local_recipient("erin")];

    let first = derive_recipients(&addressing, &mentions, &followers);
    let second = derive_recipients(&addressing, &mentions, &followers);

    assert_eq!(first, second);
}

// -- ActorRef with a remote mention round-trips through both functions --

#[test]
fn remote_mention_recipient_is_preserved_through_addressing_and_recipients() {
    let status = sample_status(Visibility::Direct);
    let mentions = vec![remote_mention(
        "https://remote.test/users/carol",
        "https://remote.test/inbox",
    )];

    let addressing = derive_addressing(&status, &mentions, FOLLOWERS_URI);
    let recipients = derive_recipients(&addressing, &mentions, &[]);

    assert_eq!(
        recipients,
        vec![Recipient::Remote {
            inbox: "https://remote.test/inbox".to_string(),
            shared_inbox: None,
        }]
    );
}
