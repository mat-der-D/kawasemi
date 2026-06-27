# Brief: api-foundation

## Problem
Mastodon 互換 API は単なる REST 群ではなく、OAuth・ページネーション・エラー/レート制限互換という横断土台に全機能が乗る。ここを各機能 spec で再発明すると不整合が出て、契約テストの基準も定まらない。

## Current State
core-runtime（起動・設定・DI・エラー骨格）と actor-model（複数アクター・署名鍵）が存在。認証・ページネーション・契約テストハーネスは未着手。

## Desired Outcome
標準クライアントが OAuth でログイン（複数アクター×1トークンのアクター選択を含む）でき、全リスト API が `Link` + `max_id`/`since_id`/`min_id` で一貫し、エラー JSON 形と `X-RateLimit-*` が Mastodon 互換で、エンティティ JSON 契約のゴールデンテスト基盤が用意された状態。

## Approach
OAuth 2.0 サーバー（アプリ登録 / 認可コード + 承認画面 / トークン発行・失効 / スコープ検査 / Bearer ミドルウェア）。複数アクター対応は `authorize` 時のアクター選択として実装（Mastodon 標準から外れる独自設計点・要明示）。ページネーション規約とエラー/レート制限互換を共通ミドルウェア層として先に固め、後続カテゴリはこれに乗るだけにする。契約テスト（ゴールデン/スナップショット）のハーネスもここで用意し、各機能 spec がエンティティ契約を足す。

## Scope
- **In**: OAuth2 サーバー全体、アクター選択ログイン、ページネーション規約（`Link`+cursor、カテゴリ毎カーソル型の考慮）、エラー JSON 互換、`X-RateLimit-*`、HTTP ステータス互換、契約テストハーネス（Account/Status/Instance 等のゴールデン基盤）。
- **Out**: 各エンティティの具体的 JSON 契約内容（各機能 spec が所有）、Streaming 認証の WebSocket 側（→ streaming、ただしトークン検査は再利用）、Web Push スコープの購読実体（→ web-push）。

## Boundary Candidates
- OAuth2 サーバー（最上流の認証入口）
- ページネーション規約（全リスト API の基盤）
- エラー/レート制限互換ミドルウェア
- 契約テストハーネス

## Out of Boundary
- 個別エンドポイントのビジネスロジック
- エンティティ契約の中身（各機能 spec）

## Upstream / Downstream
- **Upstream**: core-runtime, actor-model。
- **Downstream**: 認証/リスト/契約を要する全 API spec（accounts/statuses/timelines/...）。

## Existing Spec Touchpoints
- **Extends**: core-runtime のエラー/レスポンス骨格を API 互換形へ拡張。
- **Adjacent**: actor-model（アクター選択）、streaming（トークン検査の再利用）。

## Constraints
- 仕様の一次情報は Mastodon 本体の実レスポンス。レート上限は緩くてよいがヘッダ/エラー形は厳密一致。
- `since_id` vs `min_id` の意味差、カテゴリ毎カーソル型（bookmarks/favourites/notifications は status id でない）に注意。
- 複数アクター×1トークンのアクター選択は独自設計点として明示する。
