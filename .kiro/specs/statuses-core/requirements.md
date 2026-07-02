# Requirements Document

## Introduction

statuses-core は kawasemi の Mastodon 互換 API における **投稿（Status）のコア状態モデルと API** を確立する spec である。投稿はタイムライン・通知・投票・ブックマーク等ほぼ全機能が依存するハブであり、本プロジェクト最大の工数ブロックである。本 spec は、投稿の作成・取得・削除・編集（履歴/source 含む）・スレッド context、ブースト（reblog）・お気に入り（favourite）・ブックマーク（bookmark）・ピン留め（pin）、投票（Poll）、投稿時の冪等性（`Idempotency-Key`）、そして Status / Poll エンティティの JSON 契約を所有する。

最重要の制約は「**可視性 / addressing がローカル最適化パスとリモート連合パスで同一結果になる**」ことである。投稿の生成・可視性判定・状態遷移は federation-core が確立した共通コードパス（`DeliveryService`・`InboundActivityHandler` レジストリ）を必ず通し、分岐させるのは物理配送手段のみとする。ローカルでは顕在化せずリモート連合でだけ壊れるバグを構造的に排除し、それをテストで担保する。

本 spec が完了すると、標準クライアント（Ivory・Elk・Phanpy 等）から Mastodon 互換の投稿操作が一通り行え、Status / Poll の JSON 契約が api-foundation の契約ハーネス上でゴールデン固定され、連合往復（2 インスタンス）で投稿関連 Activity（Create / Announce / Like / Delete / Update）の意味論がローカル/リモートで同値に振る舞う状態になる。タイムライン集約・通知生成・引用/絵文字リアクションの方言正規化・検索は本 spec のスコープ外であり、それぞれ下流 spec に委ねる。

## Boundary Context

- **In scope**: 投稿の作成/取得/削除/編集（編集履歴・source 取得を含む）/スレッド context、ブースト（reblog/unreblog）、お気に入り（favourite/unfavourite）、ブックマーク（bookmark/unbookmark）、ピン留め（pin/unpin）、投票（Poll の作成と投票）、投稿時の冪等性（`Idempotency-Key`）、可視性と addressing の共通パス（ローカル/リモート同一結果）、投稿関連の受信 Activity（Create/Note・Announce・Like・Delete・Update）の意味論処理をディスパッチ境界へ登録すること、Status / Poll エンティティの JSON 契約（ゴールデン固定）。
- **Out of scope**: タイムライン集約（home/public/local/tag は timelines）、通知の生成（notifications）、引用投稿・絵文字リアクション・MFM 等の連合方言の正規化（custom-federation）、検索（search）、フォロー/ブロック/ミュート等の関係操作（social-graph）、Account / Instance エンティティ契約そのもの（accounts-and-instance。本 spec は Account を埋め込み参照するのみ）、OAuth・ページネーション・エラー/レート制限・契約ハーネス基盤（api-foundation。本 spec は適用のみ）、HTTP Signatures・配送キュー・受信パイプライン・WebFinger 等の連合配管（federation-core。本 spec は配送依頼とハンドラ登録のみ）、メディアの非同期処理・MediaAttachment 契約そのもの（media-pipeline。本 spec はメディア識別子で添付するのみ）。
- **Adjacent expectations**: 本 spec は api-foundation の Bearer 認証・スコープ（`read:statuses` / `write:statuses` / `write:favourites` / `write:bookmarks` / `read:bookmarks` 等）・ページネーション規約・Mastodon 互換エラー本文・`X-RateLimit-*`・契約テストハーネスに依存する。federation-core の配送共通パス（`DeliveryService`）・受信ディスパッチ境界（`InboundActivityHandler` レジストリ）・「意味論は対称・物理配送のみ最適化」境界に依存する。media-pipeline の MediaAttachment 契約とメディア識別子に依存する。accounts-and-instance の Account 契約に依存し、Status に投稿者 Account を埋め込む。下流の timelines / notifications / search / custom-federation は本 spec の Status / Poll 契約と状態モデルに依存する。

## Requirements

### Requirement 1: Status エンティティ JSON 契約

