# Research & Design Decisions

## Summary

- **Feature**: `statuses-core`
- **Discovery Scope**: Extension（federation-core の配送/受信境界・api-foundation の横断土台・media-pipeline の添付契約の上に乗る、投稿コア機能の垂直スライス）
- **Key Findings**:
  - 投稿の「可視性 / addressing」は本 spec が所有する単一ロジックとし、federation-core の `DeliveryService` 共通パスへ確定済み recipient を渡すことで「意味論対称・物理配送のみ最適化」を構造的に満たす。これが最重要リスクの封じ込め点。
  - reblog=Announce / favourite=Like / poll vote=Create(Note Vote) は連合 Activity を伴うが、bookmark / pin はローカルのみ（連合しない）。状態モデルでこの差を明示する。
  - 引用・絵文字リアクションは方言が乱立するためコア状態モデルに持ち込まず、受信ディスパッチ登録も方言非依存に保つ（custom-federation へ委譲）。
  - マイグレーション番号は既存 spec と衝突を避けるため `0007_statuses.sql` を採用（後述の調整事項参照）。

## Research Log

### 上流境界の消費契約（api-foundation / federation-core / media-pipeline）

- **Context**: 本 spec は 3 つの上流 spec の確立済み境界に乗る。各境界の契約を取り違えると下流が破綻する。
- **Sources Consulted**: `.kiro/specs/api-foundation/design.md` / `requirements.md`、`.kiro/specs/federation-core/design.md`、`.kiro/specs/media-pipeline/design.md`、`.kiro/steering/*`。
- **Findings**:
  - api-foundation: 統一エラー JSON（`{"error": ...}` + 任意 `error_description`、`MastodonError` / `mastodon_status_for`）、Bearer 認証ミドルウェア（`RequestActorContext` = 単一アクター + `ScopeSet`、`authenticate` / `require_scope`）、スコープ内包判定（`Scope::is_satisfied_by`、上位は細分を包含）、ページネーション（`PageParams` / `Cursor` trait / `Page<T>` / `build_link_header`、カテゴリ毎カーソル）、`X-RateLimit-*` レイヤー、契約ハーネス（`assert_golden` / `register_fixture`、決定的 `RuntimeContext`）。
  - federation-core: 配送共通パス `DeliveryService::deliver(DeliveryRequest{activity, sender, recipients})`（分岐前に正規 Activity を一度だけ生成・検証、local は in-process sink、remote はキュー）、受信ディスパッチ `InboundActivityHandler`（`activity_types()` / `handle(&ParsedActivity, &InboundContext)`）+ `InboundActivityDispatcher::register`、`process_local` と `process_inbound` が同一の重複排除・ディスパッチに合流（10.3/10.5 の構造的担保）、`ActorUrls`（オブジェクト/コレクション URL 構築）、`JsonLdCodec`、`BlockPolicy`（social-graph 供給）。
  - media-pipeline: MediaAttachment 契約（`id`/`type`/`url`/`preview_url`/`remote_url`/`meta`/`focus`/`description`/`blurhash`）と `find_owned(media_id, actor_id)`（所有スコープ取得）。本 spec はメディア識別子で添付し、所有検証は本 spec が行う。
- **Implications**: 本 spec は新規の認証/ページネーション/エラー/配送/受信配管/メディア処理を作らず、上記契約に「乗るだけ」。Status に埋め込む `media_attachments` は media-pipeline の Serializer を再利用する。

### Account 埋め込み（accounts-and-instance との接合）

- **Context**: Status は投稿者 Account を埋め込む。Account 契約は accounts-and-instance が所有し、並行生成中。
- **Findings**: accounts-and-instance はローカルアクター（actor-model）とリモートアカウントを同一 Account 契約に揃える。本 spec は Account シリアライズ点（`AccountSerializer` 相当）を消費するのみで、Account 契約自体は所有しない。
- **Implications**: Status シリアライザは Account シリアライズを上流に委譲する境界を設ける。並行生成のため、Account シリアライズの呼び出し境界を抽象参照とし、契約確定後に結線する。

