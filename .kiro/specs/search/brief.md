# Brief: search

## Problem
クライアントは検索（アカウント・投稿・ハッシュタグ）を前提とする。だが日本語全文検索を最初から作り込むと配布の簡単さ（拡張不要）と衝突する。将来の差し替え余地を残した最小実装が要る。

## Current State
accounts-and-instance・statuses-core が確立。federation-core の WebFinger（`acct:` 解決）が利用可能。

## Desired Outcome
search v2（accounts/statuses/hashtags、`acct:` でのリモート解決を含む）が Mastodon 互換で動き、検索処理が抽象インターフェースの背後に置かれ、後から日本語対応へ差し替え可能なマイグレーション経路が確保された状態。

## Approach
検索を抽象レイヤー（インターフェース）の背後に置き、初期は標準 PostgreSQL の範囲でできる最小実装。API 契約は Mastodon の検索 API 形に留め、特定エンジン前提を表に出さない。`acct:user@domain` の解決は WebFinger(federation-core) を使う。`pg_bigm` 等の日本語拡張は任意オプションに留め必須にしない（スキーマ/インデックスを後付けできる形）。

## Scope
- **In**: search v2(accounts/statuses/hashtags)、`acct:` リモート解決、検索の抽象境界、標準 PostgreSQL の最小実装、後付け拡張のマイグレーション経路。
- **Out**: `pg_bigm` 等の日本語拡張の必須化（任意オプションに留める）、外部検索エンジン、trends/suggestions（stub は別）。

## Boundary Candidates
- 検索の抽象インターフェース
- 標準 PostgreSQL 最小実装
- `acct:` リモート解決（WebFinger 連携）

## Out of Boundary
- 日本語拡張の必須化
- 外部検索エンジン

## Upstream / Downstream
- **Upstream**: accounts-and-instance, statuses-core, federation-core(WebFinger)。
- **Downstream**: 将来の日本語検索強化（差し替え）。

## Existing Spec Touchpoints
- **Extends**: なし。
- **Adjacent**: accounts-and-instance/statuses-core（検索対象）。

## Constraints
- 検索は抽象境界の背後。呼び出し側を特定エンジンに依存させない。
- 初期は拡張不要の最小実装（配布の簡単さと両立）。`pg_bigm` 等は任意オプション。
- API 契約は Mastodon 検索 API 形に留める。
- 標準 `to_tsvector` は日本語を分かち書きできない既知制約を踏まえる。
