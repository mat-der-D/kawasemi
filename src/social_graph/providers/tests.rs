//! DB-backed tests for `BlockPolicyImpl` (Requirements 6.1, 6.2, 6.3, 6.4),
//! per task 4.2's own observable completion condition: "ブロック中の署名者
//! について個別アクター向け文脈でブロック判定が真を返し、共有 inbox 文脈
//! では常に偽を返し、解除後に偽へ戻ることを単体/統合で確認できる状態".
//!
//! Mirrors `inbound/tests.rs`'s established convention:
//! `crate::test_harness::db_fixture::spawn_test_db` for an isolated, already-migrated
//! schema; `create_test_actor` for real `local_actors` rows (needed here
//! because [`BlockPolicyImpl`]'s destination-side resolution goes through a
//! real [`ActorDirectory`]); a small in-memory `FakeActorUriResolver` (not
//! `ProdActorUriResolver`) standing in for signer-side resolution, per that
//! module's own doc comment rationale for the identical choice.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;

use super::*;
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{ActorState, ActorType, Handle};
use crate::domain::Id;
use crate::federation::inbound::block_policy::{BlockPolicy, LocalRecipientContext};
use crate::social_graph::model::Block;
use crate::social_graph::repository as sg_repository;
use crate::test_harness::db_fixture::{TestDb, spawn_test_db};

const TEST_DOMAIN: &str = "kawasemi.example";

