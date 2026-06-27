# Brief: accounts-and-instance

## Problem
クライアントがログイン後に最初に叩くのはアカウント・インスタンス情報・カスタム絵文字。ここの JSON 契約がずれるとあらゆるクライアントが破綻する。リモートアカウントのフェッチ/正規化も必要。

## Current State
api-foundation（OAuth/ページネーション/契約ハーネス）、federation-core（リモートフェッチ）、media-pipeline（アバター/ヘッダ）が利用可能。

## Desired Outcome
`verify_credentials` / `accounts/:id` / `accounts/:id/statuses` / `relationships` / `update_credentials`、`instance`(v2)、`custom_emojis`(read) が Mastodon 互換で動き、Account/Instance/Relationship エンティティの JSON 契約がゴールデンテストで固定された状態。

## Approach
api-foundation の契約ハーネス上で Account/Instance/Relationship の JSON 契約を先に固定（ゴールデン）。ローカルアクター（actor-model）のシリアライズと、リモートアカウントのフェッチ/正規化（federation-core）を Account 形に揃える。アバター/ヘッダは media-pipeline 経由。instance v2 は運用設定（core-runtime の二層設定 DB 側）を反映。custom_emojis は read のみ。

## Scope
- **In**: accounts(verify_credentials/:id/:id/statuses/relationships/update_credentials)、instance v2、custom_emojis(read)、Account/Instance/Relationship 契約、リモートアカウント正規化の Account 形変換。
- **Out**: follow/block 等の関係変更操作（→ social-graph、relationships の読みはここ）、statuses 本体（→ statuses-core）、custom_emojis の管理/アップロード（→ custom-federation/admin）、familiar_followers（later）。

## Boundary Candidates
- Account エンティティ契約とシリアライズ（ローカル/リモート統一）
- instance v2 + 運用設定の反映
- custom_emojis(read)

## Out of Boundary
- 関係の変更（follow/block アクション）は social-graph
- 投稿の取得本体は statuses-core

## Upstream / Downstream
- **Upstream**: api-foundation, federation-core, media-pipeline, actor-model。
- **Downstream**: statuses-core(Account 埋め込み)、social-graph(relationships)、timelines、search。

## Existing Spec Touchpoints
- **Extends**: actor-model（ローカルアクターを Account 形にシリアライズ）。
- **Adjacent**: social-graph（relationships の書き込み側）。

## Constraints
- 一次情報は Mastodon 実レスポンス。Account/Instance/Relationship 契約はゴールデンで固定。
- ローカルアクターとリモートアカウントを同一 Account 契約に揃える。
- `familiar_followers` 等は後回し。
