# Requirements Document

## Introduction

social-graph は kawasemi のソーシャルグラフ（フォロー・フォローリクエスト・ミュート・ブロック）を Mastodon 互換で提供する spec である。ホームタイムラインも安全性機能も、フォロー・ブロック・ミュートが成立して初めて機能するため、本 spec は MVP の中核に位置する。フォローとブロックは連合 Activity の往復（Follow / Accept / Reject / Block / Undo）を伴い、ブロック先からの署名拒否やタイムライン・通知のフィルタにも波及する。

本 spec が完了すると、(1) `POST /api/v1/accounts/:id/follow`・`/unfollow`、`GET /api/v1/follow_requests`・`POST /api/v1/follow_requests/:id/authorize`・`/reject`、`POST /api/v1/accounts/:id/mute`・`/unmute`、`POST /api/v1/accounts/:id/block`・`/unblock` が Mastodon 互換で動作し、(2) これらの操作が federation-core の共通配送パスを通じて Follow / Accept / Reject / Block / Undo の Activity をローカル・リモート対称に往復させ、(3) 受信側でも同 Activity 群を処理してフォロワー・フォローリクエスト・被ブロック状態を更新し、(4) ブロックした相手からの署名付き要求が連合層で拒否され、(5) 「同一サーバー内のフォローは承認不要」が単一箇所に定義された管理者特権として振る舞い、(6) 関係状態が accounts-and-instance の Relationship 契約に整合し、タイムライン・通知のフィルタへ供給可能になる状態になる。

最重要の設計上の論点は、(a) ローカル宛・リモート宛で Activity 生成・状態遷移・可視性影響を共通コードパスで処理し配送手段のみ分岐させること（意味論対称）、(b) 「同一サーバー承認スキップ」を意味論対称の上に乗る**明示的な例外（管理者特権）**として一箇所に閉じ込めること、(c) 関係状態の真実源を本 spec が所有しつつ、Relationship の JSON 契約は accounts-and-instance が所有する契約を**消費**（再定義しない）することである。

## Boundary Context

- **In scope**: follow / unfollow、follow_requests（一覧 / authorize / reject）、mute / unmute（通知ミュート・期間オプション含む）、block / unblock（被ブロックに伴う関係解消含む）、Follow / Accept / Reject / Block / Undo の送信側 Activity 生成と受信側 Activity 処理（ローカル・リモート対称）、ブロック判定の federation-core 委譲境界（`BlockPolicy`）への実装供給（ブロック先署名拒否）、同一サーバー承認スキップの管理者特権（単一定義）、関係状態（フォロー・保留フォローリクエスト・ミュート・ブロック）の永続化と書き込み、accounts-and-instance の関係状態委譲境界（`RelationshipStateProvider`）への実装供給、タイムライン・通知が消費するための関係状態問い合わせ。
- **Out of scope**: Relationship / Account エンティティの JSON 契約定義そのもの（accounts-and-instance が所有。本 spec は消費）、`relationships` 読み取りエンドポイント本体（accounts-and-instance が所有）、ホーム/公開タイムラインと通知の本体実装およびそのフィルタ適用処理（timelines / notifications が所有。本 spec は関係状態を供給するのみ）、`domain_blocks`（ドメインブロック）の実装（後回し / later）、HTTP Signatures の生成・検証・double-knock・配送キューそのもの（federation-core が所有）、認証・スコープ・エラー本文・ページネーション・レート制限・契約ハーネス基盤（api-foundation が所有）、アクター・署名鍵モデル（actor-model）、リモートアカウントのフェッチ・正規化（accounts-and-instance）。
- **Adjacent expectations**: 本 spec は api-foundation（Bearer 認証・`follow` スコープと内包判定・Mastodon 互換エラー本文・`X-RateLimit-*`・ページネーション規約・契約ハーネス）、federation-core（`DeliveryService` 共通配送パス・`InboundActivityHandler` 登録レジストリ・`BlockPolicy` 委譲境界・署名検証境界・アクター URL/inbox 解決・「意味論対称・物理配送のみ分岐」境界）、accounts-and-instance（`RelationshipView` を含む Relationship JSON 契約と `RelationshipSerializer`、`RelationshipStateProvider` 委譲境界、ローカル/リモートアカウント解決）、actor-model（`ActorDirectory` のアクター解決）、core-runtime（`AppState` / `RuntimeContext` / `PgPool` / `AppError` / マイグレーション基盤 / テストハーネス）に依存する。下流の timelines / notifications は本 spec が公開する関係状態問い合わせを消費してフィルタを行う。inbound-move-flag は本 spec のフォロー関係を前提に受信側 Move のフォロー追従を行う。

## Requirements

### Requirement 1: フォロー / アンフォロー（ローカル・リモート対称）

