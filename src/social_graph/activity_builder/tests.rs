//! Unit tests for `ActivityBuilder` (Requirements 1.2, 1.3, 1.4, 2.3, 2.4,
//! 5.3, 5.4), per task 2.3's observable completion condition: "ローカル宛・
//! リモート宛で同一の論理 Activity が生成され、Undo が元 Activity を参照する
//! ことを単体テストで確認できる状態".
//!
//! Pure in-memory: no Postgres/DB anywhere in this file. [`MockLocalLookup`]/
//! [`MockRemoteLookup`] are plain in-memory [`LocalActorLookup`]/
//! [`RemoteActorLookup`] test doubles (mirrors
//! `crate::statuses::activity_builder::tests::MockActorLookup`'s identical
//! precedent).

use std::collections::HashMap;

use axum::http::StatusCode;

use super::*;
use crate::error::ErrorKind;
use crate::runtime::SeqIdGenerator;

// --- Test doubles -----------------------------------------------------

/// In-memory [`LocalActorLookup`]: `Id -> Handle` for known local actors, a
/// `404`-shaped [`AppError`] otherwise.
struct MockLocalLookup {
    known: HashMap<i64, Handle>,
}

impl MockLocalLookup {
    fn with_actors(pairs: &[(i64, &str)]) -> Self {
        Self {
            known: pairs
                .iter()
                .map(|(id, handle)| (*id, Handle::new(*handle).expect("valid test handle")))
                .collect(),
        }
    }
}

impl LocalActorLookup for MockLocalLookup {
    async fn resolve_handle(&self, id: Id) -> Result<Handle, AppError> {
        self.known.get(&id.as_i64()).cloned().ok_or_else(|| {
            AppError::client(
                StatusCode::NOT_FOUND,
                format!("account id {id:?} not known to MockLocalLookup"),
            )
        })
    }
}

/// In-memory [`RemoteActorLookup`]: `Id -> actor_uri` for known remote
/// accounts, a `404`-shaped [`AppError`] otherwise.
struct MockRemoteLookup {
    known: HashMap<i64, String>,
}

impl MockRemoteLookup {
    fn with_actors(pairs: &[(i64, &str)]) -> Self {
        Self {
            known: pairs
                .iter()
                .map(|(id, uri)| (*id, (*uri).to_string()))
                .collect(),
        }
    }
}

impl RemoteActorLookup for MockRemoteLookup {
    async fn resolve_actor_uri(&self, id: Id) -> Result<String, AppError> {
        self.known.get(&id.as_i64()).cloned().ok_or_else(|| {
            AppError::client(
                StatusCode::NOT_FOUND,
                format!("account id {id:?} not known to MockRemoteLookup"),
            )
        })
    }
}

// --- Fixtures -----------------------------------------------------------

type TestBuilder = ActivityBuilder<MockLocalLookup, MockRemoteLookup>;

fn builder(local_actors: &[(i64, &str)], remote_actors: &[(i64, &str)]) -> TestBuilder {
    let urls = ActorUrls::new("kawasemi.example");
    let ids = Arc::new(SeqIdGenerator::new(1000)) as Arc<dyn IdGenerator>;
    ActivityBuilder::new(
        urls,
        ids,
        MockLocalLookup::with_actors(local_actors),
        MockRemoteLookup::with_actors(remote_actors),
    )
}

fn value_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("expected string field {key:?} in {value}"))
}

// -- build_follow ------------------------------------------------------

#[tokio::test]
async fn build_follow_produces_canonical_follow_with_object_as_followee_actor_uri() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/bob")]);
    let follower = AccountRef::Local(Id::from_i64(1));
    let followee = AccountRef::Remote(Id::from_i64(2));

    let (id, activity) = b
        .build_follow(&follower, &followee)
        .await
        .expect("build_follow must succeed");

    assert_eq!(value_str(&activity, "id"), id);
    assert_eq!(value_str(&activity, "type"), "Follow");
    assert_eq!(
        value_str(&activity, "actor"),
        "https://kawasemi.example/users/alice"
    );
    assert_eq!(
        value_str(&activity, "object"),
        "https://remote.example/users/bob"
    );
}

