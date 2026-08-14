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
use crate::media::media_repository::insert_media;
use crate::media::model::{Focus, Media, MediaState, MediaType};
use crate::media::store::ObjectKey;
use crate::statuses::interaction_repository::{add_bookmark, add_favourite, set_pin};
use crate::statuses::model::{PollOption, Tag};
use crate::statuses::poll_repository::{find_poll_by_id, insert_poll, tally};
use crate::statuses::status_repository::{attach_media, insert_status};
use crate::statuses::tag_repository::{associate_tag, upsert_tag};
use crate::test_harness::query_log::{QueryKind, QueryLog, record_queries};
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
    fn resolve_many<'a>(&'a self, poll_ids: &'a [Id], viewer: Option<Id>) -> PollResolution<'a> {
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
    fn resolve_many<'a>(&'a self, poll_ids: &'a [Id], viewer: Option<Id>) -> PollResolution<'a> {
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

// -- custom emoji ------------------------------------------------------------

/// Seeds a locally-registered custom emoji, mirroring
/// `tests/statuses_endpoints_it.rs`'s identical test-local helper.
async fn seed_custom_emoji(app: &TestApp, shortcode: &str) {
    let now = app.runtime.clock.now();
    let url = format!("https://example.test/emoji/{shortcode}.png");
    sqlx::query(
        "INSERT INTO custom_emojis \
             (shortcode, domain, url, static_url, visible_in_picker, category, updated_at) \
         VALUES ($1, '', $2, $2, TRUE, NULL, $3)",
    )
    .bind(shortcode)
    .bind(&url)
    .bind(now)
    .execute(&app.pool)
    .await
    .expect("seeding a custom_emojis row must succeed");
}

/// Every render path resolves the status's own shortcodes.
///
/// Three of the five used to and two did not, so the same post arrived at a
/// client as an image from one endpoint and as literal `:shortcode:` text
/// from another. There is now one path, so there is one answer.
#[tokio::test]
async fn resolves_registered_shortcodes_in_the_status_content() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "emojiauthor").await;
    seed_custom_emoji(&app, "kawasemi").await;
    let status = create_test_status(&app, author, "hello :kawasemi: world").await;

    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: None,
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: None,
        polls: &polls,
    };

    let json = assembler(&app)
        .assemble_one(&status, None, &ctx)
        .await
        .expect("assembling must succeed");

    let emojis = json["emojis"].as_array().expect("emojis must be an array");
    assert_eq!(emojis.len(), 1, "the registered shortcode must resolve");
    assert_eq!(emojis[0]["shortcode"], serde_json::json!("kawasemi"));

    app.cleanup().await;
}

/// The same applies to a poll's option titles, which carry their own
/// shortcodes independently of the status content.
#[tokio::test]
async fn resolves_registered_shortcodes_in_poll_option_titles() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "pollemojiauthor").await;
    seed_custom_emoji(&app, "yes").await;

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
            title: "definitely :yes:".to_string(),
            votes_count: 0,
        },
        PollOption {
            poll_id,
            idx: 1,
            title: "no".to_string(),
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
    };

    let json = assembler(&app)
        .assemble_one(&status, None, &ctx)
        .await
        .expect("assembling must succeed");

    let emojis = json["poll"]["emojis"]
        .as_array()
        .expect("poll emojis must be an array");
    assert_eq!(emojis.len(), 1, "the option title's shortcode must resolve");
    assert_eq!(emojis[0]["shortcode"], serde_json::json!("yes"));

    app.cleanup().await;
}

// -- resolved-material characterization -------------------------------------

