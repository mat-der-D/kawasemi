# Research & Design Decisions

## Summary
- **Feature**: `accounts-and-instance`
- **Discovery Scope**: Extension（既存の横断 spec 群 api-foundation / federation-core / media-pipeline / actor-model / core-runtime に乗る統合中心の機能追加）
- **Key Findings**:
  - 本 spec はエンティティ JSON 契約（Account / CredentialAccount / Relationship / Instance(v2) / CustomEmoji）と API 表層を所有するが、`accounts/:id/statuses` の Status 本体（statuses-core 所有）と `relationships` の関係状態（social-graph 所有）は **下流 spec が所有**する。依存方向は「下流→本 spec」であり、本 spec から下流を参照できないため、federation-core の `BlockPolicy` / `InboundActivityHandler` と同型の **委譲境界（port + 既定実装）** を本 spec が定義し、下流が実装登録する設計が必須。
  - core-runtime は運用設定（DB 保存値）のスキーマ/読み書きを所有せず後続 spec に委ねている（core-runtime design L24/43）。instance v2 の運用可変項目（title/description/contact/rules/registrations）を反映するには、本 spec が **運用設定の読み取りモデルと初期既定** を所有する必要がある（書き込み/管理 UI は admin-frontend）。
  - マイグレーション番号は 0001(core-runtime) / 0002(actor-model) / 0003(federation+oauth 衝突) / 0004(media) が使用済み。本 spec は **0005** を採用し衝突回避する。

## Research Log

### 委譲境界の必要性（下流所有情報の取り込み）
- **Context**: brief は `accounts/:id/statuses`（投稿一覧）と `relationships`（関係読み）を In scope とするが、Status 本体は statuses-core、関係状態は social-graph が所有する。両者は本 spec に依存する（roadmap 依存順）。
- **Sources Consulted**: roadmap.md（依存順）、brief.md（Scope/Out of Boundary/Upstream-Downstream）、federation-core design（`BlockPolicy` / `InboundActivityDispatcher` の委譲境界パターン）、api-foundation design（契約ハーネス拡張点）。
- **Findings**:
  - federation-core は「本 spec はブロック実体を持たず既定 no-op、social-graph が実装供給」という委譲境界を確立済み。同じ型を踏襲できる。
  - 本 spec が下流を `use` すると依存が逆流するため不可。port（trait）を本 spec が定義し、`AppState` のレジストリ経由で下流が登録する。
- **Implications**: `AccountStatusesProvider`（既定: 空）と `RelationshipStateProvider`（既定: 関係なし）を本 spec が所有。statuses-core / social-graph が後で実装登録。エンドポイント・ページネーション・可視性フィルタ条件の受け渡し・エンティティ表現の取りまとめは本 spec が担う。

### Account 契約のローカル/リモート統一
- **Context**: ローカルアクター（actor-model `ResolvedActor`）とリモートアカウント（federation 取得文書）を単一 Account 契約に揃える。
- **Sources Consulted**: actor-model design（`ResolvedActor` は owner 非露出、`Handle`/`display_name`/`summary`/`state`）、federation-core design（`ActivityPubDocumentBuilder` / `FederationHttpClient.fetch` / `ActorUrls`）、media-pipeline design（MediaAttachment・`MediaStore.public_url`）、Mastodon 実 Account 形。
- **Findings**:
  - ローカルは `ResolvedActor` + 本 spec 所有の「プロフィール拡張」（avatar/header/fields/locked/bot/discoverable/source 既定）から構築。`url`/`uri`/acct は federation-core の `ActorUrls`（または同等のドメイン情報）と Handle から導出。
  - リモートは federation の取得文書を正規化テーブル `remote_accounts` にキャッシュし、そこから構築。
  - counts（followers/following/statuses/last_status_at）は本 spec が真実源を持たない。ローカルは social-graph / statuses-core が真実源だが未登録時は 0。委譲境界（カウント供給）または既定 0 を返す。MVP は count を委譲可能な値供給にし、未供給時 0 とする。
- **Implications**: `AccountSerializer` がローカル/リモートの 2 入力を共通の Account JSON へ写像。avatar/header 未設定時はデフォルト画像 URL（Req 1.5）。emojis は CustomEmoji 読み取りモデルから本文ショートコード抽出で構築。

### 運用設定（instance v2）の所有
- **Context**: instance v2 は運用可変項目（title/description/contact/rules/registrations）を反映する必要があるが core-runtime はこれを所有しない。
- **Sources Consulted**: core-runtime design（運用設定は後続 spec 所有, L24/43）、tech.md（設定の二層化: 起動=TOML、運用=DB+管理画面）、Mastodon instance v2 形。
- **Findings**: 本 spec が instance 運用設定の読み取りモデルと初期既定（シード）を所有するのが最小・整合的。書き込み/管理 UI は admin-frontend に委ねる（Out of scope）。`configuration` は本サーバーの実制約（投稿上限・media-pipeline の上限等）から構成。
- **Implications**: `instance_settings` テーブル（単一行 or key-value）と既定値。`InstanceService` が運用設定 + 静的能力（version/configuration）を合成して Instance(v2) を返す。

