# Requirements Document

## Introduction

notifications は kawasemi の Mastodon 互換 API における **通知（Notification）v1 のエンティティ契約・取得 API・単一の通知生成点** を確立する spec である。メンション・フォロー・フォローリクエスト・お気に入り・ブースト（reblog）・投票終了などの通知が無ければ、標準クライアント（Ivory・Elk・Phanpy 等）は実用的に運用できない。また通知は後段の Streaming 配信・Web Push 配送の前提であり、二重発火を避けるため**通知を生成する点を一箇所に集約**する必要がある。

本 spec が完了すると、(1) `GET /api/v1/notifications`（一覧・ページネーション・種別フィルタ）・`GET /api/v1/notifications/:id`（単一）・`POST /api/v1/notifications/clear`（全消去）・`POST /api/v1/notifications/:id/dismiss`（個別消去）が Mastodon 互換で動作し、(2) Notification エンティティの JSON 契約が api-foundation の契約ハーネス上でゴールデン固定され、(3) 通知が statuses-core（お気に入り・ブースト・メンション・投票終了）と social-graph（フォロー・フォローリクエスト）の**既存イベントを消費して**単一の生成点で生成され、(4) ブロック / ミュート（通知ミュート含む）に基づき生成段でフィルタされ、(5) 後段の Streaming / Web Push が再利用できる単一の通知生成・配信シームが用意された状態になる。

最重要の設計上の論点は、(a) 通知を**再検出（DB 走査による独自検知）ではなく上流イベントの消費**で生成し、ローカル発生・リモート受信で同一の生成点を通すこと（意味論対称・二重発火防止）、(b) Notification 契約が埋め込む Account（accounts-and-instance 所有）と Status（statuses-core 所有）を**消費**し再定義しないこと、(c) ブロック / ミュートのフィルタを social-graph の関係状態問い合わせに委ねることである。

## Boundary Context

- **In scope**: 通知 v1 の種別（`mention` / `follow` / `follow_request` / `favourite` / `reblog` / `poll` / `status` / `update`）、Notification エンティティの JSON 契約（ゴールデン固定、Account / Status の埋め込み参照）、通知一覧取得（ページネーション・種別フィルタ（`types[]` / `exclude_types[]`）・`account_id` フィルタ）、単一通知取得、通知の全消去（clear）と個別消去（dismiss）、上流イベント（statuses-core / social-graph）を消費する単一の通知生成点、ブロック / ミュート（通知ミュート・期限考慮）に基づく生成段フィルタ、通知の重複排除、後段（Streaming / Web Push）が再利用するための通知配信シーム（生成点の共有）。
- **Out of scope**: notifications v2（グループ化・`group_key` 等）・notification policy・notification requests（後回し / experience-expansion）、`admin.sign_up` / `admin.report` 等の管理通知（後回し）、Streaming の WebSocket 配信そのもの（streaming）、Web Push の購読管理・VAPID・配送そのもの（web-push）、Status / Account / Relationship エンティティの JSON 契約定義（statuses-core / accounts-and-instance が所有。本 spec は埋め込み参照のみ）、フォロー / ブロック / ミュートの関係操作と関係状態の所有（social-graph）、お気に入り / ブースト / メンション / 投票の検出・状態モデルそのもの（statuses-core）、OAuth・ページネーション・エラー / レート制限・契約ハーネス基盤（api-foundation。本 spec は適用のみ）。
- **Adjacent expectations**: 本 spec は api-foundation の Bearer 認証・スコープ（`read:notifications` / `write:notifications`）・ページネーション規約（`Link` + カーソル）・Mastodon 互換エラー本文・`X-RateLimit-*`・契約テストハーネスに依存する。statuses-core の Status JSON 契約・シリアライザと、お気に入り / ブースト / メンション / 投票終了の発生イベントに依存する。social-graph のフォロー / フォローリクエスト発生イベントと、ブロック / ミュート（通知ミュート・期限考慮）の関係状態問い合わせに依存する。accounts-and-instance の Account JSON 契約・シリアライザに依存し、通知に通知元 Account を埋め込む。下流の streaming / web-push は本 spec が確立する単一の通知生成点と配信シームに乗る。

## Requirements

### Requirement 1: Notification エンティティ JSON 契約

**Objective:** クライアント実装者として、通知の JSON 表現が Mastodon 互換でゴールデン固定されていてほしい。これにより、出力ドリフトなく既存クライアントが通知一覧を正しく表示できる。

#### Acceptance Criteria

