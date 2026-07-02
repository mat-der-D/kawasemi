# Requirements Document

## Introduction

search は、標準クライアント（Ivory・Elk・Phanpy 等）が前提とする検索機能（アカウント・投稿・ハッシュタグ）を Mastodon 互換 API（`GET /api/v2/search`）として提供する spec である。本プロジェクトの配布方針は「拡張不要・ランタイムはアプリ + PostgreSQL のみ」であり、日本語全文検索を最初から作り込むことはこの方針と衝突する。したがって本 spec の最重要責務は、(1) 検索処理を**抽象境界（ポート）の背後**に置き呼び出し側を特定エンジン実装に依存させないこと、(2) 初期は**標準 PostgreSQL の範囲でできる最小実装**を提供すること、(3) `pg_bigm` 等の日本語拡張や将来の外部エンジンへ、API 契約と呼び出し側を変えずに**後から差し替え可能なマイグレーション経路**を確保すること、の 3 点である。

本 spec が完了すると、(a) `GET /api/v2/search` が `accounts` / `statuses` / `hashtags` を含む Mastodon 互換の SearchResults を返し、(b) `type` による種別絞り込み・`resolve` によるリモート解決・ページネーションが機能し、(c) `acct:user@domain` 形式および URL でのリモートアカウント/投稿の解決が federation-core の WebFinger（`acct:` 解決）と連合取得を通じて行え、(d) 検索バックエンドが `SearchBackend` ポートの背後に隔離され、標準 PostgreSQL の最小実装（`PgSearchBackend`）が既定で配線され、(e) 日本語拡張インデックスを後付けする独立マイグレーション経路が用意された状態になる。

本 spec は Account / Status の JSON エンティティ契約を**再定義しない**。アカウント・投稿の検索結果は、それぞれ accounts-and-instance と statuses-core が確立した Account / Status 契約・シリアライズ・可視性判定を消費して構築する。ハッシュタグ結果のための Tag エンティティ（検索結果に必要な最小形）と、ハッシュタグ検索のための読み取り用インデックスは本 spec が所有する。trends / suggestions（ディスカバリ）と、`pg_bigm` 等の日本語拡張の必須化、外部検索エンジンは本 spec のスコープ外である。

## Boundary Context

- **In scope**: `GET /api/v2/search`（統一検索エンドポイント）、SearchResults（`accounts` / `statuses` / `hashtags`）の組み立てと JSON 形、Tag エンティティ（検索結果用の最小契約）とそのゴールデン、アカウント検索（ローカル/既知リモートのテキスト一致）、投稿検索（閲覧者の可視性に閉じた標準 PostgreSQL 最小実装）、ハッシュタグ検索（ハッシュタグ名一致）とハッシュタグ読み取りインデックスの所有、`acct:user@domain` および URL によるリモート解決（`resolve=true`、WebFinger と連合取得の消費）、検索の抽象境界（`SearchBackend` ポート）と標準 PostgreSQL 最小実装（`PgSearchBackend`）、日本語拡張・将来エンジンを後付けする独立マイグレーション経路、本 spec 全エンドポイントへの api-foundation 横断規約（認証・スコープ・エラー・ページネーション・レート制限）の適用。
- **Out of scope**: trends（流行ハッシュタグ）・suggestions（おすすめアカウント）等のディスカバリ機能、`pg_bigm` 等の日本語拡張の**必須化**（任意オプションに留め、後付け経路のみ提供）、外部検索エンジンの導入、Account / Status / Poll / CustomEmoji エンティティ契約そのものの定義（accounts-and-instance / statuses-core 所有。本 spec は消費のみ）、ハッシュタグのタイムライン（tag タイムラインは timelines）・featured_tags・フォロー対象ハッシュタグの管理、OAuth・ページネーション規約・エラー本文・レート制限・契約ハーネス基盤（api-foundation 所有。本 spec は適用のみ）、HTTP Signatures・WebFinger ハンドラ・連合取得配管そのもの（federation-core 所有。本 spec は消費のみ）。
- **Adjacent expectations**: 本 spec は api-foundation（Bearer 認証・`read:search` を含むスコープ内包判定・Mastodon 互換エラー本文・`X-RateLimit-*`・ページネーション規約・プロキシ尊重 URL・契約テストハーネス）、accounts-and-instance（Account JSON 契約とシリアライズ・`AccountService` のアカウント解決・リモートアカウントのフェッチ/正規化境界）、statuses-core（Status JSON 契約とシリアライズ・可視性判定 `VisibilityPolicy`・可視投稿の解決）、federation-core（WebFinger による `acct:` 解決・`FederationHttpClient` による連合取得・`ActorUrls`/JSON-LD 安全展開）、actor-model（`ActorDirectory` のローカルアクター解決）、core-runtime（`AppState` / `RuntimeContext` / `PgPool` / `AppError` / マイグレーション基盤 / テストハーネス）に依存する。下流の将来 spec（日本語検索強化）は、`SearchBackend` ポートを差し替えることで本 spec の API 契約・呼び出し側を変えずに検索品質を置換できることを前提とする。

