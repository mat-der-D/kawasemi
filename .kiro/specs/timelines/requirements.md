# Requirements Document

## Introduction

timelines は kawasemi の Mastodon 互換 API における **タイムライン集約レイヤー** を確立する spec である。投稿（statuses-core）と社会グラフ（social-graph）が確立していても、それらを時系列に集約して提供する点がなければ標準クライアント（Ivory・Elk・Phanpy 等）は何も表示できない。本 spec は home / public / local / tag の各タイムラインを、一貫したカーソルページネーションと可視性・ブロック/ミュートフィルタの下で提供する。

本 spec の最重要方針は「**自前で可視性・関係フィルタを再実装しない**」ことである。可視性判定は statuses-core の `VisibilityPolicy`、ブロック/被ブロック/ミュート/フォローの関係集合は social-graph の関係問い合わせ（`FilterQuery`）、ページネーション・認証・スコープ・エラー互換は api-foundation の規約を、いずれも上流の確立済み実装をそのまま消費する。Status エンティティの JSON 契約も statuses-core が所有するものを消費し、本 spec では再定義しない。

もう一つの方針は「**タイムライン membership（ある投稿が、ある閲覧者の、あるタイムラインに属するか）の判定を単一の点に集約する**」ことである。これは後段の Streaming spec が REST タイムラインと二重発火せず同一ロジックを再利用するための前提（共有シーム）であり、本 spec が確立する。

本 spec が完了すると、home / public / local / tag タイムラインが Mastodon 互換で動作し、フォロー関係・ブロック/ミュート・可視性が正しく反映され、`Link` + `max_id`/`since_id`/`min_id` のページネーションが全タイムラインで一貫し、Streaming が再利用できる単一の生成/クエリ点が存在する状態になる。list タイムライン・Streaming 配信そのもの・filters の `filtered` 適用は本 spec のスコープ外であり、それぞれ下流 spec に委ねる。

## Boundary Context

- **In scope**: home / public / local / tag（ハッシュタグ）タイムラインの提供、各タイムラインへの可視性フィルタ（statuses-core `VisibilityPolicy` の再利用）とブロック/ミュートフィルタ（social-graph 関係問い合わせの再利用）の適用、`Link` + `max_id`/`since_id`/`min_id`/`limit` の一貫したカーソルページネーション（api-foundation 規約の再利用）、Streaming が再利用するタイムライン membership 判定の単一生成点の確立、Status JSON 契約（statuses-core 所有）の消費による応答シリアライズ。
- **Out of scope**: list タイムライン（experience-expansion）、リアルタイム配信そのもの（WebSocket Streaming 配信は streaming）、filters の `filtered` フラグ適用・キーワードフィルタ（later / experience-expansion）、Status / Poll / Account エンティティの JSON 契約定義（statuses-core / accounts-and-instance）、可視性判定ロジックの定義（statuses-core）、フォロー/ブロック/ミュートの関係状態の所有・書き込み（social-graph）、ハッシュタグの抽出・正規化（statuses-core が投稿作成時に抽出）、検索・trends・featured_tags（search / 別 spec）、OAuth・ページネーション規約・エラー/レート制限・契約ハーネスの基盤（api-foundation）。
- **Adjacent expectations**: 本 spec は statuses-core の Status エンティティ契約・`VisibilityPolicy`（可視性判定）・投稿永続データ（投稿者・可視性・ハッシュタグ関連・ブースト/返信関係）の読み取り、social-graph の関係問い合わせ（ブロック/被ブロック/ミュートの集合、フォロー集合、ブーストの表示可否）、api-foundation の Bearer 認証・`read:statuses` スコープ・ページネーション規約・Mastodon 互換エラー本文・`X-RateLimit-*` に依存する。下流の streaming は本 spec が確立する単一生成点（タイムライン membership 判定）を再利用し、experience-expansion は list タイムラインを本 spec のタイムライン基盤の上に追加する。

## Requirements

### Requirement 1: ホームタイムライン

**Objective:** 標準クライアントのユーザーとして、自分がフォローしているアカウントと自分自身の投稿を時系列で見たい。これにより、フォロー中の話題を一覧で追える。

#### Acceptance Criteria

1. When 認証済みアクターがホームタイムラインを要求したとき, the Timelines shall `read:statuses` スコープを検証したうえで、当該アクターがフォローしているアカウントおよび当該アクター自身の投稿（ならびにフォロー中アカウントによるブースト）を新しい順に集約して返す。
2. While ホームタイムラインを集約する間, the Timelines shall 当該閲覧者から不可視の投稿（statuses-core `VisibilityPolicy` の判定で不可視となるもの。例: 非フォロー相手のフォロワー限定投稿）を結果から除外する。
3. The Timelines shall ダイレクト（`direct`）可視性の投稿をホームタイムラインに含めない。
4. When フォロー中アカウントによるブーストを含むとき, the Timelines shall 当該フォロー関係でブースト表示が無効化されているアカウントのブーストをホームタイムラインから除外する。
5. While ホームタイムラインを集約する間, the Timelines shall ブロック・被ブロック・ミュート関係にあるアカウントが投稿者またはブースト実行者である投稿を結果から除外する。
6. If 認証されていない要求でホームタイムラインが要求されたとき, then the Timelines shall Mastodon 互換のエラー応答（認証要求）で要求を拒否する。