/// Inserts a ready `media` row owned by `actor_id` and returns its id.
///
/// No bytes are stored: [`to_media_attachment`](crate::media::serializer::to_media_attachment)
/// derives its URLs from the id and the store's public-URL rule alone, so a
/// row is the whole fixture a render needs.
async fn create_test_media(app: &TestApp, actor_id: Id) -> Id {
    let media_id = app.runtime.ids.next_id();
    let media = Media {
        id: media_id,
        actor_id,
        media_type: MediaType::Image,
        state: MediaState::Ready,
        description: Some("an attachment".to_string()),
        focus: Focus::default(),
        meta: None,
        blurhash: None,
        created_at: app.runtime.clock.now(),
    };
    insert_media(
        &app.pool,
        &media,
        ObjectKey::original(media_id).as_str(),
        "image/png",
    )
    .await
    .expect("insert_media must succeed");

    media_id
}

/// Registers `name` as a tag and associates it with `status_id`.
async fn attach_tag(app: &TestApp, status_id: Id, name: &str) {
    let tag = Tag {
        id: app.runtime.ids.next_id(),
        name: name.to_string(),
        created_at: app.runtime.clock.now(),
    };
    let tag = upsert_tag(&app.pool, &tag)
        .await
        .expect("upsert_tag must succeed");
    associate_tag(&app.pool, status_id, tag.id)
        .await
        .expect("associate_tag must succeed");
}

/// Pulls one field out of every element of a JSON array, tolerating a
/// non-array (a `null` `poll` indexes to `null`, not a panic).
fn field_list(items: &Value, field: &str) -> Vec<Value> {
    items
        .as_array()
        .map(|items| items.iter().map(|item| item[field].clone()).collect())
        .unwrap_or_default()
}

/// Projects exactly the fields the assembler resolves for itself — the
/// author's Account, the attachments, the tags, the emoji, the poll, and the
/// viewer's interaction state — recursing into `reblog`.
///
/// Deliberately a projection rather than a whole-document snapshot: the rest
/// of Status JSON is `status_to_json`'s contract, already pinned by that
/// module's own golden tests, and pinning it a second time here would turn
/// every unrelated contract change into a failure in the wrong file. What is
/// left is precisely what changes if the assembly glue resolves a different
/// set of materials, or the same set in a different order.
fn material_fingerprint(json: &Value) -> Value {
    serde_json::json!({
        "account": json["account"]["id"],
        "media": field_list(&json["media_attachments"], "id"),
        "tags": field_list(&json["tags"], "name"),
        "tag_urls": field_list(&json["tags"], "url"),
        "emojis": field_list(&json["emojis"], "shortcode"),
        "poll_options": field_list(&json["poll"]["options"], "title"),
        "poll_emojis": field_list(&json["poll"]["emojis"], "shortcode"),
        "favourited": json["favourited"],
        "reblogged": json["reblogged"],
        "bookmarked": json["bookmarked"],
        "pinned": json["pinned"],
        "muted": json["muted"],
        "reblog": match json["reblog"] {
            Value::Null => Value::Null,
            ref reblog => material_fingerprint(reblog),
        },
    })
}

