# Research & Design Decisions

## Summary

- **Feature**: `search`
- **Discovery Scope**: Complex Integration（上流 accounts-and-instance / statuses-core / federation-core に乗る新規 API 機能。検索エンジン非依存・将来差し替えの抽象境界が中核）
- **Key Findings**:
  - 検索の本質的リスクは「日本語全文検索の作り込み」と「拡張不要での配布」の衝突。これを `SearchBackend` ポート（照合の抽象境界）で吸収し、初期は標準 PostgreSQL の最小実装、将来は `pg_bigm`/外部エンジンを呼び出し側・API 契約を変えずに差し替える構造で解く。
  - Account / Status エンティティ契約は上流（accounts-and-instance / statuses-core）が所有。本 spec は**識別子で照合 → 上流シリアライザで具体化**する二層構造にし、契約を再定義しない。Tag（検索結果用）とハッシュタグ読み取りインデックスのみ本 spec が所有する。
  - `acct:` / URL のリモート解決は本 spec が「解決オーケストレーション」を持ち、実体取得は federation-core（WebFinger・`FederationHttpClient`）と accounts-and-instance（リモートアカウント正規化）へ委譲する。`resolve=true` かつ認証済みに限定し、未認証・既知のみ検索と分離する。

## Research Log

### Mastodon `GET /api/v2/search` の契約と最小実装の現実

- **Context**: brief が「API 契約は Mastodon 検索 API 形に留める」「特定エンジン前提を表に出さない」を必須とする。Mastodon は全文検索を Elasticsearch（任意）で提供し、未導入時は機能が縮退する。
- **Sources Consulted**: Mastodon API（`/api/v2/search` の `q` / `type` / `resolve` / `following` / `account_id` / `exclude_unreviewed` / `limit` / `offset` / `min_id` / `max_id`、SearchResults `{accounts, statuses, hashtags}`、`read:search` スコープ）、Mastodon の Elasticsearch 未導入時の縮退挙動、`docs/mastodon-api-compat.md`（一次情報）、roadmap.md（外部検索エンジン非使用の制約）。
- **Findings**:
  - SearchResults は `accounts`（Account[]）/ `statuses`（Status[]）/ `hashtags`（Tag[]）の 3 配列。空種別は `[]`。
  - Mastodon 本体も全文検索エンジン未導入時は投稿検索の一致範囲が限定的（自分が関与した投稿・URL 解決等が中心）になる。「縮退した最小実装」は Mastodon 互換として許容される現実的範囲。
  - `resolve=true` は `acct:`/URL のリモート取り込みを伴い、認証済み・`read:search` 前提。
  - Tag エンティティは `name` / `url` / `history`（日次の `day`/`uses`/`accounts`）を含むが、最小実装では `history` は空配列または最小集計で許容される。
- **Implications**: 本 spec はエンドポイント契約・パラメータ・スコープ・SearchResults 形を固定し、照合品質は `SearchBackend` の実装事項として隔離する。投稿検索は「閲覧者可視に閉じた標準 PostgreSQL 照合」という縮退最小実装で開始する。

### 標準 PostgreSQL での日本語全文検索の制約

- **Context**: tech.md / brief.md が「標準 `to_tsvector` は日本語を分かち書きできない既知制約」を明記。`pg_bigm` 等は任意オプションに留める。
- **Sources Consulted**: PostgreSQL 全文検索（`to_tsvector`/`tsquery`、`simple`/言語別 config）、`pg_bigm`（bigram、日本語向け、要 CREATE EXTENSION）、`pg_trgm`、`ILIKE`/`LIKE` の挙動、tech.md「全文検索は PostgreSQL 内で完結」。
- **Findings**:
  - 標準構成（拡張不要）で日本語に最も素直に効くのは `ILIKE '%q%'` 系の部分一致（語の分かち書き不要）。語幹化や関連度ランクは犠牲になるが「拡張不要」を満たす。
  - `to_tsvector('simple', ...)` は空白区切りトークン化のため、日本語の連続文字列を語に割れず実用度が低い。アカウント名/ハンドル/ハッシュタグ名のような短い識別子の前方/部分一致には十分。
  - `pg_bigm` は bigram GIN インデックスで日本語の部分一致を高速化するが拡張導入が前提。これは**後付けインデックスのマイグレーション**として、既定バックエンドの SQL 契約を変えずに追加できる（インデックスはクエリ結果を変えず性能のみ改善、または代替バックエンドが利用）。
- **Implications**: 既定 `PgSearchBackend` は標準 SQL（識別子系は部分一致、本文は可視範囲の部分一致）で実装。日本語拡張は「独立した後付けマイグレーション + 代替/拡張バックエンド」で導入し、`SearchBackend` の呼び出し契約と `GET /api/v2/search` の API 契約を不変に保つ。

