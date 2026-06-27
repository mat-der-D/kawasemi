# Brief: core-runtime

## Problem
すべての機能 spec が乗る土台が無いと、各 spec が起動・設定・マイグレーション・DI・可観測性を個別に再発明し、不整合と flaky テストの温床になる。AI 自律 TDD では決定性の欠如がループを壊す。

## Current State
実装コードはゼロ。`docs/` に技術スタックと配布方針が確定しているのみ。

## Desired Outcome
axum アプリが起動し、二層設定を読み、起動時に埋め込みマイグレーションを自動適用し、注入可能な非決定性境界（clock/id/rng/署名鍵）と統一エラー/レスポンス基盤・構造化ログを備えた状態。以降の spec はこの土台に機能を足すだけでよい。

## Approach
axum + tokio + sqlx(PostgreSQL) のモノリス骨格。設定は二層（起動設定=TOML/環境変数、運用設定=DB+後続で管理画面）。`sqlx migrate` を埋め込み起動時自動実行（up とデータ保持、失敗時安全停止をテスト）。clock / id generator / RNG / 署名鍵プロバイダを trait 等の差し替え可能境界に置く。失敗時にリクエスト/レスポンス/実行 SQL を出せる可観測性を最初から組み込む。

## Scope
- **In**: アプリ起動・graceful shutdown、DB プール、二層設定の起動設定側、埋め込みマイグレーション基盤と自動実行、DI 境界(clock/id/rng/署名鍵)、統一エラー型と HTTP レスポンス変換の骨格、構造化ログ/診断出力、テストハーネスの土台（DB 込み統合テストの起動）。
- **Out**: 個別 API エンドポイント、OAuth、ページネーション規約（→ api-foundation）、連合（→ federation-core）、運用設定の管理画面 UI、配布/ACME（→ distribution）。

## Boundary Candidates
- 起動/設定/シャットダウンのライフサイクル
- マイグレーション実行と検証
- 非決定性 DI 境界（clock/id/rng/署名鍵プロバイダ）
- 統一エラー/レスポンス・可観測性

## Out of Boundary
- 認証・認可（OAuth は api-foundation）
- ドメインモデル（アクター・投稿等は後続 spec）
- 配布形態・TLS（distribution）

## Upstream / Downstream
- **Upstream**: なし（最上流）。
- **Downstream**: すべての spec が依存する。

## Existing Spec Touchpoints
- **Extends**: なし（新規・最初の spec）。
- **Adjacent**: api-foundation（エラー/レスポンス規約を共有・拡張する接合点）。

## Constraints
- 外部ジョブブローカー/検索エンジンに依存しない。
- 非決定性は必ず注入可能境界の背後（flaky 禁止）。
- マイグレーションは埋め込み + 起動時自動。失敗時は安全停止。