#[tokio::test]
async fn build_follow_generates_the_identical_logical_activity_for_local_and_remote_followee() {
    // Requirements 1.3, 10.3: local-bound and remote-bound Follow must be the
    // same logical Activity (identical JSON shape/keys), differing only in
    // which URL the followee resolves to -- never in structure.
    let b = ActivityBuilder::new(
        ActorUrls::new("kawasemi.example"),
        Arc::new(SeqIdGenerator::new(3000)) as Arc<dyn IdGenerator>,
        MockLocalLookup::with_actors(&[(1, "alice"), (2, "carol")]),
        MockRemoteLookup::with_actors(&[(20, "https://remote.example/users/dave")]),
    );
    let follower = AccountRef::Local(Id::from_i64(1));
    let local_followee = AccountRef::Local(Id::from_i64(2));
    let remote_followee = AccountRef::Remote(Id::from_i64(20));

    let (_, local_activity) = b
        .build_follow(&follower, &local_followee)
        .await
        .expect("local build_follow must succeed");
    let (_, remote_activity) = b
        .build_follow(&follower, &remote_followee)
        .await
        .expect("remote build_follow must succeed");

    // Same set of top-level keys, same types for actor/object -- symmetric
    // shape regardless of followee locality.
    let mut local_keys: Vec<&String> = local_activity.as_object().unwrap().keys().collect();
    let mut remote_keys: Vec<&String> = remote_activity.as_object().unwrap().keys().collect();
    local_keys.sort();
    remote_keys.sort();
    assert_eq!(local_keys, remote_keys);
    assert_eq!(value_str(&local_activity, "type"), "Follow");
    assert_eq!(value_str(&remote_activity, "type"), "Follow");
    assert!(local_activity.get("object").unwrap().is_string());
    assert!(remote_activity.get("object").unwrap().is_string());
    assert_eq!(
        value_str(&local_activity, "object"),
        "https://kawasemi.example/users/carol"
    );
    assert_eq!(
        value_str(&remote_activity, "object"),
        "https://remote.example/users/dave"
    );
}

#[tokio::test]
async fn build_follow_mints_distinct_activity_ids_across_calls() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/bob")]);
    let follower = AccountRef::Local(Id::from_i64(1));
    let followee = AccountRef::Remote(Id::from_i64(2));

    let (id1, _) = b.build_follow(&follower, &followee).await.unwrap();
    let (id2, _) = b.build_follow(&follower, &followee).await.unwrap();

    assert_ne!(id1, id2, "each Follow must get its own fresh id");
}

#[tokio::test]
async fn build_follow_fails_when_local_follower_is_unknown() {
    let b = builder(&[], &[(2, "https://remote.example/users/bob")]);
    let follower = AccountRef::Local(Id::from_i64(999));
    let followee = AccountRef::Remote(Id::from_i64(2));

    let err = b
        .build_follow(&follower, &followee)
        .await
        .expect_err("unknown local follower must fail");
    assert_eq!(err.kind, ErrorKind::Client);
}

#[tokio::test]
async fn build_follow_fails_when_remote_followee_is_unknown() {
    let b = builder(&[(1, "alice")], &[]);
    let follower = AccountRef::Local(Id::from_i64(1));
    let followee = AccountRef::Remote(Id::from_i64(999));

    let err = b
        .build_follow(&follower, &followee)
        .await
        .expect_err("unknown remote followee must fail");
    assert_eq!(err.kind, ErrorKind::Client);
}

// -- build_block ---------------------------------------------------------

