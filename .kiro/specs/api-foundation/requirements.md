# Requirements Document

## Introduction

api-foundation は Mastodon 互換 API 全体が乗る「横断土台」を確立する spec である。Mastodon 互換 API は単なる REST エンドポイント群ではなく、(1) OAuth 2.0 による認証、(2) 全リスト系で一貫したページネーション規約、(3) エラー JSON とレート制限ヘッダの互換、(4) エンティティ JSON 契約を固定するゴールデンテスト基盤、という横断サブシステムの上に成立する。これらを各機能 spec が個別に再発明すると、互換性のドリフト・契約テスト基準の不在・無限ループや歯抜けを生むページネーション不整合といった問題が必発する。

本 spec が完了すると、標準クライアント（Ivory・Elk・Phanpy 等）が OAuth でログインでき（複数アクター × 1 トークンのアクター選択を含む）、全リスト API が `Link` ヘッダ + `max_id`/`since_id`/`min_id` で一貫し、エラー JSON 形と `X-RateLimit-*` ヘッダが Mastodon 互換で、エンティティ JSON 契約のゴールデンテスト基盤が用意された状態になる。以降の機能 spec（accounts/statuses/timelines 等）は、この土台に「乗るだけ」でよい。

最重要の独自設計点は「複数アクター × 1 アクセストークン」の橋渡しである。Mastodon API は「1 アクセストークン = 1 アカウント」を前提とするため、本プロジェクトでは認可フローの承認時にオーナーの保有アクターから 1 つを選択させ、発行トークンを単一アクターに結びつける。これは Mastodon 標準から外れる独自分岐点であり、本 spec が明示的に所有する。

## Boundary Context

- **In scope**: OAuth 2.0 サーバー全体（アプリ登録、認可コードフロー + 承認画面、アクター選択ログイン、トークン発行・失効、スコープモデルと検証、Bearer 認証ミドルウェア）、ページネーション規約（`Link` ヘッダ + カーソルパラメータ、カテゴリ毎カーソル種別の表現）、Mastodon 互換エラー JSON 形、`X-RateLimit-*` ヘッダ、HTTP ステータス互換方針、エンティティ JSON 契約のゴールデン/スナップショットテストハーネス。
- **Out of scope**: 個別エンティティ（Account/Status/Notification/Poll/Relationship/Instance 等）の具体的 JSON 契約内容（各機能 spec が所有し、本 spec のハーネスに足す）、各エンドポイントのビジネスロジック、WebSocket Streaming の接続そのもの（streaming spec。ただしトークン検査ロジックは本 spec のものを再利用する）、Web Push の購読実体と VAPID 鍵管理（web-push spec。ただし `push` スコープの定義は本 spec が所有）、レート制限の厳格な実値・分散カウンタ（一人鯖前提で実値は緩く、ヘッダ形のみ厳守）。
- **Adjacent expectations**: 本 spec は core-runtime が提供する起動・DB プール・非決定性 DI 境界（clock/id/rng）・統一エラー型 `AppError`（Mastodon 互換 JSON への拡張点を含む）・構造化ログ・テストハーネス（`spawn_test_app`）に依存する。また actor-model が提供するオーナー別アクター一覧（`list_actors_for_owner`）をアクター選択候補の供給源として消費し、トークンに結びつけるアクター識別子の正当性を actor-model のアクター解決に依存する。本 spec はアクター・オーナーのデータモデルそのものは所有しない。下流の全 API spec は、本 spec の Bearer 認証・ページネーション規約・エラー/レート制限互換・契約テストハーネスに依存する。

## Requirements

### Requirement 1: OAuth アプリケーション登録

**Objective:** 標準クライアントの開発者として、サーバーにクライアントアプリケーションを登録してクライアント資格情報を取得したい。これにより、クライアントが OAuth 認可フローを開始できる。

#### Acceptance Criteria

1. When クライアントがアプリケーション登録（クライアント名・リダイレクト URI・要求スコープを含む）を要求したとき, the API Foundation shall クライアント識別子とクライアントシークレットを発行し、登録されたアプリケーション情報を返す。
2. If アプリケーション登録要求に必須項目（クライアント名・リダイレクト URI）が欠落または形式不正であるとき, then the API Foundation shall Mastodon 互換のエラー応答で要求を拒否する。
3. When 登録要求が要求スコープを指定したとき, the API Foundation shall 既知のスコープのみを受理し、未知のスコープを含む要求を拒否する。
4. When 登録要求がリダイレクト URI を指定したとき, the API Foundation shall そのリダイレクト URI を当該アプリケーションに紐づけて保管し、以降の認可フローで完全一致による検証に用いる。
5. When クライアントが自身のクライアント資格情報の検証を要求したとき, the API Foundation shall 当該アプリケーションの公開情報を返し、無効な資格情報に対しては認証エラーを返す。

### Requirement 2: 認可コードフローとアクター選択承認

