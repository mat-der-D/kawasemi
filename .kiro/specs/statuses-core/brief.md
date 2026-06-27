# Brief: statuses-core

## Problem
投稿はプロジェクト最大の工数ブロック（XL）であり、タイムライン・通知・投票・ブックマーク等ほぼ全機能が依存するハブ。可視性/addressing がローカル最適化パスとリモート連合パスで一致しないと、ローカルでは動くがリモートで壊れる最重要リスクが顕在化する。

## Current State
api-foundation・federation-core（配送/可視性の共通パス）・media-pipeline（添付）が利用可能。Account 契約は accounts-and-instance で確立。

## Desired Outcome
投稿の作成/取得/削除/編集/context、reblog/favourite/bookmark/pin、投票(Poll)、冪等性(`Idempotency-Key`)が Mastodon 互換で動き、Status/Poll エンティティの JSON 契約がゴールデンで固定され、可視性/addressing がローカル/リモートで同一結果になることがテストで担保された状態。

## Approach
Status/Poll 契約を先に固定（ゴールデン）。投稿の生成・可視性判定・状態遷移は federation-core の共通コードパスを通し、配送関数のみ分岐。CW・編集履歴・添付（media-pipeline）・投票を含む。冪等性キーで二重投稿を防止。reblog/favourite/bookmark/pin を実装。引用・絵文字リアクションの方言対応は custom-federation に委ね、ここではコア状態モデルに方言を持ち込まない境界を保つ。

## Scope
- **In**: statuses post/get/delete/edit(+履歴/source)/context、reblog/favourite/bookmark/pin、Poll(作成/投票)、CW、可視性/addressing(共通パス)、`Idempotency-Key`、Status/Poll 契約。
- **Out**: タイムライン集約（→ timelines）、通知生成（→ notifications）、引用/絵文字リアクションの方言正規化（→ custom-federation）、検索（→ search）。

## Boundary Candidates
- Status エンティティ契約と CRUD/編集
- 可視性/addressing の共通パス（federation-core 上）
- reblog/favourite/bookmark/pin
- Poll
- 冪等性

## Out of Boundary
- 方言（引用/絵文字リアクション）は custom-federation
- TL 集約・通知は別 spec

## Upstream / Downstream
- **Upstream**: api-foundation, federation-core, media-pipeline, accounts-and-instance。
- **Downstream**: timelines, notifications, search, custom-federation, bookmarks/favourites の一覧。

## Existing Spec Touchpoints
- **Extends**: federation-core（共通配送/可視性パスを Create/Note 等で具体化）。
- **Adjacent**: custom-federation（引用/リアクションは別境界）、timelines/notifications（下流）。

## Constraints
- 可視性/addressing はローカル最適化パスと HTTP 連合パスで同一結果（最重要リスク・必ずテスト）。
- 一次情報は Mastodon 実レスポンス。Status/Poll 契約はゴールデン固定。
- 時刻/ID/RNG は注入可能境界（決定性）。
- コア状態モデルに連合方言を漏らさない。