#[tokio::test]
async fn build_block_produces_canonical_block_with_object_as_blocked_actor_uri() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/eve")]);
    let blocker = AccountRef::Local(Id::from_i64(1));
    let blocked = AccountRef::Remote(Id::from_i64(2));

    let (id, activity) = b
        .build_block(&blocker, &blocked)
        .await
        .expect("build_block must succeed");

    assert_eq!(value_str(&activity, "id"), id);
    assert_eq!(value_str(&activity, "type"), "Block");
    assert_eq!(
        value_str(&activity, "actor"),
        "https://kawasemi.example/users/alice"
    );
    assert_eq!(
        value_str(&activity, "object"),
        "https://remote.example/users/eve"
    );
}

#[tokio::test]
async fn build_block_generates_the_identical_logical_activity_for_local_and_remote_blocked() {
    let b = ActivityBuilder::new(
        ActorUrls::new("kawasemi.example"),
        Arc::new(SeqIdGenerator::new(4000)) as Arc<dyn IdGenerator>,
        MockLocalLookup::with_actors(&[(1, "alice"), (2, "carol")]),
        MockRemoteLookup::with_actors(&[(20, "https://remote.example/users/dave")]),
    );
    let blocker = AccountRef::Local(Id::from_i64(1));
    let local_blocked = AccountRef::Local(Id::from_i64(2));
    let remote_blocked = AccountRef::Remote(Id::from_i64(20));

    let (_, local_activity) = b.build_block(&blocker, &local_blocked).await.unwrap();
    let (_, remote_activity) = b.build_block(&blocker, &remote_blocked).await.unwrap();

    let mut local_keys: Vec<&String> = local_activity.as_object().unwrap().keys().collect();
    let mut remote_keys: Vec<&String> = remote_activity.as_object().unwrap().keys().collect();
    local_keys.sort();
    remote_keys.sort();
    assert_eq!(local_keys, remote_keys);
    assert_eq!(value_str(&local_activity, "type"), "Block");
    assert_eq!(value_str(&remote_activity, "type"), "Block");
}

// -- build_accept / build_reject ------------------------------------------

#[tokio::test]
async fn build_accept_embeds_the_received_follow_id_type_actor_and_object() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/bob")]);
    let target = AccountRef::Local(Id::from_i64(1));
    let source = AccountRef::Remote(Id::from_i64(2));

    let accept = b
        .build_accept(
            &target,
            "https://remote.example/activities/follow/42",
            &source,
        )
        .await
        .expect("build_accept must succeed");

    assert_eq!(value_str(&accept, "type"), "Accept");
    assert_eq!(
        value_str(&accept, "actor"),
        "https://kawasemi.example/users/alice"
    );
    let inner = accept.get("object").expect("object present");
    assert_eq!(
        value_str(inner, "id"),
        "https://remote.example/activities/follow/42"
    );
    assert_eq!(value_str(inner, "type"), "Follow");
    assert_eq!(
        value_str(inner, "actor"),
        "https://remote.example/users/bob"
    );
    assert_eq!(
        value_str(inner, "object"),
        "https://kawasemi.example/users/alice"
    );
}

#[tokio::test]
async fn build_reject_embeds_the_received_follow_id_and_uses_reject_type() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/bob")]);
    let target = AccountRef::Local(Id::from_i64(1));
    let source = AccountRef::Remote(Id::from_i64(2));

    let reject = b
        .build_reject(
            &target,
            "https://remote.example/activities/follow/43",
            &source,
        )
        .await
        .expect("build_reject must succeed");

    assert_eq!(value_str(&reject, "type"), "Reject");
    let inner = reject.get("object").expect("object present");
    assert_eq!(
        value_str(inner, "id"),
        "https://remote.example/activities/follow/43"
    );
    assert_eq!(value_str(inner, "type"), "Follow");
}

#[tokio::test]
async fn build_accept_and_build_reject_differ_only_in_outer_type() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/bob")]);
    let target = AccountRef::Local(Id::from_i64(1));
    let source = AccountRef::Remote(Id::from_i64(2));

    let accept = b
        .build_accept(&target, "same-follow-id", &source)
        .await
        .unwrap();
    let reject = b
        .build_reject(&target, "same-follow-id", &source)
        .await
        .unwrap();

    assert_eq!(value_str(&accept, "type"), "Accept");
    assert_eq!(value_str(&reject, "type"), "Reject");
    assert_eq!(accept.get("actor"), reject.get("actor"));
    assert_eq!(accept.get("object"), reject.get("object"));
}

