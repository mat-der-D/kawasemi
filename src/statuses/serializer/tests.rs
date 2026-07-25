//! Unit tests for `StatusSerializer`/`PollSerializer` (task 3.3, Requirements
//! 1.1-1.6, 2.1-2.4, 15.1), per this task's own observable completion
//! condition: "決定的 RuntimeContext 下で通常/reblog/編集済み/投票付きの
//! Status と Poll のゴールデンが一致する（契約テストがグリーン）". Also
//! covers null discipline (1.5), reblog nesting (1.3), `expired` computation
//! (2.3), and this task's own self-review point that no dialect field is
//! ever emitted (1.6, 15.1).

use time::macros::datetime;

use super::*;
use crate::statuses::poll_repository::PollTally;

fn sample_status(id: i64) -> Status {
    Status {
        id: Id::from_i64(id),
        actor_id: Id::from_i64(1),
        uri: format!("https://kawasemi.example/statuses/{id}"),
        url: Some(format!("https://kawasemi.example/@alice/{id}")),
        content: "<p>hello world</p>".to_string(),
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
        created_at: datetime!(2026-07-01 12:00:00 UTC),
        edited_at: None,
    }
}

fn account_json() -> Value {
    serde_json::json!({
        "id": "1",
        "username": "alice",
        "acct": "alice",
        "display_name": "Alice",
    })
}

fn no_interactions() -> StatusInteractionState {
    StatusInteractionState::default()
}

fn minimal_input(status: &Status) -> StatusRenderInput<'_> {
    StatusRenderInput {
        status,
        account: account_json(),
        media_attachments: Vec::new(),
        mentions: Vec::new(),
        tags: Vec::new(),
        emojis: Vec::new(),
        poll: None,
        interactions: no_interactions(),
        reblog: None,
    }
}

// -- Requirement 1.1: field presence -----------------------------------

#[test]
fn status_to_json_emits_every_requirement_1_1_field() {
    let status = sample_status(1);
    let json = status_to_json(&minimal_input(&status));

    for field in [
        "id",
        "uri",
        "url",
        "account",
        "content",
        "created_at",
        "visibility",
        "sensitive",
        "spoiler_text",
        "media_attachments",
        "mentions",
        "tags",
        "emojis",
        "reblogs_count",
        "favourites_count",
        "replies_count",
        "in_reply_to_id",
        "in_reply_to_account_id",
        "reblog",
        "poll",
        "language",
        "edited_at",
    ] {
        assert!(
            json.get(field).is_some(),
            "expected Status JSON to contain field {field:?}, got {json}"
        );
    }
    assert_eq!(json["id"], "1");
    assert_eq!(json["visibility"], "public");
}

// -- Requirement 1.2: viewer-scoped operation state ---------------------

#[test]
fn status_to_json_reflects_pre_resolved_operation_state() {
    let status = sample_status(1);
    let mut input = minimal_input(&status);
    input.interactions = StatusInteractionState {
        favourited: true,
        reblogged: true,
        bookmarked: true,
        pinned: true,
        muted: true,
    };
    let json = status_to_json(&input);

    assert_eq!(json["favourited"], true);
    assert_eq!(json["reblogged"], true);
    assert_eq!(json["bookmarked"], true);
    assert_eq!(json["pinned"], true);
    assert_eq!(json["muted"], true);
}

#[test]
fn status_to_json_operation_state_defaults_to_false_when_unresolved() {
    let status = sample_status(1);
    let json = status_to_json(&minimal_input(&status));

    assert_eq!(json["favourited"], false);
    assert_eq!(json["reblogged"], false);
    assert_eq!(json["bookmarked"], false);
    assert_eq!(json["pinned"], false);
    assert_eq!(json["muted"], false);
}

// -- Requirement 1.3: reblog nesting -------------------------------------

#[test]
fn status_to_json_nests_the_boosted_status_under_reblog() {
    let original = sample_status(1);
    let mut boost = sample_status(2);
    boost.reblog_of_id = Some(original.id);
    boost.content = String::new();

    let original_input = minimal_input(&original);
    let mut boost_input = minimal_input(&boost);
    boost_input.reblog = Some(Box::new(original_input));

    let json = status_to_json(&boost_input);

    assert_eq!(json["id"], "2");
    assert!(json["reblog"].is_object());
    assert_eq!(json["reblog"]["id"], "1");
    assert_eq!(json["reblog"]["content"], "<p>hello world</p>");
}

#[test]
fn status_to_json_reblog_is_null_when_not_a_boost() {
    let status = sample_status(1);
    let json = status_to_json(&minimal_input(&status));
    assert_eq!(json["reblog"], Value::Null);
}

// -- Requirement 1.5: null discipline ------------------------------------

#[test]
fn status_to_json_null_discipline_for_absent_optional_fields() {
    let status = sample_status(1);
    let json = status_to_json(&minimal_input(&status));

    assert_eq!(json["poll"], Value::Null);
    assert_eq!(json["in_reply_to_id"], Value::Null);
    assert_eq!(json["in_reply_to_account_id"], Value::Null);
    assert_eq!(json["edited_at"], Value::Null);
    assert_eq!(json["reblog"], Value::Null);
}