**Objective:** クライアント実装者として、投稿の JSON 表現が Mastodon 互換でゴールデン固定されていてほしい。これにより、出力ドリフトなく既存クライアントが投稿を正しく表示できる。

#### Acceptance Criteria

1. When 投稿を JSON にシリアライズするとき, the Statuses Core shall Mastodon 互換の Status フィールド（少なくとも `id` / `uri` / `url` / `account` / `content` / `created_at` / `visibility` / `sensitive` / `spoiler_text` / `media_attachments` / `mentions` / `tags` / `emojis` / `reblogs_count` / `favourites_count` / `replies_count` / `in_reply_to_id` / `in_reply_to_account_id` / `reblog` / `poll` / `language` / `edited_at`）を出力する。
2. When 認証済みアクター文脈で投稿を返すとき, the Statuses Core shall 当該アクターの操作状態（`favourited` / `reblogged` / `bookmarked` / `pinned` / `muted`）を投稿に反映する。
3. When ブースト投稿（reblog）をシリアライズするとき, the Statuses Core shall `reblog` フィールドに被ブースト元投稿をネストし、ブースト側の本文系フィールドを Mastodon 互換の規律で表現する。
4. The Statuses Core shall Status JSON 契約を api-foundation の契約テストハーネスにゴールデンとして登録し、決定的な非決定性境界（時刻・ID）の下で再現可能にする。
5. While Status をシリアライズする間, the Statuses Core shall null 規律（例: `poll` 無しは `null`、`in_reply_to_id` 無しは `null`、`edited_at` 未編集は `null`）を Mastodon 実レスポンスに合わせて一貫して維持する。
6. The Statuses Core shall コア Status 契約に連合方言フィールド（引用・絵文字リアクション等）を含めない。

### Requirement 2: Poll エンティティ JSON 契約

**Objective:** クライアント実装者として、投票の JSON 表現が Mastodon 互換でゴールデン固定されていてほしい。これにより、投票 UI が正しく描画・集計表示できる。

#### Acceptance Criteria

1. When 投票を JSON にシリアライズするとき, the Statuses Core shall Mastodon 互換の Poll フィールド（少なくとも `id` / `expires_at` / `expired` / `multiple` / `votes_count` / `voters_count` / `options`（`title` と `votes_count`）/ `emojis`）を出力する。
2. When 認証済みアクター文脈で投票を返すとき, the Statuses Core shall 当該アクターの投票状態（`voted` / `own_votes`）を反映する。
3. While 投票が締切前である間, the Statuses Core shall `expired` を `false` とし、締切時刻到来後は `expired` を `true` とする。
4. The Statuses Core shall Poll JSON 契約を api-foundation の契約テストハーネスにゴールデンとして登録し、決定的に再現可能にする。

### Requirement 3: 投稿の作成

**Objective:** 標準クライアントのユーザーとして、本文・警告（CW）・メディア・言語・返信先を指定して投稿を作成したい。これにより、Mastodon 同等の表現で発言できる。

#### Acceptance Criteria

1. When 認証済みアクターが投稿作成（本文または添付メディアの少なくとも一方を含む）を要求したとき, the Statuses Core shall `write:statuses` スコープを検証したうえで投稿を作成し、作成された Status を返す。
2. If 投稿作成要求が本文・メディア・投票のいずれも欠き内容が空であるとき, then the Statuses Core shall Mastodon 互換のエラー応答で要求を拒否する。
3. When 投稿作成要求が閲覧注意（`spoiler_text`）と sensitive 指定を含むとき, the Statuses Core shall それらを投稿に反映する。
4. When 投稿作成要求がメディア識別子を含むとき, the Statuses Core shall 当該メディアが要求アクターの所有であることを検証して添付し、非所有・未存在のメディア識別子を含む要求を拒否する。
5. When 投稿作成要求が返信先投稿 ID を含むとき, the Statuses Core shall 返信関係（`in_reply_to_id` / `in_reply_to_account_id`）を確立する。
6. When 投稿作成要求が言語コードを含むとき, the Statuses Core shall 言語を投稿に記録し、`content` から本文中の言及・ハッシュタグ・カスタム絵文字ショートコードを抽出して Status の `mentions` / `tags` / `emojis` に反映する。