### Mastodon Status / Poll の一次情報

- **Context**: steering の「一次情報は Mastodon 実レスポンス（ドキュメント < 実レスポンス）」に従う。
- **Findings**: Status / Poll のフィールド・null 規律・reblog ネスト・編集（`edited_at` / `/history` / `/source`）・投票（`voted` / `own_votes` / `expired`）は Mastodon 実レスポンスを基準にゴールデン固定する。実クライアント（Ivory・Elk・Phanpy）のキャプチャをフィクスチャとして受け入れ基準化する（api-foundation `register_fixture`）。
- **Implications**: 契約テストを実装に先んじて固定する（契約 → 実装 → グリーン）。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| レイヤード（API → Service → Repository）+ 可視性/配送の共通サービス | 投稿業務を service に集約し、可視性/addressing を単一サービス化、配送は federation-core 共通パスへ委譲 | steering 準拠（レイヤー分離・意味論対称）、境界が明快、テスト容易 | 可視性/配送サービスが肥大化しやすい → 可視性判定・addressing 導出・Activity 生成を分離 | 採用 |
| 機能別縦割り（reblog/fav/bookmark/pin を独立モジュール） | 各操作を独立実装 | 並行実装しやすい | 状態カウンタ・配送・契約が重複し DRY を崩す | 部分採用（操作は薄く、共通の配送/カウンタ/シリアライズを共有） |
| 受信処理を本 spec で独自配管 | inbox 受信を自前で処理 | - | federation-core 境界違反・重複 | 却下（ディスパッチ登録のみ行う） |

## Design Decisions

### Decision: 可視性 / addressing を単一ロジックで所有し配送共通パスへ確定 recipient を渡す

- **Context**: 最重要リスク「ローカルでは動くがリモートで壊れる」を排除する（要件 4）。
- **Alternatives Considered**:
  1. ローカル宛は最適化のため可視性判定を簡略化 — 二重実装になりドリフトする。却下。
  2. 可視性判定を federation-core に持たせる — federation-core は「recipient を受け取るのみ」の境界。違反。却下。
- **Selected Approach**: 本 spec に `VisibilityPolicy`（可視性判定）と `Addressing`（`to`/`cc`/recipient 導出）を置き、`StatusActivityBuilder` が正規 Activity を生成、`DeliveryService::deliver` に `recipients` を確定して渡す。ローカル/リモートの差は federation-core の sink 分岐のみ。
- **Rationale**: steering「意味論は対称・物理配送のみ最適化」を本 spec のコードパスで体現。federation-core 境界（recipient を渡す側が確定）を尊重。
- **Trade-offs**: 可視性ロジックが本 spec 集中 → 単一責務に分割して肥大化を回避。
- **Follow-up**: 2 インスタンス連合テストでローカル/HTTP 結果同値を必ず検証（要件 4.5）。

### Decision: reblog/favourite/poll-vote は連合、bookmark/pin はローカルのみ

- **Context**: Mastodon 互換の意味論に合わせ、連合する操作としない操作を分離（要件 9–13）。
- **Selected Approach**: reblog=Announce(+Undo)、favourite=Like(+Undo)、poll vote=Create(Note `type:Vote`) を配送共通パスへ。bookmark / pin は連合せずローカル状態のみ。
- **Rationale**: bookmark は私的、pin はプロフィール表示（featured collection の連合は MVP 範囲外で federation-core/accounts 側の将来課題）。
- **Trade-offs**: pin の featured collection 連合を後回しにする（コア状態のみ）。

### Decision: 冪等性を投稿作成に限定し、(actor_id, idempotency_key) で一意化

