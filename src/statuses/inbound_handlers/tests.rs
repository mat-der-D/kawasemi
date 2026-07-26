//! DB-backed tests for `InboundHandlers` (Requirements 13.6, 14.1-14.5,
//! 15.2, 15.3), per task 6.1's own observable completion condition: "リモー
//! ト Create でローカル Status が取り込まれ、Announce/Like でカウンタが更
//! 新され、Delete/Update が反映され、投票ワイヤ形の Create は投票記録とし
//! て反映される（受信結合テストがグリーン）".
//!
//! Mirrors `poll_service/tests.rs`'s established convention
//! (`crate::test_harness::spawn_test_app` for an isolated, migrated schema
//! plus a deterministic `RuntimeContext`) adapted for this module's own
//! dependency: an in-memory [`FakeRemoteActors`] standing in for
//! `RemoteAccountFetcher` (no real HTTP fetch in these tests — see
//! `inbound_handlers.rs`'s own doc comment, "Resolving `actor_uri -> Id`").
//! Status/poll fixtures are inserted directly via
//! `status_repository::insert_status`/`poll_repository::insert_poll` (this
//! module's own boundary does not create posts or polls).

use std::collections::HashMap;
use std::sync::Mutex;

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::*;
use crate::domain::Visibility;
use crate::error::ErrorKind;
use crate::federation::VerifiedSigner;
use crate::statuses::model::{Poll, PollOption};
use crate::test_harness::{TestApp, spawn_test_app};

// --- Test doubles -----------------------------------------------------------

/// An in-memory [`RemoteActorResolver`]: hands out a stable [`Id`] per
/// `actor_uri`, minting a fresh one (via the app's own deterministic
/// `IdGenerator`) on first sight and remembering it thereafter — no real
/// network fetch, mirroring `poll_service/tests.rs::MockActorLookup`'s
/// identical "narrow in-memory fake over a heavier real port" precedent.
struct FakeRemoteActors {
    runtime: RuntimeContext,
    by_uri: Mutex<HashMap<String, Id>>,
}

impl FakeRemoteActors {
    fn new(runtime: RuntimeContext) -> Self {
        Self {
            runtime,
            by_uri: Mutex::new(HashMap::new()),
        }
    }
}

impl RemoteActorResolver for FakeRemoteActors {
    async fn resolve_remote_actor(&self, actor_uri: &str) -> Result<Id, AppError> {
        let mut map = self.by_uri.lock().unwrap();
        if let Some(id) = map.get(actor_uri) {
            return Ok(*id);
        }
        let id = self.runtime.ids.next_id();
        map.insert(actor_uri.to_string(), id);
        Ok(id)
    }
}

const REMOTE_ALICE: &str = "https://remote.example/actors/alice";
const REMOTE_BOB: &str = "https://remote.example/actors/bob";

fn ctx_for(actor_uri: &str) -> InboundContext {
    InboundContext {
        signer: VerifiedSigner {
            key_id: format!("{actor_uri}#main-key"),
            actor_uri: actor_uri.to_string(),
        },
    }
}

/// Builds a [`ParsedActivity`] from a `serde_json::json!` value, extracting
/// `id`/`type` exactly like `jsonld::parse_activity` does.
fn activity_from(value: Value) -> ParsedActivity {
    let map = value.as_object().expect("test activity must be an object");
    let id = map
        .get("id")
        .and_then(Value::as_str)
        .expect("test activity must carry a string 'id'")
        .to_string();
    let activity_type = map
        .get("type")
        .and_then(Value::as_str)
        .expect("test activity must carry a string 'type'")
        .to_string();
    ParsedActivity {
        id,
        activity_type,
        raw: value,
    }
}

/// Inserts a real `statuses` row directly, owned by `actor_id` — mirrors
/// `poll_service/tests.rs::insert_test_status`'s identical helper, with an
/// added `local`/`poll_id` parameter this module's own tests need.
async fn insert_test_status(
    app: &TestApp,
    actor_id: Id,
    visibility: Visibility,
    local: bool,
    poll_id: Option<Id>,
) -> Status {
    let id = app.runtime.ids.next_id();
    let now = app.runtime.clock.now();
    let status = Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: Some(format!("https://kawasemi.example/@actor/{}", id.as_i64())),
        content: "hello #kawasemi".to_string(),
        visibility,
        sensitive: false,
        spoiler_text: String::new(),
        in_reply_to_id: None,
        in_reply_to_account_id: None,
        reblog_of_id: None,
        poll_id,
        language: None,
        reblogs_count: 0,
        favourites_count: 0,
        replies_count: 0,
        local,
        created_at: now,
        edited_at: None,
    };
    status_repository::insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed for a fresh id/uri");
    status
}

