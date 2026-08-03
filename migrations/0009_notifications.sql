-- 0009_notifications.sql
--
-- notifications task 1.1 (通知スキーマのマイグレーション; Requirement 8:
-- 通知の重複排除). Adds this spec's own owned persistent state: the single
-- `notifications` table later tasks' `NotificationRepository` (task 1.2)
-- and `NotificationGenerator`/`NotificationSerializer` (task 1.3+) read and
-- write. This migration only adds schema — no Rust code in this task.
--
-- Migration numbering: `0009` is not a guess. `research.md`'s "Migration
-- Numbering Coordination" section records that, at the time specs were
-- generated in parallel, notifications was assigned `0009` and no other
-- spec's research.md claimed it (federation-core -> `0008`, search ->
-- `0010`, both confirmed non-colliding with this spec's own `0009`).
-- `migrations/0011_status_mentions_and_remote_attachments.sql`'s own
-- naming-note comment independently corroborates this from the other
-- direction: it documents that, as of its own authoring, `0008`/`0009`/
-- `0010` were still unmaterialized as files but already reserved --
-- `0009_notifications.sql` by name -- for this spec, and picks `0011` as
-- the next free slot specifically *because* `0009` was already spoken for.
-- This repo's real migration history as of this task
-- (`0001_init_runtime.sql` core-runtime, `0002_actors.sql` actor-model,
-- `0003_oauth.sql` api-foundation, `0004_federation.sql` federation-core,
-- `0005_media.sql` media-pipeline, `0006_accounts.sql`
-- accounts-and-instance, `0007_statuses.sql` statuses-core,
-- `0011_status_mentions_and_remote_attachments.sql` statuses-core,
-- `0012_social_graph.sql` social-graph) has no file at `0009`, so this
-- migration claims that reserved, still-free slot. sqlx's migrator
-- (`src/migrate.rs`) applies embedded migrations in ascending
-- filename-version order tracked in `_sqlx_migrations` and does not require
-- version numbers to be contiguous -- only unique and never reused -- so
-- `0009` landing after `0007` and before the already-applied `0011`/`0012`
-- is a fully ordinary forward-only append, not a reordering of any already
-- recorded migration.
--
-- Purpose (design.md "Logical Data Model" / "Physical Data Model";
-- Requirement 8: 通知の重複排除, acceptance criteria 8.1, 8.2):
--   - `notifications`: one row per generated notification. `recipient_id`
--     is a logical-only reference to actor-model/accounts-and-instance's
--     local actor (no `REFERENCES`, mirroring this repo's already-
--     established cross-module-boundary convention of never taking a hard
--     `REFERENCES` across a spec boundary -- see `migrations/0012_social_
--     graph.sql`'s doc comment for the identical pattern applied to
--     `follows.follower_id`/`followee_id`) -- notifications does not own
--     actor-model/accounts-and-instance. Requirement 5.3 (受信者はローカル
--     アクター限定) is enforced by the generation-time `NotificationGenerator`
--     (a later task), not by a schema-level constraint, matching this
--     repo's convention of enforcing cross-module invariants in the owning
--     service rather than via a foreign key it cannot take. `kind` is the
--     notification's own type (`mention`/`follow`/`follow_request`/
--     `favourite`/`reblog`/`poll`/`status`/`update`, design.md's v1 kind
--     set). `origin_id`/`origin_kind` represent the notification's origin
--     actor as a logical (kind, id) pair -- `'local'` | `'remote'` -- the
--     same `AccountRef`-shaped polymorphic-actor-reference convention
--     `migrations/0012_social_graph.sql`'s `follower_kind`/`follower_id`
--     and `migrations/0004_federation.sql`'s remote-actor references
--     already establish, since a notification's origin actor may be local
--     or remote. `status_id` is a nullable logical reference to
--     statuses-core's `statuses(id)` (no `REFERENCES`, same cross-boundary
--     convention -- notifications does not own `statuses`), present only
--     for the post-related kinds (`mention`/`favourite`/`reblog`/`poll`/
--     `status`/`update`) per design.md's null discipline and absent
--     (`NULL`) for `follow`/`follow_request`. `dismissed` is the消去 flag
--     (`DEFAULT FALSE`), driving both the dedup partial index below and the
--     list/get exclusion later tasks implement (Requirement 4.4). `id` is
--     a plain `BIGINT PRIMARY KEY` with no `SERIAL`/`IDENTITY` default,
--     following 0001-0007/0011/0012's established convention: identifiers
--     are always minted by the application's own core-runtime
--     `IdGenerator` boundary, never by the database -- and doubles as the
--     notification list cursor (design.md "Temporal": 一覧カーソルは `id`
--     降順).
--
--   - `notifications_dedup_idx` (task 1.1's own explicit instruction): a
--     **partial** unique index on `(recipient_id, kind, origin_kind,
--     origin_id, COALESCE(status_id, 0))` scoped `WHERE NOT dismissed`.
--     `COALESCE(status_id, 0)` folds the nullable `status_id` into the key
--     so that `follow`/`follow_request` notifications (no `status_id`) are
--     deduplicated on the same shape as post-related kinds, since Postgres
--     unique indexes otherwise treat every `NULL` as distinct and would
--     never collide on a bare `status_id` column. Scoping the index `WHERE
--     NOT dismissed` (Requirement 8.1, 8.2's "既存の未消去通知とのみ重複判定")
--     means the uniqueness constraint only ever considers *not-yet-
--     dismissed* rows: once a notification with a given key is marked
--     `dismissed = TRUE`, that key becomes free again, so a fresh event
--     for the identical (recipient, kind, origin, status) tuple inserts a
--     brand-new row instead of being rejected. This is the schema-level
--     half of the unfollow -> re-follow / undismiss -> redismiss
--     re-notification semantics design.md's "Consistency" section
--     describes and later tasks' `NotificationRepository.insert_dedup`
--     (an `ON CONFLICT (...) WHERE NOT dismissed DO NOTHING` upsert) relies
--     on for idempotent generation (Requirement 8.1: 重複した通知を新規生成
--     しない, 8.2: 上流イベント再送に対して冪等).
--
--   - `notifications_recipient_idx` on `(recipient_id, id DESC)` (task
--     1.1's own explicit instruction, "受信者カーソルインデックス"): backs
--     the recipient-scoped, `id`-descending cursor pagination later tasks'
--     `GET /api/v1/notifications` implements, mirroring this repo's
--     existing cursor-pagination index convention for other list-fetchable,
--     per-owner tables (e.g. `migrations/0007_statuses.sql`'s
--     `statuses_actor_idx`, `migrations/0012_social_graph.sql`'s
--     `follows_followee_idx`): a composite index whose leading column is
--     the list's scoping key and whose trailing column orders the cursor.
--
-- Out of scope for this migration: any Rust code referencing `notifications`
-- (notifications' model/repository/service/endpoint modules, added by later
-- tasks in this feature, starting with task 1.2's `NotificationRepository`)
-- and any further schema evolution not required by task 1.1's acceptance
-- bullets above.

CREATE TABLE notifications (
    id            BIGINT PRIMARY KEY,             -- core-runtime IdGenerator 採番（一覧カーソル兼用）
    recipient_id  BIGINT NOT NULL,                -- 受信者ローカルアクター（actor-model 論理参照）
    kind          TEXT   NOT NULL,                -- mention/follow/follow_request/favourite/reblog/poll/status/update
    origin_id     BIGINT NOT NULL,                -- 通知元アカウント（local/remote 論理参照, AccountRef 形状）
    origin_kind   TEXT   NOT NULL,                -- 'local' | 'remote'
    status_id     BIGINT,                         -- 対象投稿 statuses(id) 論理参照（投稿関連種別のみ、任意）
    dismissed     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    TIMESTAMPTZ NOT NULL
);

-- 受信者カーソルインデックス（通知一覧のページネーション: recipient_id 絞り込み + id 降順カーソル）
CREATE INDEX notifications_recipient_idx ON notifications(recipient_id, id DESC);

-- 重複排除（未消去限定の部分一意インデックス）: 同一 (recipient, kind, origin,
-- status) の組について、未消去（dismissed = FALSE）の通知同士でのみ一意性を強制
-- する。COALESCE(status_id, 0) は status_id を伴わない種別（follow/
-- follow_request）でも NULL 同士が別扱いにならないよう key に折り込む。
-- 消去済み（dismissed = TRUE）の通知はこの一意制約の対象外となるため、
-- dismiss 後や unfollow -> re-follow 等の取り消し -> 再実行で、同一キーの
-- 通知を新規生成できる（Mastodon の実挙動に合わせる、8.1/8.2）。
CREATE UNIQUE INDEX notifications_dedup_idx
    ON notifications(recipient_id, kind, origin_kind, origin_id, COALESCE(status_id, 0))
    WHERE NOT dismissed;