### `acct:` / URL リモート解決の委譲構成

- **Context**: brief が「`acct:user@domain` の解決は WebFinger(federation-core) を使う」と指定。一方リモートアカウントの完全プロフィール正規化は accounts-and-instance の責務。
- **Sources Consulted**: federation-core design（`FederationHttpClient.fetch`、WebFinger ハンドラ＝自ドメイン向け `acct:` 解決、`ActorUrls`、JSON-LD 安全展開）、accounts-and-instance design（`RemoteAccountFetcher.fetch_and_normalize(actor_uri)`、`RemoteAccountRepository`）、WebFinger RFC 7033。
- **Findings**:
  - federation-core の WebFinger ハンドラは「自インスタンスのローカルアクターを `acct:` で外部へ提供する」インバウンド用。リモートの `acct:user@domain` を解決するには、`https://domain/.well-known/webfinger?resource=acct:user@domain` を `FederationHttpClient.fetch` でアウトバウンド取得し、JRD の `self` リンク（`type=application/activity+json`）からアクター URI を得る必要がある。
  - アクター URI を得た後は accounts-and-instance の `RemoteAccountFetcher.fetch_and_normalize(actor_uri)` でリモートアカウントを正規化・キャッシュし、Account 契約で表現できる。
  - リモート投稿 URL は、connegした連合取得（`FederationHttpClient.fetch` + JSON-LD 安全展開）で Note を取得し、statuses-core の受信取り込み（Create(Note) ハンドラ）または取得経路を通じて Status 化する。MVP では「URL → 既知化 → Status 表現」を statuses-core の取り込み経路に委ねる。
- **Implications**: 本 spec は `RemoteResolver`（`acct:`/URL の判別と WebFinger アウトバウンド照会のオーケストレーション）を所有し、実体取得・正規化・Status 取り込みは上流へ委譲する。`FederationHttpClient` がモック可能境界のため決定性テストが可能。`acct:` のアウトバウンド WebFinger 照会は federation-core のインバウンド WebFinger とは別物である点を実装時に明確化する（下記 Decision 参照）。

### ハッシュタグ読み取りインデックスの所有境界

- **Context**: ハッシュタグ検索には検索可能なタグ表現が要る。statuses-core は投稿作成時に本文からハッシュタグを抽出し Status の `tags` に反映するが、design の物理モデルに第一級の tags テーブルは明示されていない。
- **Sources Consulted**: statuses-core requirements 3.6（mentions/tags/emojis 抽出）、statuses-core design（Status.tags シリアライズ、physical model）、Mastodon の `tags`/`statuses_tags`、roadmap.md（timelines が tag タイムラインを所有）。
- **Findings**:
  - statuses-core は tags を抽出・保持するが、検索に最適化された問い合わせ可能インデックスを公開境界として提供していない。
  - 依存方向は search → statuses-core（search が下流）。上流に「投稿作成イベント」を search へ通知させる（上流→下流呼び出し）のは依存方向に反する。
  - 下流が上流の保持データを read-only で参照（共有 `PgPool`）し、自らの読み取りモデルを構成するのは依存方向に整合する。
- **Implications**: 本 spec はハッシュタグ読み取りインデックス（`search_tags` / `search_status_tags`）を所有し、statuses-core の投稿保持データから read-only で導出・保持する。第一級の tags テーブルを statuses-core が将来公開した場合は、本 spec の読み取りインデックスをそれに置換できる（Revalidation Trigger 化）。詳細は Decision 参照。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| Ports & Adapters（採用） | 検索照合を `SearchBackend` ポートに外出し、結果組み立て（上流シリアライザ消費）はポートの外 | エンジン差し替え・決定性テスト・契約再定義回避を構造で担保。brief/steering の「抽象境界」要件に直結 | ポート + アダプタの配線が増える | steering「検索の抽象境界」「注入可能な非決定性境界」に合致 |
| 直接 SQL 直書き（エンドポイントで PostgreSQL 照合を直書き） | ハンドラに検索 SQL を集約 | 初期実装が速い | エンジン差し替え不能・呼び出し側がエンジンに密結合・将来の日本語対応で全面改修 | brief 制約「呼び出し側を特定エンジンに依存させない」に違反。却下 |
| 外部検索エンジン（Elasticsearch 等）導入 | 専用エンジンで全文検索 | 検索品質が高い | roadmap「外部検索エンジン非使用・DB 完結・配布の簡単さ」に違反 | 却下（将来 `SearchBackend` 実装の一候補としてのみ） |
| バックエンドが JSON を返す | ポートが Account/Status JSON まで構築 | 結果組み立てが単純 | 上流エンティティ契約をバックエンドが知る必要があり、契約再定義・密結合を招く | 却下（ポートは識別子のみ返す） |

## Design Decisions

