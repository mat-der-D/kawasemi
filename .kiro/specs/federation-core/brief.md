# Brief: federation-core

## Problem
ActivityPub 連合の土台（署名・WebFinger・NodeInfo・inbox/outbox・オブジェクト配信・JSON-LD）が無いと外部からフォローも配送もできない。さらに「意味論は対称・物理配送のみ最適化」を構造で担保しないと、ローカルでは顕在化せずリモート連合でだけ壊れるバグを生む。

## Current State
core-runtime と actor-model（アクター・署名鍵）が存在。連合エンドポイント・署名検証・配送経路は未実装。

## Desired Outcome
外部インスタンスから WebFinger でアクターを解決でき、署名付き Activity を inbox/shared inbox で受信・検証し、outbox とオブジェクトを `application/activity+json` で配信でき、配送が「論理的に同一の Activity を生成・検証してから配送関数のみ分岐（in-process / HTTP）」する形で実装され、ローカル最適化パスと HTTP 連合パスが同一結果になることがテストで担保された状態。

## Approach
HTTP Signatures（draft-cavage と RFC 9421 双方 + double-knocking フォールバック交渉、公開鍵取得/キャッシュ、ブロック先署名は拒否）、WebFinger（`acct:` → アクター URL、複数アクター分）、NodeInfo（最小限の公開統計）、inbox/outbox/shared inbox、アクター/オブジェクト/コレクションの activity+json GET、JSON-LD `@context` と未知プロパティの安全な展開。配送は DB ジョブキューで非同期化。可視性判定・状態遷移は共通コードパスとし、配送関数（in-process 関数呼び出し or HTTP 送信）のみ分岐。連合テストは自前インスタンスを 2 つ起動して Activity 往復を検証。

## Scope
- **In**: HTTP Signatures(送受信)、WebFinger、NodeInfo、inbox/outbox/shared inbox、object/actor/collection GET、JSON-LD/@context、配送抽象（in-process↔HTTP 分岐）、DB 配送キュー、署名検証/公開鍵取得のモック可能境界、連合テスト基盤（2インスタンス往復）。
- **Out**: 具体 Activity の意味論（Create/Follow/Block 等の業務処理は各機能 spec）、独自方言（絵文字リアクション/引用）の正規化（→ custom-federation）、受信側 Move/Flag（→ inbound-move-flag）、Mastodon REST API。

## Boundary Candidates
- HTTP Signatures（送受信・double-knock）
- WebFinger / NodeInfo
- inbox/outbox/shared inbox と object GET
- 配送抽象（対称意味論・物理配送のみ分岐）+ DB 配送キュー
- JSON-LD 展開境界

## Out of Boundary
- 各 Activity 種別の業務ロジック（statuses/social-graph 等）
- 独自連合方言の正規化（custom-federation）

## Upstream / Downstream
- **Upstream**: core-runtime, actor-model。
- **Downstream**: accounts-and-instance(リモートフェッチ)、statuses-core(配送/可視性)、social-graph(Follow/Block 往復)、search(WebFinger)、custom-federation、inbound-move-flag。

## Existing Spec Touchpoints
- **Extends**: なし。
- **Adjacent**: statuses-core（可視性/addressing の共有シーム）、custom-federation（方言は別境界）。

## Constraints
- 配送は「Activity 生成・検証は共通 → 配送関数のみ分岐」。ローカル/HTTP の同一結果をテストで担保（mastodon-api-compat 4.5）。
- draft-cavage と RFC 9421 双方 + double-knocking に互換対応。ブロック先署名は拒否。
- 署名検証・公開鍵取得・ネットワークはモック可能境界に切り出す。
- セキュアモード時は authorized fetch（署名付き GET 要求）に接続できる構造。
