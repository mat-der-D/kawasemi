# Requirements Document

## Introduction

accounts-and-instance は、標準クライアント（Ivory・Elk・Phanpy 等）がログイン直後に最初に参照する「アカウント・インスタンス情報・カスタム絵文字」の Mastodon 互換 API を提供する spec である。これらの JSON 契約がずれると、プロフィール表示・自分自身の判定・絵文字描画・インスタンス機能ネゴシエーションといったクライアント基礎動作が軒並み破綻する。したがって本 spec は、Account / CredentialAccount / Relationship / Instance(v2) / CustomEmoji の各エンティティ JSON 契約を、実装に先んじてゴールデンテストで固定することを最重要の責務とする。

本 spec が完了すると、(1) `GET /api/v1/accounts/verify_credentials`・`GET /api/v1/accounts/:id`・`GET /api/v1/accounts/:id/statuses`・`GET /api/v1/accounts/relationships`・`PATCH /api/v1/accounts/update_credentials`、(2) `GET /api/v2/instance`、(3) `GET /api/v1/custom_emojis` が Mastodon 互換で動作し、ローカルアクター（actor-model）とリモートアカウント（federation-core 経由でフェッチ・正規化）が同一の Account 契約で表現され、アバター/ヘッダは media-pipeline のメディアとして提供され、instance v2 が運用設定（DB 保存値）を反映する状態になる。

最重要の設計上の論点は (a) ローカル/リモートのアカウントを単一 Account 契約へ統一すること、(b) 本 spec が所有しない情報（投稿本体・関係状態）を、所有 spec が後から供給できる「委譲境界」を通して取り込みつつ、エンティティ契約と API 表層は本 spec が所有することである。`accounts/:id/statuses` の Status 本体は statuses-core が、`relationships` の関係状態は social-graph が所有するが、いずれの spec も本 spec に依存する（依存方向は下流→本 spec）。このため本 spec はそれらを委譲境界の背後から読み取り、既定では空/関係なしを返す。

## Boundary Context

- **In scope**: Account / CredentialAccount / Relationship / Instance(v2) / CustomEmoji の JSON 契約とシリアライズ、`accounts/verify_credentials`・`accounts/:id`・`accounts/:id/statuses`・`accounts/relationships`・`accounts/update_credentials` の各エンドポイント、`instance`(v2) エンドポイントと運用設定（DB 保存値）の反映、`custom_emojis`(read) エンドポイントとカスタム絵文字の読み取りモデル、ローカルアクターの Account シリアライズ、リモートアカウントのフェッチ/正規化と Account 形への変換・キャッシュ、ローカルアカウントのプロフィール拡張（アバター/ヘッダ/フィールド/ロック/BOT/公開範囲既定等）の保持と更新、上記契約のゴールデンテスト（api-foundation 契約ハーネスへの登録）。
- **Out of scope**: フォロー/ブロック/ミュート等の関係**変更**操作（social-graph。relationships の**読み**のみ本 spec）、投稿（Status）本体の取得・CRUD・シリアライズ（statuses-core。`accounts/:id/statuses` のルート/ページネーション/可視性フィルタは本 spec、Status 表現は statuses-core）、カスタム絵文字の**管理/アップロード/連合取り込み**（custom-federation / admin-frontend。**読み**のみ本 spec）、運用設定の**書き込み/管理画面**（admin-frontend。本 spec は読み取りと初期既定のみ）、`familiar_followers`・プロフィールディレクトリ・アカウント移転の送信（後回し/非目標）。
- **Adjacent expectations**: 本 spec は api-foundation（Bearer 認証・スコープ内包判定・Mastodon 互換エラー本文・`X-RateLimit-*`・ページネーション規約・プロキシ尊重 URL・契約テストハーネス）、federation-core（アクター解決・WebFinger・公開鍵/リモート取得のための `FederationHttpClient` / `ActivityPubDocumentBuilder` 等の連合参照）、media-pipeline（MediaAttachment 契約・`MediaStore`/公開 URL 生成）、actor-model（`ActorDirectory` のハンドル解決・`ResolvedActor`）、core-runtime（`AppState` / `RuntimeContext` / `PgPool` / `AppError` / マイグレーション基盤 / テストハーネス）に依存する。core-runtime は運用設定（DB 保存値）のスキーマ/読み書きを所有せず後続 spec に委ねているため、本 spec が instance v2 のための運用設定読み取りモデルと初期既定を所有する。下流の statuses-core は `accounts/:id/statuses` の Status 供給を、social-graph は relationships の関係状態供給を、本 spec の委譲境界へ実装登録する。