async fn insert_test_poll(app: &TestApp, status_id: Id, poll_id: Id, titles: &[&str]) -> Poll {
    let poll = Poll {
        id: poll_id,
        status_id,
        expires_at: None,
        multiple: false,
    };
    let options: Vec<PollOption> = titles
        .iter()
        .enumerate()
        .map(|(idx, title)| PollOption {
            poll_id: poll.id,
            idx: idx as i32,
            title: title.to_string(),
            votes_count: 0,
        })
        .collect();
    poll_repository::insert_poll(&app.pool, &poll, &options)
        .await
        .expect("insert_poll must succeed for a fresh poll");
    poll
}

fn deps(app: &TestApp) -> StatusInboundDeps<FakeRemoteActors> {
    StatusInboundDeps {
        pool: app.pool.clone(),
        runtime: app.runtime.clone(),
        remote_actors: Arc::new(FakeRemoteActors::new(app.runtime.clone())),
    }
}

// -- CreateNoteHandler: ordinary ingestion -----------------------------------

/// Requirement 14.2: an inbound `Create(Note)` ingests a new remote
/// `Status`, reflecting reply/visibility.
#[tokio::test]
async fn create_note_ingests_a_remote_status_with_reply_and_visibility() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let parent = insert_test_status(&app, local_author, Visibility::Public, true, None).await;

    let handler = CreateNoteHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
    );

    let activity = activity_from(json!({
        "id": "https://remote.example/activities/create-1",
        "type": "Create",
        "actor": REMOTE_ALICE,
        "object": {
            "id": "https://remote.example/notes/1",
            "type": "Note",
            "attributedTo": REMOTE_ALICE,
            "content": "hi from remote! #kawasemi",
            "inReplyTo": parent.uri,
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
            "cc": [format!("{REMOTE_ALICE}/followers")],
        }
    }));

    let outcome = handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("a well-formed Create(Note) must be handled");
    assert_eq!(outcome, HandleOutcome::Handled);

    let ingested = status_repository::find_by_uri(&app.pool, "https://remote.example/notes/1")
        .await
        .expect("query must succeed")
        .expect("the remote Note must be ingested as a Status");
    assert_eq!(ingested.content, "hi from remote! #kawasemi");
    assert_eq!(ingested.visibility, Visibility::Public);
    assert!(!ingested.local, "an ingested remote post must not be local");
    assert_eq!(ingested.in_reply_to_id, Some(parent.id));
    assert_eq!(ingested.in_reply_to_account_id, Some(parent.actor_id));

    let refreshed_parent = status_repository::find_by_id(&app.pool, parent.id)
        .await
        .expect("query must succeed")
        .expect("parent must still exist");
    assert_eq!(
        refreshed_parent.replies_count, 1,
        "ingesting a reply must increment the parent's replies_count"
    );

    let tags = tag_repository::tags_for_status(&app.pool, ingested.id)
        .await
        .expect("tag lookup must succeed");
    assert!(
        tags.iter().any(|t| t.name == "kawasemi"),
        "hashtag extraction must reuse status_service::extract_content_tokens"
    );

    app.cleanup().await;
}

/// Requirement 15.2 (fan-out ownership): a `Create` wrapping a non-`Note`
/// inner object is not owned by this handler.
#[tokio::test]
async fn create_ignores_a_non_note_inner_object() {
    let app = spawn_test_app().await;
    let handler = CreateNoteHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
    );

    let activity = activity_from(json!({
        "id": "https://remote.example/activities/create-2",
        "type": "Create",
        "actor": REMOTE_ALICE,
        "object": {
            "id": "https://remote.example/questions/1",
            "type": "Question",
        }
    }));

    let outcome = handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must not error");
    assert_eq!(outcome, HandleOutcome::Ignored);

    app.cleanup().await;
}

