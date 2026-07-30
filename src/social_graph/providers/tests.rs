//! DB-backed tests for `BlockPolicyImpl` (Requirements 6.1, 6.2, 6.3, 6.4),
//! per task 4.2's own observable completion condition: "ブロック中の署名者
//! について個別アクター向け文脈でブロック判定が真を返し、共有 inbox 文脈
//! では常に偽を返し、解除後に偽へ戻ることを単体/統合で確認できる状態".
//!
//! Mirrors `inbound/tests.rs`'s established convention:
//! `crate::test_harness::spawn_test_app` for an isolated, already-migrated
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
use crate::test_harness::{TestApp, spawn_test_app};

const TEST_DOMAIN: &str = "kawasemi.example";

/// Creates a real owner + local actor row, returning the actor's `Id` --
/// mirrors `inbound/tests.rs::create_test_actor` exactly.
async fn create_test_actor(app: &TestApp, handle: &str) -> Id {
    let now = app.runtime.clock.now();
    let owner_id = app.runtime.ids.next_id();
    create_owner(&app.pool, owner_id, now)
        .await
        .expect("creating the owner must succeed");

    let actor_id = app.runtime.ids.next_id();
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
    let mut tx = app
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
    app: &TestApp,
    actor_uris: FakeActorUriResolver,
) -> BlockPolicyImpl<FakeActorUriResolver> {
    BlockPolicyImpl::new(
        app.pool.clone(),
        TEST_DOMAIN,
        Arc::new(ActorDirectory::new(app.pool.clone())),
        actor_uris,
    )
}

async fn upsert_block(app: &TestApp, blocker: AccountRef, blocked: AccountRef) {
    sg_repository::upsert_block(
        &app.pool,
        app.runtime.ids.next_id(),
        &Block {
            blocker,
            blocked,
            activity_id: "https://example.test/activities/block-1".to_string(),
            created_at: app.runtime.clock.now(),
        },
    )
    .await
    .expect("upsert_block must succeed");
}

/// Requirements 6.1, 6.2: a blocked remote signer is reported blocked from
/// the destination local actor's own `Actor`-context perspective.
#[tokio::test]
async fn actor_context_reports_true_when_destination_has_blocked_the_signer() {
    let app = spawn_test_app().await;
    let dest_id = create_test_actor(&app, "dest").await;
    let dest = AccountRef::Local(dest_id);
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(app.runtime.ids.next_id());

    upsert_block(&app, dest, signer).await;

    let policy = build_policy(&app, FakeActorUriResolver::new().with(signer_uri, signer));

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

    app.cleanup().await;
}

/// Requirement 6.1: an unblocked signer is reported not-blocked.
#[tokio::test]
async fn actor_context_reports_false_when_no_block_exists() {
    let app = spawn_test_app().await;
    create_test_actor(&app, "dest").await;
    let signer_uri = "https://remote.example/users/friend";
    let signer = AccountRef::Remote(app.runtime.ids.next_id());

    let policy = build_policy(&app, FakeActorUriResolver::new().with(signer_uri, signer));

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

    app.cleanup().await;
}

/// Requirement 6.1: the judgment is scoped to the *destination* local
/// actor -- a block recorded by a different local actor must not leak into
/// another local actor's own judgment.
#[tokio::test]
async fn actor_context_is_scoped_to_the_destination_local_actor() {
    let app = spawn_test_app().await;
    let dest_id = create_test_actor(&app, "dest").await;
    let other_id = create_test_actor(&app, "other").await;
    let dest = AccountRef::Local(dest_id);
    let other = AccountRef::Local(other_id);
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(app.runtime.ids.next_id());

    // `other` (not `dest`) has blocked the signer.
    upsert_block(&app, other, signer).await;

    let policy = build_policy(&app, FakeActorUriResolver::new().with(signer_uri, signer));

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

    app.cleanup().await;
}

/// Requirement 6.3: a `SharedInbox` context must always answer `false`,
/// even when the signer is genuinely blocked by a known local actor --
/// never bulk-reject at this point in the pipeline.
#[tokio::test]
async fn shared_inbox_context_always_reports_false_even_when_blocked() {
    let app = spawn_test_app().await;
    let dest_id = create_test_actor(&app, "dest").await;
    let dest = AccountRef::Local(dest_id);
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(app.runtime.ids.next_id());

    upsert_block(&app, dest, signer).await;

    let policy = build_policy(&app, FakeActorUriResolver::new().with(signer_uri, signer));

    let blocked = policy
        .is_blocked(signer_uri, LocalRecipientContext::SharedInbox)
        .await
        .expect("is_blocked must succeed");
    assert!(
        !blocked,
        "SharedInbox must never be bulk-rejected, even for a genuinely blocked signer"
    );

    app.cleanup().await;
}

/// Requirement 6.4: once a block is undone (row deleted), the very next
/// `is_blocked` call for the same pair must answer `false` again -- live DB
/// state each call, no cached verdict.
#[tokio::test]
async fn returns_false_again_after_unblock() {
    let app = spawn_test_app().await;
    let dest_id = create_test_actor(&app, "dest").await;
    let dest = AccountRef::Local(dest_id);
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(app.runtime.ids.next_id());

    upsert_block(&app, dest, signer).await;

    let policy = build_policy(&app, FakeActorUriResolver::new().with(signer_uri, signer));
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

    sg_repository::delete_block(&app.pool, &dest, &signer)
        .await
        .expect("delete_block must succeed");

    assert!(
        !policy
            .is_blocked(signer_uri, destination)
            .await
            .expect("is_blocked must succeed"),
        "must not be blocked after unblock"
    );

    app.cleanup().await;
}

/// This module's own doc comment ("Resolving the destination"): an `Actor`
/// URI that does not currently name a known local actor is a benign `false`,
/// not an error.
#[tokio::test]
async fn actor_context_reports_false_when_destination_does_not_resolve() {
    let app = spawn_test_app().await;
    let signer_uri = "https://remote.example/users/attacker";
    let signer = AccountRef::Remote(app.runtime.ids.next_id());

    let policy = build_policy(&app, FakeActorUriResolver::new().with(signer_uri, signer));

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

    app.cleanup().await;
}

/// A same-server (local-to-local) block is judged the same way as a
/// remote signer's -- the destination-side resolution path does not
/// special-case the signer's own locality.
#[tokio::test]
async fn actor_context_reports_true_for_a_blocked_local_signer() {
    let app = spawn_test_app().await;
    let dest_id = create_test_actor(&app, "dest").await;
    let signer_id = create_test_actor(&app, "signer").await;
    let dest = AccountRef::Local(dest_id);
    let signer = AccountRef::Local(signer_id);
    let signer_uri = actor_url("signer");

    upsert_block(&app, dest, signer).await;

    let policy = build_policy(
        &app,
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

    app.cleanup().await;
}