## Requirements

### Requirement 1: SearchResults エンティティ JSON 契約

**Objective:** クライアント開発者として、検索結果が Mastodon 互換の SearchResults 形でゴールデン固定されていてほしい。これにより、既存クライアントが検索結果を出力ドリフトなく描画できる。

#### Acceptance Criteria

1. When 検索結果を JSON にシリアライズするとき, the Search Service shall Mastodon 互換の SearchResults として `accounts`（Account 配列）/ `statuses`（Status 配列）/ `hashtags`（Tag 配列）の 3 フィールドを含む JSON を生成する。
2. The Search Service shall `accounts` の各要素を accounts-and-instance が所有する Account JSON 契約のシリアライズで生成し、`statuses` の各要素を statuses-core が所有する Status JSON 契約のシリアライズで生成し、これらのエンティティ契約を本 spec で再定義しない。
3. When ハッシュタグ結果を表現するとき, the Search Service shall Tag エンティティとして少なくとも `name` / `url` / `history` の各フィールドを含める。
4. While いずれかの種別の結果が存在しない間, the Search Service shall 該当フィールドを空配列（`[]`）として返し、null にしない。
5. The Search Service shall SearchResults と Tag の JSON 契約を api-foundation の契約テストハーネスにゴールデンとして登録し、決定的な非決定性境界（時刻・ID）の下で再現可能にする。

### Requirement 2: 統一検索エンドポイント（search v2）

**Objective:** 標準クライアントのユーザーとして、単一のエンドポイントでアカウント・投稿・ハッシュタグを横断検索したい。これにより、Mastodon 同等の検索 UX を得られる。

#### Acceptance Criteria

1. When 認証済みリクエストが検索クエリ（`q`）で `GET /api/v2/search` を要求したとき, the Search Service shall `read:search` スコープを検証したうえで、`accounts` / `statuses` / `hashtags` を含む SearchResults を返す。
2. Where リクエストが `type`（`accounts` / `statuses` / `hashtags`）を指定する場合, the Search Service shall 指定された種別のみを検索し、他の種別の結果を空配列で返す。
3. If リクエストが空または空白のみの `q` を伴うとき, then the Search Service shall Mastodon 互換のエラー応答で要求を拒否する。
4. If リクエストが有効な Bearer トークンを欠く、または要求スコープ（`read:search` 相当）を満たさないとき, then the Search Service shall Mastodon 互換エラー応答（401 または 403 相当）を返す。
5. Where リクエストが結果件数の上限（`limit`）を指定する場合, the Search Service shall api-foundation のページネーション規約に従って上限を解釈し、規約上の最大値を超える指定を丸める。

### Requirement 3: アカウント検索

**Objective:** 標準クライアントのユーザーとして、表示名・ユーザー名・ハンドルでアカウントを検索したい。これにより、フォロー対象や言及先を見つけられる。

#### Acceptance Criteria