/// A redelivered `Create(Note)` (same object `id`) is a safe idempotent
/// no-op, not a duplicate row / unique-violation error.
#[tokio::test]
async fn create_note_redelivery_is_idempotent() {
    let app = spawn_test_app().await;
    let handler = CreateNoteHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
    );

    let activity = activity_from(json!({
        "id": "https://remote.example/activities/create-3",
        "type": "Create",
        "actor": REMOTE_ALICE,
        "object": {
            "id": "https://remote.example/notes/3",
            "type": "Note",
            "content": "redelivered",
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
        }
    }));

    let first = handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("first delivery must succeed");
    assert_eq!(first, HandleOutcome::Handled);

    let second = handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("redelivery must not error");
    assert_eq!(second, HandleOutcome::Handled);

    app.cleanup().await;
}

// -- CreateNoteHandler: vote wire-form branch (Requirement 13.6) ------------

/// A `Create{Note, name=...}` whose `inReplyTo` targets a locally-owned
/// poll-bearing Status branches into `PollService`'s own vote path
/// (`poll_repository::record_vote`) instead of ingesting a Status.
#[tokio::test]
async fn create_note_matching_the_vote_wire_shape_records_a_vote_not_a_status() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let poll_id = app.runtime.ids.next_id();
    let poll_status =
        insert_test_status(&app, local_author, Visibility::Public, true, Some(poll_id)).await;
    insert_test_poll(&app, poll_status.id, poll_id, &["Pizza", "Sushi", "Tacos"]).await;

    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let handler = CreateNoteHandler::new(app.pool.clone(), app.runtime.clone(), remote_actors);

    let activity = activity_from(json!({
        "id": "https://remote.example/activities/create-vote",
        "type": "Create",
        "actor": REMOTE_BOB,
        "object": {
            "id": "https://remote.example/notes/vote-1",
            "type": "Note",
            "name": "Sushi",
            "inReplyTo": poll_status.uri,
            "to": [poll_status.uri],
        }
    }));

    let outcome = handler
        .handle(&activity, &ctx_for(REMOTE_BOB))
        .await
        .expect("a valid vote wire-shape Create must be handled");
    assert_eq!(outcome, HandleOutcome::Handled);

    assert!(
        status_repository::find_by_uri(&app.pool, "https://remote.example/notes/vote-1")
            .await
            .expect("query must succeed")
            .is_none(),
        "a detected vote must not be ingested as a Status"
    );

    let tally = poll_repository::tally(&app.pool, poll_id, None)
        .await
        .expect("tally must succeed");
    let sushi = tally
        .options
        .iter()
        .find(|o| o.title == "Sushi")
        .expect("Sushi option must exist");
    assert_eq!(sushi.votes_count, 1, "the vote must be recorded");

    app.cleanup().await;
}

/// A `Create{Note, name=...}` whose `name` matches no option on the target
/// poll falls through to ordinary Note ingestion instead of erroring.
#[tokio::test]
async fn create_note_with_unmatched_name_falls_through_to_ordinary_ingestion() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let poll_id = app.runtime.ids.next_id();
    let poll_status =
        insert_test_status(&app, local_author, Visibility::Public, true, Some(poll_id)).await;
    insert_test_poll(&app, poll_status.id, poll_id, &["Pizza", "Sushi"]).await;

    let handler = CreateNoteHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
    );

    let activity = activity_from(json!({
        "id": "https://remote.example/activities/create-not-a-vote",
        "type": "Create",
        "actor": REMOTE_BOB,
        "object": {
            "id": "https://remote.example/notes/not-a-vote",
            "type": "Note",
            "name": "Ramen",
            "inReplyTo": poll_status.uri,
            "content": "not actually a vote",
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
        }
    }));

    let outcome = handler
        .handle(&activity, &ctx_for(REMOTE_BOB))
        .await
        .expect("must be handled as an ordinary Note");
    assert_eq!(outcome, HandleOutcome::Handled);

    assert!(
        status_repository::find_by_uri(&app.pool, "https://remote.example/notes/not-a-vote")
            .await
            .expect("query must succeed")
            .is_some(),
        "an unmatched vote-shaped Create must ingest as an ordinary Status"
    );

    app.cleanup().await;
}

// -- AnnounceHandler ----------------------------------------------------------

