-- 0012_social_graph.sql
--
-- social-graph task 1.1 (Requirement 8.1: 関係状態の永続化・単一の真実源).
-- Adds this spec's own owned persistent state: `follows` / `follow_requests`
-- / `mutes` / `blocks`, the tables `RelationshipRepository` (task 1.2) reads
-- and writes.
--
-- Naming note: the task text (`.kiro/specs/social-graph/tasks.md` task 1.1)
-- and design.md's "File Structure Plan"/"Physical Data Model" name this file
-- `0006_social_graph.sql`. That number is stale, for the identical reason
-- `migrations/0006_accounts.sql`'s own naming-note comment already
-- documents for this exact `0005`→`0006` class of clash (each spec's
-- design.md was assigned a migration number in parallel by
-- `/kiro-spec-batch`, and those numbers never reflected real implementation
-- order): this repo's real `0006` slot is already taken by
-- accounts-and-instance (`migrations/0006_accounts.sql`), and `0007` by
-- statuses-core (`migrations/0007_statuses.sql`). `migrations/0011_status_
-- mentions_and_remote_attachments.sql`'s own naming-note comment further
-- documents that `0008`/`0009`/`0010` are already reserved by federation-
-- core/notifications/search respectively (confirmed by that file's
-- cross-spec grep at its authoring time), and that `0011` was the next free
-- slot back then. No spec's tasks.md/research.md claims `0012` or higher as
-- of this migration, so `0012` is the next genuinely free slot, following
-- `0001_init_runtime.sql`'s "sequential numeric version prefix, forward-only
-- append" convention. Only the filename numeral differs from design.md; the
-- table/column/index/constraint substance below is otherwise taken as-is
-- from design.md's "Physical Data Model" `0006_social_graph.sql` SQL block.
--
-- Purpose (design.md "Logical Data Model" / "Physical Data Model";
-- Requirement 8.1):
--   - `follows`: established follows (follower -> followee; `show_reblogs`/
--     `notify`/`languages` back the `Follow` model's same-named fields;
--     `activity_id` is the outbound Follow Activity id, needed later to
--     address Undo(Follow)). Every account reference is a logical (kind,
--     id) pair rather than a hard FK, mirroring `AccountRef`'s own
--     `Local(Id)`/`Remote(Id)` shape (`src/domain/primitives.rs`) and this
--     repo's already-established cross-module-boundary convention of never
--     taking a hard `REFERENCES` across a spec boundary (see
--     `migrations/0006_accounts.sql`'s `account_profiles.actor_id`/
--     `remote_accounts` and `migrations/0011_...sql`'s `status_mentions.
--     actor_id` doc comments for the identical pattern) — `follower`/
--     `followee` here are actor-model/accounts-and-instance-owned local or
--     remote accounts, which this spec does not own. The table-level
--     `UNIQUE (follower_kind, follower_id, followee_kind, followee_id)`
--     constraint is this relation's one uniqueness constraint (task 1.1),
--     giving `establish_follow`'s upsert idempotency (Requirements 1.6,
--     7.7). `follows_followee_idx` on `(followee_kind, followee_id)` is the
--     followee reverse-lookup index the task requires, backing
--     "followers-of-X" queries (`RelationshipRepository::load_states`/
--     `blocked_by`-style batch reverse lookups, `count_followers`) without a
--     full scan.
--   - `follow_requests`: pending follow requests (requester -> target);
--     `direction` (`'outbound'` | `'inbound'`) distinguishes a locally
--     initiated pending request from one recorded on receipt, so both an
--     outbound and an inbound row can coexist for the same (requester,
--     target) pair without colliding (`FollowRequestDirection` model,
--     design.md). `activity_id` is the Follow Activity id Accept/Reject
--     Activities reference. `UNIQUE (requester_kind, requester_id,
--     target_kind, target_id, direction)` is this relation's one uniqueness
--     constraint, scoped by direction precisely because in/outbound rows
--     for the same pair are legitimately distinct (task 1.1's own
--     parenthetical). `follow_requests_target_idx` on `(target_kind,
--     target_id, direction)` is the task's required target+direction
--     reverse-lookup index, backing "pending requests for X" (`GET
--     /api/v1/follow_requests`'s `list_inbound_requests`, Requirement 2.2).
--   - `mutes`: mutes (muter -> muted); `notifications` backs
--     `muting_notifications`, `expires_at` is nullable (NULL = no
--     expiration) and backs the duration-option decay described in
--     Requirement 4.3/9.3. `UNIQUE (muter_kind, muter_id, muted_kind,
--     muted_id)` is this relation's one uniqueness constraint. No reverse-
--     lookup index is added beyond the implicit unique-constraint index:
--     the task's own reverse-lookup list (followee / target+direction /
--     blocked) does not name a mutes reverse index, and Requirement 9's
--     muted-set query (`muted_targets`) is always muter-scoped (a viewer
--     asking "who have I muted", never "who has muted X"), which the
--     unique constraint's leading `(muter_kind, muter_id, ...)` columns
--     already serve.
--   - `blocks`: blocks (blocker -> blocked); `activity_id` is the outbound
--     Block Activity id Undo(Block) references. Per design.md's Logical
--     Data Model ("被ブロックは逆方向行で表現" / Requirement 6: a signer's
--     blocked-by status is derived from a `blocks` row in the opposite
--     direction, not a separate table), being blocked is represented by an
--     existing `blocks` row with the *other* account as `blocker` — no
--     separate "blocked_by" table is needed. `UNIQUE (blocker_kind,
--     blocker_id, blocked_kind, blocked_id)` is this relation's one
--     uniqueness constraint. `blocks_blocked_idx` on `(blocked_kind,
--     blocked_id)` is the task's required blocked reverse-lookup index,
--     backing "is X blocked by anyone" / "who has blocked X" queries
--     (`BlockPolicyImpl`'s signer rejection check, Requirement 6.2;
--     `blocked_by`, Requirement 9.1) without a full scan.
--
-- Consistency (design.md "Data Models" -> "Physical Data Model" bullets):
-- `apply_block` (task 1.2+) runs the bidirectional `follows` deletion, both-
-- direction `follow_requests` deletion, and the `blocks` insert in a single
-- transaction (Requirement 5.2); each relation's own UNIQUE constraint is
-- what lets that repository layer use an idempotent upsert (Requirements
-- 1.6, 7.7) instead of a separate existence check.
--
-- Out of scope for this migration: any Rust code referencing these tables
-- (social-graph's model/repository/service/endpoint modules, added by
-- later tasks in this feature, starting with task 1.2's
-- `RelationshipRepository`) and any further schema evolution not required
-- by the acceptance criteria above.

CREATE TABLE follows (
    id            BIGINT PRIMARY KEY,             -- core-runtime IdGenerator 採番
    follower_id   BIGINT NOT NULL,                -- local/remote アカウント論理参照（AccountRef）
    follower_kind TEXT   NOT NULL,                -- 'local' | 'remote'
    followee_id   BIGINT NOT NULL,
    followee_kind TEXT   NOT NULL,
    show_reblogs  BOOLEAN NOT NULL DEFAULT TRUE,
    notify        BOOLEAN NOT NULL DEFAULT FALSE,
    languages     JSONB  NOT NULL DEFAULT '[]',
    activity_id   TEXT   NOT NULL,                -- 送信 Follow Activity id（Undo 用）
    created_at    TIMESTAMPTZ NOT NULL,
    UNIQUE (follower_kind, follower_id, followee_kind, followee_id)
);
-- followee 逆引き（フォロワー一覧・被フォロー集計）
CREATE INDEX follows_followee_idx ON follows(followee_kind, followee_id);

CREATE TABLE follow_requests (
    id             BIGINT PRIMARY KEY,
    requester_id   BIGINT NOT NULL,
    requester_kind TEXT   NOT NULL,
    target_id      BIGINT NOT NULL,
    target_kind    TEXT   NOT NULL,
    direction      TEXT   NOT NULL,                -- 'outbound' | 'inbound'
    activity_id    TEXT   NOT NULL,                -- Follow Activity id（Accept/Reject 参照）
    created_at     TIMESTAMPTZ NOT NULL,
    UNIQUE (requester_kind, requester_id, target_kind, target_id, direction)
);
-- target+direction 逆引き（宛先アクター向けの保留中フォローリクエスト一覧）
CREATE INDEX follow_requests_target_idx ON follow_requests(target_kind, target_id, direction);

CREATE TABLE mutes (
    id            BIGINT PRIMARY KEY,
    muter_id      BIGINT NOT NULL,
    muter_kind    TEXT   NOT NULL,
    muted_id      BIGINT NOT NULL,
    muted_kind    TEXT   NOT NULL,
    notifications BOOLEAN NOT NULL DEFAULT TRUE,
    expires_at    TIMESTAMPTZ,                    -- NULL = 無期限
    created_at    TIMESTAMPTZ NOT NULL,
    UNIQUE (muter_kind, muter_id, muted_kind, muted_id)
);

CREATE TABLE blocks (
    id            BIGINT PRIMARY KEY,
    blocker_id    BIGINT NOT NULL,
    blocker_kind  TEXT   NOT NULL,
    blocked_id    BIGINT NOT NULL,
    blocked_kind  TEXT   NOT NULL,
    activity_id   TEXT   NOT NULL,                -- 送信 Block Activity id（Undo 用）
    created_at    TIMESTAMPTZ NOT NULL,
    UNIQUE (blocker_kind, blocker_id, blocked_kind, blocked_id)
);
-- blocked 逆引き（被ブロック判定・署名拒否・blocked_by 集計）
CREATE INDEX blocks_blocked_idx ON blocks(blocked_kind, blocked_id);
