-- 0005_media.sql
--
-- media-pipeline: adds the persistence for the media-attachment boundary's
-- own state: the media (attachment) records themselves and the DB-backed
-- processing job queue that drives asynchronous derivative generation
-- (thumbnail / BlurHash / dimension metadata).
--
-- Naming note: design.md's "File Structure Plan" and "Physical Data Model"
-- name this file `0004_media.sql`. That number is stale: this repo already
-- has `migrations/0001_init_runtime.sql` (core-runtime), `0002_actors.sql`
-- (actor-model), `0003_oauth.sql` (api-foundation), and
-- `0004_federation.sql` (federation-core, whose own naming-note comment
-- documents the same discrepancy — each spec's design.md was assigned a
-- migration number in parallel by `/kiro-spec-batch`, and those numbers
-- never reflected real implementation order; federation-core claimed the
-- real `0004` slot first, having been implemented before media-pipeline).
-- Per `0001_init_runtime.sql`'s "sequential numeric version prefix,
-- forward-only append" convention, and because sqlx's migrator
-- (`sqlx::migrate!("./migrations")`, see `src/migrate.rs`) applies
-- migrations in ascending version order and rejects out-of-order insertion,
-- this file uses the real next sequential slot, `0005`, instead. Only the
-- filename numeral differs from design.md; the table/column/index/
-- constraint substance below is otherwise taken as-is from design.md's
-- `0004_media.sql` SQL block.
--
-- Purpose (design.md "Logical Data Model" / "Physical Data Model";
-- Requirements 1.2, 4.1, 4.2):
--   - `media`: one row per uploaded media attachment (Requirement 1.1:
--     accepting an upload persists the original and mints a media
--     identifier before processing completes). `id` is a plain
--     `BIGINT PRIMARY KEY` with no `SERIAL`/`IDENTITY` default, following
--     0001/0002/0003/0004's convention: identifiers are always minted by
--     the application's own `IdGenerator` boundary, never by the database.
--     `actor_id` is a logical-only reference to actor-model's
--     `local_actors.id` (no `REFERENCES`, mirroring how 0003/0004 keep
--     module boundaries between specs without a hard cross-spec FK —
--     media-pipeline does not own `local_actors`) and is required
--     (Requirement 1.2: every upload is bound to a single owning actor).
--     `media_type` and `state` hold the domain enums (`image`/`gifv`/
--     `video`/`audio`/`unknown` and `processing`/`ready`/`failed`
--     respectively; state is the truth source for processing progress per
--     design.md's "Logical Data Model"). `description` and `focus_x`/
--     `focus_y` hold the optional alt text and the focal point (default
--     center, `0`/`0`, range `-1.0..=1.0` enforced by application code per
--     Requirement 7.1/7.2/7.4). `orig_width`/`orig_height`/`small_width`/
--     `small_height` and `blurhash` hold the derived metadata a completed
--     processing job fills in (Requirement 6.3). `object_key` (required:
--     the original's storage key is known at insert time) and `thumb_key`
--     (nullable until a derivative exists) are the decisive storage keys
--     `MediaStore` resolves (Requirement 5.3). `content_type` is required
--     (known from the upload itself). The index `media_actor_idx` on
--     `actor_id` supports the owner-scoped lookups `MediaRepository.
--     find_owned` performs (Requirement 2.3/2.4's non-owner invisibility
--     check; ties to this task's "index actor_id" instruction).
--   - `media_processing_jobs`: one row per media-processing job driving the
--     DB job queue (Requirement 4.1: an external broker/message queue is
--     never required). `id` is likewise an application-minted
--     `BIGINT PRIMARY KEY`. `media_id` is a required `REFERENCES media(id)`
--     (the job's target media, unlike `media.actor_id` this is a same-spec
--     reference so a real FK is used, matching design.md's "Logical Data
--     Model": `media (1) ──< media_processing_jobs (N)`). `state` holds
--     `queued`/`processing`/`failed` (a completed job is deleted/finalized
--     per `ProcessingJobQueue.complete`, so there is no `done` value to
--     store). `attempts` and `run_at` implement the backoff retry schedule
--     (Requirement 4.4), `locked_at` marks a job as claimed by a worker so
--     `FOR UPDATE SKIP LOCKED` exclusive claims (Requirement 4.2) can also
--     detect an expired lease to reclaim, and `last_error` holds
--     sufficient diagnostic detail for the failure path (Requirement 4.5).
--     The composite index `media_jobs_due_idx` on `(state, run_at)`
--     supports the worker's "find due work" query for *both* newly-queued
--     jobs (`state='queued' AND run_at <= now`) and lease-expired jobs to
--     reclaim (`state='processing' AND locked_at < now - lease_duration`)
--     without a full table scan (Requirement 4.2). The partial predicate
--     `state IN ('queued', 'processing')` narrows the index to only the
--     rows either access pattern could ever match (a `failed` job is never
--     claimed again); `locked_at`'s comparison against `now() -
--     lease_duration` cannot itself be folded into the partial-index
--     predicate (`now()` is not immutable), so that half of the reclaim
--     condition is evaluated as a residual filter over the `processing`
--     subset the partial index already narrows to — an accepted,
--     documented trade-off at single-server scale (design.md's Physical
--     Data Model).
--
-- Out of scope for this migration: any Rust code referencing these tables
-- (media-pipeline's service/repository/queue/worker/store/processor
-- modules, added by later tasks in this feature) and any further schema
-- evolution not required by the acceptance criteria above.

CREATE TABLE media (
    id            BIGINT PRIMARY KEY,            -- core-runtime IdGenerator 採番
    actor_id      BIGINT NOT NULL,               -- 所有アクター（actor-model 論理参照、1.2）
    media_type    TEXT   NOT NULL,               -- image/gifv/video/audio/unknown
    state         TEXT   NOT NULL,               -- processing/ready/failed
    description   TEXT,
    focus_x       REAL   NOT NULL DEFAULT 0,      -- -1.0..1.0（既定中央、範囲検証はアプリ層）
    focus_y       REAL   NOT NULL DEFAULT 0,
    orig_width    INTEGER,
    orig_height   INTEGER,
    small_width   INTEGER,
    small_height  INTEGER,
    blurhash      TEXT,
    object_key    TEXT   NOT NULL,               -- 原本の決定的ストレージキー
    thumb_key     TEXT,                          -- サムネイルの決定的ストレージキー
    content_type  TEXT   NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL
);
-- 所有アクター単位の取得（find_owned 等）を効率化するインデックス
CREATE INDEX media_actor_idx ON media(actor_id);

CREATE TABLE media_processing_jobs (
    id            BIGINT PRIMARY KEY,
    media_id      BIGINT NOT NULL REFERENCES media(id),  -- 対象メディア（同一 spec 内参照のため実 FK）
    state         TEXT   NOT NULL,               -- queued/processing/failed
    attempts      INTEGER NOT NULL DEFAULT 0,
    run_at        TIMESTAMPTZ NOT NULL,          -- 取得対象は run_at <= now
    locked_at     TIMESTAMPTZ,
    last_error    TEXT,
    created_at    TIMESTAMPTZ NOT NULL
);
-- ワーカーの取得効率化：新規投入分（queued かつ run_at 到来）とリース期限切れ
-- の再取得対象（processing かつ locked_at がリース期間超過）の双方をカバーす
-- る複合インデックス（4.2）。locked_at 側の now() 依存条件は部分インデックス
-- 述語にできないため、state IN ('queued','processing') に絞り込んだ上で残余
-- フィルタとして評価する。
CREATE INDEX media_jobs_due_idx ON media_processing_jobs(state, run_at)
    WHERE state IN ('queued', 'processing');