### Requirement 4: 可視性と addressing の共通パス

**Objective:** 連合運用者として、投稿の可視性と宛先（addressing）がローカル宛とリモート宛で同一に決まってほしい。これにより、ローカルでは動くがリモートで壊れる最重要リスクを排除できる。

#### Acceptance Criteria

1. The Statuses Core shall 投稿の可視性（`public` / `unlisted` / `private` / `direct`）を単一の判定ロジックで決定し、ローカル宛とリモート宛で同一の判定を適用する。
2. When 投稿を作成・配送するとき, the Statuses Core shall 可視性から `to` / `cc`（公開アドレッシング・フォロワーコレクション・メンション宛先）を導出する単一の addressing ロジックを通し、その結果を federation-core の配送共通パスへ渡す。
3. When 投稿を配送するとき, the Statuses Core shall 論理的に同一の正規 Activity を生成・検証してから、配送手段（ローカル in-process / リモート HTTP）のみを federation-core に分岐させる。
4. While 投稿の宛先にローカル受信者とリモート受信者が混在する間, the Statuses Core shall 双方に対して同一の意味論（可視性・状態遷移）を適用し、物理配送手段だけを違える。
5. The Statuses Core shall ローカル最適化パス（in-process 配送）と HTTP 連合パスが投稿関連の業務処理について同一結果になることを連合テスト（2 インスタンス往復）で検証可能にする。

### Requirement 5: 投稿時の冪等性

**Objective:** 標準クライアントのユーザーとして、ネットワーク再送時に投稿が二重作成されないようにしたい。これにより、再試行が安全になる。

#### Acceptance Criteria

1. When 投稿作成要求が冪等キー（`Idempotency-Key`）を伴うとき, the Statuses Core shall 同一アクター・同一キーの最初の要求で投稿を作成し、そのキーと作成結果を関連付けて記録する。
2. When 同一アクターが同一冪等キーで投稿作成を再要求したとき, the Statuses Core shall 新規投稿を作成せず、最初に作成された投稿を同一の応答として返す。
3. Where 投稿作成要求が冪等キーを伴わない場合, the Statuses Core shall 通常どおり毎回新規投稿を作成する。

### Requirement 6: 投稿の取得とスレッド context

**Objective:** 標準クライアントのユーザーとして、単一投稿とそのスレッド（祖先・子孫）を取得したい。これにより、会話の文脈を表示できる。

#### Acceptance Criteria

1. When クライアントが投稿 ID で取得を要求したとき, the Statuses Core shall 要求アクターから可視な投稿のみを返し、不可視・未存在の投稿に対しては未検出を返す。
2. When クライアントが投稿の context を要求したとき, the Statuses Core shall 当該投稿の祖先（ancestors）と子孫（descendants）を返信関係に基づいて返す。
3. While context を構成する間, the Statuses Core shall 要求アクターから不可視の投稿を祖先・子孫から除外する。
4. Where 取得が認証されていない場合, the Statuses Core shall 公開可視性の投稿のみを返す。

### Requirement 7: 投稿の削除

**Objective:** 標準クライアントのユーザーとして、自分の投稿を削除したい。これにより、誤投稿や不要な投稿を取り消せる。

#### Acceptance Criteria

1. When 認証済みアクターが自身の投稿の削除を要求したとき, the Statuses Core shall `write:statuses` スコープを検証して投稿を削除し、Mastodon 互換の削除応答（削除された投稿の表現）を返す。
2. If 削除要求の対象が要求アクターの所有でないとき, then the Statuses Core shall 削除を行わず権限エラーまたは未検出を返す。
3. When 投稿が削除されたとき, the Statuses Core shall 削除を表す正規 Activity（Delete）を可視性に応じた宛先へ配送共通パス経由で配送する。
4. When 投稿が削除されたとき, the Statuses Core shall 当該投稿に対するブースト・お気に入り・ブックマーク・返信参照の整合を保つように関連状態を処理する。

### Requirement 8: 投稿の編集・編集履歴・source