**Objective:** 標準クライアントの利用者として、ローカル/リモートを問わず任意のアカウントをフォロー・アンフォローしたい。これにより、相手の投稿をホームタイムラインで受け取り、また受け取りを止められる。

#### Acceptance Criteria

1. When 認証済みリクエストがあるアカウントへのフォローを要求したとき, the Social Graph Service shall フォロー関係（または保留中のフォローリクエスト）を作成し、accounts-and-instance の Relationship 契約に従った関係状態を返す。
2. When フォロー対象がリモートアクターであるとき, the Social Graph Service shall Follow Activity を生成し、federation-core の共通配送パスを通じて対象アクターの inbox へ配送する。
3. When フォロー対象がローカルアクターであるとき, the Social Graph Service shall リモート宛と同一の Follow Activity を生成し、配送手段のみ in-process に分岐させて同一の関係状態遷移を起こす。
4. When 認証済みリクエストがあるアカウントへのアンフォローを要求したとき, the Social Graph Service shall 既存のフォロー関係または保留中フォローリクエストを解消し、Undo(Follow) Activity を共通配送パスで配送し、更新後の関係状態を返す。
5. When フォロー要求がフォロー挙動オプション（reblogs 表示可否・通知可否・対象言語）を含むとき, the Social Graph Service shall 対応する関係フラグ（`showing_reblogs` / `notifying` / `languages`）へ反映する。
6. If 既にフォロー済み（または保留中）のアカウントへ重複してフォローを要求したとき, then the Social Graph Service shall 重複した関係や重複 Activity を作らず、現在の関係状態を冪等に返す。
7. If フォロー対象が自分自身であるとき, then the Social Graph Service shall フォロー関係を作成せず、Mastodon 互換のエラー応答を返す。
8. If フォロー/アンフォロー要求が有効な Bearer トークンを欠く、または `follow` 相当のスコープを満たさないとき, then the Social Graph Service shall api-foundation の Mastodon 互換エラー応答（401 または 403 相当）を返す。

### Requirement 2: フォロー承認フロー（フォローリクエスト）

**Objective:** 一人鯖のオーナーとして、ロック済みアクター宛のフォローを承認制で管理したい。これにより、誰が自分をフォローするかを制御できる。

#### Acceptance Criteria

1. When ロック済み（手動承認）のローカルアクター宛にフォロー要求が到達したとき, the Social Graph Service shall フォロー関係を即時に確立せず、保留中のフォローリクエストとして記録する。
2. When 認証済みリクエストがフォローリクエスト一覧を要求したとき, the Social Graph Service shall 当該アクター宛の保留中フォローリクエストの送信元アカウント一覧を、api-foundation のページネーション規約に従って返す。
3. When 認証済みリクエストが特定のフォローリクエストの承認（authorize）を要求したとき, the Social Graph Service shall フォロー関係を確立し、送信元アクターへ Accept(Follow) Activity を共通配送パスで配送し、更新後の関係状態を返す。
4. When 認証済みリクエストが特定のフォローリクエストの拒否（reject）を要求したとき, the Social Graph Service shall 保留中フォローリクエストを削除し、送信元アクターへ Reject(Follow) Activity を共通配送パスで配送し、更新後の関係状態を返す。
5. When 送信したフォローに対する Accept(Follow) Activity を受信したとき, the Social Graph Service shall 対応する送信中フォローリクエストを確立済みフォローへ遷移させる。
6. When 送信したフォローに対する Reject(Follow) Activity を受信したとき, the Social Graph Service shall 対応する送信中フォローリクエストを削除し、フォロー関係を確立しない。
7. If 承認/拒否要求が有効な Bearer トークンを欠く、または `follow` 相当のスコープを満たさないとき, then the Social Graph Service shall Mastodon 互換エラー応答（401 または 403 相当）を返す。

### Requirement 3: 同一サーバー承認スキップ（管理者特権の単一定義）

**Objective:** 一人鯖のオーナーとして、自分の管理下にあるアクター同士のフォローでは承認待ちを発生させたくない。これにより、複数アクターを一元管理する運用が滑らかになる。

#### Acceptance Criteria

1. While フォローの送信元と宛先がいずれも同一サーバーのローカルアクターである間, the Social Graph Service shall 宛先アクターがロック済みであってもフォローリクエストを保留にせず、フォロー関係を即時に確立する。
2. When 同一サーバー内フォローが即時確立されたとき, the Social Graph Service shall 通常のフォロー確立と同一の関係状態遷移と（ローカル in-process の）Accept 相当の意味論処理を起こす。
3. The Social Graph Service shall 「同一サーバー承認スキップ」を、意味論対称の共通パス上に乗る**明示的な単一の管理者特権**として一箇所にのみ定義し、フォロー処理経路に分散した特例分岐を作らない。
4. While フォローの送信元または宛先のいずれかがリモートアクターである間, the Social Graph Service shall 同一サーバー承認スキップ特権を適用せず、通常の承認フロー（ロック済みなら保留）に従う。

