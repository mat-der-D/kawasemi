# Brief: social-graph

## Problem
フォロー・ブロック・ミュートが無いとホームタイムラインも安全性機能も成立しない。Follow/Block は連合 Activity の往復を伴い、ブロック先からの署名拒否やタイムライン/通知フィルタにも波及する。

## Current State
api-foundation・federation-core（Activity 往復・署名拒否境界）・accounts-and-instance（relationships の読み）が利用可能。

## Desired Outcome
follow/follow_requests・mute・block が Mastodon 互換で動き、Follow/Block/Undo が連合で往復し、ブロック先の署名が拒否され、同一サーバー内フォロー承認スキップが管理者特権として一箇所に定義され、関係状態がタイムライン/通知のフィルタに反映される状態。

## Approach
follow/unfollow/follow_requests(承認・拒否)・mute・block を実装。各操作は federation-core の共通パスで Activity（Follow/Accept/Reject/Block/Undo）を生成し配送関数のみ分岐。ブロック先からの署名拒否は federation-core の検証境界に接続。「同一サーバー内はフォロー承認不要」はサーバー層の明示的な管理者特権として一箇所に定義（意味論対称の上に乗る例外）。関係状態は relationships(accounts-and-instance) と整合。

## Scope
- **In**: follow/unfollow、follow_requests(authorize/reject)、mute/unmute、block/unblock(+署名拒否連携)、Follow/Accept/Reject/Block/Undo の連合往復、同一サーバー承認スキップ特権、relationships の書き込み側。
- **Out**: domain_blocks（must→later）、relationships の純読み取り契約（accounts-and-instance）、TL/通知本体（フィルタ要件は提供するが実装は各 spec）。

## Boundary Candidates
- フォロー関係と承認フロー（連合往復）
- mute/block と署名拒否連携
- 同一サーバー承認スキップの管理者特権（一箇所定義）

## Out of Boundary
- domain_blocks は後回し
- TL/通知の実装本体

## Upstream / Downstream
- **Upstream**: api-foundation, federation-core, accounts-and-instance。
- **Downstream**: timelines(home/フィルタ)、notifications(フィルタ)、inbound-move-flag。

## Existing Spec Touchpoints
- **Extends**: federation-core（Follow/Block Activity を具体化）、accounts-and-instance（relationships 書き込み）。
- **Adjacent**: timelines/notifications（フィルタ消費側）。

## Constraints
- Follow/Block は連合往復。ローカル/リモートで同一結果をテスト。
- ブロック先署名は拒否（federation-core 連携）。
- 同一サーバー承認スキップは管理者特権として一箇所に定義（意味論対称の明示的例外）。
- domain_blocks は後回し。