/// Requirement 14.3: an inbound `Announce` of a local post records a reblog
/// row and increments `reblogs_count`.
#[tokio::test]
async fn announce_of_a_local_post_records_reblog_and_increments_count() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let target = insert_test_status(&app, local_author, Visibility::Public, true, None).await;

    let handler = AnnounceHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
    );

    let activity = activity_from(json!({
        "id": "https://remote.example/activities/announce-1",
        "type": "Announce",
        "actor": REMOTE_ALICE,
        "object": target.uri,
    }));

    let outcome = handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must be handled");
    assert_eq!(outcome, HandleOutcome::Handled);

    let refreshed = status_repository::find_by_id(&app.pool, target.id)
        .await
        .expect("query must succeed")
        .expect("target must still exist");
    assert_eq!(refreshed.reblogs_count, 1);

    app.cleanup().await;
}

/// Requirement 14.3: an `Announce` of a **remote** (not-locally-owned) post
/// is not this handler's concern.
#[tokio::test]
async fn announce_of_a_remote_post_is_ignored() {
    let app = spawn_test_app().await;
    let remote_author = app.runtime.ids.next_id();
    let target = insert_test_status(&app, remote_author, Visibility::Public, false, None).await;

    let handler = AnnounceHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
    );

    let activity = activity_from(json!({
        "id": "https://remote.example/activities/announce-2",
        "type": "Announce",
        "actor": REMOTE_ALICE,
        "object": target.uri,
    }));

    let outcome = handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must not error");
    assert_eq!(outcome, HandleOutcome::Ignored);

    app.cleanup().await;
}

// -- LikeHandler ---------------------------------------------------------------

/// Requirement 14.3: an inbound `Like` of a local post increments
/// `favourites_count`, and a duplicate `Like` does not double-count.
#[tokio::test]
async fn like_of_a_local_post_increments_count_and_is_idempotent() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let target = insert_test_status(&app, local_author, Visibility::Public, true, None).await;

    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let handler = LikeHandler::new(app.pool.clone(), app.runtime.clone(), remote_actors);

    let activity = activity_from(json!({
        "id": "https://remote.example/activities/like-1",
        "type": "Like",
        "actor": REMOTE_ALICE,
        "object": target.uri,
    }));

    handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must be handled");
    handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("a duplicate Like must not error");

    let refreshed = status_repository::find_by_id(&app.pool, target.id)
        .await
        .expect("query must succeed")
        .expect("target must still exist");
    assert_eq!(
        refreshed.favourites_count, 1,
        "a duplicate Like must not double-count"
    );

    app.cleanup().await;
}

// -- DeleteHandler --------------------------------------------------------------

/// Requirement 14.4: an inbound `Delete` removes a remote-origin status
/// owned by the signed actor.
#[tokio::test]
async fn delete_removes_a_remote_origin_status_owned_by_the_signed_actor() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let actor_id = remote_actors
        .resolve_remote_actor(REMOTE_ALICE)
        .await
        .expect("resolve must succeed");
    let target = insert_test_status(&app, actor_id, Visibility::Public, false, None).await;

    let handler = DeleteHandler::new(app.pool.clone(), Arc::clone(&remote_actors));
    let activity = activity_from(json!({
        "id": "https://remote.example/activities/delete-1",
        "type": "Delete",
        "actor": REMOTE_ALICE,
        "object": target.uri,
    }));

    let outcome = handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must be handled");
    assert_eq!(outcome, HandleOutcome::Handled);

    assert!(
        status_repository::find_by_id(&app.pool, target.id)
            .await
            .expect("query must succeed")
            .is_none(),
        "the status must be deleted"
    );

    app.cleanup().await;
}

/// A `Delete` naming a status owned by a *different* remote actor is
/// rejected (a spoofing attempt), not silently applied.
#[tokio::test]
async fn delete_is_forbidden_when_the_signed_actor_does_not_own_the_target() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let owner_id = remote_actors
        .resolve_remote_actor(REMOTE_ALICE)
        .await
        .expect("resolve must succeed");
    let target = insert_test_status(&app, owner_id, Visibility::Public, false, None).await;

    let handler = DeleteHandler::new(app.pool.clone(), Arc::clone(&remote_actors));
    let activity = activity_from(json!({
        "id": "https://remote.example/activities/delete-2",
        "type": "Delete",
        "actor": REMOTE_BOB,
        "object": target.uri,
    }));

    let err = handler
        .handle(&activity, &ctx_for(REMOTE_BOB))
        .await
        .expect_err("a non-owning actor's Delete must be rejected");
    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    assert!(
        status_repository::find_by_id(&app.pool, target.id)
            .await
            .expect("query must succeed")
            .is_some(),
        "the status must not have been deleted"
    );

    app.cleanup().await;
}