// -- build_undo ------------------------------------------------------------

#[tokio::test]
async fn build_undo_follow_references_the_original_activity_id_not_a_freshly_minted_one() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/bob")]);
    let follower = AccountRef::Local(Id::from_i64(1));
    let followee = AccountRef::Remote(Id::from_i64(2));

    // The caller rebuilds the logical Follow to hand to build_undo (this
    // spec's Follow row persists only the outbound `activity_id`, not the
    // full original JSON -- see model.rs's own doc comment), discarding the
    // freshly-minted id this second build_follow call produces.
    let (_fresh_id, rebuilt_follow) = b.build_follow(&follower, &followee).await.unwrap();
    let original_activity_id = "https://kawasemi.example/activities/999";

    let undo = b
        .build_undo(&follower, original_activity_id, rebuilt_follow)
        .await
        .expect("build_undo must succeed");

    assert_eq!(value_str(&undo, "type"), "Undo");
    assert_eq!(
        value_str(&undo, "actor"),
        "https://kawasemi.example/users/alice"
    );
    let inner = undo.get("object").expect("inner object present");
    assert_eq!(
        value_str(inner, "id"),
        original_activity_id,
        "Undo's object must reference the original Activity's persisted id, \
         not whatever id happened to be baked into the rebuilt wrapped value"
    );
    assert_eq!(value_str(inner, "type"), "Follow");
    assert_ne!(
        value_str(&undo, "id"),
        value_str(inner, "id"),
        "the outer Undo gets its own fresh id, distinct from the wrapped reference"
    );
}

#[tokio::test]
async fn build_undo_block_references_the_original_activity_id() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/eve")]);
    let blocker = AccountRef::Local(Id::from_i64(1));
    let blocked = AccountRef::Remote(Id::from_i64(2));

    let (_fresh_id, rebuilt_block) = b.build_block(&blocker, &blocked).await.unwrap();
    let original_activity_id = "https://kawasemi.example/activities/555";

    let undo = b
        .build_undo(&blocker, original_activity_id, rebuilt_block)
        .await
        .expect("build_undo must succeed");

    assert_eq!(value_str(&undo, "type"), "Undo");
    let inner = undo.get("object").expect("inner object present");
    assert_eq!(value_str(inner, "id"), original_activity_id);
    assert_eq!(value_str(inner, "type"), "Block");
}

#[tokio::test]
async fn build_undo_mints_a_fresh_outer_id_distinct_across_calls() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/bob")]);
    let actor = AccountRef::Local(Id::from_i64(1));
    let followee = AccountRef::Remote(Id::from_i64(2));
    let (_, wrapped1) = b.build_follow(&actor, &followee).await.unwrap();
    let (_, wrapped2) = b.build_follow(&actor, &followee).await.unwrap();

    let undo1 = b.build_undo(&actor, "orig-1", wrapped1).await.unwrap();
    let undo2 = b.build_undo(&actor, "orig-1", wrapped2).await.unwrap();

    assert_ne!(
        value_str(&undo1, "id"),
        value_str(&undo2, "id"),
        "each Undo call must mint its own fresh outer id"
    );
}

#[tokio::test]
async fn build_undo_fails_when_actor_is_unknown() {
    let b = builder(&[(1, "alice")], &[(2, "https://remote.example/users/bob")]);
    let known_follower = AccountRef::Local(Id::from_i64(1));
    let followee = AccountRef::Remote(Id::from_i64(2));
    let (_, wrapped) = b.build_follow(&known_follower, &followee).await.unwrap();

    let unknown_actor = AccountRef::Local(Id::from_i64(404));
    let err = b
        .build_undo(&unknown_actor, "orig-1", wrapped)
        .await
        .expect_err("unknown actor must fail");
    assert_eq!(err.kind, ErrorKind::Client);
}