## Requirements

### Requirement 1: Account エンティティ JSON 契約（ローカル/リモート統一）

**Objective:** クライアント開発者として、ローカルアカウントとリモートアカウントが同一の Account JSON 契約で表現されることを保証したい。これにより、出自を意識せずプロフィールを描画できる。

#### Acceptance Criteria

1. The Accounts Service shall Mastodon 互換の Account エンティティとして、少なくとも `id` / `username` / `acct` / `display_name` / `locked` / `bot` / `discoverable` / `group` / `created_at` / `note` / `url` / `uri` / `avatar` / `avatar_static` / `header` / `header_static` / `followers_count` / `following_count` / `statuses_count` / `last_status_at` / `emojis` / `fields` の各フィールドを含む JSON を生成する。
2. When ローカルアクターを Account として表現するとき, the Accounts Service shall `acct` をローカルハンドル（ドメイン部なし）として出力し、`url` / `uri` を当該ローカルアクターの公開 URL として出力する。
3. When リモートアカウントを Account として表現するとき, the Accounts Service shall `acct` を `username@domain` 形式で出力し、リモート由来の正規化済み値を用いる。
4. Where アカウントの `display_name` または `note` がカスタム絵文字のショートコードを含む場合, the Accounts Service shall 該当するカスタム絵文字を `emojis` 配列として併せて出力する。
5. While アバターまたはヘッダ画像が設定されていない間, the Accounts Service shall それぞれに既定（デフォルト）の画像 URL を出力し、`avatar` / `header` を null にしない。
6. The Accounts Service shall Account JSON のフィールド有無・型・null 規律を、api-foundation の契約テストハーネスにゴールデンとして登録し固定する。

### Requirement 2: 認証アカウントの取得（verify_credentials）

**Objective:** 標準クライアントとして、ログイン中のアクター自身のアカウント情報を取得したい。これにより、自分自身の判定・編集元データの取得・公開範囲既定の表示ができる。

#### Acceptance Criteria

1. When 認証済みリクエストが `verify_credentials` を要求したとき, the Accounts Service shall Bearer トークンに結びついた単一アクターを CredentialAccount として返す。
2. The Accounts Service shall CredentialAccount を、Account の全フィールドに加えて `source`（少なくとも `privacy` / `sensitive` / `language` / `note` / `fields` / `follow_requests_count`）と `role` を含む形で返す。
3. If リクエストが有効な Bearer トークンを欠く、または要求スコープ（`read:accounts` 相当）を満たさないとき, then the Accounts Service shall api-foundation の Mastodon 互換エラー応答（401 または 403 相当）を返す。
4. The Accounts Service shall CredentialAccount JSON 契約を契約テストハーネスにゴールデンとして登録し固定する。

### Requirement 3: 任意アカウントの取得（accounts/:id）

**Objective:** 標準クライアントとして、識別子を指定して任意のアカウント（ローカル/リモート）を取得したい。これにより、プロフィール画面を表示できる。

#### Acceptance Criteria

1. When 指定された識別子がローカルアクターを指すとき, the Accounts Service shall そのローカルアクターを Account として返す。
2. When 指定された識別子が既知のリモートアカウントを指すとき, the Accounts Service shall 正規化済みのリモートアカウントを Account として返す。
3. If 指定された識別子に対応するアカウントが存在しないとき, then the Accounts Service shall 未検出（404 相当）の Mastodon 互換エラー応答を返す。
4. Where エンドポイントが認証を任意とする場合, the Accounts Service shall トークン未提示でも公開情報としての Account を返せるようにする。

### Requirement 4: アカウント投稿一覧（accounts/:id/statuses）

**Objective:** 標準クライアントとして、特定アカウントの投稿一覧をページネーション付きで取得したい。これにより、プロフィール上のタイムラインを表示できる。

#### Acceptance Criteria