### Requirement 4: ミュート / アンミュート

**Objective:** 標準クライアントの利用者として、あるアカウントの投稿や通知を自分の画面から見えなくしたい。これにより、フォロー関係を維持したまま表示を抑制できる。

#### Acceptance Criteria

1. When 認証済みリクエストがあるアカウントのミュートを要求したとき, the Social Graph Service shall ミュート関係を記録し、更新後の関係状態（`muting` を真）を返す。
2. When ミュート要求が通知ミュート指定を含むとき, the Social Graph Service shall 通知ミュート状態（`muting_notifications`）を対応するフラグへ反映する。
3. Where ミュート要求が有効期間（duration）を指定する場合, the Social Graph Service shall 当該期間の経過後にミュートが自動解除されるよう有効期限を記録する。
4. When 認証済みリクエストがあるアカウントのアンミュートを要求したとき, the Social Graph Service shall ミュート関係を解消し、更新後の関係状態を返す。
5. The Social Graph Service shall ミュートを連合 Activity として外部へ配送しないローカル限定の関係として扱う。
6. If ミュート/アンミュート要求が有効な Bearer トークンを欠く、または要求スコープを満たさないとき, then the Social Graph Service shall Mastodon 互換エラー応答（401 または 403 相当）を返す。

### Requirement 5: ブロック / アンブロック

**Objective:** 一人鯖の運用者として、あるアカウントを完全に遮断したい。これにより、相互のフォロー関係を解消し、相手からの到達と相手への露出を断てる。

#### Acceptance Criteria

1. When 認証済みリクエストがあるアカウントのブロックを要求したとき, the Social Graph Service shall ブロック関係を記録し、更新後の関係状態（`blocking` を真）を返す。
2. When ブロックが成立したとき, the Social Graph Service shall ブロック元からブロック先への既存フォロー関係、ブロック先からブロック元への既存フォロー関係、および両方向の保留中フォローリクエストを解消する。
3. When ブロック対象がリモートアクターであるとき, the Social Graph Service shall Block Activity を生成し、federation-core の共通配送パスを通じて対象アクターの inbox へ配送する。
4. When 認証済みリクエストがあるアカウントのアンブロックを要求したとき, the Social Graph Service shall ブロック関係を解消し、Undo(Block) Activity を共通配送パスで配送し、更新後の関係状態を返す。
5. When ブロック対象がローカルアクターであるとき, the Social Graph Service shall リモート宛と同一の Block / Undo Activity を生成し、配送手段のみ in-process に分岐させて同一の関係状態遷移を起こす。
6. If ブロック/アンブロック要求が有効な Bearer トークンを欠く、または要求スコープを満たさないとき, then the Social Graph Service shall Mastodon 互換エラー応答（401 または 403 相当）を返す。

### Requirement 6: ブロック先への署名拒否（連合層遮断）

**Objective:** 一人鯖の運用者として、ブロックした相手からの署名付き要求を連合層で受理したくない。これにより、ブロックの意図を API 層だけでなく連合層でも貫ける。

#### Acceptance Criteria

1. The Social Graph Service shall federation-core のブロック判定委譲境界（`BlockPolicy`）に、本 spec が保持するブロック関係に基づく判定実装を供給する。
2. When 受信した署名付き要求の署名者が、当該宛先ローカルアクターからブロックされていると判定されたとき, the Social Graph Service shall その署名者をブロック対象として委譲境界に報告し、federation-core が当該要求を拒否できるようにする。
3. While あるアクターがブロックされている間, the Social Graph Service shall そのアクターからの受信 Follow / その他 Activity を、ブロック解除まで継続して拒否対象として判定する。
4. When ブロックが解除されたとき, the Social Graph Service shall 当該アクターを以降の署名拒否対象から外す。

### Requirement 7: 受信側 Activity 処理（フォロー・ブロックの往復成立）

**Objective:** 連合の実装者として、外部インスタンスから届く Follow / Accept / Reject / Block / Undo を処理したい。これにより、フォローとブロックがローカル・リモートで同一結果になる往復を成立させられる。

#### Acceptance Criteria