/// Requirement 14.4 (ownership boundary): a `Delete` naming a **local**
/// status is ignored, never actioned.
#[tokio::test]
async fn delete_of_a_local_status_is_ignored() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let target = insert_test_status(&app, local_author, Visibility::Public, true, None).await;

    let handler = DeleteHandler::new(
        app.pool.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
    );
    let activity = activity_from(json!({
        "id": "https://remote.example/activities/delete-3",
        "type": "Delete",
        "actor": REMOTE_ALICE,
        "object": target.uri,
    }));

    let outcome = handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must not error");
    assert_eq!(outcome, HandleOutcome::Ignored);

    assert!(
        status_repository::find_by_id(&app.pool, target.id)
            .await
            .expect("query must succeed")
            .is_some(),
        "a local status must never be deleted via an inbound Delete"
    );

    app.cleanup().await;
}

// -- UpdateHandler --------------------------------------------------------------

/// Requirement 14.4: an inbound `Update` edits a remote-origin status'
/// content/spoiler_text/sensitive.
#[tokio::test]
async fn update_edits_a_remote_origin_status() {
    let app = spawn_test_app().await;
    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let actor_id = remote_actors
        .resolve_remote_actor(REMOTE_ALICE)
        .await
        .expect("resolve must succeed");
    let target = insert_test_status(&app, actor_id, Visibility::Public, false, None).await;

    let handler = UpdateHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::clone(&remote_actors),
    );
    let activity = activity_from(json!({
        "id": "https://remote.example/activities/update-1",
        "type": "Update",
        "actor": REMOTE_ALICE,
        "object": {
            "id": target.uri,
            "type": "Note",
            "content": "edited content",
            "summary": "cw",
            "sensitive": true,
        }
    }));

    let outcome = handler
        .handle(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must be handled");
    assert_eq!(outcome, HandleOutcome::Handled);

    let refreshed = status_repository::find_by_id(&app.pool, target.id)
        .await
        .expect("query must succeed")
        .expect("target must still exist");
    assert_eq!(refreshed.content, "edited content");
    assert_eq!(refreshed.spoiler_text, "cw");
    assert!(refreshed.sensitive);
    assert!(refreshed.edited_at.is_some());

    let history = status_repository::list_edits(&app.pool, target.id)
        .await
        .expect("history query must succeed");
    assert_eq!(history.len(), 1, "the pre-edit content must be archived");

    app.cleanup().await;
}

// -- UndoHandler -----------------------------------------------------------------

/// Requirement 14.3: `Undo(Announce)` reverts a previously-recorded reblog
/// and decrements the target's `reblogs_count`.
#[tokio::test]
async fn undo_announce_reverts_reblog_and_decrements_count() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let target = insert_test_status(&app, local_author, Visibility::Public, true, None).await;

    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let announce_handler = AnnounceHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::clone(&remote_actors),
    );
    let announce = activity_from(json!({
        "id": "https://remote.example/activities/announce-undo",
        "type": "Announce",
        "actor": REMOTE_ALICE,
        "object": target.uri,
    }));
    announce_handler
        .handle(&announce, &ctx_for(REMOTE_ALICE))
        .await
        .expect("Announce must be handled");

    let undo_handler = UndoHandler::new(app.pool.clone(), Arc::clone(&remote_actors));
    let undo = activity_from(json!({
        "id": "https://remote.example/activities/undo-announce-1",
        "type": "Undo",
        "actor": REMOTE_ALICE,
        "object": {
            "id": "https://remote.example/activities/announce-undo",
            "type": "Announce",
            "actor": REMOTE_ALICE,
            "object": target.uri,
        }
    }));

    let outcome = undo_handler
        .handle(&undo, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must be handled");
    assert_eq!(outcome, HandleOutcome::Handled);

    let refreshed = status_repository::find_by_id(&app.pool, target.id)
        .await
        .expect("query must succeed")
        .expect("target must still exist");
    assert_eq!(refreshed.reblogs_count, 0);

    app.cleanup().await;
}