1. When 検索が `accounts` を対象とするとき, the Search Service shall クエリ文字列に一致するローカルアクターおよび既知のリモートアカウントを、表示名・ユーザー名・ハンドル（`acct`）に対する一致で抽出する。
2. The Search Service shall 抽出した各アカウントを accounts-and-instance の Account 契約で表現して `accounts` 配列に格納する。
3. Where リクエストが `following=true` を指定する場合, the Search Service shall 結果を閲覧アクターがフォローしているアカウントに限定できるよう、フォロー関係の判定を上流（social-graph 供給の関係状態）に委ねる。
4. Where リクエストがアカウント検索の件数上限・オフセットを指定する場合, the Search Service shall 指定に従って結果件数を制限し、オフセットを適用する。
5. While アカウント検索結果を返す間, the Search Service shall 同一アカウントが重複して現れないよう結果を一意化する。

### Requirement 4: 投稿検索（標準 PostgreSQL 最小実装・可視性に閉じる）

**Objective:** 標準クライアントのユーザーとして、本文に基づいて投稿を検索したい。これにより、過去の発言や会話を見つけられる。

#### Acceptance Criteria

1. When 検索が `statuses` を対象とするとき, the Search Service shall クエリ文字列に一致する投稿を、閲覧アクターから可視な投稿の範囲に限定して抽出する。
2. The Search Service shall 投稿の可視性判定を statuses-core の可視性ロジックに委ね、閲覧アクターから不可視の投稿を結果に含めない。
3. Where リクエストが `account_id` を指定する場合, the Search Service shall 投稿検索を当該アカウントの投稿に限定する。
4. The Search Service shall 投稿検索を標準 PostgreSQL の範囲で実装し、外部検索エンジンや必須の追加拡張（`pg_bigm` 等）に依存しない。
5. The Search Service shall 標準 PostgreSQL の全文検索が日本語を分かち書きできない既知制約を踏まえ、最小実装としてクエリに対する一致範囲が限定的であることを許容し、その範囲を将来のバックエンド差し替えで拡張可能に保つ。
6. Where リクエストが投稿検索の件数上限・オフセットを指定する場合, the Search Service shall 指定に従って結果件数を制限し、オフセットを適用する。

### Requirement 5: ハッシュタグ検索

**Objective:** 標準クライアントのユーザーとして、ハッシュタグ名で関連タグを検索したい。これにより、話題を追える。

#### Acceptance Criteria

1. When 検索が `hashtags` を対象とするとき, the Search Service shall クエリ文字列に一致するハッシュタグを、本 spec が保持するハッシュタグ読み取りインデックスから抽出する。
2. The Search Service shall 各ハッシュタグを Tag エンティティ（`name` / `url` / `history`）で表現して `hashtags` 配列に格納する。
3. The Search Service shall ハッシュタグ読み取りインデックスを、statuses-core が確立する投稿の保持データ（投稿に抽出されたハッシュタグ）から導出して保持し、検索対象とする。
4. Where リクエストが `exclude_unreviewed=true` を指定する場合, the Search Service shall Mastodon 互換の挙動として当該パラメータを受理し、本サーバーの最小実装に整合する結果を返す。
5. Where リクエストがハッシュタグ検索の件数上限・オフセットを指定する場合, the Search Service shall 指定に従って結果件数を制限し、オフセットを適用する。

### Requirement 6: `acct:` および URL によるリモート解決

**Objective:** 連合の利用者として、未取得のリモートアカウントや投稿を `acct:user@domain` 形式や URL で直接解決したい。これにより、外部のユーザーや投稿をローカルに取り込んで参照できる。

#### Acceptance Criteria

1. When 検索クエリが `acct:user@domain`（または `@user@domain`）形式で、かつリクエストが `resolve=true` を伴うとき, the Search Service shall federation-core の WebFinger による `acct:` 解決を用いてリモートアクターを特定し、accounts-and-instance のリモートアカウント取得・正規化を通じて当該アカウントを Account として結果に含める。
2. When 検索クエリがリモートアクターまたはリモート投稿の URL で、かつリクエストが `resolve=true` を伴うとき, the Search Service shall 連合取得を通じて当該リソースを解決し、対応する Account または Status として結果に含める。
3. While リクエストが `resolve` を伴わない、または `resolve=false` である間, the Search Service shall リモート取得を行わず、ローカルに既知の対象のみを検索する。
4. If `resolve=true` のリモート解決対象の取得または正規化に失敗したとき, then the Search Service shall 当該対象を結果に含めず、エンドポイント自体は正常に応答する。
5. While `resolve=true` のリモート解決を行う間, the Search Service shall リモート解決を認証済みリクエストに限定し、未認証リクエストではリモート取得を行わない。