### マイグレーション番号の衝突回避
- **Context**: 既存設計に 0003 の二重利用（federation / oauth）と 0004(media) が存在。
- **Findings**: 0001/0002/0003/0004 使用済み。本 spec は 0005 を採用。
- **Implications**: `migrations/0005_accounts.sql` に本 spec 所有テーブル（`account_profiles` / `remote_accounts` / `custom_emojis` / `instance_settings`）を定義。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| Repository + Service + 委譲 port（採用） | エンティティ契約・API 表層・正規化は本 spec、下流所有情報は port 経由で取得 | 依存逆流を防ぐ／契約を一箇所に集約／下流が独立して乗れる | port の既定実装で「空」応答になる期間の挙動を明示する必要 | federation-core の `BlockPolicy` と同型で一貫 |
| 下流 spec への直接依存 | statuses-core/social-graph を直接参照 | port 不要で単純 | roadmap の依存方向に反する（循環/逆流）。実装不能 | 却下 |
| 契約を api-foundation に置く | エンティティ契約を土台に集約 | ハーネスと同居 | brief で本 spec が契約を所有と明記。境界違反 | 却下（ハーネスは拡張点のみ利用） |

## Design Decisions

### Decision: 下流所有情報の委譲境界（port + 既定実装）
- **Context**: `accounts/:id/statuses`（Status 本体=statuses-core）と `relationships`（関係状態=social-graph）。
- **Alternatives Considered**:
  1. 下流を直接参照 — 依存逆流で不可。
  2. port + 既定実装（空/関係なし）を本 spec が所有、下流が登録 — federation-core と一貫。
- **Selected Approach**: `AccountStatusesProvider`（既定: 空ページ）と `RelationshipStateProvider`（既定: 関係なし）、および任意で `AccountCountsProvider`（既定: 0）を本 spec が定義し `AppState` のレジストリに保持。下流 spec が実装を差し込む。
- **Rationale**: 依存方向を守りつつ、契約・表層・ページネーション・可視性条件の受け渡しを本 spec に集約できる。
- **Trade-offs**: 下流未登録時は空/0 応答（テストで明示）。エンドポイントは常に正常応答する。
- **Follow-up**: statuses-core / social-graph 側で provider 実装登録タスクを各 spec が持つ（本 spec のタスクではない）。

### Decision: ローカル/リモートを単一 AccountSerializer へ統一
- **Context**: 出自に依らず同一 Account 契約。
- **Selected Approach**: `AccountSerializer` がローカル入力（`ResolvedActor` + `AccountProfile`）とリモート入力（`RemoteAccount`）の双方を共通 Account JSON へ写像。avatar/header は `MediaStore.public_url`／デフォルト URL、emojis は CustomEmoji 読み取りモデルから抽出。
- **Rationale**: 契約固定（ゴールデン）を 1 シリアライザに集約しドリフトを防ぐ。
- **Trade-offs**: ローカルとリモートで欠落しうるフィールドの既定化（counts=0、fields=空）を明示的に扱う必要。

### Decision: instance 運用設定を本 spec が読み取り所有
- **Context**: core-runtime 非所有。
- **Selected Approach**: `instance_settings` テーブル + 既定値 + `InstanceSettingsRepository`（read）。書き込みは admin-frontend。
- **Rationale**: brief 要件（運用設定反映）を依存衝突なく満たす最小実装。

## Risks & Mitigations
- **委譲 provider 未登録期間の挙動が曖昧になる** — 既定実装（空/関係なし/0）を本 spec が提供し、その応答を統合テストで固定する。
- **リモート正規化が他実装の方言で壊れる** — federation-core の安全展開（未知プロパティ無視）に準拠し、必須欠落のみ失敗扱い（Req 7.4/7.5）。独自方言の解釈は custom-federation に委ね本 spec は標準フィールドのみ。
- **counts の真実源不在で 0 固定になる** — MVP は `AccountCountsProvider`（既定 0）で吸収。将来 statuses-core/social-graph が供給。契約上 0 は許容値。
- **Account/Instance 契約のドリフト** — 全エンティティをハーネスにゴールデン登録し、決定的 `RuntimeContext` で再現（api-foundation Req 9 準拠）。

## References
- Mastodon API: Account / CredentialAccount / Relationship / Instance(v2) / CustomEmoji エンティティ（一次情報は Mastodon 実レスポンス、ドキュメント < 実レスポンス）。
- `.kiro/specs/api-foundation/design.md` — Bearer/Scope/MastodonError/Pagination/ContractHarness。
- `.kiro/specs/federation-core/design.md` — `ActorUrls` / `FederationHttpClient` / `ActivityPubDocumentBuilder` / 委譲境界パターン。
- `.kiro/specs/media-pipeline/design.md` — MediaAttachment / `MediaStore`。
- `.kiro/specs/actor-model/design.md` — `ActorDirectory` / `ResolvedActor`。
- `.kiro/specs/core-runtime/design.md` — 運用設定非所有・マイグレーション追加規約。