### Requirement 2: 公開（連合）タイムライン

**Objective:** 標準クライアントのユーザーとして、サーバーが観測している全アカウントの公開投稿を時系列で見たい。これにより、ローカル外も含む広い話題を発見できる。

#### Acceptance Criteria

1. When 公開タイムラインが要求されたとき, the Timelines shall 公開（`public`）可視性の投稿（ローカル・リモート双方）を新しい順に集約して返す。
2. The Timelines shall 公開タイムラインからブースト（reblog）を除外し、元投稿のみを含める。
3. Where 公開タイムライン要求が `local` 指定を含む場合, the Timelines shall ローカルアカウントの公開投稿のみに結果を限定する。
4. Where 公開タイムライン要求が `remote` 指定を含む場合, the Timelines shall リモートアカウントの公開投稿のみに結果を限定する。
5. Where 公開タイムライン要求が `only_media` 指定を含む場合, the Timelines shall メディア添付を持つ投稿のみに結果を限定する。
6. While 認証済みアクターが公開タイムラインを要求する間, the Timelines shall ブロック・被ブロック・ミュート関係にあるアカウントの投稿を結果から除外する。

### Requirement 3: ローカルタイムライン

**Objective:** 標準クライアントのユーザーとして、自サーバーのローカルアカウントの公開投稿だけを時系列で見たい。これにより、サーバー内の話題に集中できる。

#### Acceptance Criteria

1. When ローカルタイムラインが要求されたとき, the Timelines shall ローカルアカウントの公開（`public`）可視性の投稿のみを新しい順に集約して返す。
2. The Timelines shall ローカルタイムラインからブースト（reblog）を除外し、ローカル発の元投稿のみを含める。
3. While 認証済みアクターがローカルタイムラインを要求する間, the Timelines shall ブロック・被ブロック・ミュート関係にあるアカウントの投稿を結果から除外する。
4. Where ローカルタイムライン要求が `only_media` 指定を含む場合, the Timelines shall メディア添付を持つ投稿のみに結果を限定する。

### Requirement 4: タグ（ハッシュタグ）タイムライン

**Objective:** 標準クライアントのユーザーとして、特定のハッシュタグを含む公開投稿を時系列で見たい。これにより、話題単位で投稿を追える。

#### Acceptance Criteria

1. When ハッシュタグを指定したタグタイムラインが要求されたとき, the Timelines shall 当該ハッシュタグを含む公開（`public`）可視性の投稿（ローカル・リモート双方）を新しい順に集約して返す。
2. The Timelines shall ハッシュタグ照合を大文字小文字を区別しない正規化済みのタグ名で行う。
3. The Timelines shall タグタイムラインからブースト（reblog）を除外し、元投稿のみを含める。
4. Where タグタイムライン要求が追加タグの `any` / `all` / `none` 指定を含む場合, the Timelines shall いずれかを含む（any）/ すべてを含む（all）/ いずれも含まない（none）の条件で結果を絞り込む。
5. Where タグタイムライン要求が `local` または `only_media` 指定を含む場合, the Timelines shall それぞれローカル投稿のみ / メディア添付を持つ投稿のみに結果を限定する。
6. While 認証済みアクターがタグタイムラインを要求する間, the Timelines shall ブロック・被ブロック・ミュート関係にあるアカウントの投稿を結果から除外する。

### Requirement 5: 可視性フィルタの再利用

**Objective:** 連合運用者として、タイムラインの可視性判定が投稿側（statuses-core）と完全に同一であってほしい。これにより、投稿では見えるがタイムラインでは漏れる/逆に見えてはいけない投稿が出るといった不整合を排除できる。

#### Acceptance Criteria

1. The Timelines shall 投稿が閲覧者から可視かどうかを statuses-core の `VisibilityPolicy` の判定で決定し、可視性判定ロジックを本 spec 内で再実装しない。
2. While 認証されていない閲覧者にタイムラインを返す間, the Timelines shall 公開（`public`）可視性の投稿のみを結果に含める。
3. The Timelines shall ローカル投稿とリモート投稿に対して同一の可視性判定を適用し、ローカル/リモートで判定差を設けない。
4. When フォロワー限定（`private`）投稿をタイムラインに含めるか判定するとき, the Timelines shall 閲覧者と投稿者のフォロー関係を反映した `VisibilityPolicy` の判定結果に従う。

### Requirement 6: ブロック/ミュートフィルタの再利用

**Objective:** 標準クライアントのユーザーとして、ブロック/ミュートした相手や自分をブロックした相手の投稿がタイムラインに出ないようにしたい。これにより、関係設定がタイムラインに正しく反映される。

