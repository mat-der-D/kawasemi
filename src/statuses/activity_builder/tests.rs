//! Unit tests for `StatusActivityBuilder` (Requirements 4.2, 4.3, 4.4, 7.3,
//! 8.4, 9.2, 9.4, 10.2, 10.3, 13.6), per task 4.1's observable completion
//! condition: "各操作で正規 Activity が生成され、ローカル/リモート混在
//! recipient が同一 Activity で `DeliveryService` に渡り、投票配送が
//! `Create{Note, name=...}` ワイヤ形（`Vote` type ではない）で生成される".
//!
//! Pure in-memory: no Postgres/DB anywhere in this file. [`MockActorLookup`]
//! is a plain in-memory [`ActorHandleLookup`] test double;
//! [`MockLocalActorLookup`] is the analogous [`LocalActorLookup`] double
//! `DeliveryService`'s own `RecipientTargetResolver` needs (mirrors
//! `crate::federation::outbound::delivery::tests`'s and
//! `crate::federation::outbound::target::tests`'s identical precedent);
//! [`RecordingSink`] records every `DeliverySink::dispatch` call so tests can
//! assert on the exact `CanonicalActivity` JSON each target observed.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::http::StatusCode;
use time::macros::datetime;

use super::*;
use crate::actor::{ActorState, ActorType, ResolvedActor};
use crate::domain::Visibility;
use crate::error::ErrorKind;
use crate::federation::outbound::sink::CanonicalActivity;
use crate::federation::outbound::target::{DeliveryTarget, RecipientTargetResolver};
use crate::runtime::SeqIdGenerator;
use crate::statuses::addressing::derive_addressing;

// --- Test doubles -----------------------------------------------------

/// In-memory [`ActorHandleLookup`]: `Id -> Handle` for known actors, a
/// `404`-shaped [`AppError`] otherwise (mirrors
/// `ActorDirectory::resolve_actor_by_id`'s "no error for absence" contract,
/// projected through [`ActorHandleLookup::resolve_handle`]'s own
/// "not-found is an error" contract — see that trait's doc comment).
struct MockActorLookup {
    known: HashMap<i64, Handle>,
}

impl MockActorLookup {
    fn with_actors(pairs: &[(i64, &str)]) -> Self {
        Self {
            known: pairs
                .iter()
                .map(|(id, handle)| (*id, Handle::new(*handle).expect("valid test handle")))
                .collect(),
        }
    }
}

impl ActorHandleLookup for MockActorLookup {
    async fn resolve_handle(&self, actor_id: Id) -> Result<Handle, AppError> {
        self.known.get(&actor_id.as_i64()).cloned().ok_or_else(|| {
            AppError::client(
                StatusCode::NOT_FOUND,
                format!("actor id {actor_id:?} not known to MockActorLookup"),
            )
        })
    }
}