**Objective:** 標準クライアントのユーザーとして、投稿を編集し、その編集履歴と編集用ソースを参照したい。これにより、誤りの訂正と編集の透明性を両立できる。

#### Acceptance Criteria

1. When 認証済みアクターが自身の投稿の編集（本文・CW・sensitive・メディア・言語の変更）を要求したとき, the Statuses Core shall `write:statuses` スコープを検証して投稿を更新し、`edited_at` を更新時刻に設定した Status を返す。
2. When 投稿が編集されたとき, the Statuses Core shall 編集前後の内容を編集履歴として保持し、履歴取得要求に対して各版（本文・CW・sensitive・メディア・作成/編集時刻）を返す。
3. When クライアントが投稿の編集用ソース取得を要求したとき, the Statuses Core shall 編集に適した素の本文（`text`）と `spoiler_text` を返す。
4. When 投稿が編集されたとき, the Statuses Core shall 編集を表す正規 Activity（Update）を可視性に応じた宛先へ配送共通パス経由で配送する。
5. If 編集要求の対象が要求アクターの所有でないとき, then the Statuses Core shall 編集を行わず権限エラーまたは未検出を返す。

### Requirement 9: ブースト（reblog）

**Objective:** 標準クライアントのユーザーとして、投稿をブースト/ブースト解除したい。これにより、他者の投稿を自分のフォロワーへ広められる。

#### Acceptance Criteria

1. When 認証済みアクターが可視な投稿のブーストを要求したとき, the Statuses Core shall `write:statuses` スコープを検証してブーストを表す投稿を作成し、被ブースト元をネストした Status を返す。
2. When ブーストが作成されたとき, the Statuses Core shall ブーストを表す正規 Activity（Announce）を可視性に応じた宛先へ配送共通パス経由で配送し、被ブースト元投稿の `reblogs_count` を更新する。
3. When 同一アクターが既にブースト済みの投稿を再びブースト要求したとき, the Statuses Core shall 重複したブーストを作成しない。
4. When 認証済みアクターがブースト解除を要求したとき, the Statuses Core shall ブーストを取り消し、取り消しを表す正規 Activity（Undo Announce）を配送共通パス経由で配送し、`reblogs_count` を更新する。
5. If ブースト対象が要求アクターから不可視であるとき, then the Statuses Core shall ブーストを拒否する。

### Requirement 10: お気に入り（favourite）

**Objective:** 標準クライアントのユーザーとして、投稿をお気に入り登録/解除したい。これにより、賛意を示し後から振り返れる。

#### Acceptance Criteria

1. When 認証済みアクターが可視な投稿のお気に入り登録を要求したとき, the Statuses Core shall `write:favourites` スコープを検証してお気に入りを記録し、`favourited=true` を反映した Status を返す。
2. When お気に入りが登録されたとき, the Statuses Core shall お気に入りを表す正規 Activity（Like）を被お気に入り元アクターへ配送共通パス経由で配送し、`favourites_count` を更新する。
3. When 認証済みアクターがお気に入り解除を要求したとき, the Statuses Core shall お気に入りを取り消し、取り消しを表す正規 Activity（Undo Like）を配送共通パス経由で配送し、`favourites_count` を更新する。
4. When 同一アクターが既にお気に入り済みの投稿を再び登録要求したとき, the Statuses Core shall 重複したお気に入りを作成しない。

### Requirement 11: ブックマーク（bookmark）

**Objective:** 標準クライアントのユーザーとして、投稿をブックマーク登録/解除し一覧で参照したい。これにより、私的に投稿を保存できる。

#### Acceptance Criteria

1. When 認証済みアクターが可視な投稿のブックマーク登録を要求したとき, the Statuses Core shall `write:bookmarks` スコープを検証してブックマークを記録し、`bookmarked=true` を反映した Status を返す。
2. When 認証済みアクターがブックマーク解除を要求したとき, the Statuses Core shall ブックマークを取り消し、`bookmarked=false` を反映した Status を返す。
3. When 認証済みアクターがブックマーク一覧を要求したとき, the Statuses Core shall `read:bookmarks` スコープを検証して当該アクターのブックマーク投稿を、api-foundation のページネーション規約（ブックマーク固有カーソル）で返す。
4. The Statuses Core shall ブックマークをローカルのみの状態として扱い、連合 Activity を配送しない。

