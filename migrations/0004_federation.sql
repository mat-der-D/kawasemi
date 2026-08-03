-- 0004_federation.sql
--
-- federation-core: adds the persistence for the ActivityPub federation
-- boundary's own state: the outbound delivery queue, the inbound
-- idempotency ledger, the remote public-key cache, and the double-knocking
-- signature-format memory.
--
-- Naming note: design.md's "Physical Data Model" names this file
-- `0008_federation.sql`, following the per-spec migration numbers each
-- spec's own design.md was assigned when generated in parallel by
-- `/kiro-spec-batch`. Those numbers do not reflect actual implementation
-- order: only `0001_init_runtime.sql` (core-runtime), `0002_actors.sql`
-- (actor-model), and `0003_oauth.sql` (api-foundation) exist so far, and
-- federation-core is the next spec implemented per
-- `.kiro/steering/roadmap.md`'s dependency order (in fact,
-- accounts-and-instance/statuses-core/social-graph each *depend on*
-- federation-core yet were assigned lower numbers than it in their own
-- design docs, confirming those numbers were never a real sequencing
-- decision). Per 0001's "sequential numeric version prefix, forward-only
-- append" convention, and because sqlx's migrator
-- (`sqlx::migrate!("./migrations")`, see `src/migrate.rs`) applies
-- migrations in ascending version order and rejects out-of-order
-- insertion, this file uses the real next sequential slot, `0004`, instead.
-- Only the filename numeral differs from design.md; the table/column/
-- index/constraint substance below is otherwise taken as-is from
-- design.md's `0008_federation.sql` SQL block.
--
-- Purpose (design.md "Logical Data Model" / "Physical Data Model";
-- Requirements 7.4, 11.1, 11.4):
--   - `delivery_jobs`: one row per outbound remote delivery attempt
--     (Requirement 11.1: enqueuing a delivery persists it instead of
--     blocking the caller until delivery completes). `id` is a plain
--     `BIGINT PRIMARY KEY` with no `SERIAL`/`IDENTITY` default, following
--     0001/0002/0003's convention: identifiers are always minted by the
--     application's own `IdGenerator` boundary, never by the database.
--     `sender_actor_id` is a logical-only reference to actor-model's
--     `local_actors.id` (no `REFERENCES`, mirroring how 0003 keeps module
--     boundaries between specs without a hard cross-spec FK — federation-
--     core does not own `local_actors`). `status` holds one of
--     'pending' | 'in_progress' | 'done' | 'failed', driving the delivery
--     worker's polling and retry state machine (Requirement 11.2, 11.3,
--     11.5). `attempts` and `next_attempt_at` implement the backoff retry
--     schedule (Requirement 11.3). The index `delivery_jobs_due_idx` on
--     `(status, next_attempt_at)` supports the delivery worker's "find due
--     pending jobs" query without a full table scan (Requirement 11.2).
--     The unique index `delivery_jobs_dedup_idx` on
--     `(target_inbox, (activity->>'id'))` enforces at the data layer that
--     the same Activity is never enqueued twice to the same shared inbox
--     (Requirement 11.4's shared-inbox delivery deduplication).
--   - `received_activities`: the inbound idempotency ledger (Requirement
--     7.4: an Activity id that has already been received must not be
--     dispatched to business-logic handling a second time).
--     `activity_id` (the Activity's own `id`, its idempotency key) is the
--     primary key, so a second insert for the same id is rejected as a
--     unique violation and the caller can treat that as "already
--     processed" rather than re-dispatching.
--   - `remote_public_keys`: the keyId -> public-key-material cache
--     Requirement 2.3/2.4 (referenced by this task's Requirement 7.4 inbox
--     verification path) require so signature verification does not
--     re-fetch a remote actor's key on every request. `key_id` is the
--     primary key (one cached row per keyId). `fetched_at` supports
--     cache-staleness checks by the (later-task) key resolver.
--   - `instance_signature_capabilities`: the double-knocking negotiation
--     memory (Requirement 3.2, 3.3: once a signature format has succeeded
--     for a destination host, prefer it on subsequent sends). `host` is
--     the primary key (one remembered format per remote host). `format`
--     holds one of 'draft_cavage' | 'rfc9421'.
--
-- Out of scope for this migration: any Rust code referencing these tables
-- (federation-core's delivery worker/inbox/signature-verification modules,
-- added by later tasks in this feature) and any further schema evolution
-- not required by the acceptance criteria above.

CREATE TABLE delivery_jobs (
    id              BIGINT PRIMARY KEY,            -- core-runtime IdGenerator 採番
    sender_actor_id BIGINT NOT NULL,                -- local_actors(id) 論理参照（FK は境界方針により任意）
    target_inbox    TEXT   NOT NULL,
    activity        JSONB  NOT NULL,                -- 正規 Activity（意味論共通の生成物）
    status          TEXT   NOT NULL,                -- 'pending' | 'in_progress' | 'done' | 'failed'
    attempts        INT    NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL
);
-- 配送ワーカーの期限索引（status, next_attempt_at）
CREATE INDEX delivery_jobs_due_idx ON delivery_jobs(status, next_attempt_at);
-- 同一 Activity を同一 inbox へ二重投入しない（shared inbox 重複排除の補助、11.4）
CREATE UNIQUE INDEX delivery_jobs_dedup_idx ON delivery_jobs(target_inbox, (activity->>'id'));

CREATE TABLE received_activities (
    activity_id  TEXT PRIMARY KEY,                  -- Activity の id（冪等キー、7.4）
    received_at  TIMESTAMPTZ NOT NULL
);

CREATE TABLE remote_public_keys (
    key_id          TEXT PRIMARY KEY,
    actor_uri       TEXT NOT NULL,
    public_key_pem  TEXT NOT NULL,
    fetched_at      TIMESTAMPTZ NOT NULL
);

CREATE TABLE instance_signature_capabilities (
    host          TEXT PRIMARY KEY,
    format        TEXT NOT NULL,                    -- 'draft_cavage' | 'rfc9421'
    updated_at    TIMESTAMPTZ NOT NULL
);