#### Acceptance Criteria

1. The Timelines shall ブロック・被ブロック・ミュート・フォローの関係集合を social-graph の関係問い合わせ（`FilterQuery`）から取得し、関係状態を本 spec 内で再実装しない。
2. While 認証済みアクターのタイムラインを集約する間, the Timelines shall 当該アクターがブロックしているアカウント、当該アクターをブロックしているアカウント、当該アクターがミュートしているアカウントの投稿を結果から除外する。
3. When ブーストを評価するとき, the Timelines shall ブースト実行者または被ブースト元投稿の投稿者が当該閲覧者のブロック/被ブロック/ミュート対象である場合に当該ブーストを除外する。
4. While ミュートの有効期限を考慮する間, the Timelines shall 期限切れのミュートを除外対象として扱わない（social-graph の期限考慮済み集合に従う）。

### Requirement 7: ページネーション規約

**Objective:** クライアント実装者として、全タイムラインのページネーションが Mastodon 互換で一貫していてほしい。これにより、既存クライアントが追加読み込み・先頭更新を正しく行える。

#### Acceptance Criteria

1. The Timelines shall 全タイムラインのページネーションを api-foundation のページネーション規約（`max_id` / `since_id` / `min_id` / `limit`）で解釈し、独自のページング方式を設けない。
2. When タイムライン応答を返すとき, the Timelines shall 次ページ・前ページへの `Link` ヘッダ（`next` / `prev`）を api-foundation の `Link` 生成で付与する。
3. The Timelines shall `since_id`（最新固定）と `min_id`（古い側から進む）の挙動差を api-foundation 規約どおりに保持する。
4. The Timelines shall `limit` を api-foundation 規約の上限で丸め、欠落・重複・無限ループのない安定したカーソル順序（投稿の時系列順）で結果を返す。

### Requirement 8: 単一生成点（Streaming 再利用シーム）

**Objective:** 下流 Streaming spec の実装者として、ある投稿がどのタイムラインに属するかの判定を REST と共有したい。これにより、REST タイムラインと Streaming 配信が二重ロジック化・二重発火せず一貫する。

#### Acceptance Criteria

1. The Timelines shall ある投稿が、ある閲覧者の、あるタイムライン種別（home / public / local / tag）に属するか（可視性・関係フィルタ・タイムライン種別条件を含む）を判定する単一の membership 判定点を提供する。
2. The Timelines shall REST タイムライン取得とタイムライン membership 判定が同一のフィルタ・可視性・タイムライン種別条件を用いるようにし、判定ロジックを二重定義しない。
3. The Timelines shall タイムライン membership 判定点を、下流 Streaming spec が再利用できる公開シームとして提供する。
4. The Timelines shall 配信（リアルタイム送出）そのものを本 spec のスコープに含めず、membership 判定点の確立のみを担う。

### Requirement 9: 認証・スコープ・エラー互換

**Objective:** クライアント実装者として、タイムライン API の認証・スコープ・エラー応答が他の Mastodon 互換 API と一貫していてほしい。これにより、横断的に同じ作法で扱える。

#### Acceptance Criteria

1. The Timelines shall 認証・スコープ検証を api-foundation の Bearer 認証・`Scope` 内包判定で行い、ホームタイムラインに `read:statuses` スコープを要求する。
2. Where 公開・ローカル・タグタイムラインが認証なしで要求された場合, the Timelines shall 公開投稿のみを返す形でこれを許容する（未認証アクセスを一律拒否しない）。
3. If スコープ不足のトークンで保護対象タイムラインが要求されたとき, then the Timelines shall api-foundation の Mastodon 互換エラー本文で 403 相当を返す。
4. The Timelines shall すべての失敗応答を api-foundation の Mastodon 互換エラー本文（`error` / 任意の `error_description`）で返し、`X-RateLimit-*` 付与を横断レイヤーに委ねる。

### Requirement 10: Status 契約の消費

**Objective:** クライアント実装者として、タイムラインの各要素が投稿 API と同一の Status JSON 表現であってほしい。これにより、表示ロジックを共有できる。

#### Acceptance Criteria

1. The Timelines shall タイムライン要素のシリアライズを statuses-core の Status シリアライザで行い、Status JSON 契約を本 spec で再定義しない。
2. When 認証済みアクター文脈でタイムラインを返すとき, the Timelines shall 各投稿に当該アクターの操作状態（`favourited` / `reblogged` / `bookmarked` 等）が反映されるよう、閲覧者文脈を上流シリアライザへ渡す。
3. When ブースト投稿をタイムラインに含めるとき, the Timelines shall statuses-core の規律に従い被ブースト元投稿を `reblog` にネストした Status 表現で返す。
4. The Timelines shall Account・メディア等の埋め込み表現を statuses-core / 上流シリアライザの委譲に委ね、本 spec で独自表現を持たない。