### Decision: 検索照合（`SearchBackend`）と結果具体化（上流シリアライザ消費）の二層分離

- **Context**: 「呼び出し側を特定エンジンに依存させない」「Account/Status 契約を再定義しない」を同時に満たす必要。
- **Alternatives Considered**:
  1. バックエンドが完成 JSON を返す — 上流契約への密結合・再定義リスク。
  2. エンドポイントで直接 SQL — エンジン差し替え不能。
- **Selected Approach**: `SearchBackend` は照合結果として**上流の識別子**（`AccountRef`（ローカル/リモート）・Status `Id`・ハッシュタグ名）のみを返す。`SearchService` がそれらを accounts-and-instance の Account シリアライズ・statuses-core の Status シリアライズ（可視性適用）・本 spec の Tag シリアライズで具体化して SearchResults を組み立てる。
- **Rationale**: 照合（エンジン依存）と表現（上流契約依存）を直交分離し、差し替え時の影響を照合層に閉じ込める。
- **Trade-offs**: 照合と具体化で 2 段のデータ往復（ID 照合 → ID から具体化）。一人鯖規模では許容。
- **Follow-up**: 具体化時の N+1 を避けるため、ID 群をバッチ取得できる上流呼び出し（複数 ID 一括解決）を実装時に優先する。

### Decision: 標準 PostgreSQL 最小実装と日本語拡張の後付けマイグレーション経路

- **Context**: 拡張不要での配布と将来の日本語対応の両立。
- **Selected Approach**: 既定 `PgSearchBackend` を標準 SQL（識別子系は部分一致、本文は可視範囲の部分一致、ハッシュタグ名は前方/部分一致）で実装。本 spec 所有テーブルは `migrations/0010_search.sql`。日本語拡張（`pg_bigm` 等）は将来の**独立した後付けマイグレーション**（`CREATE EXTENSION` + GIN インデックス）として導入し、既定バックエンドの SQL 契約・`GET /api/v2/search` 契約を変えない。拡張前提の照合は代替/拡張 `SearchBackend` 実装が担う。
- **Rationale**: brief「`pg_bigm` 等は任意オプションに留め必須にしない（スキーマ/インデックスを後付けできる形）」を直接実現。
- **Trade-offs**: 初期の検索品質（特に日本語本文）は限定的。これは brief が許容する縮退最小実装。
- **Follow-up**: 後付けマイグレーションが既定構成に存在しなくても初期配布が成立することを起動・マイグレーションテストで担保。

### Decision: `acct:`/URL リモート解決のオーケストレーションを本 spec が所有し実体取得は委譲

- **Context**: WebFinger（federation-core）とリモート正規化（accounts-and-instance）は上流所有。search はそれらを束ねる。
- **Selected Approach**: 本 spec の `RemoteResolver` がクエリ種別（プレーン語 / `acct:user@domain`・`@user@domain` / URL）を判定し、`acct:` はアウトバウンド WebFinger 照会（`FederationHttpClient.fetch` で JRD 取得 → アクター URI 抽出）→ accounts-and-instance `RemoteAccountFetcher.fetch_and_normalize(actor_uri)` で Account 化。URL は連合取得 + statuses-core/accounts-and-instance の取り込み経路で Account/Status 化。`resolve=true` かつ認証済みに限定。
- **Rationale**: federation-core の WebFinger（インバウンド）と区別しつつ、`acct:` 解決に WebFinger プロトコル（federation-core の `FederationHttpClient`）を使うという brief 指定に沿う。実体取得・正規化を再実装せず上流へ委譲。
- **Trade-offs**: アウトバウンド WebFinger 照会ロジックは本 spec が薄く持つ（federation-core が将来アウトバウンド WebFinger リゾルバを公開したら置換）。
- **Follow-up**: federation-core がアウトバウンド WebFinger 解決を公開した場合は本 spec の薄い照会を置換（Revalidation Trigger）。

### Decision: ハッシュタグ読み取りインデックスを本 spec 所有・上流データから導出

- **Context**: 検索可能なタグ表現が必要だが statuses-core は問い合わせ可能なタグ境界を公開していない。依存方向は search → statuses-core。
- **Selected Approach**: 本 spec が `search_tags`（正規化タグ名・`url` 構築元・使用集計）と `search_status_tags`（status_id ↔ タグ）を所有し、statuses-core の投稿保持データから read-only で導出・保持する（共有 `PgPool` を read-only 参照）。`history` は最小集計（または空）で開始。
- **Rationale**: 上流→下流の通知（依存方向違反）を避けつつ、search が自身の読み取りモデルを所有してスワップ可能境界に乗せる。
- **Trade-offs**: タグインデックスの鮮度維持は本 spec のインデックス更新（投稿取り込み経路の参照・バックフィル）に依存。第一級 tags を statuses-core が後で持てば重複が生じうる。
- **Follow-up**: statuses-core が第一級 tags 境界を公開したら本 spec の読み取りインデックスを置換（Revalidation Trigger に明記）。インデックス更新の駆動（取り込み時 or 定期バックフィル）は実装時に最小コストで確定。