#[test]
fn status_to_json_edited_at_is_populated_once_edited() {
    let mut status = sample_status(1);
    status.edited_at = Some(datetime!(2026-07-02 09:30:00 UTC));
    let json = status_to_json(&minimal_input(&status));

    assert_ne!(json["edited_at"], Value::Null);
    assert_eq!(json["edited_at"], "2026-07-02T09:30:00Z");
}

#[test]
fn status_to_json_poll_is_populated_when_input_carries_one() {
    let status = sample_status(1);
    let mut input = minimal_input(&status);
    input.poll = Some(serde_json::json!({"id": "9"}));
    let json = status_to_json(&input);

    assert_ne!(json["poll"], Value::Null);
    assert_eq!(json["poll"]["id"], "9");
}

// -- Requirement 1.6, 15.1: no dialect fields ----------------------------

#[test]
fn status_to_json_contains_no_dialect_fields() {
    let status = sample_status(1);
    let json = status_to_json(&minimal_input(&status));
    let obj = json.as_object().expect("Status JSON is an object");

    for dialect_field in [
        "quote",
        "quote_id",
        "quoted_status_id",
        "emoji_reactions",
        "reactions",
    ] {
        assert!(
            !obj.contains_key(dialect_field),
            "Status JSON must not contain dialect field {dialect_field:?}"
        );
    }
}

// -- Determinism ----------------------------------------------------------

#[test]
fn status_to_json_is_deterministic_for_identical_input() {
    let status = sample_status(1);
    let first = status_to_json(&minimal_input(&status));
    let second = status_to_json(&minimal_input(&status));
    assert_eq!(first, second);
}

// ==== Poll ================================================================

fn sample_poll(id: i64, expires_at: Option<time::OffsetDateTime>, multiple: bool) -> Poll {
    Poll {
        id: Id::from_i64(id),
        status_id: Id::from_i64(100),
        expires_at,
        multiple,
    }
}

fn sample_tally(
    poll_id: Id,
    votes: &[(i32, &str, i64)],
    voters_count: i64,
    own_votes: Vec<i32>,
) -> PollTally {
    PollTally {
        poll_id,
        options: votes
            .iter()
            .map(
                |(idx, title, votes_count)| crate::statuses::model::PollOption {
                    poll_id,
                    idx: *idx,
                    title: title.to_string(),
                    votes_count: *votes_count,
                },
            )
            .collect(),
        voters_count,
        own_votes,
    }
}

fn ctx_at(now: time::OffsetDateTime) -> SerializeContext {
    SerializeContext {
        viewer: Some(Id::from_i64(1)),
        now,
    }
}

// -- Requirement 2.1: field presence -------------------------------------

#[test]
fn poll_to_json_emits_every_requirement_2_1_field() {
    let poll = sample_poll(1, Some(datetime!(2026-08-01 00:00:00 UTC)), false);
    let tally = sample_tally(poll.id, &[(0, "Yes", 3), (1, "No", 1)], 4, Vec::new());
    let ctx = ctx_at(datetime!(2026-07-01 00:00:00 UTC));

    let json = poll_to_json(&poll, &tally, &[], &ctx);

    for field in [
        "id",
        "expires_at",
        "expired",
        "multiple",
        "votes_count",
        "voters_count",
        "options",
        "emojis",
    ] {
        assert!(
            json.get(field).is_some(),
            "expected Poll JSON to contain field {field:?}, got {json}"
        );
    }
    assert_eq!(json["votes_count"], 4);
    assert_eq!(json["voters_count"], 4);
    let options = json["options"].as_array().unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options[0]["title"], "Yes");
    assert_eq!(options[0]["votes_count"], 3);
}

// -- Requirement 2.2: voter state -----------------------------------------

#[test]
fn poll_to_json_reflects_voted_and_own_votes_when_viewer_has_voted() {
    let poll = sample_poll(1, None, true);
    let tally = sample_tally(poll.id, &[(0, "A", 1), (1, "B", 1)], 1, vec![0, 1]);
    let ctx = ctx_at(datetime!(2026-07-01 00:00:00 UTC));

    let json = poll_to_json(&poll, &tally, &[], &ctx);

    assert_eq!(json["voted"], true);
    assert_eq!(json["own_votes"], serde_json::json!([0, 1]));
}

#[test]
fn poll_to_json_voted_is_false_when_viewer_has_not_voted() {
    let poll = sample_poll(1, None, false);
    let tally = sample_tally(poll.id, &[(0, "A", 0)], 0, Vec::new());
    let ctx = ctx_at(datetime!(2026-07-01 00:00:00 UTC));

    let json = poll_to_json(&poll, &tally, &[], &ctx);

    assert_eq!(json["voted"], false);
    assert_eq!(json["own_votes"], serde_json::json!([]));
}

// -- Requirement 2.3: expired computation from ctx.now, never wall-clock --

