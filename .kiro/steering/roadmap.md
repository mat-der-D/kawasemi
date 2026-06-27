# Roadmap

## Overview

kawasemi は一人鯖（single-user server）運用に特化した Fediverse サーバーのフルスクラッチ実装（fork ではない）。ActivityPub で連合し、Mastodon 互換 API を通じて既存クライアント（Ivory・Elk・Phanpy 等）から利用でき、独自機能（絵文字リアクション・引用・MFM 等）と全アクター統合管理画面を独自フロントで提供する。Rust + axum + PostgreSQL のモノリスで、ローカル配送も意味論はリモートと対称・物理配送のみ最適化する。

本ロードマップは `docs/fediverse-design.md` / `docs/mastodon-api-compat.md` / `docs/mastodon-api-estimate.md` を一次情報として、責務境界で spec に分解したもの。

## Approach Decision

- **Chosen**: ドキュメント確定済みの技術スタック（Rust/axum/tokio/sqlx/PostgreSQL、TS+React 埋め込みフロント、DB ジョブキュー）を前提に、責務境界での **マルチスペック分解 + 依存波順の垂直スライス実装**。
- **Why**: 技術選定・スコープ・MVP 境界・フェーズ計画は `docs/` で既に確定。discovery の価値は「独立レビュー可能な責務境界への分解」と「依存順序の明示」にある。AI 自律 TDD（Opus）前提のため、設計判断とレビューが律速 = 境界を小さく切り独立検証可能にすることが重要。
- **Rejected alternatives**: (1) 単一巨大 spec — 20タスクを大幅超過しレビュー不能。(2) 既存実装の fork — プロダクト方針でフルスクラッチと確定。(3) 技術スタックの再検討 — `docs/tech.md` で確定済み、再litigateしない。

## Scope

- **In**: ActivityPub 連合基盤、Mastodon 互換 API（MVP: read+write+follow+media）、独自機能の連合相互運用、受信側 Move/Flag、全アクター統合管理画面、簡単インストール配布。
- **Out**: ディスカバリー（プロフィールディレクトリ）、アカウント移転の送信、通報の送信、Misskey API 互換（初期スコープ外・将来検討）。複数の人間ユーザー（一人鯖前提）。擬似匿名性（同一オーナーの秘匿は非目標）。

## Constraints

- バックエンド Rust / axum / tokio / sqlx / PostgreSQL のモノリス。外部ジョブブローカー・外部検索エンジンは使わない（DB で完結）。
- 非決定性（clock / id / RNG / 署名鍵）は注入可能境界の背後に置く（自律 TDD のため flaky 禁止）。
- ローカル／リモートで Activity 生成・可視性判定・状態遷移は共通コードパス。分岐は配送手段のみ。
- 仕様の一次情報は Mastodon 本体の実レスポンス（ドキュメント < 実レスポンス）。実クライアントの実リクエストをフィクスチャ化して受け入れ基準にする。
- 配布は事前ビルド済み（ユーザーにコンパイルさせない）。ランタイムは「アプリ + PostgreSQL」のみを目指す。
- Python 補助ツールは uv を使う（pip 非推奨）。
- プロジェクトファイルの Markdown は日本語で書く（spec.json.language）。

## Boundary Strategy

- **Why this split**: 横断サブシステム（OAuth・ページネーション・エラー/レート制限・連合基盤・メディア）を最上流の独立 spec に切り出し、各機能 spec がそれに乗る形にすることで、機能 spec を小さく独立レビュー可能に保つ。エンティティ JSON 契約（Account/Status/Notification/Poll/Relationship/Instance）は各機能 spec が所有するが、契約テストのハーネスは `api-foundation` で先に用意する。
- **Shared seams to watch**:
  - 「意味論は対称・物理配送のみ最適化」の境界 — `federation-core` が所有し、ローカル最適化パスと HTTP 連合パスが同一結果になることを連合テストで担保。`statuses-core` の可視性/addressing がここに依存。
  - OAuth の「複数アクター × 1トークン」のアクター選択 — Mastodon 標準から外れる独自設計点。`api-foundation` と `actor-model` の接合部。
  - Streaming の単一生成点 — REST タイムライン/通知と二重発火させない。`streaming` が read/write 確定後に乗る。
  - 独自連合方言（絵文字リアクション/引用）の正規化境界 — `custom-federation` に集約し、コア状態モデルから隔離。