/// Requirement 14.3: `Undo(Like)` reverts a previously-recorded favourite
/// and decrements the target's `favourites_count`.
#[tokio::test]
async fn undo_like_reverts_favourite_and_decrements_count() {
    let app = spawn_test_app().await;
    let local_author = app.runtime.ids.next_id();
    let target = insert_test_status(&app, local_author, Visibility::Public, true, None).await;

    let remote_actors = Arc::new(FakeRemoteActors::new(app.runtime.clone()));
    let like_handler = LikeHandler::new(
        app.pool.clone(),
        app.runtime.clone(),
        Arc::clone(&remote_actors),
    );
    let like = activity_from(json!({
        "id": "https://remote.example/activities/like-undo",
        "type": "Like",
        "actor": REMOTE_ALICE,
        "object": target.uri,
    }));
    like_handler
        .handle(&like, &ctx_for(REMOTE_ALICE))
        .await
        .expect("Like must be handled");

    let undo_handler = UndoHandler::new(app.pool.clone(), Arc::clone(&remote_actors));
    let undo = activity_from(json!({
        "id": "https://remote.example/activities/undo-like-1",
        "type": "Undo",
        "actor": REMOTE_ALICE,
        "object": {
            "id": "https://remote.example/activities/like-undo",
            "type": "Like",
            "actor": REMOTE_ALICE,
            "object": target.uri,
        }
    }));

    undo_handler
        .handle(&undo, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must be handled");

    let refreshed = status_repository::find_by_id(&app.pool, target.id)
        .await
        .expect("query must succeed")
        .expect("target must still exist");
    assert_eq!(refreshed.favourites_count, 0);

    app.cleanup().await;
}

/// Task 6.1's own explicit fan-out requirement: `UndoHandler` ignores an
/// `Undo` wrapping a `Follow` (social-graph's own concern), never erroring.
#[tokio::test]
async fn undo_ignores_non_announce_or_like_inner_types() {
    let app = spawn_test_app().await;
    let handler = UndoHandler::new(
        app.pool.clone(),
        Arc::new(FakeRemoteActors::new(app.runtime.clone())),
    );

    let undo = activity_from(json!({
        "id": "https://remote.example/activities/undo-follow-1",
        "type": "Undo",
        "actor": REMOTE_ALICE,
        "object": {
            "id": "https://remote.example/activities/follow-1",
            "type": "Follow",
            "actor": REMOTE_ALICE,
            "object": "https://kawasemi.example/@someone",
        }
    }));

    let outcome = handler
        .handle(&undo, &ctx_for(REMOTE_ALICE))
        .await
        .expect("must not error");
    assert_eq!(outcome, HandleOutcome::Ignored);

    app.cleanup().await;
}

// -- register_status_handlers / dispatcher integration ------------------------

/// Requirement 14.1: `register_status_handlers` registers all six handlers
/// against a real `InboundActivityDispatcher`, and dispatching a `Create`
/// through it actually ingests a `Status` — proving the wiring function
/// itself, not just each handler in isolation.
#[tokio::test]
async fn register_status_handlers_wires_create_through_the_real_dispatcher() {
    let app = spawn_test_app().await;
    let mut dispatcher = InboundActivityDispatcher::new();
    register_status_handlers(&mut dispatcher, deps(&app));

    let activity = activity_from(json!({
        "id": "https://remote.example/activities/create-dispatched",
        "type": "Create",
        "actor": REMOTE_ALICE,
        "object": {
            "id": "https://remote.example/notes/dispatched-1",
            "type": "Note",
            "content": "dispatched via the real InboundActivityDispatcher",
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
        }
    }));

    dispatcher
        .dispatch(&activity, &ctx_for(REMOTE_ALICE))
        .await
        .expect("dispatch must succeed");

    let ingested =
        status_repository::find_by_uri(&app.pool, "https://remote.example/notes/dispatched-1")
            .await
            .expect("query must succeed");
    assert!(
        ingested.is_some(),
        "dispatching a Create through the registered handlers must ingest a Status"
    );

    app.cleanup().await;
}