#[test]
fn poll_to_json_expired_is_false_before_the_deadline() {
    let poll = sample_poll(1, Some(datetime!(2026-08-01 00:00:00 UTC)), false);
    let tally = sample_tally(poll.id, &[(0, "A", 0)], 0, Vec::new());
    let ctx = ctx_at(datetime!(2026-07-01 00:00:00 UTC));

    let json = poll_to_json(&poll, &tally, &[], &ctx);
    assert_eq!(json["expired"], false);
}

#[test]
fn poll_to_json_expired_is_true_after_the_deadline() {
    let poll = sample_poll(1, Some(datetime!(2026-08-01 00:00:00 UTC)), false);
    let tally = sample_tally(poll.id, &[(0, "A", 0)], 0, Vec::new());
    let ctx = ctx_at(datetime!(2026-09-01 00:00:00 UTC));

    let json = poll_to_json(&poll, &tally, &[], &ctx);
    assert_eq!(json["expired"], true);
}

#[test]
fn poll_to_json_never_expires_when_no_deadline_is_set() {
    let poll = sample_poll(1, None, false);
    let tally = sample_tally(poll.id, &[(0, "A", 0)], 0, Vec::new());
    let ctx = ctx_at(datetime!(2099-01-01 00:00:00 UTC));

    let json = poll_to_json(&poll, &tally, &[], &ctx);
    assert_eq!(json["expired"], false);
    assert_eq!(json["expires_at"], Value::Null);
}

// ---- Requirements 1.4, 2.4: contract-harness golden registration ----
//
// Registers Status (normal/reblog/edited/poll-attached) and Poll
// (open/expired) JSON shapes as goldens via `crate::contract::assert_golden`
// (task 3.3), mirroring `accounts/serializer/tests.rs`'s identical
// precedent: a pure serializer has nothing non-deterministic upstream to
// inject a `RuntimeContext` boundary for -- literal `datetime!`/`Id::from_i64`
// fixtures (plus an explicit `ctx.now` for the Poll goldens) already satisfy
// Requirement 1.4/2.4's "決定的に再現可能" the same way every other
// serializer's own golden tests in this crate do.

#[test]
fn normal_status_json_matches_the_registered_contract_golden() {
    let mut status = sample_status(10);
    status.language = Some("en".to_string());
    let input = minimal_input(&status);

    let json = status_to_json(&input);

    crate::contract::assert_golden("tests/golden/statuses/status_normal.json", &json);
}

#[test]
fn reblog_status_json_matches_the_registered_contract_golden() {
    let original = sample_status(20);
    let mut boost = sample_status(21);
    boost.reblog_of_id = Some(original.id);
    boost.content = String::new();
    boost.reblogs_count = 5;

    let original_input = minimal_input(&original);
    let mut boost_input = minimal_input(&boost);
    boost_input.reblog = Some(Box::new(original_input));
    boost_input.interactions.reblogged = true;

    let json = status_to_json(&boost_input);

    crate::contract::assert_golden("tests/golden/statuses/status_reblog.json", &json);
}

#[test]
fn edited_status_json_matches_the_registered_contract_golden() {
    let mut status = sample_status(30);
    status.content = "<p>hello world, edited</p>".to_string();
    status.edited_at = Some(datetime!(2026-07-03 08:15:00 UTC));
    let input = minimal_input(&status);

    let json = status_to_json(&input);

    crate::contract::assert_golden("tests/golden/statuses/status_edited.json", &json);
}

#[test]
fn poll_attached_status_json_matches_the_registered_contract_golden() {
    let mut status = sample_status(40);
    let poll = sample_poll(4, Some(datetime!(2026-08-01 00:00:00 UTC)), false);
    status.poll_id = Some(poll.id);
    let tally = sample_tally(poll.id, &[(0, "Cats", 3), (1, "Dogs", 2)], 5, vec![0]);
    let ctx = ctx_at(datetime!(2026-07-15 00:00:00 UTC));
    let poll_json = poll_to_json(&poll, &tally, &[], &ctx);

    let mut input = minimal_input(&status);
    input.poll = Some(poll_json);

    let json = status_to_json(&input);

    crate::contract::assert_golden("tests/golden/statuses/status_with_poll.json", &json);
}

#[test]
fn open_poll_json_matches_the_registered_contract_golden() {
    let poll = sample_poll(5, Some(datetime!(2026-08-01 00:00:00 UTC)), true);
    let tally = sample_tally(poll.id, &[(0, "Yes", 2), (1, "No", 1)], 3, vec![0]);
    let ctx = ctx_at(datetime!(2026-07-01 00:00:00 UTC));

    let json = poll_to_json(&poll, &tally, &[], &ctx);

    crate::contract::assert_golden("tests/golden/statuses/poll_open.json", &json);
}

#[test]
fn expired_poll_json_matches_the_registered_contract_golden() {
    let poll = sample_poll(6, Some(datetime!(2026-08-01 00:00:00 UTC)), false);
    let tally = sample_tally(poll.id, &[(0, "Yes", 2), (1, "No", 1)], 3, Vec::new());
    let ctx = ctx_at(datetime!(2026-09-01 00:00:00 UTC));

    let json = poll_to_json(&poll, &tally, &[], &ctx);

    crate::contract::assert_golden("tests/golden/statuses/poll_expired.json", &json);
}