- **Context**: 二重投稿防止（要件 5）。api-foundation は冪等性そのものは所有しない（横断土台に含まれない）ため本 spec が所有。
- **Selected Approach**: `status_idempotency_keys(actor_id, idempotency_key)` で一意制約を張り、初回作成時に作成 status_id を記録。再送時は記録済み status を返す。
- **Rationale**: Mastodon 互換（`Idempotency-Key` ヘッダ）。一意制約 + 競合時の再取得で原子性を担保。
- **Trade-offs**: キーの保持期間は運用設定（当面は無期限/十分長め）。

### Decision: 引用・絵文字リアクションをコアに持ち込まない

- **Context**: steering「独自連合方言の正規化境界」（要件 15）。
- **Selected Approach**: コア Status / 状態テーブルに方言フィールドを持たせず、受信ディスパッチ登録も方言非依存。未知プロパティは federation-core `JsonLdCodec` の安全展開で保持し意味論解釈しない。
- **Rationale**: 方言は custom-federation が受信正規化/送信出し分けで集約する。

### Decision: マイグレーション番号 `0007_statuses.sql`

- **Context**: 既存 spec のマイグレーション番号に衝突がある（後述）。
- **Selected Approach**: `migrations/0007_statuses.sql` を採用。
- **Rationale**: 衝突回避と前方追加規約の遵守（下記調整事項）。

## Migration Numbering Coordination（重要・調整事項）

既存 spec のマイグレーション番号には既知の衝突がある。実装時に統合担当が連番を最終確定する必要がある。

- `0001` — core-runtime
- `0002` — actor-model
- `0003` — api-foundation（`0003_oauth.sql`）
- `0004` — media-pipeline（`0004_media.sql`、設計内に「既存に 0003 重複があるため実装時に未使用番号と整合させる」と注記あり）
- `0005`–`0006` — accounts-and-instance（並行生成中）および social-graph が利用見込み（未確定）
- `0008` — federation-core（`0008_federation.sql`。番号調整により旧 `0003_federation.sql` から繰り下げ済み）

**本 spec の方針**: 明確に非衝突な高い番号として `0007_statuses.sql` を採用する。ただし sqlx の連番マイグレーションはグローバルに一意・単調増加でなければならないため、Phase 1 全 spec のマイグレーション番号は **統合時に一括リナンバリング**が必要になる可能性が高い。本 spec は「投稿コアのテーブル群は federation-core / media-pipeline / accounts-and-instance のテーブル確定後に適用される」前提で、`0007` を仮番号とし、統合担当が最終連番を確定する。

## Risks & Mitigations

- **可視性/addressing のローカル/リモート不一致（最重要）** — 単一 `VisibilityPolicy` + `Addressing` を共通パス化し、2 インスタンス連合テストで結果同値を必須検証（要件 4.5）。
- **マイグレーション番号衝突** — `0007` 仮番号 + 統合時リナンバリングを research に明記（上記）。
- **Account 契約の並行未確定** — Account シリアライズを抽象参照で受け、契約確定後に結線。契約テストはスタブ Account で先行可能。
- **冪等キーの競合** — 一意制約 + 競合時の既存 status 再取得で原子化。
- **編集履歴とカウンタ整合** — 編集は本文系の版管理のみで、reblog/fav/bookmark カウンタは不変（Mastodon 同様）であることをテストで固定。
- **削除時の参照整合** — 削除で関連（ブースト・お気に入り・ブックマーク・返信参照）の整合を保つ処理を明示（要件 7.4）。

## References

- Mastodon API: Status / Poll エンティティおよび statuses / polls エンドポイント（実レスポンスを一次情報とする）。
- `.kiro/specs/api-foundation/design.md`（OAuth・スコープ・ページネーション・エラー・契約ハーネス）。
- `.kiro/specs/federation-core/design.md`（`DeliveryService` / `InboundActivityHandler` / 意味論対称境界）。
- `.kiro/specs/media-pipeline/design.md`（MediaAttachment 契約・所有スコープ取得）。
- `.kiro/steering/tech.md` / `structure.md`（意味論対称・決定性・方言隔離・契約集約）。