### Requirement 12: ピン留め（pin）

**Objective:** 標準クライアントのユーザーとして、自分の投稿をプロフィールにピン留め/解除したい。これにより、強調したい投稿を上部に固定できる。

#### Acceptance Criteria

1. When 認証済みアクターが自身の投稿のピン留めを要求したとき, the Statuses Core shall `write:statuses` スコープを検証してピン状態を記録し、`pinned=true` を反映した Status を返す。
2. When 認証済みアクターがピン解除を要求したとき, the Statuses Core shall ピン状態を取り消し、`pinned=false` を反映した Status を返す。
3. If ピン留め対象が要求アクターの所有でないとき, then the Statuses Core shall ピン留めを拒否する。
4. If ピン留め対象が直接（`direct`）可視性の投稿であるとき, then the Statuses Core shall ピン留めを拒否する。

### Requirement 13: 投票（Poll）

**Objective:** 標準クライアントのユーザーとして、投票付き投稿を作成し、他者の投票へ参加したい。これにより、フォロワーの意見を集約できる。

#### Acceptance Criteria

1. When 投稿作成要求が投票（選択肢・締切・単一/複数選択）を含むとき, the Statuses Core shall 投票を作成して投稿に紐づけ、メディア添付との同時指定を拒否する。
2. When 認証済みアクターが締切前の可視な投票に対し有効な選択肢で投票したとき, the Statuses Core shall `write:statuses` スコープを検証して投票を記録し、更新後の集計を反映した Poll を返す。
3. If 締切を過ぎた投票へ投票要求があったとき, then the Statuses Core shall 投票を拒否する。
4. If 単一選択の投票へ複数選択肢が指定されたとき、または範囲外の選択肢インデックスが指定されたとき, then the Statuses Core shall 投票を拒否する。
5. When 同一アクターが既に投票済みの投票へ再投票を要求したとき, the Statuses Core shall 重複した投票を記録しない。
6. When 投票が記録されたとき, the Statuses Core shall 投票を表す正規 Activity を投票対象元アクターへ配送共通パス経由で配送する。

### Requirement 14: 投稿関連の受信 Activity 処理

**Objective:** 連合運用者として、リモートからの投稿関連 Activity が正しく取り込まれてほしい。これにより、リモートの投稿・ブースト・お気に入り・削除・編集がローカルに反映される。

#### Acceptance Criteria

1. The Statuses Core shall 投稿関連の Activity 種別（Create/Note・Announce・Like・Delete・Update と対応する Undo）の意味論処理を federation-core の受信ディスパッチ境界へハンドラとして登録する。
2. When リモートから Create（Note）を受信したとき, the Statuses Core shall リモート投稿をローカルの Status モデルへ取り込み、返信・可視性・添付・メンションを反映する。
3. When リモートから Announce / Like を受信したとき, the Statuses Core shall 対象ローカル投稿のブースト/お気に入り状態と集計を更新する。
4. When リモートから Delete / Update を受信したとき, the Statuses Core shall 対象投稿の削除/編集を反映する。
5. While 受信 Activity を意味論処理する間, the Statuses Core shall ローカル発生時と同一の状態遷移ロジック（共通コードパス）を適用する。

### Requirement 15: 連合方言の隔離境界

**Objective:** アーキテクトとして、引用・絵文字リアクション等の連合方言をコア状態モデルから隔離したい。これにより、方言の乱立がコアを汚染するのを防げる。

#### Acceptance Criteria

1. The Statuses Core shall コアの投稿状態モデルに連合方言（引用関係・絵文字リアクション集計等）を保持しない。
2. Where 受信 Activity が未知の方言プロパティを含む場合, the Statuses Core shall それらを意味論として解釈せずコア処理を継続する。
3. The Statuses Core shall 方言の正規化・出し分けを custom-federation の境界に委ねられるよう、コア状態モデルとディスパッチ登録を方言非依存に保つ。