/// In-memory [`LocalActorLookup`] for `DeliveryService`'s own recipient
/// resolution (a separate concern from [`MockActorLookup`] above — this one
/// answers "does this `Handle` resolve to a *recipient*'s local actor",
/// mirroring `target/tests.rs::MockLocalActorLookup`).
struct MockLocalActorLookup {
    known_handles: std::collections::HashSet<String>,
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

/// Records every [`DeliverySink::dispatch`] call
/// (`(DeliveryTarget, CanonicalActivity, Handle)`), mirroring
/// `crate::federation::outbound::delivery::tests::RecordingSink`.
struct RecordingSink {
    calls: Mutex<Vec<(DeliveryTarget, CanonicalActivity, Handle)>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(DeliveryTarget, CanonicalActivity, Handle)> {
        self.calls.lock().unwrap().clone()
    }
}

impl DeliverySink for RecordingSink {
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError> {
        self.calls
            .lock()
            .unwrap()
            .push((target, activity.clone(), sender.clone()));
        Ok(())
    }
}

// --- Fixtures -----------------------------------------------------------

fn handle(raw: &str) -> Handle {
    Handle::new(raw).expect("test handle must be valid")
}

fn remote(inbox: &str) -> Recipient {
    Recipient::Remote {
        inbox: inbox.to_string(),
        shared_inbox: None,
    }
}

fn sample_status(id: i64, actor_id: i64) -> Status {
    Status {
        id: Id::from_i64(id),
        actor_id: Id::from_i64(actor_id),
        uri: format!("https://kawasemi.example/statuses/{id}"),
        url: Some(format!("https://kawasemi.example/@alice/{id}")),
        content: "hello world".to_string(),
        visibility: Visibility::Public,
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

fn sample_addressing() -> Addressing {
    derive_addressing(
        &sample_status(1, 10),
        &[],
        "https://kawasemi.example/users/alice/followers",
    )
}

/// The concrete `StatusActivityBuilder` instantiation every test in this
/// module exercises, factored into a named alias (rather than spelled out at
/// each use site) per this crate's own precedent for a test helper's
/// otherwise-unwieldy generic return type (mirrors
/// `crate::api::ratelimit::tests::Calls`'s identical "factor into a `type`"
/// convention) — also what `clippy::type_complexity` (`-D warnings`, this
/// repo's validation gate) requires here.
type TestBuilder = StatusActivityBuilder<
    MockActorLookup,
    MockLocalActorLookup,
    Arc<RecordingSink>,
    Arc<RecordingSink>,
>;

fn builder(
    known_actors: &[(i64, &str)],
    known_local_handles: &[&str],
) -> (TestBuilder, Arc<RecordingSink>, Arc<RecordingSink>) {
    let actor_lookup = MockActorLookup::with_actors(known_actors);
    let local_lookup = MockLocalActorLookup::with_handles(known_local_handles);
    let local_sink = Arc::new(RecordingSink::new());
    let http_sink = Arc::new(RecordingSink::new());
    let delivery = DeliveryService::new(
        RecipientTargetResolver::new(local_lookup),
        Arc::clone(&local_sink),
        Arc::clone(&http_sink),
    );
    let urls = ActorUrls::new("kawasemi.example");
    let ids = Arc::new(SeqIdGenerator::new(1000)) as Arc<dyn IdGenerator>;
    let builder = StatusActivityBuilder::new(urls, ids, actor_lookup, Arc::new(delivery));
    (builder, local_sink, http_sink)
}

fn value_str<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("expected string field {key:?} in {value}"))
}

fn value_array_of_strings(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected array field {key:?} in {value}"))
        .iter()
        .map(|v| {
            v.as_str()
                .expect("array entry must be a string")
                .to_string()
        })
        .collect()
}

// Bring `DeliverySink`/`LocalActorLookup`/`Arc` blanket impls into scope for
// `Arc<RecordingSink>` (delegates to the inner sink) so `builder()` above can
// hand out cloned `Arc` handles while `DeliveryService` still owns concrete
// `L`/`H` values.
impl DeliverySink for Arc<RecordingSink> {
    async fn dispatch(
        &self,
        target: DeliveryTarget,
        activity: &CanonicalActivity,
        sender: &Handle,
    ) -> Result<(), AppError> {
        self.as_ref().dispatch(target, activity, sender).await
    }
}

// -- deliver_create --------------------------------------------------------

#[tokio::test]
async fn deliver_create_builds_canonical_create_note_with_addressing_and_reaches_mixed_recipients()
{
    let (builder, local_sink, http_sink) = builder(&[(10, "alice")], &["alice"]);
    let status = sample_status(1, 10);
    let addressing = sample_addressing();
    let recipients = vec![
        Recipient::Local(handle("alice")),
        remote("https://remote-a.example/inbox"),
    ];

    builder
        .deliver_create(&status, &addressing, recipients, None, None)
        .await
        .expect("deliver_create must succeed");

    let local_calls = local_sink.calls();
    let http_calls = http_sink.calls();
    assert_eq!(local_calls.len(), 1);
    assert_eq!(http_calls.len(), 1);

    // Requirement 4.4: identical canonical Activity reaches both local and
    // remote targets from the one deliver_create call.
    let canonical = &local_calls[0].1;
    assert_eq!(&http_calls[0].1, canonical);

    assert_eq!(canonical.parsed().activity_type, "Create");
    let raw = canonical.as_value();
    assert_eq!(value_str(raw, "type"), "Create");
    assert_eq!(
        raw.get("actor").and_then(|v| v.as_str()),
        Some("https://kawasemi.example/users/alice")
    );
    let object = raw.get("object").expect("object present");
    assert_eq!(value_str(object, "type"), "Note");
    assert_eq!(value_str(object, "id"), status.uri);
    assert_eq!(value_str(object, "content"), "hello world");
    assert_eq!(value_array_of_strings(object, "to"), addressing.to);
    assert_eq!(value_array_of_strings(object, "cc"), addressing.cc);
    assert_eq!(value_array_of_strings(raw, "to"), addressing.to);
    assert!(object.get("inReplyTo").is_none());

    assert_eq!(local_calls[0].2, handle("alice"));
    assert_eq!(http_calls[0].2, handle("alice"));
}

#[tokio::test]
async fn deliver_create_includes_in_reply_to_when_provided() {
    let (builder, local_sink, _http_sink) = builder(&[(10, "alice")], &["alice"]);
    let status = sample_status(2, 10);
    let addressing = sample_addressing();

    builder
        .deliver_create(
            &status,
            &addressing,
            vec![Recipient::Local(handle("alice"))],
            Some("https://kawasemi.example/statuses/1"),
            None,
        )
        .await
        .expect("deliver_create must succeed");

    let calls = local_sink.calls();
    let object = calls[0].1.as_value().get("object").unwrap();
    assert_eq!(
        value_str(object, "inReplyTo"),
        "https://kawasemi.example/statuses/1"
    );
}

#[tokio::test]
async fn deliver_create_includes_summary_only_when_spoiler_text_present() {
    let (builder, local_sink, _http_sink) = builder(&[(10, "alice")], &["alice"]);
    let mut status = sample_status(3, 10);
    status.spoiler_text = "content warning".to_string();
    let addressing = sample_addressing();

    builder
        .deliver_create(
            &status,
            &addressing,
            vec![Recipient::Local(handle("alice"))],
            None,
            None,
        )
        .await
        .expect("deliver_create must succeed");

    let calls = local_sink.calls();
    let object = calls[0].1.as_value().get("object").unwrap();
    assert_eq!(value_str(object, "summary"), "content warning");
}

#[tokio::test]
async fn deliver_create_mints_distinct_activity_ids_across_calls() {
    let (builder, local_sink, _http_sink) = builder(&[(10, "alice")], &["alice"]);
    let addressing = sample_addressing();

    builder
        .deliver_create(
            &sample_status(1, 10),
            &addressing,
            vec![Recipient::Local(handle("alice"))],
            None,
            None,
        )
        .await
        .expect("first deliver_create must succeed");
    builder
        .deliver_create(
            &sample_status(2, 10),
            &addressing,
            vec![Recipient::Local(handle("alice"))],
            None,
            None,
        )
        .await
        .expect("second deliver_create must succeed");

    let calls = local_sink.calls();
    assert_eq!(calls.len(), 2);
    let first_id = value_str(calls[0].1.as_value(), "id").to_string();
    let second_id = value_str(calls[1].1.as_value(), "id").to_string();
    assert_ne!(
        first_id, second_id,
        "each Activity must get its own fresh id"
    );
}

#[tokio::test]
async fn deliver_create_fails_when_sender_actor_is_unknown() {
    let (builder, local_sink, http_sink) = builder(&[], &["alice"]);
    let status = sample_status(1, 999);
    let addressing = sample_addressing();

    let err = builder
        .deliver_create(
            &status,
            &addressing,
            vec![Recipient::Local(handle("alice"))],
            None,
            None,
        )
        .await
        .expect_err("unknown sender actor must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(
        local_sink.calls().len(),
        0,
        "no sink runs before sender resolves"
    );
    assert_eq!(http_sink.calls().len(), 0);
}

// -- deliver_create: poll embedding (Requirement 13.7, task 10.1) ----------

#[tokio::test]
async fn deliver_create_embeds_single_choice_poll_as_question_with_one_of_and_end_time() {
    let (builder, local_sink, _http_sink) = builder(&[(10, "alice")], &["alice"]);
    let mut status = sample_status(20, 10);
    let poll_id = Id::from_i64(500);
    status.poll_id = Some(poll_id);
    let addressing = sample_addressing();
    let poll = Poll {
        id: poll_id,
        status_id: status.id,
        expires_at: Some(datetime!(2026-08-01 00:00:00 UTC)),
        multiple: false,
    };
    let options = vec![
        PollOption {
            poll_id,
            idx: 0,
            title: "Cats".to_string(),
            votes_count: 3,
        },
        PollOption {
            poll_id,
            idx: 1,
            title: "Dogs".to_string(),
            votes_count: 5,
        },
    ];

    builder
        .deliver_create(
            &status,
            &addressing,
            vec![Recipient::Local(handle("alice"))],
            None,
            Some((&poll, &options)),
        )
        .await
        .expect("deliver_create must succeed");

    let calls = local_sink.calls();
    let object = calls[0].1.as_value().get("object").unwrap();
    assert_eq!(
        value_str(object, "type"),
        "Question",
        "a poll-bearing Create's object must be a Question, not a plain Note"
    );
    assert!(
        object.get("anyOf").is_none(),
        "single-choice poll must not use anyOf"
    );
    let one_of = object
        .get("oneOf")
        .and_then(|v| v.as_array())
        .expect("oneOf present for a single-choice poll");
    assert_eq!(one_of.len(), 2);
    assert_eq!(value_str(&one_of[0], "type"), "Note");
    assert_eq!(value_str(&one_of[0], "name"), "Cats");
    let replies0 = one_of[0].get("replies").expect("replies present");
    assert_eq!(value_str(replies0, "type"), "Collection");
    assert_eq!(replies0.get("totalItems").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(value_str(&one_of[1], "name"), "Dogs");
    assert_eq!(
        one_of[1]
            .get("replies")
            .and_then(|r| r.get("totalItems"))
            .and_then(|v| v.as_i64()),
        Some(5)
    );
    assert!(
        value_str(object, "endTime").starts_with("2026-08-01"),
        "endTime must reflect Poll::expires_at"
    );
}

#[tokio::test]
async fn deliver_create_embeds_multiple_choice_poll_under_any_of() {
    let (builder, local_sink, _http_sink) = builder(&[(10, "alice")], &["alice"]);
    let mut status = sample_status(21, 10);
    let poll_id = Id::from_i64(501);
    status.poll_id = Some(poll_id);
    let addressing = sample_addressing();
    let poll = Poll {
        id: poll_id,
        status_id: status.id,
        expires_at: None,
        multiple: true,
    };
    let options = vec![
        PollOption {
            poll_id,
            idx: 0,
            title: "Tabs".to_string(),
            votes_count: 0,
        },
        PollOption {
            poll_id,
            idx: 1,
            title: "Spaces".to_string(),
            votes_count: 0,
        },
    ];

    builder
        .deliver_create(
            &status,
            &addressing,
            vec![Recipient::Local(handle("alice"))],
            None,
            Some((&poll, &options)),
        )
        .await
        .expect("deliver_create must succeed");

    let calls = local_sink.calls();
    let object = calls[0].1.as_value().get("object").unwrap();
    assert_eq!(value_str(object, "type"), "Question");
    assert!(
        object.get("oneOf").is_none(),
        "multi-choice poll must not use oneOf"
    );
    let any_of = object
        .get("anyOf")
        .and_then(|v| v.as_array())
        .expect("anyOf present for a multi-choice poll");
    assert_eq!(any_of.len(), 2);
    assert!(
        object.get("endTime").is_none(),
        "endTime must be omitted when Poll::expires_at is None"
    );
}

#[tokio::test]
async fn deliver_create_without_poll_still_emits_a_plain_note() {
    let (builder, local_sink, _http_sink) = builder(&[(10, "alice")], &["alice"]);
    let status = sample_status(22, 10);
    let addressing = sample_addressing();

    builder
        .deliver_create(
            &status,
            &addressing,
            vec![Recipient::Local(handle("alice"))],
            None,
            None,
        )
        .await
        .expect("deliver_create must succeed");

    let calls = local_sink.calls();
    let object = calls[0].1.as_value().get("object").unwrap();
    assert_eq!(value_str(object, "type"), "Note");
    assert!(object.get("oneOf").is_none());
    assert!(object.get("anyOf").is_none());
    assert!(object.get("endTime").is_none());
}

// -- deliver_announce --------------------------------------------------------

#[tokio::test]
async fn deliver_announce_builds_canonical_announce_with_object_as_target_uri() {
    let (builder, local_sink, _http_sink) = builder(&[(20, "bob")], &["bob"]);
    let target = sample_status(1, 10);
    let reblog = sample_status(2, 20);
    let addressing = sample_addressing();

    builder
        .deliver_announce(
            &reblog,
            &target,
            &addressing,
            vec![Recipient::Local(handle("bob"))],
        )
        .await
        .expect("deliver_announce must succeed");

    let calls = local_sink.calls();
    let raw = calls[0].1.as_value();
    assert_eq!(value_str(raw, "type"), "Announce");
    assert_eq!(
        raw.get("actor").and_then(|v| v.as_str()),
        Some("https://kawasemi.example/users/bob")
    );
    assert_eq!(value_str(raw, "object"), target.uri);
    assert_eq!(value_array_of_strings(raw, "to"), addressing.to);
}

// -- deliver_like / deliver_undo --------------------------------------------

#[tokio::test]
async fn deliver_like_addresses_single_recipient_and_object_is_target_uri() {
    let (builder, local_sink, http_sink) = builder(&[(30, "carol")], &[]);
    let target = sample_status(5, 10);
    let recipient = ActorRef {
        uri: "https://remote.example/users/dave".to_string(),
        recipient: remote("https://remote.example/inbox"),
    };

    builder
        .deliver_like(Id::from_i64(30), &target, recipient.clone())
        .await
        .expect("deliver_like must succeed");

    assert_eq!(local_sink.calls().len(), 0, "recipient here is remote-only");
    let calls = http_sink.calls();
    assert_eq!(calls.len(), 1);
    let raw = calls[0].1.as_value();
    assert_eq!(value_str(raw, "type"), "Like");
    assert_eq!(
        raw.get("actor").and_then(|v| v.as_str()),
        Some("https://kawasemi.example/users/carol")
    );
    assert_eq!(value_str(raw, "object"), target.uri);
    assert_eq!(
        value_array_of_strings(raw, "to"),
        vec![recipient.uri.clone()]
    );
}

#[tokio::test]
async fn deliver_undo_announce_embeds_inner_announce_object() {
    let (builder, _local_sink, http_sink) = builder(&[(40, "erin")], &[]);
    let target = sample_status(6, 10);
    let recipient = ActorRef {
        uri: "https://remote.example/users/frank".to_string(),
        recipient: remote("https://remote.example/inbox"),
    };

    builder
        .deliver_undo(
            Id::from_i64(40),
            UndoKind::Announce,
            &target,
            recipient.clone(),
        )
        .await
        .expect("deliver_undo must succeed");

    let calls = http_sink.calls();
    let raw = calls[0].1.as_value();
    assert_eq!(value_str(raw, "type"), "Undo");
    let inner = raw.get("object").expect("inner object present");
    assert_eq!(value_str(inner, "type"), "Announce");
    assert_eq!(value_str(inner, "object"), target.uri);
    assert_ne!(
        value_str(raw, "id"),
        value_str(inner, "id"),
        "outer Undo and inner Announce must have distinct minted ids"
    );
}

#[tokio::test]
async fn deliver_undo_like_embeds_inner_like_object() {
    let (builder, _local_sink, http_sink) = builder(&[(41, "gina")], &[]);
    let target = sample_status(7, 10);
    let recipient = ActorRef {
        uri: "https://remote.example/users/harry".to_string(),
        recipient: remote("https://remote.example/inbox"),
    };

    builder
        .deliver_undo(Id::from_i64(41), UndoKind::Like, &target, recipient)
        .await
        .expect("deliver_undo must succeed");

    let calls = http_sink.calls();
    let raw = calls[0].1.as_value();
    let inner = raw.get("object").expect("inner object present");
    assert_eq!(value_str(inner, "type"), "Like");
}

// -- deliver_delete / deliver_update -----------------------------------------

#[tokio::test]
async fn deliver_delete_builds_canonical_delete_with_object_as_status_uri() {
    let (builder, local_sink, _http_sink) = builder(&[(10, "alice")], &["alice"]);
    let status = sample_status(8, 10);
    let addressing = sample_addressing();

    builder
        .deliver_delete(
            &status,
            &addressing,
            vec![Recipient::Local(handle("alice"))],
        )
        .await
        .expect("deliver_delete must succeed");

    let calls = local_sink.calls();
    let raw = calls[0].1.as_value();
    assert_eq!(value_str(raw, "type"), "Delete");
    assert_eq!(value_str(raw, "object"), status.uri);
}

#[tokio::test]
async fn deliver_update_builds_canonical_update_with_updated_timestamp() {
    let (builder, local_sink, _http_sink) = builder(&[(10, "alice")], &["alice"]);
    let mut status = sample_status(9, 10);
    status.edited_at = Some(datetime!(2026-07-25 12:00:00 UTC));
    let addressing = sample_addressing();

    builder
        .deliver_update(
            &status,
            &addressing,
            vec![Recipient::Local(handle("alice"))],
            None,
        )
        .await
        .expect("deliver_update must succeed");

    let calls = local_sink.calls();
    let raw = calls[0].1.as_value();
    assert_eq!(value_str(raw, "type"), "Update");
    let object = raw.get("object").expect("object present");
    assert_eq!(value_str(object, "type"), "Note");
    assert!(value_str(object, "updated").starts_with("2026-07-25"));
}

// -- deliver_vote ------------------------------------------------------------

#[tokio::test]
async fn deliver_vote_builds_create_note_with_name_and_never_emits_vote_type() {
    let (builder, _local_sink, http_sink) = builder(&[(50, "iris")], &[]);
    let target = sample_status(11, 10);
    let poll = Poll {
        id: Id::from_i64(200),
        status_id: target.id,
        expires_at: None,
        multiple: false,
    };
    let recipient = ActorRef {
        uri: "https://remote.example/users/jill".to_string(),
        recipient: remote("https://remote.example/inbox"),
    };

    builder
        .deliver_vote(
            Id::from_i64(50),
            &poll,
            &target,
            &["Option A".to_string()],
            recipient.clone(),
        )
        .await
        .expect("deliver_vote must succeed");

    let calls = http_sink.calls();
    assert_eq!(calls.len(), 1);
    let raw = calls[0].1.as_value();
    assert_eq!(value_str(raw, "type"), "Create");
    let object = raw.get("object").expect("object present");
    assert_eq!(value_str(object, "type"), "Note");
    assert_eq!(value_str(object, "name"), "Option A");
    assert_eq!(value_str(object, "inReplyTo"), target.uri);
    assert_eq!(
        value_array_of_strings(raw, "to"),
        vec![recipient.uri.clone()]
    );

    // The literal "Vote" must never appear as any Activity/object `type`.
    let serialized = raw.to_string();
    assert!(
        !serialized.contains("\"Vote\""),
        "vote wire form must never emit a Vote Activity type: {serialized}"
    );
}

#[tokio::test]
async fn deliver_vote_with_multiple_choices_delivers_one_create_per_choice() {
    let (builder, _local_sink, http_sink) = builder(&[(51, "kate")], &[]);
    let target = sample_status(12, 10);
    let poll = Poll {
        id: Id::from_i64(201),
        status_id: target.id,
        expires_at: None,
        multiple: true,
    };
    let recipient = ActorRef {
        uri: "https://remote.example/users/liam".to_string(),
        recipient: remote("https://remote.example/inbox"),
    };

    builder
        .deliver_vote(
            Id::from_i64(51),
            &poll,
            &target,
            &["Option A".to_string(), "Option B".to_string()],
            recipient,
        )
        .await
        .expect("deliver_vote must succeed");

    let calls = http_sink.calls();
    assert_eq!(calls.len(), 2, "one Create per selected choice");
    let names: Vec<String> = calls
        .iter()
        .map(|(_, activity, _)| {
            value_str(activity.as_value().get("object").unwrap(), "name").to_string()
        })
        .collect();
    assert_eq!(names, vec!["Option A".to_string(), "Option B".to_string()]);

    let ids: Vec<String> = calls
        .iter()
        .map(|(_, activity, _)| value_str(activity.as_value(), "id").to_string())
        .collect();
    assert_ne!(ids[0], ids[1], "each per-choice Create must get its own id");
}

#[tokio::test]
async fn deliver_vote_fails_when_voting_actor_is_unknown() {
    let (builder, _local_sink, http_sink) = builder(&[], &[]);
    let target = sample_status(13, 10);
    let poll = Poll {
        id: Id::from_i64(202),
        status_id: target.id,
        expires_at: None,
        multiple: false,
    };
    let recipient = ActorRef {
        uri: "https://remote.example/users/mona".to_string(),
        recipient: remote("https://remote.example/inbox"),
    };

    let err = builder
        .deliver_vote(
            Id::from_i64(999),
            &poll,
            &target,
            &["Option A".to_string()],
            recipient,
        )
        .await
        .expect_err("unknown voting actor must fail");

    assert_eq!(err.kind, ErrorKind::Client);
    assert_eq!(http_sink.calls().len(), 0);
}
