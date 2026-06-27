# Brief: timelines

## Problem
投稿と社会グラフがあってもタイムラインが無ければクライアントは何も表示できない。home/public/local/tag を一貫したページネーションと可視性フィルタで提供する必要がある。

## Current State
statuses-core（投稿・可視性）と social-graph（フォロー/ブロック/ミュート）が確立。api-foundation のページネーション規約が利用可能。

## Desired Outcome
home/public/local/tag タイムラインが Mastodon 互換で動き、フォロー関係・ブロック/ミュート・可視性が正しく反映され、`Link`+cursor のページネーションが一貫し、後段の Streaming が再利用できる単一の生成/クエリ点になっている状態。

## Approach
home(フォロー基準)・public・local・tag の各タイムラインを、statuses-core の可視性判定と social-graph のフィルタを使って構築。ページネーションは api-foundation 規約に乗る。Streaming が同じ結果を二重発火せず再利用できるよう、タイムライン生成のクエリ/生成点を単一に保つ（streaming spec の前提）。

## Scope
- **In**: home/public/local/tag タイムライン、可視性・ブロック/ミュートフィルタ、ページネーション、Streaming が再利用する単一生成点の確立。
- **Out**: list タイムライン（later）、Streaming 配信そのもの（→ streaming）、filters の `filtered` 適用（later）。

## Boundary Candidates
- 各タイムラインのクエリと可視性/フィルタ適用
- 単一生成点（Streaming 再利用の前提）

## Out of Boundary
- list TL は後回し
- リアルタイム配信は streaming

## Upstream / Downstream
- **Upstream**: statuses-core, social-graph, api-foundation。
- **Downstream**: streaming（同じ生成点を再利用）、experience-expansion(list TL)。

## Existing Spec Touchpoints
- **Extends**: statuses-core/social-graph を集約。
- **Adjacent**: streaming（単一生成点の共有シーム）。

## Constraints
- ページネーションは `Link`+`max_id`/`since_id`/`min_id` で一貫。
- Streaming と二重発火しない単一生成点を保つ。
- 可視性/フィルタはローカル/リモート共通の判定を使う。