/// The characterization this module's batching is measured against: one
/// batch carrying a boost of a status that has every kind of attached
/// material at once — two attachments plus a reaped third, two tags, two
/// shortcodes in its content, a poll whose option titles carry a third
/// shortcode, and a viewer who has favourited, bookmarked and boosted it —
/// alongside a second post by the *same* author, with a mute context that
/// covers that author but not the booster.
///
/// Every expectation below was captured from the sequential implementation
/// before the materials were batched, so a difference here is a difference
/// in rendered output, which batching must not produce. Four of them
/// are load-bearing in ways a smaller fixture would miss:
///
/// - `emojis` comes back `["alpha", "zulu"]` while the content mentions
///   `:zulu:` first: resolution order is the repository's `ORDER BY
///   shortcode`, not the content's first-appearance order. Batching the
///   resolution must not turn that back into appearance order.
/// - `media` skips the reaped middle attachment and keeps the surviving two
///   in `position` order.
/// - `tags` is ordered by tag id, not by name (`zebra` was registered
///   first).
/// - `muted` is `true` on the boosted post and `false` on the boost, from a
///   single mute set — each status judged by its own author.
#[tokio::test]
async fn a_rich_batch_keeps_every_resolved_material_and_its_order() {
    let app = spawn_test_app().await;
    let booster = create_test_actor(&app, "richbatch_booster").await;
    let author = create_test_actor(&app, "richbatch_author").await;
    let viewer = create_test_actor(&app, "richbatch_viewer").await;
    let now = app.runtime.clock.now();

    for shortcode in ["alpha", "zulu", "yes"] {
        seed_custom_emoji(&app, shortcode).await;
    }

    // The boosted post: content shortcodes deliberately out of alphabetical
    // order, a poll whose option titles carry a shortcode of their own.
    let poll_id = app.runtime.ids.next_id();
    let target_id = app.runtime.ids.next_id();
    let target = sample_status(
        target_id,
        author,
        "boosted :zulu: and :alpha:",
        Some(poll_id),
        now,
    );
    insert_status(&app.pool, &target)
        .await
        .expect("insert_status must succeed");
    insert_poll(
        &app.pool,
        &Poll {
            id: poll_id,
            status_id: target_id,
            expires_at: None,
            multiple: false,
        },
        &[
            PollOption {
                poll_id,
                idx: 0,
                title: "definitely :yes:".to_string(),
                votes_count: 0,
            },
            PollOption {
                poll_id,
                idx: 1,
                title: "no".to_string(),
                votes_count: 0,
            },
        ],
    )
    .await
    .expect("insert_poll must succeed");

    let first_media = create_test_media(&app, author).await;
    let reaped_media = app.runtime.ids.next_id(); // associated, never inserted
    let last_media = create_test_media(&app, author).await;
    attach_media(
        &app.pool,
        target_id,
        &[first_media, reaped_media, last_media],
    )
    .await
    .expect("attach_media must succeed");

    attach_tag(&app, target_id, "zebra").await;
    attach_tag(&app, target_id, "apple").await;

    add_favourite(&app.pool, viewer, target_id, now)
        .await
        .expect("add_favourite must succeed");
    add_bookmark(&app.pool, app.runtime.ids.next_id(), viewer, target_id, now)
        .await
        .expect("add_bookmark must succeed");
    // The viewer's own boost of the target: a `statuses` row, not an
    // interaction row, and the only thing `reblogged` reads.
    let viewer_boost_id = app.runtime.ids.next_id();
    let mut viewer_boost = sample_status(viewer_boost_id, viewer, "", None, now);
    viewer_boost.reblog_of_id = Some(target_id);
    insert_status(&app.pool, &viewer_boost)
        .await
        .expect("insert_status must succeed");

    // The boost the batch renders, and a second post by the same author as
    // the boosted one — the case author memoization exists for.
    let mut boost = create_test_status(&app, booster, "").await;
    boost.reblog_of_id = Some(target_id);
    let sibling = create_test_status(&app, author, "plain :alpha: post").await;
    set_pin(&app.pool, viewer, sibling.id, true, now)
        .await
        .expect("set_pin must succeed");

    let muted: HashSet<Id> = HashSet::from([author]);
    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: Some(viewer),
        now,
        origin: &origin,
        muted: Some(&muted),
        polls: &polls,
    };

    let rendered = assembler(&app)
        .assemble_many(
            &[boost.clone(), sibling.clone()],
            &[Some(target.clone()), None],
            &ctx,
        )
        .await
        .expect("assembling must succeed");

    assert_eq!(rendered.len(), 2);
    assert_eq!(
        rendered[0]["id"],
        serde_json::json!(boost.id.as_i64().to_string()),
        "the boost stays first"
    );
    assert_eq!(
        rendered[1]["id"],
        serde_json::json!(sibling.id.as_i64().to_string()),
        "the sibling post stays second"
    );

    let tag_url = |name: &str| format!("https://kawasemi.example/tags/{name}");
    assert_eq!(
        material_fingerprint(&rendered[0]),
        serde_json::json!({
            "account": booster.as_i64().to_string(),
            "media": [],
            "tags": [],
            "tag_urls": [],
            "emojis": [],
            "poll_options": [],
            "poll_emojis": [],
            "favourited": false,
            "reblogged": false,
            "bookmarked": false,
            "pinned": false,
            "muted": false,
            "reblog": {
                "account": author.as_i64().to_string(),
                "media": [
                    first_media.as_i64().to_string(),
                    last_media.as_i64().to_string(),
                ],
                "tags": ["zebra", "apple"],
                "tag_urls": [tag_url("zebra"), tag_url("apple")],
                "emojis": ["alpha", "zulu"],
                "poll_options": ["definitely :yes:", "no"],
                "poll_emojis": ["yes"],
                "favourited": true,
                "reblogged": true,
                "bookmarked": true,
                "pinned": false,
                "muted": true,
                "reblog": null,
            },
        }),
    );

    assert_eq!(
        material_fingerprint(&rendered[1]),
        serde_json::json!({
            "account": author.as_i64().to_string(),
            "media": [],
            "tags": [],
            "tag_urls": [],
            "emojis": ["alpha"],
            "poll_options": [],
            "poll_emojis": [],
            "favourited": false,
            "reblogged": false,
            "bookmarked": false,
            "pinned": true,
            "muted": true,
            "reblog": null,
        }),
    );

    app.cleanup().await;
}