1. When `accounts/:id/statuses` が要求されたとき, the Accounts Service shall 対象アカウントに属する投稿の一覧を、api-foundation のページネーション規約（`Link` ヘッダ + `max_id`/`since_id`/`min_id`/`limit`）に従って返す。
2. The Accounts Service shall 投稿一覧の取得とアカウント所属の解決を本 spec で所有し、各 Status エンティティの表現は statuses-core が供給する委譲境界を通じて取得する。
3. While statuses-core による投稿供給が未登録の間, the Accounts Service shall 当該一覧を空の結果として返し、エンドポイント自体は正常に応答する。
4. Where リクエストが投稿の絞り込み（例: `pinned` / `only_media` / `exclude_replies` / `exclude_reblogs`）を指定する場合, the Accounts Service shall その絞り込み条件を委譲境界へ伝達し、結果に反映できるようにする。
5. While リクエストが認証コンテキストを持つ間, the Accounts Service shall 閲覧者から見て可視な投稿のみが返るよう、可視性判定を委譲境界の供給に委ねる。

### Requirement 5: 関係性の読み取り（relationships）

**Objective:** 標準クライアントとして、自分と複数アカウントとの関係状態をまとめて取得したい。これにより、フォロー/ブロック/ミュート等のボタン状態を表示できる。

#### Acceptance Criteria

1. When 認証済みリクエストが 1 つ以上のアカウント識別子で `relationships` を要求したとき, the Accounts Service shall 各識別子に対応する Relationship エンティティの配列を返す。
2. The Accounts Service shall Relationship エンティティとして、少なくとも `id` / `following` / `showing_reblogs` / `notifying` / `languages` / `followed_by` / `blocking` / `blocked_by` / `muting` / `muting_notifications` / `requested` / `requested_by` / `domain_blocking` / `endorsed` / `note` の各フィールドを含める。
3. The Accounts Service shall 関係状態そのものを所有せず、social-graph が供給する委譲境界から各関係フラグを取得する。
4. While 関係状態の供給が未登録の間, the Accounts Service shall すべての関係フラグを「関係なし」（真偽値は false、件数は 0、`note` は空）として Relationship を返す。
5. If リクエストが有効な Bearer トークンを欠く、または要求スコープ（`read:follows` 相当）を満たさないとき, then the Accounts Service shall Mastodon 互換エラー応答（401 または 403 相当）を返す。
6. The Accounts Service shall Relationship JSON 契約を契約テストハーネスにゴールデンとして登録し固定する。

### Requirement 6: プロフィール更新（update_credentials）

**Objective:** 一人鯖のオーナーとして、ログイン中のアクターの表示プロフィールを更新したい。これにより、表示名・自己紹介・アバター/ヘッダ・プロフィールフィールド・公開範囲既定などを変更できる。

#### Acceptance Criteria

1. When 認証済みリクエストがプロフィール項目（`display_name` / `note` / `locked` / `bot` / `discoverable` / `fields_attributes` / 公開範囲・既定言語・既定の `sensitive` などの source 項目）の更新を要求したとき, the Accounts Service shall 指定された項目のみを更新し、更新後の CredentialAccount を返す。
2. When 更新要求がアバターまたはヘッダ画像を含むとき, the Accounts Service shall その画像を media-pipeline 経由で取り込み、対象アクターのプロフィール画像として関連付ける。
3. If 更新項目が検証規約（例: フィールド数上限・各値の長さ上限・公開範囲の許容値・focus 範囲）に違反するとき, then the Accounts Service shall 更新を行わず、Mastodon 互換のエラー応答（422 相当）を返す。
4. If リクエストが有効な Bearer トークンを欠く、または要求スコープ（`write:accounts` 相当）を満たさないとき, then the Accounts Service shall Mastodon 互換エラー応答（401 または 403 相当）を返す。
5. The Accounts Service shall プロフィール更新の結果が、以降の `verify_credentials` および当該アクターの `accounts/:id` の応答に反映されるよう、ローカルアカウントのプロフィール拡張を永続化する。

### Requirement 7: リモートアカウントのフェッチ・正規化

**Objective:** 連合の利用者として、未取得のリモートアカウントを取得して Account 形に正規化したい。これにより、リモートユーザーのプロフィールをローカルと同一契約で表示できる。

#### Acceptance Criteria