1. When 通知を JSON にシリアライズするとき, the Notifications Service shall Mastodon 互換の Notification フィールド（少なくとも `id` / `type` / `created_at` / `account`）を出力する。
2. When 通知が投稿に関連する種別（`mention` / `favourite` / `reblog` / `poll` / `status` / `update`）であるとき, the Notifications Service shall 関連する投稿を `status` フィールドに埋め込み、statuses-core の Status シリアライズを委譲して表現する。
3. When 通知元アカウントを表現するとき, the Notifications Service shall accounts-and-instance の Account シリアライズを委譲して `account` フィールドを構成し、Account 契約を再定義しない。
4. While Notification をシリアライズする間, the Notifications Service shall null 規律（例: 投稿を伴わない `follow` / `follow_request` 種別では `status` を含めないか `null` とする）を Mastodon 実レスポンスに合わせて一貫して維持する。
5. The Notifications Service shall Notification の `type` を本 spec が定義する v1 種別集合（`mention` / `follow` / `follow_request` / `favourite` / `reblog` / `poll` / `status` / `update`）に限定し、範囲外（v2 グループ化・管理通知）を出力しない。
6. The Notifications Service shall Notification JSON 契約を api-foundation の契約テストハーネスにゴールデンとして登録し、決定的な非決定性境界（時刻・ID）の下で再現可能にする。

### Requirement 2: 通知一覧の取得

**Objective:** 標準クライアントのユーザーとして、自分宛の通知を新着順で一覧取得したい。これにより、メンションやリアクションを見落とさずに確認できる。

#### Acceptance Criteria

1. When 認証済みアクターが通知一覧を要求したとき, the Notifications Service shall `read:notifications` スコープを検証して当該アクター宛の通知を、api-foundation のページネーション規約（通知 ID カーソル・`Link` ヘッダ）で返す。
2. When 通知一覧要求が種別フィルタ（`types[]` または `exclude_types[]`）を含むとき, the Notifications Service shall 指定された種別のみを含める、または指定された種別を除外して返す。
3. When 通知一覧要求が `account_id` フィルタを含むとき, the Notifications Service shall 当該アカウントが通知元である通知のみを返す。
4. The Notifications Service shall 消去済み（dismiss / clear 済み）の通知を一覧結果に含めない。
5. If 通知一覧要求が有効な Bearer トークンを欠く、または `read:notifications` 相当のスコープを満たさないとき, then the Notifications Service shall api-foundation の Mastodon 互換エラー応答（401 または 403 相当）を返す。

### Requirement 3: 単一通知の取得

**Objective:** 標準クライアントのユーザーとして、特定の通知を ID 指定で取得したい。これにより、個別の通知詳細を参照できる。

#### Acceptance Criteria

1. When 認証済みアクターが通知 ID を指定して取得を要求したとき, the Notifications Service shall `read:notifications` スコープを検証し、当該アクター宛の通知であれば Notification を返す。
2. If 指定された通知が要求アクター宛でない、または存在しないとき, then the Notifications Service shall Mastodon 互換の未検出応答（404 相当）を返す。

### Requirement 4: 通知の消去（clear / dismiss）

**Objective:** 標準クライアントのユーザーとして、確認済みの通知を全消去または個別消去したい。これにより、通知一覧を整理できる。

#### Acceptance Criteria

1. When 認証済みアクターが通知の全消去を要求したとき, the Notifications Service shall `write:notifications` スコープを検証して当該アクター宛の全通知を消去し、Mastodon 互換の成功応答を返す。
2. When 認証済みアクターが通知 ID を指定して個別消去を要求したとき, the Notifications Service shall `write:notifications` スコープを検証して当該通知を消去し、Mastodon 互換の成功応答を返す。
3. If 個別消去の対象が要求アクター宛でない、または存在しないとき, then the Notifications Service shall Mastodon 互換の未検出応答（404 相当）を返す。
4. When 通知が消去されたとき, the Notifications Service shall 以降の一覧取得・単一取得から当該通知を除外する。

### Requirement 5: 単一の通知生成点（上流イベントの消費）

**Objective:** アーキテクトとして、通知を一箇所で生成し、Streaming / Web Push がそこを再利用できるようにしたい。これにより、配信経路ごとの二重発火と検出ロジックの重複を防げる。

#### Acceptance Criteria