1. The Social Graph Service shall federation-core の受信ディスパッチ境界（`InboundActivityHandler`）に Follow / Accept / Reject / Block / Undo の処理ハンドラを登録する。
2. When 受信した Follow Activity の宛先がロックされていないローカルアクター（または同一サーバー特権が適用される）とき, the Social Graph Service shall フォロー関係を確立し、送信元へ Accept(Follow) Activity を配送する。
3. When 受信した Follow Activity の宛先がロック済みローカルアクターであるとき, the Social Graph Service shall 保留中のフォローリクエストとして記録し、即時の Accept を行わない。
4. When 受信した Block Activity を処理したとき, the Social Graph Service shall 送信元から宛先への被ブロック状態（`blocked_by`）を記録し、両者間の既存フォロー関係および保留フォローリクエストを解消する。
5. When 受信した Undo(Follow) Activity を処理したとき, the Social Graph Service shall 送信元から宛先へのフォロー関係（またはフォロワー登録）を解消する。
6. When 受信した Undo(Block) Activity を処理したとき, the Social Graph Service shall 送信元から宛先への被ブロック状態を解消する。
7. If 受信した Activity が既に処理済み（重複）であるとき, then the Social Graph Service shall 関係状態を二重に変更せず冪等に処理する。

### Requirement 8: 関係状態の所有と Relationship 契約の消費

**Objective:** 下流 spec（accounts-and-instance / timelines / notifications）の実装者として、関係状態の単一の真実源にアクセスしたい。これにより、Relationship 表示・タイムライン/通知フィルタが一貫した関係状態に基づける。

#### Acceptance Criteria

1. The Social Graph Service shall フォロー・保留フォローリクエスト・ミュート・ブロック・被ブロックの関係状態を、本 spec が所有する単一の真実源として永続化する。
2. The Social Graph Service shall accounts-and-instance の関係状態委譲境界（`RelationshipStateProvider`）へ、閲覧者アクターと対象アカウント群に対する関係フラグを供給する実装を登録する。
3. When フォロー/ミュート/ブロック等の操作が成功し関係状態を返すとき, the Social Graph Service shall accounts-and-instance が所有する Relationship JSON 契約（`RelationshipView` / `RelationshipSerializer`）を消費して応答を生成し、Relationship 契約を再定義しない。
4. The Social Graph Service shall 閲覧者と対象の関係に対し、Relationship 契約が要求する全フラグ（`following` / `showing_reblogs` / `notifying` / `languages` / `followed_by` / `blocking` / `blocked_by` / `muting` / `muting_notifications` / `requested` / `requested_by` / `endorsed` / `note`）を、本 spec が保持する状態から決定論的に導出する。
5. Where `domain_blocking` フラグが要求される場合, the Social Graph Service shall ドメインブロック機能は本 spec の範囲外（後回し）であるため当該フラグを常に偽として供給する。

### Requirement 9: タイムライン・通知フィルタへの関係状態供給

**Objective:** timelines / notifications の実装者として、関係状態に基づくフィルタ判定の入力を得たい。これにより、ブロック/ミュートした相手の投稿・通知を各 spec 側で除外できる。

#### Acceptance Criteria

1. The Social Graph Service shall 閲覧者アクターを起点に、ブロック・被ブロック・ミュート・通知ミュートの対象アカウント集合を問い合わせ可能な形で公開する。
2. The Social Graph Service shall 閲覧者アクターのフォロー対象アカウント集合を、ホームタイムライン構成のために問い合わせ可能な形で公開する。
3. While ミュートに有効期限が設定され期限が経過している間, the Social Graph Service shall 当該ミュートをフィルタ対象集合に含めない。
4. The Social Graph Service shall タイムライン・通知のフィルタ適用処理そのものは実装せず、フィルタ判定に必要な関係状態の問い合わせ手段の提供に限定する。

### Requirement 10: 横断的な API 規律（認証・エラー・冪等・対称性検証）

**Objective:** クライアント開発者として、ソーシャルグラフ操作 API が他の Mastodon 互換 API と一貫した振る舞いをすることを期待する。これにより、既存クライアントが追加実装なく利用できる。

#### Acceptance Criteria

1. The Social Graph Service shall follow / unfollow / mute / unmute / block / unblock / follow_requests 操作に対し、api-foundation の Bearer 認証とスコープ（`follow` または `write:follows` / `read:follows` 相当）の内包判定を適用する。
2. The Social Graph Service shall 操作失敗時に api-foundation の Mastodon 互換エラー本文（`error` / 必要時 `error_description`）と対応する HTTP ステータスを返す。
3. While 関係変更操作が連合配送を伴う間, the Social Graph Service shall ローカル宛配送（in-process）とリモート宛配送（HTTP）で同一の Activity を生成し、同一の関係状態遷移結果になることを保証する。
4. The Social Graph Service shall follow_requests 一覧などのリスト系応答に api-foundation のページネーション（`Link` ヘッダ・カーソル）を適用する。
5. If 対象アカウント識別子が存在しないアカウントを指すとき, then the Social Graph Service shall Mastodon 互換の未検出応答（404 相当）を返す。