## Migration Numbering Coordination

- **確定した非衝突の連番**: マイグレーション番号は次のとおり衝突なく確定している。0001 core-runtime（`0001_init_runtime.sql`）/ 0002 actor-model（`0002_actors.sql`）/ 0003 api-foundation（`0003_oauth.sql`、OAuth）/ 0004 media-pipeline（`0004_media.sql`）/ 0005 accounts-and-instance（`0005_accounts.sql`）/ 0006 social-graph（`0006_social_graph.sql`）/ 0007 statuses-core（`0007_statuses.sql`）/ 0008 federation-core（`0008_federation.sql`）/ 0009 notifications（`0009_notifications.sql`）/ 0010 search（`0010_search.sql`）。
- **0003 衝突の解消**: 以前は federation-core が `0003_federation.sql` を用い api-foundation の `0003_oauth.sql` と 0003 を二重利用していたが、federation-core が 0003 の衝突を解消して **`0008_federation.sql`** へ移動し 0008 を所有することで解消済み。0003 は api-foundation（OAuth）が単独所有する。
- **0008 の調整**: timelines が以前予約していた 0008 を解放し、0008 は federation-core が所有する。timelines は別番号へ後段で割当される（本 spec の番号には影響しない）。
- **本 spec の選択**: search は依存波の最下流（accounts-and-instance / statuses-core に依存）であり、上記連番の末尾 **`migrations/0010_search.sql`** を所有する。本 spec は 0001–0009 のいずれとも衝突しない。
- **後付け拡張**: 日本語拡張インデックス（`pg_bigm` 等）は将来の独立番号（例 `00NN_search_bigm.sql`）で追加し、`0010_search.sql` の既定スキーマと既定バックエンド契約を破壊しない。

## Risks & Mitigations

- 検索バックエンドの差し替えが API/呼び出し側へ波及する — ポートは識別子のみ返し、具体化を結果組み立て層に閉じ込めることで、差し替え影響を照合層に限定。契約テスト（SearchResults/Tag ゴールデン）で API 不変を担保。
- 日本語本文検索の品質が低い（標準 PG の制約）— brief が許容する縮退最小実装と明記。後付けマイグレーション + 拡張バックエンドで品質を引き上げる経路を設計上確保。
- `acct:`/URL リモート解決の取得失敗で検索全体が失敗する — 失敗対象は結果から除外し、エンドポイントは正常応答（Req 6.4）。`FederationHttpClient` モックで失敗系を決定的にテスト。
- ハッシュタグインデックスの鮮度・重複 — read-only 導出 + バックフィルで最小維持。statuses-core の第一級 tags 公開を Revalidation Trigger 化。
- 投稿検索が不可視投稿を漏らす（最重要・プライバシー）— 可視性判定を statuses-core の `VisibilityPolicy` に委譲し、照合結果の具体化時に可視フィルタを必ず適用。可視性回帰を統合テストで固定。
- リモート解決の濫用（未認証での外部取得）— `resolve=true` を認証済みに限定（Req 6.5）。

## References

- [Mastodon API: search (`GET /api/v2/search`)](https://docs.joinmastodon.org/methods/search/) — パラメータ・SearchResults・`read:search` スコープの一次情報。
- [Mastodon Entity: Search / Tag](https://docs.joinmastodon.org/entities/Search/) — SearchResults と Tag（`name`/`url`/`history`）の形。
- [PostgreSQL Full Text Search](https://www.postgresql.org/docs/current/textsearch.html) — `to_tsvector`/`tsquery` と言語 config の制約。
- [pg_bigm](https://pgbigm.osdn.jp/) — 日本語向け bigram 全文検索拡張（任意・後付け）。
- [WebFinger RFC 7033](https://www.rfc-editor.org/rfc/rfc7033) — `acct:` のアウトバウンド解決と JRD。
- `.kiro/steering/tech.md` / `.kiro/steering/structure.md` — 「検索の抽象境界」「全文検索は PostgreSQL 内で完結」「注入可能な非決定性境界」の一次方針。
- `.kiro/specs/accounts-and-instance/design.md` — Account 契約・`RemoteAccountFetcher`・`ActorDirectory` 消費点。
- `.kiro/specs/statuses-core/design.md` — Status 契約・`VisibilityPolicy`・可視投稿解決の消費点。
- `.kiro/specs/federation-core/design.md` — WebFinger・`FederationHttpClient`・JSON-LD 安全展開の消費点。
- `.kiro/specs/api-foundation/design.md` — Bearer/Scope/MastodonError/Pagination/RateLimit/契約ハーネスの適用点。