1. The Notifications Service shall statuses-core（お気に入り・ブースト・メンション・投票終了・編集等）および social-graph（フォロー・フォローリクエスト）が発生させる既存イベントを消費して通知を生成し、独自の DB 走査による再検出を行わない。
2. The Notifications Service shall 上流イベントを受け取る単一の通知生成点を提供し、ローカル発生・リモート受信のいずれの経路から発生したイベントも同一の生成点を通す。
3. When 通知生成イベントを受信したとき, the Notifications Service shall 通知の受信者がローカルアクターである場合にのみ通知を生成し、リモートアクター宛の通知は生成しない。
4. The Notifications Service shall 上流イベントを未消費・未登録の状態でも上流処理が成功するよう、通知生成シームを上流に対して任意（既定で no-op）として供給する。
5. When 通知が生成・永続化されたとき, the Notifications Service shall 後段の配信（Streaming / Web Push）が再利用できる配信シームへ生成済み通知を引き渡し、配信手段そのものは実装しない。

### Requirement 6: 通知種別ごとの生成

**Objective:** 標準クライアントのユーザーとして、種別ごとに適切な通知が生成されてほしい。これにより、どの操作が誰によって行われたかを把握できる。

#### Acceptance Criteria

1. When あるローカルアクターの投稿が他アクターにお気に入り登録されたイベントを受信したとき, the Notifications Service shall 当該投稿の作者宛に `favourite` 通知を生成する。
2. When あるローカルアクターの投稿が他アクターにブーストされたイベントを受信したとき, the Notifications Service shall 当該投稿の作者宛に `reblog` 通知を生成する。
3. When あるローカルアクターをメンションする投稿の発生イベントを受信したとき, the Notifications Service shall メンションされたアクター宛に `mention` 通知を生成する。
4. When あるローカルアクターへのフォロー成立イベントを受信したとき, the Notifications Service shall フォローされたアクター宛に `follow` 通知を生成する。
5. When ロック済みローカルアクター宛の保留フォローリクエスト発生イベントを受信したとき, the Notifications Service shall 当該アクター宛に `follow_request` 通知を生成する。
6. When ローカルアクターが作成または投票した投票の終了イベントを受信したとき, the Notifications Service shall 当該アクター宛に `poll` 通知を生成する。

### Requirement 7: ブロック / ミュートによる生成段フィルタ

**Objective:** 一人鯖の運用者として、ブロック / ミュートした相手からの通知を受け取りたくない。これにより、遮断・抑制の意図が通知にも一貫して反映される。

#### Acceptance Criteria

1. When 通知生成イベントの通知元アクターが受信者にブロックされている、または受信者が通知元アクターにブロックされているとき, the Notifications Service shall 当該通知を生成しない。
2. When 通知生成イベントの通知元アクターが受信者に通知ミュート（`muting_notifications`）されているとき, the Notifications Service shall 当該通知を生成しない。
3. While ミュートに有効期限が設定され期限が経過している間, the Notifications Service shall 当該ミュートを通知抑制の対象に含めない。
4. The Notifications Service shall ブロック / ミュートの関係状態を social-graph の関係状態問い合わせから取得し、関係状態を本 spec で再保持・再判定しない。

### Requirement 8: 通知の重複排除

**Objective:** 標準クライアントのユーザーとして、同一の操作で重複した通知を受け取りたくない。これにより、通知一覧が冗長にならない。

#### Acceptance Criteria

1. When 同一の通知元アクター・同一種別・同一対象（投稿等）について重複した生成イベントを受信したとき, the Notifications Service shall 重複した通知を新規生成しない。
2. While 上流イベントが再送・再受信される間, the Notifications Service shall 通知生成を冪等に保ち、同一の通知を二重に永続化しない。

### Requirement 9: 横断的な API 規律（認証・エラー・ページネーション・レート制限）

**Objective:** クライアント開発者として、通知 API が他の Mastodon 互換 API と一貫した振る舞いをすることを期待する。これにより、既存クライアントが追加実装なく利用できる。

#### Acceptance Criteria

1. The Notifications Service shall 通知の取得系に `read:notifications`、消去系（clear / dismiss）に `write:notifications` の api-foundation スコープ内包判定を適用する。
2. The Notifications Service shall 操作失敗時に api-foundation の Mastodon 互換エラー本文（`error` / 必要時 `error_description`）と対応する HTTP ステータスを返す。
3. The Notifications Service shall 一覧応答に api-foundation のページネーション（`Link` ヘッダ・通知 ID カーソル）を適用する。
4. The Notifications Service shall 通知エンドポイント群を api-foundation の `X-RateLimit-*` レイヤー適用点に乗せる。