1. When リモートアクターの Account 表現が必要で、かつローカルに正規化済みデータが無い、または陳腐化しているとき, the Accounts Service shall federation-core の連合取得境界を用いてリモートアクター文書を取得する。
2. When リモートアクター文書を取得したとき, the Accounts Service shall ActivityPub アクター表現を Mastodon Account 契約のフィールド（`acct` / `display_name` / `note` / `url` / `uri` / アバター/ヘッダ / `fields` / `bot` / `locked` 等）へ正規化して保持する。
3. While 正規化済みリモートアカウントが有効に保持されている間, the Accounts Service shall 再取得を行わず保持済みデータから Account を生成する。
4. If リモートアクター文書の取得または必須プロパティの解釈に失敗したとき, then the Accounts Service shall そのアカウントを生成せず、未検出または取得失敗を示す Mastodon 互換エラー応答を返す。
5. The Accounts Service shall リモートアカウントの正規化において、未知の拡張プロパティによって正規化処理を失敗させない。

### Requirement 8: インスタンス情報（instance v2）

**Objective:** 標準クライアントとして、サーバーのメタ情報と機能設定を取得したい。これにより、投稿可能文字数・メディア上限・登録可否などをネゴシエートできる。

#### Acceptance Criteria

1. When `GET /api/v2/instance` が要求されたとき, the Instance Service shall Mastodon 互換の Instance(v2) エンティティとして、少なくとも `domain` / `title` / `version` / `source_url` / `description` / `usage` / `thumbnail` / `languages` / `configuration` / `registrations` / `contact` / `rules` を含む JSON を返す。
2. The Instance Service shall `title` / `description` / `contact` / `rules` / `registrations` などの運用可変項目を、運用設定（DB 保存値）から読み取って反映する。
3. While 運用設定に値が未設定の項目がある間, the Instance Service shall その項目に安全な初期既定値を用いて応答する。
4. The Instance Service shall `configuration`（`statuses` / `media_attachments` / `polls` / `accounts` 等の上限・許容値）を、本サーバーの実際の制約と整合する値として返す。
5. The Instance Service shall Instance(v2) JSON 契約を契約テストハーネスにゴールデンとして登録し固定する。

### Requirement 9: カスタム絵文字の読み取り（custom_emojis）

**Objective:** 標準クライアントとして、サーバーで利用可能なカスタム絵文字一覧を取得したい。これにより、絵文字ピッカーと本文中ショートコードの描画ができる。

#### Acceptance Criteria

1. When `GET /api/v1/custom_emojis` が要求されたとき, the Custom Emoji Service shall ピッカーに表示可能なカスタム絵文字の一覧を CustomEmoji エンティティの配列として返す。
2. The Custom Emoji Service shall CustomEmoji エンティティとして、少なくとも `shortcode` / `url` / `static_url` / `visible_in_picker` / `category` の各フィールドを含める。
3. The Custom Emoji Service shall カスタム絵文字の**読み取り**のみを所有し、その登録・アップロード・連合取り込み・管理は本 spec で行わない。
4. Where Account の `emojis` 配列を構築する場合, the Custom Emoji Service shall `custom_emojis` と同一の読み取りモデル・同一の CustomEmoji 表現を用いる。
5. The Custom Emoji Service shall CustomEmoji JSON 契約を契約テストハーネスにゴールデンとして登録し固定する。

### Requirement 10: 横断規約の適用（認証・スコープ・エラー・ページネーション・レート制限）

**Objective:** API 実装者として、本 spec の全エンドポイントが api-foundation の横断規約に一貫して乗ることを保証したい。これにより、認証・エラー・ページネーション・レート制限の挙動がサーバー全体で統一される。

#### Acceptance Criteria

1. The Accounts Service shall 認証を要するエンドポイント（`verify_credentials` / `relationships` / `update_credentials`）に api-foundation の Bearer 認証ミドルウェアとスコープ内包判定を適用する。
2. When 認証任意または公開エンドポイント（`accounts/:id` / `accounts/:id/statuses` / `instance` / `custom_emojis`）が呼ばれたとき, the Accounts Service shall トークン未提示でも適切な公開応答を返す。
3. The Accounts Service shall すべてのエラー応答を、api-foundation の Mastodon 互換エラー本文（`error` / 任意 `error_description`）とステータス対応で返す。
4. While リスト系応答（`accounts/:id/statuses` / `relationships`）を返す間, the Accounts Service shall api-foundation のページネーション規約・プロキシ尊重の絶対 URL を用いる。
5. The Accounts Service shall 本 spec の応答に api-foundation の `X-RateLimit-*` 付与レイヤーが適用される装着点に乗せる。