**Objective:** 一人鯖のオーナーとして、標準クライアントから OAuth でログインする際に、どのローカルアクターとしてログインするかを選択したい。これにより、複数アクターを単一トークン前提の標準クライアントから個別に操作できる。

#### Acceptance Criteria

1. When クライアントが認可エンドポイントへ認可コードフロー（クライアント識別子・リダイレクト URI・要求スコープ・応答種別を含む）を開始したとき, the API Foundation shall クライアント識別子とリダイレクト URI の登録一致を検証し、不一致のときは認可コードを発行せず拒否する。
2. When 認可要求が検証を通過したとき, the API Foundation shall オーナーの保有アクター一覧を提示し、どのアクターとしてログインするかと要求スコープへの承認を求める最小限の承認画面を返す。
3. When オーナーが承認画面で 1 つのアクターを選択して承認したとき, the API Foundation shall 選択されたアクターと承認されたスコープに結びついた認可コードを発行し、登録済みリダイレクト URI へ返す。
4. If オーナーが承認を拒否したとき, then the API Foundation shall 認可コードを発行せず、OAuth 仕様に沿ったアクセス拒否を登録済みリダイレクト URI へ返す。
5. The API Foundation shall 発行する認可コードを短命とし、一度引き換えられた認可コードの再利用を拒否する。
6. Where 認可要求がコード交換用の検証情報（PKCE チャレンジ）を伴う場合, the API Foundation shall そのチャレンジを認可コードに紐づけて保持し、トークン交換時に検証する。

### Requirement 3: アクセストークンの発行・失効

**Objective:** 標準クライアントとして、認可コードをアクセストークンへ交換し、不要になったトークンを失効させたい。これにより、ユーザーに代わって認証付き API を呼び出し、ログアウトを実現できる。

#### Acceptance Criteria

1. When クライアントが有効な認可コードとクライアント資格情報でトークン交換を要求したとき, the API Foundation shall その認可コードに紐づくアクターとスコープを持つアクセストークンを発行し、Mastodon 互換のトークン応答として返す。
2. If トークン交換要求の認可コード・クライアント資格情報・リダイレクト URI のいずれかが不正または不一致であるとき, then the API Foundation shall アクセストークンを発行せず、OAuth 仕様に沿ったエラー応答で拒否する。
3. Where トークン交換要求が PKCE 検証情報を伴う場合, the API Foundation shall 認可コードに紐づくチャレンジとの整合を検証し、不整合のときはトークンを発行しない。
4. When クライアントが発行済みアクセストークンの失効を要求したとき, the API Foundation shall 当該トークンを以降の認証で無効化する。
5. While アクセストークンが有効である間, the API Foundation shall そのトークンに結びついたアクター識別子と承認済みスコープ集合を認証時に解決可能な状態で保持する。
6. The API Foundation shall アクセストークン値を平文のまま診断ログへ出力しない。

### Requirement 4: スコープモデルと検証

**Objective:** API 実装者として、エンドポイントごとに必要な権限を OAuth スコープで宣言し検証したい。これにより、トークンの権限を超えた操作を一貫して拒否できる。

#### Acceptance Criteria

1. The API Foundation shall Mastodon 互換のスコープ体系（少なくとも `read` / `write` / `follow` / `push` の上位スコープと、それらの細分スコープ）を定義する。
2. When エンドポイントが特定スコープを要求し、提示トークンの承認スコープがそれを内包するとき, the API Foundation shall アクセスを許可する。
3. If 提示トークンの承認スコープが要求スコープを内包しないとき, then the API Foundation shall アクセスを拒否し、権限不足を示す Mastodon 互換エラー応答を返す。
4. When 上位スコープ（例: `write`）が承認されているとき, the API Foundation shall その配下の細分スコープ（例: `write:statuses`）を要求する操作を許可する。
5. While スコープ検証を行う間, the API Foundation shall 認可・トークン発行・エンドポイント保護の全段階で同一のスコープ内包判定を適用する。

### Requirement 5: Bearer 認証ミドルウェア

**Objective:** API 実装者として、認証付きエンドポイントに統一された Bearer トークン認証を適用したい。これにより、各エンドポイントが認証処理を再発明せず、現在のアクター文脈を受け取れる。

#### Acceptance Criteria

1. When 認証付きエンドポイントへのリクエストが有効な Bearer アクセストークンを提示したとき, the API Foundation shall そのトークンを解決し、結びついたアクター文脈と承認スコープを後続処理へ供給する。
2. If リクエストが Bearer トークンを欠くか、提示トークンが無効・失効済みであるとき, then the API Foundation shall 認証エラー（401 相当）を Mastodon 互換エラー応答で返す。
3. The API Foundation shall トークンに結びついたアクターを「現在操作中の単一アクター」としてリクエスト文脈に確定し、複数アクターの同時操作を 1 トークンでは行わせない。
4. Where エンドポイントが認証を任意とする場合, the API Foundation shall トークンが提示されればアクター文脈を供給し、提示されなければ未認証文脈として処理を継続できるようにする。
5. The API Foundation shall トークン検査ロジックを、後続 spec（Streaming 等）が同一の判定で再利用できる形で提供する。