### Requirement 7: 検索バックエンドの抽象境界（スワップ可能なポート）

**Objective:** アーキテクトとして、検索処理を抽象インターフェースの背後に置き、呼び出し側を特定エンジン実装に依存させたくない。これにより、将来の日本語検索強化やエンジン差し替えを呼び出し側を変えずに行える。

#### Acceptance Criteria

1. The Search Service shall アカウント・投稿・ハッシュタグの検索照合を `SearchBackend` 抽象ポートの背後に置き、検索の呼び出し側（エンドポイント・結果組み立て）を特定の検索実装に依存させない。
2. The Search Service shall `SearchBackend` ポートを、照合結果として上流のエンティティ識別子（アカウント参照・投稿識別子・ハッシュタグ）を返す形に定義し、エンティティの JSON シリアライズ（Account / Status / Tag 構築）をポートの外（結果組み立て層）に保つ。
3. The Search Service shall 標準 PostgreSQL を用いる既定の `SearchBackend` 実装（`PgSearchBackend`）を提供し、起動時にこれを既定の検索バックエンドとして配線する。
4. Where 将来の検索バックエンド実装が `SearchBackend` ポートを満たす場合, the Search Service shall 呼び出し側・API 契約・結果組み立てを変更せずに、配線点で実装を差し替え可能にする。
5. The Search Service shall 検索バックエンドが決定的にテスト可能となるよう、`SearchBackend` をモック/スタブ実装で差し替え可能に保つ。

### Requirement 8: 拡張・日本語対応の後付けマイグレーション経路

**Objective:** 運用者・将来の実装者として、配布の簡単さ（拡張不要）を損なわずに、後から日本語対応や検索インデックスを追加できる経路を確保したい。これにより、初期は最小構成で配布しつつ、必要時に検索品質を引き上げられる。

#### Acceptance Criteria

1. The Search Service shall 初期配布で `pg_bigm` 等の日本語拡張や外部検索エンジンを必須とせず、標準 PostgreSQL のみで検索エンドポイントが機能する状態を提供する。
2. The Search Service shall ハッシュタグ読み取りインデックス等の本 spec 所有の永続構造を、prior spec と衝突しないマイグレーション番号で追加する。
3. Where 日本語拡張インデックス（`pg_bigm` 等）を後から導入する場合, the Search Service shall 既存スキーマ・既定バックエンドの呼び出し契約を破壊せずに、独立した後付けマイグレーションとしてインデックス/スキーマを追加できる経路を設計上確保する。
4. The Search Service shall 後付けの拡張インデックスや代替バックエンドの有無に関わらず、`GET /api/v2/search` の API 契約（SearchResults 形・パラメータ・スコープ）を一定に保つ。

### Requirement 9: 横断規約の適用（認証・スコープ・エラー・ページネーション・レート制限）

**Objective:** API 実装者として、本 spec のエンドポイントが api-foundation の横断規約に一貫して乗ることを保証したい。これにより、認証・エラー・ページネーション・レート制限の挙動がサーバー全体で統一される。

#### Acceptance Criteria

1. The Search Service shall `GET /api/v2/search` に api-foundation の Bearer 認証ミドルウェアと `read:search` のスコープ内包判定を適用する。
2. The Search Service shall すべてのエラー応答を api-foundation の Mastodon 互換エラー本文（`error` / 任意 `error_description`）とステータス対応で返す。
3. While 検索結果を返す間, the Search Service shall api-foundation のページネーション規約（`limit` / `offset` 等の解釈と、必要に応じた `Link` ヘッダ・プロキシ尊重の絶対 URL）に従う。
4. The Search Service shall 本 spec の応答に api-foundation の `X-RateLimit-*` 付与レイヤーが適用される装着点に乗せる。
5. While 検索処理が失敗または部分的に失敗する間, the Search Service shall 失敗の原因特定に十分な構造化診断（クエリ種別・対象種別・失敗箇所、秘匿値を除く）を core-runtime の観測性に出力する。