/// The same fixture shape, read without a viewer: every interaction flag is
/// `false` regardless of the rows that exist, and `muted` — which is not
/// viewer-scoped — still follows the supplied mute context.
#[tokio::test]
async fn an_unauthenticated_batch_reports_no_interactions_but_still_mutes() {
    let app = spawn_test_app().await;
    let author = create_test_actor(&app, "anonbatch_author").await;
    let viewer = create_test_actor(&app, "anonbatch_viewer").await;
    let now = app.runtime.clock.now();

    let status = create_test_status(&app, author, "hello").await;
    add_favourite(&app.pool, viewer, status.id, now)
        .await
        .expect("add_favourite must succeed");
    add_bookmark(&app.pool, app.runtime.ids.next_id(), viewer, status.id, now)
        .await
        .expect("add_bookmark must succeed");
    set_pin(&app.pool, viewer, status.id, true, now)
        .await
        .expect("set_pin must succeed");

    let muted: HashSet<Id> = HashSet::from([author]);
    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: None,
        now,
        origin: &origin,
        muted: Some(&muted),
        polls: &polls,
    };

    let rendered = assembler(&app)
        .assemble_many(std::slice::from_ref(&status), &[None], &ctx)
        .await
        .expect("assembling must succeed");

    assert_eq!(rendered[0]["favourited"], serde_json::json!(false));
    assert_eq!(rendered[0]["bookmarked"], serde_json::json!(false));
    assert_eq!(rendered[0]["pinned"], serde_json::json!(false));
    assert_eq!(rendered[0]["reblogged"], serde_json::json!(false));
    assert_eq!(rendered[0]["muted"], serde_json::json!(true));

    app.cleanup().await;
}

// -- author resolution -----------------------------------------------------

/// Inserts `count` statuses whose authors cycle through `authors`, so that a
/// list of a fixed length can be built with any number of distinct authors
/// in it.
async fn statuses_by(app: &TestApp, authors: &[Id], count: usize) -> Vec<Status> {
    let mut out = Vec::with_capacity(count);
    for nth in 0..count {
        out.push(create_test_status(app, authors[nth % authors.len()], "hello").await);
    }
    out
}

/// Assembles `statuses` (none of them boosts) and hands back what it cost.
async fn measure(app: &TestApp, statuses: &[Status], ctx: &RenderContext<'_>) -> QueryLog {
    let no_reblogs = vec![None; statuses.len()];
    let (rendered, log) = record_queries(
        &app.pool,
        assembler(app).assemble_many(statuses, &no_reblogs, ctx),
    )
    .await;
    assert_eq!(
        rendered.expect("assembling must succeed").len(),
        statuses.len(),
        "every status in the list must render"
    );
    log
}

