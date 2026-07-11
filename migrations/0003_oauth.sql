-- 0003_oauth.sql
--
-- api-foundation: adds the persistence for OAuth 2.0 applications,
-- authorization codes, and access tokens.
--
-- Purpose (design.md "Physical Data Model" / "Logical Data Model";
-- Requirements 1.1, 1.5, 2.3, 2.5, 3.1, 3.4, 3.5):
--   - `oauth_applications`: one row per registered OAuth client. `id` is a
--     plain `BIGINT PRIMARY KEY` with no `SERIAL`/`IDENTITY` default,
--     following 0001/0002's convention: identifiers are always minted by
--     the application's own `IdGenerator` boundary, never by the database.
--     `client_id` carries a table-level `UNIQUE` constraint so client
--     identifiers are unique instance-wide (Requirement 1.1).
--     `client_secret_hash` stores the client secret hashed under the same
--     convention as `oauth_access_tokens.token_hash` /
--     `oauth_authorization_codes.code_hash`: the plaintext secret is
--     returned to the caller once, at registration response time, and is
--     never persisted or logged (Requirement 1.5); credential verification
--     hashes the presented secret and compares hashes in constant time.
--   - `oauth_authorization_codes`: one row per issued authorization code.
--     `code_hash` is the primary key so the code value itself is never
--     stored — only its hash, under the same hashing convention as the
--     other two hash columns (Requirement 3.5). `app_id` is a mandatory
--     foreign key into `oauth_applications`. `actor_id` is `NOT NULL`
--     (Requirement 2.3, 3.5): it is a *logical* reference to actor-model's
--     `local_actors.id` (design.md: "FK はモジュール境界方針に従い任意"),
--     so no `REFERENCES` is declared here, mirroring how 0001/0002 keep
--     module boundaries between specs without a hard cross-spec FK.
--     `expires_at` (short-lived) plus `consumed` (defaulting to `FALSE`)
--     together implement the code's single-use, short-lived contract
--     (Requirement 2.5): a code may only be exchanged while `consumed =
--     FALSE AND expires_at > now()`, and the exchange flips `consumed` to
--     `TRUE` atomically as part of that same conditional update
--     (application-level concern of a later task; this migration only
--     provides the column shape that makes it possible). `pkce_challenge`/
--     `pkce_method` are nullable since PKCE is optional per request.
--   - `oauth_access_tokens`: one row per issued access token. `token_hash`
--     stores the token hashed under the same convention as the other two
--     hash columns and carries a table-level `UNIQUE` constraint so no two
--     tokens hash to the same value (Requirement 3.1, 3.5). `app_id` is a
--     mandatory foreign key into `oauth_applications`. `actor_id` is
--     `NOT NULL` for the same reason as on `oauth_authorization_codes`
--     (single-actor binding, Requirement 3.1, 3.5) and is likewise a
--     logical-only reference to actor-model's `local_actors.id`. `revoked`
--     (defaulting to `FALSE`) implements token revocation (Requirement
--     3.4): a revoked token must be treated as invalid on every subsequent
--     authentication check. An index on `actor_id` supports resolving/
--     revoking a given actor's tokens without a full table scan.
--
-- Out of scope for this migration: any Rust code referencing these tables
-- (api-foundation's OAuth repository/service/endpoint layers, added by
-- later tasks in this feature) and any further schema evolution not
-- required by the acceptance criteria above.

CREATE TABLE oauth_applications (
    id                  BIGINT PRIMARY KEY,            -- core-runtime IdGenerator 採番
    client_id           TEXT   NOT NULL UNIQUE,
    client_secret_hash  BYTEA  NOT NULL,                -- 平文は登録応答時のみ返却し永続化・ログ出力しない
    name                TEXT   NOT NULL,
    redirect_uris       TEXT   NOT NULL,                -- 登録 URI（完全一致検証用）
    scopes              TEXT   NOT NULL,                -- 要求スコープ
    created_at          TIMESTAMPTZ NOT NULL
);

CREATE TABLE oauth_authorization_codes (
    code_hash       BYTEA  PRIMARY KEY,                 -- コードはハッシュ保存
    app_id          BIGINT NOT NULL REFERENCES oauth_applications(id),
    actor_id        BIGINT NOT NULL,                    -- 選択アクター（actor-model 論理参照、FK は境界方針により任意）
    scopes          TEXT   NOT NULL,
    redirect_uri    TEXT   NOT NULL,
    pkce_challenge  TEXT,                                -- 任意
    pkce_method     TEXT,
    expires_at      TIMESTAMPTZ NOT NULL,
    consumed        BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE INDEX oauth_authorization_codes_app_idx ON oauth_authorization_codes(app_id);

CREATE TABLE oauth_access_tokens (
    id              BIGINT PRIMARY KEY,
    token_hash      BYTEA  NOT NULL UNIQUE,              -- トークンはハッシュ保存（平文非保存）
    app_id          BIGINT NOT NULL REFERENCES oauth_applications(id),
    actor_id        BIGINT NOT NULL,                    -- 単一アクター結びつけ（actor-model 論理参照、FK は境界方針により任意）
    scopes          TEXT   NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL,
    revoked         BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE INDEX oauth_access_tokens_actor_idx ON oauth_access_tokens(actor_id);
CREATE INDEX oauth_access_tokens_app_idx ON oauth_access_tokens(app_id);