### Requirement 6: ページネーション規約

**Objective:** 標準クライアントとして、全リスト系 API で一貫したカーソルページネーションを利用したい。これにより、タイムラインや一覧を欠落・重複・無限ループなく取得できる。

#### Acceptance Criteria

1. When クライアントがリスト系応答を取得したとき, the API Foundation shall 前後ページへの遷移リンクを `Link` ヘッダ（`rel="next"` / `rel="prev"`）として付与する。
2. When リスト要求が `max_id` を指定したとき, the API Foundation shall 指定 ID より古い（小さい）側の項目を返す。
3. When リスト要求が `min_id` を指定したとき, the API Foundation shall 指定 ID より新しい（大きい）側の項目を、古い方から進む向きで返す。
4. When リスト要求が `since_id` を指定したとき, the API Foundation shall 指定 ID より新しい側の項目を、先頭（最新）を固定した向きで返し、`min_id` との挙動差を保つ。
5. When リスト要求が `limit` を指定したとき, the API Foundation shall 取得件数を指定値に制限し、規約上の上限を超える指定は上限に丸める。
6. Where あるカテゴリのカーソルが対象エンティティの ID と異なる場合（例: bookmarks / favourites / notifications）, the API Foundation shall そのカテゴリ固有のカーソル種別を用いてページネーションを構成できる抽象を提供する。
7. While `Link` ヘッダの URL を生成する間, the API Foundation shall リバースプロキシ後段での外部ホスト名・スキームを尊重した絶対 URL を生成する。

### Requirement 7: エラー JSON 互換

**Objective:** 標準クライアントとして、サーバーが返すエラーを Mastodon 互換の JSON 形で受け取りたい。これにより、クライアントがエラー内容を正しく解釈し「不明なエラー」表示を避けられる。

#### Acceptance Criteria

1. When API がエラーを応答するとき, the API Foundation shall `error` フィールドを含む Mastodon 互換のエラー JSON 本文を返す。
2. Where エラーが追加説明を伴う場合, the API Foundation shall `error_description` フィールドを併せて返す。
3. The API Foundation shall 入力検証失敗（422 相当）・認証失敗（401 相当）・権限不足（403 相当）・未検出（404 相当）を、Mastodon 互換の HTTP ステータスとエラー本文に対応付けて出し分ける。
4. The API Foundation shall このエラー応答形を core-runtime の統一エラー型のレスポンス変換骨格に対する拡張として実現し、横断的に全エンドポイントへ適用する。
5. If システム内部エラー（5xx 相当）が発生したとき, then the API Foundation shall 内部実装の詳細を応答本文へ露出させず、互換形のエラー本文のみを返す。

### Requirement 8: レート制限ヘッダ互換

**Objective:** 標準クライアントとして、レート制限に関する標準ヘッダを受け取りたい。これにより、ヘッダ欠落に起因するクライアントの誤動作を避けられる。

#### Acceptance Criteria

1. When API がレート制限の対象応答を返すとき, the API Foundation shall `X-RateLimit-Limit` / `X-RateLimit-Remaining` / `X-RateLimit-Reset` の各ヘッダを付与する。
2. The API Foundation shall レート制限のウィンドウとリセット時刻を、core-runtime の時刻境界から取得した時刻に基づいて算出する。
3. If リクエストがレート制限の上限を超過したとき, then the API Foundation shall 上限超過を示す Mastodon 互換のステータスとエラー本文を返す。
4. Where 一人鯖運用としてレート制限の実値が緩く設定される場合, the API Foundation shall それでも上記ヘッダの形と算出規約を一貫して維持する。

### Requirement 9: エンティティ契約テストハーネス

**Objective:** AI 自律 TDD を行う実装者として、エンティティ JSON の形をゴールデン/スナップショットで先に固定する共通テスト基盤がほしい。これにより、各機能 spec が実装に先んじて契約を固定し、出力ドリフトを防げる。

#### Acceptance Criteria

1. The API Foundation shall エンティティ JSON 応答をゴールデン/スナップショットとして固定・比較できる契約テストハーネスを提供する。
2. When 契約テストが実行されるとき, the API Foundation shall 期待ゴールデンと実応答の差分を、不一致箇所が特定できる形で報告する。
3. When 契約テストがエンティティ応答を生成するとき, the API Foundation shall core-runtime の決定的な非決定性境界（時刻・ID・乱数）を用いて、再現可能なゴールデンを得られるようにする。
4. The API Foundation shall 後続 spec（accounts/statuses/instance 等）が自身のエンティティ契約を追加できる拡張点として本ハーネスを提供し、本 spec 自身は個別エンティティの契約内容を所有しない。
5. Where 標準クライアントの実リクエスト/実レスポンスをフィクスチャとして取り込む場合, the API Foundation shall それを契約テストの受け入れ基準として登録できる仕組みを提供する。