/// Creates a real owner + local actor row, returning the actor's `Id` --
/// mirrors `inbound/tests.rs::create_test_actor` exactly.
async fn create_test_actor(db: &TestDb, handle: &str) -> Id {
    let now = db.runtime.clock.now();
    let owner_id = db.runtime.ids.next_id();
    create_owner(&db.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = db.runtime.ids.next_id();
    let actor = crate::actor::model::LocalActor {
        id: actor_id,
        owner_id,
        handle: Handle::new(handle).expect("test handle must be valid"),
        actor_type: ActorType::Person,
        display_name: "Test Actor".to_string(),
        summary: "a test actor".to_string(),
        state: ActorState::Active,
        created_at: now,
        updated_at: now,
    };
    let mut tx = db
        .pool
        .begin()
        .await
        .expect("opening a transaction must succeed");
    insert_actor(&mut tx, &actor)
        .await
        .expect("insert_actor must succeed");
    tx.commit().await.expect("committing must succeed");

    actor_id
}

fn actor_url(handle: &str) -> String {
    format!("https://{TEST_DOMAIN}/users/{handle}")
}

/// An in-memory [`ActorUriResolver`] test double for the signer side --
/// mirrors `inbound/tests.rs::FakeActorUriResolver` exactly (an unmapped URI
/// is a genuine `Err`, not a silent default -- see `providers.rs`'s own doc
/// comment, "Resolving the signer", for why an unresolvable signer must
/// propagate as an error here).
#[derive(Clone, Default)]
struct FakeActorUriResolver {
    map: Arc<Mutex<HashMap<String, AccountRef>>>,
}

impl FakeActorUriResolver {
    fn new() -> Self {
        Self::default()
    }

    fn with(self, uri: impl Into<String>, account: AccountRef) -> Self {
        self.map.lock().unwrap().insert(uri.into(), account);
        self
    }
}

impl ActorUriResolver for FakeActorUriResolver {
    fn resolve_account_ref(
        &self,
        actor_uri: &str,
    ) -> impl Future<Output = Result<AccountRef, AppError>> + Send {
        let result = self
            .map
            .lock()
            .unwrap()
            .get(actor_uri)
            .copied()
            .ok_or_else(|| {
                AppError::client(
                    StatusCode::NOT_FOUND,
                    format!("unknown actor uri '{actor_uri}' in FakeActorUriResolver"),
                )
            });
        async move { result }
    }
}

fn build_policy(
    db: &TestDb,
    actor_uris: FakeActorUriResolver,
) -> BlockPolicyImpl<FakeActorUriResolver> {
    BlockPolicyImpl::new(
        db.pool.clone(),
        TEST_DOMAIN,
        Arc::new(ActorDirectory::new(db.pool.clone())),
        actor_uris,
    )
}

async fn upsert_block(db: &TestDb, blocker: AccountRef, blocked: AccountRef) {
    sg_repository::upsert_block(
        &db.pool,
        db.runtime.ids.next_id(),
        &Block {
            blocker,
            blocked,
            activity_id: "https://example.test/activities/block-1".to_string(),
            created_at: db.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_block must succeed");
}

/// Requirements 6.1, 6.2: a blocked remote signer is reported blocked from
/// the destination local actor's own `Actor`-context perspective.
#[tokio::test]
async fn actor_context_reports_true_when_destination_has_blocked_the_signer() {
    let db = spawn_test_db().await;
    let dest_id = create_test_actor(&db, "dest").await;
    let dest = AccountRef::Local(dest_id);
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(db.runtime.ids.next_id());

    upsert_block(&db, dest, signer).await;

    let policy = build_policy(&db, FakeActorUriResolver::new().with(signer_uri, signer));

    let blocked = policy
        .is_blocked(
            signer_uri,
            LocalRecipientContext::Actor {
                actor_uri: actor_url("dest"),
            },
        )
        .await
        .expect("is_blocked must succeed");
    assert!(blocked, "a blocked signer must be reported as blocked");

    db.cleanup().await;
}

/// Requirement 6.1: an unblocked signer is reported not-blocked.
#[tokio::test]
async fn actor_context_reports_false_when_no_block_exists() {
    let db = spawn_test_db().await;
    create_test_actor(&db, "dest").await;
    let signer_uri = "https://remote.example/users/friend";
    let signer = AccountRef::Remote(db.runtime.ids.next_id());

    let policy = build_policy(&db, FakeActorUriResolver::new().with(signer_uri, signer));

    let blocked = policy
        .is_blocked(
            signer_uri,
            LocalRecipientContext::Actor {
                actor_uri: actor_url("dest"),
            },
        )
        .await
        .expect("is_blocked must succeed");
    assert!(
        !blocked,
        "an unblocked signer must not be reported as blocked"
    );

    db.cleanup().await;
}

/// Requirement 6.1: the judgment is scoped to the *destination* local
/// actor -- a block recorded by a different local actor must not leak into
/// another local actor's own judgment.
#[tokio::test]
async fn actor_context_is_scoped_to_the_destination_local_actor() {
    let db = spawn_test_db().await;
    let dest_id = create_test_actor(&db, "dest").await;
    let other_id = create_test_actor(&db, "other").await;
    let dest = AccountRef::Local(dest_id);
    let other = AccountRef::Local(other_id);
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(db.runtime.ids.next_id());

    // `other` (not `dest`) has blocked the signer.
    upsert_block(&db, other, signer).await;

    let policy = build_policy(&db, FakeActorUriResolver::new().with(signer_uri, signer));

    let blocked = policy
        .is_blocked(
            signer_uri,
            LocalRecipientContext::Actor {
                actor_uri: actor_url("dest"),
            },
        )
        .await
        .expect("is_blocked must succeed");
    assert!(
        !blocked,
        "a block recorded by a different local actor must not affect this destination's own \
         judgment"
    );
    let _ = dest;

    db.cleanup().await;
}

/// Requirement 6.3: a `SharedInbox` context must always answer `false`,
/// even when the signer is genuinely blocked by a known local actor --
/// never bulk-reject at this point in the pipeline.
#[tokio::test]
async fn shared_inbox_context_always_reports_false_even_when_blocked() {
    let db = spawn_test_db().await;
    let dest_id = create_test_actor(&db, "dest").await;
    let dest = AccountRef::Local(dest_id);
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(db.runtime.ids.next_id());

    upsert_block(&db, dest, signer).await;

    let policy = build_policy(&db, FakeActorUriResolver::new().with(signer_uri, signer));

    let blocked = policy
        .is_blocked(signer_uri, LocalRecipientContext::SharedInbox)
        .await
        .expect("is_blocked must succeed");
    assert!(
        !blocked,
        "SharedInbox must never be bulk-rejected, even for a genuinely blocked signer"
    );

    db.cleanup().await;
}

/// Requirement 6.4: once a block is undone (row deleted), the very next
/// `is_blocked` call for the same pair must answer `false` again -- live DB
/// state each call, no cached verdict.
#[tokio::test]
async fn returns_false_again_after_unblock() {
    let db = spawn_test_db().await;
    let dest_id = create_test_actor(&db, "dest").await;
    let dest = AccountRef::Local(dest_id);
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(db.runtime.ids.next_id());

    upsert_block(&db, dest, signer).await;

    let policy = build_policy(&db, FakeActorUriResolver::new().with(signer_uri, signer));
    let destination = LocalRecipientContext::Actor {
        actor_uri: actor_url("dest"),
    };

    assert!(
        policy
            .is_blocked(signer_uri, destination.clone())
            .await
            .expect("is_blocked must succeed"),
        "must be blocked before unblock"
    );

    sg_repository::delete_block(&db.pool, &dest, &signer)
        .await
        .expect("delete_block must succeed");

    assert!(
        !policy
            .is_blocked(signer_uri, destination)
            .await
            .expect("is_blocked must succeed"),
        "must not be blocked after unblock"
    );

    db.cleanup().await;
}

/// This module's own doc comment ("Resolving the destination"): an `Actor`
/// URI that does not currently name a known local actor is a benign `false`,
/// not an error.
#[tokio::test]
async fn actor_context_reports_false_when_destination_does_not_resolve() {
    let db = spawn_test_db().await;
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(db.runtime.ids.next_id());

    let policy = build_policy(&db, FakeActorUriResolver::new().with(signer_uri, signer));

    let blocked = policy
        .is_blocked(
            signer_uri,
            LocalRecipientContext::Actor {
                actor_uri: actor_url("nobody"),
            },
        )
        .await
        .expect("is_blocked must succeed");
    assert!(
        !blocked,
        "an unresolvable destination actor URI must be a benign false, not an error"
    );

    db.cleanup().await;
}

/// A same-server (local-to-local) block is judged the same way as a
/// remote signer's -- the destination-side resolution path does not
/// special-case the signer's own locality.
#[tokio::test]
async fn actor_context_reports_true_for_a_blocked_local_signer() {
    let db = spawn_test_db().await;
    let dest_id = create_test_actor(&db, "dest").await;
    let signer_id = create_test_actor(&db, "signer").await;
    let dest = AccountRef::Local(dest_id);
    let signer = AccountRef::Local(signer_id);
    let signer_uri = actor_url("signer");

    upsert_block(&db, dest, signer).await;

    let policy = build_policy(
        &db,
        FakeActorUriResolver::new().with(signer_uri.clone(), signer),
    );

    let blocked = policy
        .is_blocked(
            &signer_uri,
            LocalRecipientContext::Actor {
                actor_uri: actor_url("dest"),
            },
        )
        .await
        .expect("is_blocked must succeed");
    assert!(
        blocked,
        "a blocked local-to-local signer must be reported as blocked"
    );

    db.cleanup().await;
}

// -- RelProviderImpl / FilterQuery (task 4.3, Requirements 8.2, 8.3, 8.4, ---
// -- 9.1, 9.2, 9.3, 9.4) -----------------------------------------------

use time::Duration;

use crate::accounts::ports::RelationshipStateProvider;
use crate::social_graph::model::{Follow, Mute};

async fn upsert_follow(db: &TestDb, follower: AccountRef, followee: AccountRef, reblogs: bool) {
    sg_repository::upsert_follow(
        &db.pool,
        db.runtime.ids.next_id(),
        &Follow {
            follower,
            followee,
            reblogs,
            notify: false,
            languages: Vec::new(),
            activity_id: "https://example.test/activities/follow-1".to_string(),
            created_at: db.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_follow must succeed");
}

async fn upsert_mute(
    db: &TestDb,
    muter: AccountRef,
    muted: AccountRef,
    notifications: bool,
    expires_at: Option<time::OffsetDateTime>,
) {
    sg_repository::upsert_mute(
        &db.pool,
        db.runtime.ids.next_id(),
        &Mute {
            muter,
            muted,
            notifications,
            expires_at,
            created_at: db.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_mute must succeed");
}

/// Requirements 8.2, 8.3, 8.4: `RelProviderImpl::relationships` must return
/// one `RelationshipView` per target, in the same order as `targets`, with
/// each view's flags derived from that target's real relationship state
/// (not `NoRelationshipProvider`'s always-false defaults).
#[tokio::test]
async fn rel_provider_returns_real_flags_in_target_order() {
    let db = spawn_test_db().await;
    let viewer_id = db.runtime.ids.next_id();
    let viewer = AccountRef::Local(viewer_id);
    let followed = AccountRef::Remote(db.runtime.ids.next_id());
    let blocker = AccountRef::Remote(db.runtime.ids.next_id());

    upsert_follow(&db, viewer, followed, true).await;
    upsert_block(&db, blocker, viewer).await;

    let provider = RelProviderImpl::new(db.pool.clone(), db.runtime.clone());

    // Deliberately ordered [blocker, followed] -- the reverse of insertion
    // order -- to prove the output follows the *targets* slice's order, not
    // insertion or id order.
    let targets = vec![blocker, followed];
    let views = provider
        .relationships(viewer_id, &targets)
        .await
        .expect("relationships must succeed");

    assert_eq!(views.len(), 2);
    let blocker_id = match blocker {
        AccountRef::Remote(id) => id,
        AccountRef::Local(id) => id,
    };
    let followed_id = match followed {
        AccountRef::Remote(id) => id,
        AccountRef::Local(id) => id,
    };
    assert_eq!(
        views[0].id, blocker_id,
        "output order must match targets order"
    );
    assert!(
        views[0].blocked_by,
        "blocker must be reported as blocked_by"
    );
    assert!(!views[0].following);

    assert_eq!(views[1].id, followed_id);
    assert!(
        views[1].following,
        "followed target must be reported as following"
    );
    assert!(views[1].showing_reblogs);
    assert!(!views[1].blocked_by);

    db.cleanup().await;
}

/// Requirements 8.4, 9.3: an expired mute must surface as `muting: false`
/// through `RelProviderImpl`, exactly as `RelationshipMapper`/`load_states`
/// already guarantee (task 2.4/1.3) -- this only proves the provider wires
/// those already-correct pieces together without re-introducing the
/// expired mute.
#[tokio::test]
async fn rel_provider_excludes_an_expired_mute() {
    let db = spawn_test_db().await;
    let viewer_id = db.runtime.ids.next_id();
    let viewer = AccountRef::Local(viewer_id);
    let muted = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    upsert_mute(&db, viewer, muted, true, Some(now - Duration::seconds(1))).await;

    let provider = RelProviderImpl::new(db.pool.clone(), db.runtime.clone());
    let views = provider
        .relationships(viewer_id, &[muted])
        .await
        .expect("relationships must succeed");

    assert!(
        !views[0].muting,
        "an expired mute must not be reported as an active mute"
    );
    assert!(!views[0].muting_notifications);

    db.cleanup().await;
}

/// Requirement 9.1: `FilterQuery::blocked_set` must report blocked/
/// blocked-by/muted/muted-notifications sets from real relationship state.
#[tokio::test]
async fn filter_query_blocked_set_reports_real_sets() {
    let db = spawn_test_db().await;
    let viewer = AccountRef::Local(db.runtime.ids.next_id());
    let blocked = AccountRef::Remote(db.runtime.ids.next_id());
    let blocked_by_account = AccountRef::Remote(db.runtime.ids.next_id());
    let muted_notif = AccountRef::Remote(db.runtime.ids.next_id());
    let muted_plain = AccountRef::Remote(db.runtime.ids.next_id());

    upsert_block(&db, viewer, blocked).await;
    upsert_block(&db, blocked_by_account, viewer).await;
    upsert_mute(&db, viewer, muted_notif, true, None).await;
    upsert_mute(&db, viewer, muted_plain, false, None).await;

    let query = FilterQuery::new(db.pool.clone(), db.runtime.clone());
    let sets = query
        .blocked_set(&viewer)
        .await
        .expect("blocked_set must succeed");

    assert_eq!(sets.blocked, vec![blocked]);
    assert_eq!(sets.blocked_by, vec![blocked_by_account]);
    assert_eq!(sets.muted_notifications, vec![muted_notif]);
    let mut muted = sets.muted.clone();
    muted.sort_by_key(|a| match a {
        AccountRef::Local(id) | AccountRef::Remote(id) => id.as_i64(),
    });
    let mut expected = vec![muted_notif, muted_plain];
    expected.sort_by_key(|a| match a {
        AccountRef::Local(id) | AccountRef::Remote(id) => id.as_i64(),
    });
    assert_eq!(muted, expected);

    db.cleanup().await;
}

/// Requirement 9.3: an expired mute must not appear in
/// `FilterQuery::blocked_set`'s `muted`/`muted_notifications` sets.
#[tokio::test]
async fn filter_query_blocked_set_excludes_expired_mutes() {
    let db = spawn_test_db().await;
    let viewer = AccountRef::Local(db.runtime.ids.next_id());
    let muted = AccountRef::Remote(db.runtime.ids.next_id());
    let now = db.runtime.clock.now();

    upsert_mute(&db, viewer, muted, true, Some(now - Duration::seconds(1))).await;

    let query = FilterQuery::new(db.pool.clone(), db.runtime.clone());
    let sets = query
        .blocked_set(&viewer)
        .await
        .expect("blocked_set must succeed");

    assert!(sets.muted.is_empty());
    assert!(sets.muted_notifications.is_empty());

    db.cleanup().await;
}

/// Requirement 9.2: `FilterQuery::following_set` must report the viewer's
/// established follow targets.
#[tokio::test]
async fn filter_query_following_set_reports_follow_targets() {
    let db = spawn_test_db().await;
    let viewer = AccountRef::Local(db.runtime.ids.next_id());
    let followed = AccountRef::Remote(db.runtime.ids.next_id());
    let not_followed = AccountRef::Remote(db.runtime.ids.next_id());
    let _ = not_followed;

    upsert_follow(&db, viewer, followed, true).await;

    let query = FilterQuery::new(db.pool.clone(), db.runtime.clone());
    let following = query
        .following_set(&viewer)
        .await
        .expect("following_set must succeed");

    assert_eq!(following, vec![followed]);

    db.cleanup().await;
}

/// Task 4.3's own acceptance text: `reblogs_hidden` must return the
/// `show_reblogs = false` subset of the viewer's follow targets, excluding
/// follows with reblogs shown.
#[tokio::test]
async fn filter_query_reblogs_hidden_set_returns_only_show_reblogs_false_targets() {
    let db = spawn_test_db().await;
    let viewer = AccountRef::Local(db.runtime.ids.next_id());
    let hidden = AccountRef::Remote(db.runtime.ids.next_id());
    let shown = AccountRef::Remote(db.runtime.ids.next_id());

    upsert_follow(&db, viewer, hidden, false).await;
    upsert_follow(&db, viewer, shown, true).await;

    let query = FilterQuery::new(db.pool.clone(), db.runtime.clone());
    let reblogs_hidden = query
        .reblogs_hidden_set(&viewer)
        .await
        .expect("reblogs_hidden_set must succeed");

    assert_eq!(reblogs_hidden, vec![hidden]);

    db.cleanup().await;
}

// -- AccountCountsProviderImpl (task 4.4, Requirement 8.2) -----------------

use crate::accounts::ports::AccountCountsProvider;

/// Boundary Commitments / task 4.4's own acceptance text: `counts` must
/// report real `followers`/`following` values derived from the `follows`
/// table, while leaving `statuses`/`last_status_at` at accounts-and-
/// instance's own zero/`None` defaults (this spec does not own those
/// counts).
#[tokio::test]
async fn account_counts_provider_reports_real_followers_and_following() {
    let db = spawn_test_db().await;
    let target = AccountRef::Local(db.runtime.ids.next_id());
    let follower_a = AccountRef::Remote(db.runtime.ids.next_id());
    let follower_b = AccountRef::Remote(db.runtime.ids.next_id());
    let followee = AccountRef::Remote(db.runtime.ids.next_id());

    // Two accounts follow `target` (followers = 2).
    upsert_follow(&db, follower_a, target, true).await;
    upsert_follow(&db, follower_b, target, true).await;
    // `target` follows one account (following = 1).
    upsert_follow(&db, target, followee, true).await;

    let provider = AccountCountsProviderImpl::new(db.pool.clone());
    let counts = provider.counts(&target).await.expect("counts must succeed");

    assert_eq!(counts.followers, 2);
    assert_eq!(counts.following, 1);
    assert_eq!(counts.statuses, 0, "statuses is out of this spec's scope");
    assert_eq!(
        counts.last_status_at, None,
        "last_status_at is out of this spec's scope"
    );

    db.cleanup().await;
}

/// `counts` for an account nobody follows and who follows nobody must
/// report all-zero followers/following, not error.
#[tokio::test]
async fn account_counts_provider_reports_zero_for_an_unconnected_account() {
    let db = spawn_test_db().await;
    let target = AccountRef::Local(db.runtime.ids.next_id());

    let provider = AccountCountsProviderImpl::new(db.pool.clone());
    let counts = provider.counts(&target).await.expect("counts must succeed");

    assert_eq!(counts.followers, 0);
    assert_eq!(counts.following, 0);

    db.cleanup().await;
}
