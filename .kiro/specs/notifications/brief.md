# Brief: notifications

## Problem
フォロー・メンション・リアクション・投票結果などの通知が無いと実用にならない。通知は Streaming と Web Push の前提でもあり、単一の生成点を持つ必要がある。

## Current State
statuses-core（投稿・お気に入り・reblog 等のイベント源）と social-graph（フォロー）が確立。

## Desired Outcome
通知 v1（mention/follow/favourite/reblog/poll 等）が Mastodon 互換で動き、Notification エンティティの JSON 契約がゴールデンで固定され、ブロック/ミュートでフィルタされ、Streaming/Web Push が再利用できる単一の通知生成点になっている状態。

## Approach
Notification 契約を先に固定（ゴールデン）。statuses-core / social-graph のイベントから通知を生成する単一の生成点を設け、Streaming(後段)と Web Push(後段)が同じ点を再利用する。ブロック/ミュートのフィルタを適用。v2/policy/requests は後回し。

## Scope
- **In**: 通知 v1(mention/follow/follow_request/favourite/reblog/poll など)、Notification 契約、ブロック/ミュートフィルタ、単一の通知生成点、ページネーション。
- **Out**: notifications v2 / policy / requests（later）、Streaming 配信(→ streaming)、Web Push 配送(→ web-push)。

## Boundary Candidates
- Notification 契約と生成ロジック
- 単一の通知生成点（Streaming/Push 再利用の前提）

## Out of Boundary
- v2/policy は後回し
- 配信手段（streaming/web-push）

## Upstream / Downstream
- **Upstream**: statuses-core, social-graph。
- **Downstream**: streaming, web-push, experience-expansion(v2/policy)。

## Existing Spec Touchpoints
- **Extends**: statuses-core/social-graph のイベントを消費。
- **Adjacent**: streaming/web-push（生成点の共有シーム）。

## Constraints
- Notification 契約はゴールデン固定（一次情報は Mastodon 実レスポンス）。
- 単一生成点を保ち、Streaming/Push と二重発火しない。
- v2/policy/requests は後回し。