/// Measured: the number of account-resolution
/// queries a call issues tracks the number of **distinct authors** in the
/// list, and not the number of statuses.
///
/// Nothing here is compared against a literal query count. The cost of
/// resolving one author is *derived*, from the one list whose resolution
/// count is not in question — a single status by a single author, which
/// resolves exactly one author however the assembler is written — and every
/// other assertion is stated in terms of that derived unit. So the test
/// keeps meaning what it says if `AccountService::show_account` ever grows
/// or loses a query of its own, and it still fails the moment the
/// memoization in `resolve_materials` stops collapsing repeat authors.
///
/// The list carries no polls, so the injected [`PollResolver`] is never
/// consulted — asserted, not assumed — and the counts below are the
/// assembler's own rather than partly a test double's.
#[tokio::test]
async fn author_resolution_tracks_distinct_authors_and_not_list_length() {
    let app = spawn_test_app().await;
    let viewer = create_test_actor(&app, "authorscale_viewer").await;
    let mut authors = Vec::with_capacity(20);
    for nth in 0..20 {
        authors.push(create_test_actor(&app, &format!("authorscale_{nth}")).await);
    }

    let polls = TolerantPolls(app.pool.clone());
    let origin = origin();
    let ctx = RenderContext {
        viewer: Some(viewer),
        now: app.runtime.clock.now(),
        origin: &origin,
        muted: None,
        polls: &polls,
    };

    let one_author_one_status =
        measure(&app, &statuses_by(&app, &authors[..1], 1).await, &ctx).await;
    let one_author = measure(&app, &statuses_by(&app, &authors[..1], 20).await, &ctx).await;
    let two_authors = measure(&app, &statuses_by(&app, &authors[..2], 20).await, &ctx).await;
    let twenty_authors = measure(&app, &statuses_by(&app, &authors[..20], 20).await, &ctx).await;

    let resolutions = |log: &QueryLog| log.count(QueryKind::AccountResolution);

    // One status by one author resolves one author — the unit every other
    // count below is expressed in.
    let per_author = resolutions(&one_author_one_status);
    assert!(
        per_author > 0,
        "resolving an author must cost at least one query, or this test could \
         not tell memoization from a no-op.\nobserved: {:#?}",
        one_author_one_status.per_statement(),
    );

    // Twenty statuses sharing one author resolve them once
    // — the same cost as a single status by them.
    assert_eq!(
        resolutions(&one_author),
        per_author,
        "twenty statuses by one author must resolve that author exactly once, \
         at the cost of {per_author} queries, not {}.\nobserved: {:#?}",
        resolutions(&one_author),
        one_author.per_statement(),
    );

    // K distinct authors cost K resolutions — at K = 2 and
    // at K = N, where "proportional to K" and "proportional to N" would
    // otherwise be indistinguishable.
    assert_eq!(
        resolutions(&two_authors),
        2 * per_author,
        "twenty statuses by two authors must resolve two authors.\nobserved: {:#?}",
        two_authors.per_statement(),
    );
    assert_eq!(
        resolutions(&twenty_authors),
        20 * per_author,
        "twenty statuses by twenty authors must resolve twenty authors, no more.\n\
         observed: {:#?}",
        twenty_authors.per_statement(),
    );

    // The batched per-status materials are unchanged by any of it:
    // same list length, same author count, same everything.
    one_author_one_status.require_kinds(&[
        QueryKind::Media,
        QueryKind::Tags,
        QueryKind::Interaction,
    ]);
    for kind in [QueryKind::Media, QueryKind::Tags, QueryKind::Interaction] {
        assert_eq!(
            one_author_one_status.count(kind),
            one_author.count(kind),
            "{kind:?} queries must not depend on the list's length"
        );
        assert_eq!(
            one_author.count(kind),
            twenty_authors.count(kind),
            "{kind:?} queries must not depend on the author count either"
        );
    }
    assert_eq!(
        twenty_authors.count(QueryKind::Poll) + twenty_authors.count(QueryKind::PollPerPoll),
        0,
        "a poll-less list must not consult the injected resolver at all"
    );

    app.cleanup().await;
}
