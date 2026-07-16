//! Unit tests for `RecipientTargetResolver` (Requirements 10.3, 10.4, 11.4),
//! per task 3.4's observable completion condition: "ローカル/リモート混在
//! recipient が正しく分類され、同一 shared inbox 宛が 1 件に畳まれる単体
//! テストが通る". Pure in-memory: no Postgres/DB involved anywhere in this
//! file — [`MockLocalActorLookup`] is a plain in-memory [`LocalActorLookup`]
//! test double (see this module's parent doc comment, "`LocalActorLookup`:
//! a narrow mockable port over `ActorDirectory`").

use std::collections::HashSet;

use axum::http::StatusCode;

use super::*;
use crate::actor::{ActorState, ActorType};
use crate::domain::Id;
use crate::error::ErrorKind;

/// A plain in-memory [`LocalActorLookup`] test double: `known_handles`
/// answers `Some(_)`, everything else answers `None`, mirroring
/// `ActorDirectory::resolve_actor_by_handle`'s own "no error for absence"
/// contract.
struct MockLocalActorLookup {
    known_handles: HashSet<String>,
}

impl MockLocalActorLookup {
    fn with_handles(handles: &[&str]) -> Self {
        Self {
            known_handles: handles.iter().map(|h| (*h).to_string()).collect(),
        }
    }
}

impl LocalActorLookup for MockLocalActorLookup {
    async fn resolve_actor_by_handle(
        &self,
        handle: &Handle,
    ) -> Result<Option<ResolvedActor>, AppError> {
        if self.known_handles.contains(handle.as_str()) {
            Ok(Some(ResolvedActor {
                id: Id::from_i64(1),
                handle: handle.clone(),
                actor_type: ActorType::Person,
                display_name: "Test Actor".to_string(),
                summary: String::new(),
                state: ActorState::Active,
            }))
        } else {
            Ok(None)
        }
    }
}

fn handle(raw: &str) -> Handle {
    Handle::new(raw).expect("test handle must be valid")
}

fn remote(inbox: &str) -> Recipient {
    Recipient::Remote {
        inbox: inbox.to_string(),
        shared_inbox: None,
    }
}

fn remote_with_shared(inbox: &str, shared_inbox: &str) -> Recipient {
    Recipient::Remote {
        inbox: inbox.to_string(),
        shared_inbox: Some(shared_inbox.to_string()),
    }
}

// --- 1: a local recipient for a handle that exists resolves to
// DeliveryTarget::Local. ---

#[tokio::test]
async fn local_recipient_with_existing_handle_resolves_to_local_target() {
    let resolver = RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&["alice"]));

    let targets = resolver
        .resolve(&[Recipient::Local(handle("alice"))])
        .await
        .expect("resolve must succeed for a known local handle");

    assert_eq!(
        targets,
        vec![DeliveryTarget::Local {
            handle: handle("alice")
        }]
    );
}

// --- 2: a remote recipient with no shared inbox resolves to
// DeliveryTarget::Remote using its own individual inbox. ---

#[tokio::test]
async fn remote_recipient_without_shared_inbox_resolves_to_its_own_inbox() {
    let resolver = RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&[]));

    let targets = resolver
        .resolve(&[remote("https://remote.example/users/bob/inbox")])
        .await
        .expect("resolve must succeed for a remote recipient");

    assert_eq!(
        targets,
        vec![DeliveryTarget::Remote {
            inbox: "https://remote.example/users/bob/inbox".to_string()
        }]
    );
}

// --- 3: mixed local + remote recipients in one resolve() call are
// classified into the right variants. ---

#[tokio::test]
async fn mixed_local_and_remote_recipients_are_classified_correctly() {
    let resolver = RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&["alice"]));

    let targets = resolver
        .resolve(&[
            Recipient::Local(handle("alice")),
            remote("https://remote.example/users/bob/inbox"),
        ])
        .await
        .expect("resolve must succeed for a mixed local/remote list");

    assert_eq!(
        targets,
        vec![
            DeliveryTarget::Local {
                handle: handle("alice")
            },
            DeliveryTarget::Remote {
                inbox: "https://remote.example/users/bob/inbox".to_string()
            },
        ]
    );
}

// --- 4: multiple remote recipients sharing the same shared_inbox collapse
// into exactly one DeliveryTarget::Remote. ---

#[tokio::test]
async fn remote_recipients_sharing_a_shared_inbox_collapse_into_one_target() {
    let resolver = RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&[]));

    let targets = resolver
        .resolve(&[
            remote_with_shared(
                "https://remote.example/users/bob/inbox",
                "https://remote.example/inbox",
            ),
            remote_with_shared(
                "https://remote.example/users/carol/inbox",
                "https://remote.example/inbox",
            ),
            remote_with_shared(
                "https://remote.example/users/dave/inbox",
                "https://remote.example/inbox",
            ),
        ])
        .await
        .expect("resolve must succeed for shared-inbox recipients");

    assert_eq!(
        targets,
        vec![DeliveryTarget::Remote {
            inbox: "https://remote.example/inbox".to_string()
        }],
        "3 recipients sharing one shared inbox must collapse into exactly 1 target"
    );
}

// --- 5: remote recipients with DIFFERENT shared inboxes (or no shared
// inbox) are NOT incorrectly merged together. ---

#[tokio::test]
async fn remote_recipients_with_different_shared_inboxes_are_not_merged() {
    let resolver = RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&[]));

    let targets = resolver
        .resolve(&[
            remote_with_shared(
                "https://a.example/users/bob/inbox",
                "https://a.example/inbox",
            ),
            remote_with_shared(
                "https://b.example/users/carol/inbox",
                "https://b.example/inbox",
            ),
            remote("https://c.example/users/dave/inbox"),
        ])
        .await
        .expect("resolve must succeed for distinct remote destinations");

    assert_eq!(
        targets,
        vec![
            DeliveryTarget::Remote {
                inbox: "https://a.example/inbox".to_string()
            },
            DeliveryTarget::Remote {
                inbox: "https://b.example/inbox".to_string()
            },
            DeliveryTarget::Remote {
                inbox: "https://c.example/users/dave/inbox".to_string()
            },
        ],
        "distinct shared inboxes and a no-shared-inbox recipient must all remain separate targets"
    );
}

// --- 5b: two no-shared-inbox remote recipients with the identical
// individual inbox URL also collapse (general "never two Remote entries
// with the same inbox string" rule). ---

#[tokio::test]
async fn remote_recipients_with_identical_individual_inbox_and_no_shared_inbox_collapse() {
    let resolver = RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&[]));

    let targets = resolver
        .resolve(&[
            remote("https://remote.example/users/bob/inbox"),
            remote("https://remote.example/users/bob/inbox"),
        ])
        .await
        .expect("resolve must succeed");

    assert_eq!(
        targets,
        vec![DeliveryTarget::Remote {
            inbox: "https://remote.example/users/bob/inbox".to_string()
        }]
    );
}

// --- 6: a local recipient whose handle does NOT resolve to an existing
// local actor fails the whole resolve() call with a 404-shaped Client
// AppError (this module's documented "whole call fails" decision). ---

#[tokio::test]
async fn local_recipient_with_unknown_handle_fails_the_whole_call() {
    let resolver = RecipientTargetResolver::new(MockLocalActorLookup::with_handles(&["alice"]));

    let err = resolver
        .resolve(&[
            Recipient::Local(handle("alice")),
            Recipient::Local(handle("ghost")),
        ])
        .await
        .expect_err("resolve must fail when a local handle does not resolve");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::NOT_FOUND);
}
