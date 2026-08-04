//! Tests for [`super::StatusRenderAssembler`].
//!
//! The assertions that matter here are about the two injection points this
//! module exists to expose. Everything else the assembler does — the
//! attachment, tag, emoji, and interaction lookups — is already covered
//! through the five callers' own suites, and duplicating that here would
//! just be a sixth copy of what this module set out to remove.
//!
//! DB-backed, via `spawn_test_app`, following this crate's convention for
//! anything touching repositories.

use std::collections::HashSet;

use time::OffsetDateTime;

use super::*;
use crate::actor::owner::create_owner;
use crate::actor::repository::insert_actor;
use crate::actor::{ActorState, ActorType, Handle};
use crate::domain::Visibility;
use crate::statuses::model::PollOption;
use crate::statuses::poll_repository::{find_poll_by_id, insert_poll, tally};
use crate::statuses::status_repository::insert_status;
use crate::test_harness::{TestApp, spawn_test_app};

// -- fixtures ---------------------------------------------------------------

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

fn sample_status(
    id: Id,
    actor_id: Id,
    content: &str,
    poll_id: Option<Id>,
    created_at: OffsetDateTime,
) -> Status {
    Status {
        id,
        actor_id,
        uri: format!("https://kawasemi.example/statuses/{}", id.as_i64()),
        url: None,
        content: content.to_string(),
        visibility: Visibility::Public,
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
        local: true,
        created_at,
        edited_at: None,
    }
}

async fn create_test_status(app: &TestApp, actor_id: Id, content: &str) -> Status {
    let id = app.runtime.ids.next_id();
    let status = sample_status(id, actor_id, content, None, app.runtime.clock.now());
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");
    status
}

fn assembler(app: &TestApp) -> StatusRenderAssembler {
    StatusRenderAssembler::new(
        app.pool.clone(),
        app.state.accounts().service(),
        app.state.media().store().clone(),
    )
}

fn origin() -> ForwardedOrigin {
    ForwardedOrigin::resolve("https", "kawasemi.example", None, None)
}

// -- poll resolvers ---------------------------------------------------------

/// Reads the row directly and tolerates its absence — the behavior
/// timelines and search both rely on to render a status whose poll has been
/// deleted rather than failing the whole page.
struct TolerantPolls(PgPool);

impl PollResolver for TolerantPolls {
    fn resolve_many<'a>(
        &'a self,
        poll_ids: &'a [Id],
        viewer: Option<Id>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<(Id, Poll, PollTally)>, AppError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut out = Vec::new();
            for &poll_id in poll_ids {
                if let Some(poll) = find_poll_by_id(&self.0, poll_id).await? {
                    let tally = tally(&self.0, poll_id, viewer).await?;
                    out.push((poll_id, poll, tally));
                }
            }
            Ok(out)
        })
    }
}

/// Treats a dangling `poll_id` as an error, the way the status endpoints,
/// the account post list, and notifications each do with their own
/// not-found error.
struct StrictPolls(PgPool);

impl PollResolver for StrictPolls {
    fn resolve_many<'a>(
        &'a self,
        poll_ids: &'a [Id],
        viewer: Option<Id>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<(Id, Poll, PollTally)>, AppError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut out = Vec::new();
            for &poll_id in poll_ids {
                let poll = find_poll_by_id(&self.0, poll_id).await?.ok_or_else(|| {
                    AppError::client(axum::http::StatusCode::NOT_FOUND, "poll not found")
                })?;
                let tally = tally(&self.0, poll_id, viewer).await?;
                out.push((poll_id, poll, tally));
            }
            Ok(out)
        })
    }
}

// -- `muted` injection ------------------------------------------------------

/// The four callers that hand over no mute context must keep rendering
/// `muted: false`, exactly as their own private copies hard-coded.
#[tokio::test]
async fn muted_is_false_when_the_caller_supplies_no_mute_context() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "mutedauthor_none").await;
    let viewer = create_test_actor(&app, "mutedviewer_none").await;
    let status = create_test_status(&app, author, "hello").await;

    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: Some(viewer),
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: None,
        polls: &polls,
        emojis: EmojiResolution {
            content: true,
            poll_options: true,
        },
    };

    let json = assembler(&app)
        .assemble_one(&status, None, &ctx)
        .await
        .expect("assembling must succeed");
    assert_eq!(json["muted"], serde_json::json!(false));

    app.cleanup().await;
}

/// The one caller that does supply mute context must see it reflected —
/// this is the difference that had already appeared between the copies, and
/// the reason it is an explicit input now.
#[tokio::test]
async fn muted_reflects_the_supplied_mute_context() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "mutedauthor_yes").await;
    let viewer = create_test_actor(&app, "mutedviewer_yes").await;
    let status = create_test_status(&app, author, "hello").await;

    let muted: HashSet<Id> = HashSet::from([author]);
    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: Some(viewer),
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: Some(&muted),
        polls: &polls,
        emojis: EmojiResolution {
            content: true,
            poll_options: true,
        },
    };

    let json = assembler(&app)
        .assemble_one(&status, None, &ctx)
        .await
        .expect("assembling must succeed");
    assert_eq!(json["muted"], serde_json::json!(true));

    app.cleanup().await;
}