## Specs (dependency order)

> 以下は Phase 1（MVP の実線まで）。brief.md 作成済み。`/kiro-spec-batch` はこの一覧を依存波順に並列生成する。

- [ ] core-runtime -- axum 起動・二層設定(TOML+DB)・埋め込みマイグレーション自動実行・DI境界(clock/id/rng/署名鍵)・可観測性・エラー基盤。Dependencies: none
- [ ] actor-model -- 複数アクターモデル + アクター毎署名鍵/ローテーション。管理層概念をプロトコル層に漏らさない境界。Dependencies: core-runtime
- [ ] api-foundation -- OAuth2(複数アクター×1トークンのアクター選択) + ページネーション規約 + レート制限/エラー互換 + 契約テストハーネス。Dependencies: core-runtime, actor-model
- [ ] federation-core -- HTTP Signatures(draft+RFC9421+double-knock)・WebFinger・NodeInfo・inbox/outbox/shared inbox・activity+json GET・JSON-LD・配送関数分岐・DB配送キュー。Dependencies: core-runtime, actor-model
- [ ] media-pipeline -- 非同期アップロード(202→poll)・ストレージ抽象・BlurHash・フォーカルポイント・ネイティブ依存判断ゲート。Dependencies: core-runtime, api-foundation
- [ ] accounts-and-instance -- accounts/relationships/update_credentials・instance v2・custom_emojis(read)。Dependencies: api-foundation, federation-core, media-pipeline
- [ ] statuses-core -- 投稿CRUD/編集/context・reblog/fav/bookmark/pin・投票・冪等性・可視性/addressing共通パス。Dependencies: api-foundation, federation-core, media-pipeline
- [ ] social-graph -- follow/follow_requests・mute/block(Block/Undo連合・署名拒否)・同一サーバー承認スキップ特権。Dependencies: api-foundation, federation-core, accounts-and-instance
- [ ] timelines -- home/public/local/tag タイムライン。Dependencies: statuses-core, social-graph
- [ ] notifications -- 通知 v1。Dependencies: statuses-core, social-graph
- [ ] search -- 抽象境界背後の最小実装（標準 PostgreSQL）。Dependencies: accounts-and-instance, statuses-core

## Future Phases (briefs pending)

> 計画として確定。brief.md は MVP コア確定後の再入（`/kiro-discovery` または `/kiro-spec-init`）で just-in-time 作成する。

### Phase 2 — リアルタイム
- [ ] streaming -- WebSocket Streaming（user/public/local/hashtag/list/direct、単一生成点の共有）。Dependencies: timelines, notifications
- [ ] web-push -- Web Push（VAPID・aes128gcm・購読管理）。Dependencies: notifications

### Phase 3 — 独自機能 & 受信処理
- [ ] custom-federation -- 絵文字リアクション・引用投稿・MFM の方言正規化（受信正規化/送信出し分け）。Dependencies: statuses-core, federation-core
- [ ] inbound-move-flag -- 受信側 Move（フォロー追従）・受信側 Flag（管理画面表示）。Dependencies: federation-core, social-graph

### Phase 4 — フロント & 配布
- [ ] admin-frontend -- 全アクター統合ダッシュボード・BOT管理・設定UI・インストール時テンプレ(Mastodon/Misskey-like)。Dependencies: api-foundation および主要 API
- [ ] distribution -- 事前ビルド配布(musl static / Docker)・SPA埋め込み・内蔵ACME・systemd unit(CAP_NET_BIND_SERVICE)・リバプロ後段・Postgres両対応。Dependencies: core-runtime, admin-frontend

### Phase 5 — 体験拡充
- [ ] experience-expansion -- lists/filters/conversations/markers/scheduled_statuses/featured_tags/preferences/announcements/notifications v2。Dependencies: timelines, notifications, statuses-core