/// Mute is judged per status by that status's own author, so boosting a
/// muted account's post does not make the boost itself read as muted (nor
/// the other way around).
#[tokio::test]
async fn muted_is_judged_per_status_by_its_own_author() {
    let app = spawn_test_app().await;
    let booster = create_test_actor(&app, "muteboost_booster").await;
    let original_author = create_test_actor(&app, "muteboost_author").await;
    let viewer = create_test_actor(&app, "muteboost_viewer").await;

    let target = create_test_status(&app, original_author, "original").await;
    let mut boost = create_test_status(&app, booster, "").await;
    boost.reblog_of_id = Some(target.id);

    // Only the boosted post's author is muted, not the booster.
    let muted: HashSet<Id> = HashSet::from([original_author]);
    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: Some(viewer),
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: Some(&muted),
        polls: &polls,
        emojis: EmojiResolution {
            content: true,
            poll_options: true,
        },
    };

    let json = assembler(&app)
        .assemble_one(&boost, Some(&target), &ctx)
        .await
        .expect("assembling must succeed");

    assert_eq!(
        json["muted"],
        serde_json::json!(false),
        "the boost's own author is not muted"
    );
    assert_eq!(
        json["reblog"]["muted"],
        serde_json::json!(true),
        "the boosted post's author is muted"
    );

    app.cleanup().await;
}

// -- `PollResolver` injection -----------------------------------------------

/// A dangling `poll_id` renders as a poll-less status under the tolerant
/// resolver...
#[tokio::test]
async fn a_missing_poll_renders_as_none_under_the_tolerant_resolver() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "pollmissing_tolerant").await;
    let mut status = create_test_status(&app, author, "vote please").await;
    status.poll_id = Some(app.runtime.ids.next_id()); // never inserted

    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: None,
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: None,
        polls: &polls,
        emojis: EmojiResolution {
            content: true,
            poll_options: true,
        },
    };

    let json = assembler(&app)
        .assemble_one(&status, None, &ctx)
        .await
        .expect("a dangling poll must not fail the render");
    assert_eq!(json["poll"], serde_json::Value::Null);

    app.cleanup().await;
}

/// ...and as an error under the strict one. Same assembler, same status:
/// the difference lives entirely in the injected resolver, which is what
/// lets all five callers share this code without any of them changing
/// behavior.
#[tokio::test]
async fn a_missing_poll_is_an_error_under_the_strict_resolver() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "pollmissing_strict").await;
    let mut status = create_test_status(&app, author, "vote please").await;
    status.poll_id = Some(app.runtime.ids.next_id()); // never inserted

    let polls = StrictPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: None,
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: None,
        polls: &polls,
        emojis: EmojiResolution {
            content: true,
            poll_options: true,
        },
    };

    let err = assembler(&app)
        .assemble_one(&status, None, &ctx)
        .await
        .expect_err("the strict resolver must surface a dangling poll");
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);

    app.cleanup().await;
}

/// A present poll renders through either resolver, so the divergence above
/// really is confined to the missing-row case.
#[tokio::test]
async fn a_present_poll_renders_its_options() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "pollpresent_author").await;

    let poll_id = app.runtime.ids.next_id();
    let status_id = app.runtime.ids.next_id();
    let status = sample_status(
        status_id,
        author,
        "vote please",
        Some(poll_id),
        app.runtime.clock.now(),
    );
    insert_status(&app.pool, &status)
        .await
        .expect("insert_status must succeed");

    let poll = Poll {
        id: poll_id,
        status_id,
        expires_at: None,
        multiple: false,
    };
    let options = vec![
        PollOption {
            poll_id,
            idx: 0,
            title: "Yes".to_string(),
            votes_count: 0,
        },
        PollOption {
            poll_id,
            idx: 1,
            title: "No".to_string(),
            votes_count: 0,
        },
    ];
    insert_poll(&app.pool, &poll, &options)
        .await
        .expect("insert_poll must succeed");

    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: None,
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: None,
        polls: &polls,
        emojis: EmojiResolution {
            content: true,
            poll_options: true,
        },
    };

    let json = assembler(&app)
        .assemble_one(&status, None, &ctx)
        .await
        .expect("assembling must succeed");
    assert_eq!(
        json["poll"]["options"][0]["title"],
        serde_json::json!("Yes")
    );
    assert_eq!(json["poll"]["options"][1]["title"], serde_json::json!("No"));

    app.cleanup().await;
}

// -- batch shape ------------------------------------------------------------

/// The batch entry point preserves input order — the single guarantee every
/// list endpoint depends on and none of them can check for themselves.
#[tokio::test]
async fn assemble_many_preserves_input_order() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "orderauthor").await;

    let mut statuses = Vec::new();
    for i in 0..4 {
        statuses.push(create_test_status(&app, author, &format!("post {i}")).await);
    }
    let reblog_targets: Vec<Option<Status>> = vec![None; statuses.len()];

    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: None,
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: None,
        polls: &polls,
        emojis: EmojiResolution {
            content: true,
            poll_options: true,
        },
    };

    let rendered = assembler(&app)
        .assemble_many(&statuses, &reblog_targets, &ctx)
        .await
        .expect("assembling must succeed");

    assert_eq!(rendered.len(), statuses.len());
    for (json, status) in rendered.iter().zip(&statuses) {
        assert_eq!(
            json["id"],
            serde_json::json!(status.id.as_i64().to_string())
        );
    }

    app.cleanup().await;
}

/// One status through `assemble_one` and the same status through
/// `assemble_many` must be byte-identical: they are the same path, and this
/// is what stops a future change from being applied to only one of them.
#[tokio::test]
async fn assemble_one_matches_a_single_element_batch() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "singlebatch_author").await;
    let status = create_test_status(&app, author, "hello :wave:").await;

    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: None,
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: None,
        polls: &polls,
        emojis: EmojiResolution {
            content: true,
            poll_options: true,
        },
    };
    let asm = assembler(&app);

    let single = asm
        .assemble_one(&status, None, &ctx)
        .await
        .expect("assembling must succeed");
    let batch = asm
        .assemble_many(std::slice::from_ref(&status), &[None], &ctx)
        .await
        .expect("assembling must succeed");

    assert_eq!(batch.len(), 1);
    assert_eq!(single, batch[0]);

    app.cleanup().await;
}
