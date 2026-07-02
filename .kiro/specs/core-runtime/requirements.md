# Requirements Document

## Introduction

core-runtime は kawasemi のすべての機能 spec が乗る最上流のランタイム土台である。本 spec が存在しないと、各機能 spec が起動・設定・マイグレーション・依存注入（DI）・可観測性・エラー処理を個別に再発明し、不整合と flaky テストの温床となる。AI 自律 TDD を前提とする本プロジェクトでは、決定性の欠如が実装ループそのものを破壊するため、非決定性（時刻・ID・乱数・署名鍵）を注入可能な境界の背後に閉じ込めることが土台の必須要件となる。

本 spec は、HTTP アプリケーションが起動して安全に停止し、二層設定の起動設定側を読み込み、起動時に埋め込みマイグレーションを自動適用し、注入可能な非決定性境界・統一エラー/レスポンス基盤・構造化ログ/診断出力・DB 込み統合テストの起動を担うテストハーネスを備えた状態を実現する。以降の機能 spec はこの土台に機能を足すだけでよい。

## Boundary Context

- **In scope**: アプリケーションの起動と graceful shutdown、データベース接続プールの確立、二層設定のうち起動設定側（TOML/環境変数）の読み込みと検証、埋め込みマイグレーション基盤と起動時自動実行（失敗時の安全停止を含む）、非決定性 DI 境界（clock / id generator / RNG / 署名鍵プロバイダ）、統一エラー型と HTTP レスポンス変換の骨格、構造化ログ/診断出力、DB 込み統合テストを起動できるテストハーネスの土台。
- **Out of scope**: 個別 API エンドポイント・OAuth・ページネーション規約（api-foundation が所有）、ActivityPub 連合（federation-core）、運用設定（DB 保存値）の読み書き・管理画面 UI（admin-frontend）、配布形態・内蔵 ACME・TLS 終端・systemd unit（distribution）、ドメインモデル（アクター・投稿等の後続 spec）、署名鍵の生成・ローテーション運用そのもの（actor-model。本 spec は鍵を供給する注入境界のみを所有）。
- **Adjacent expectations**: 後続のすべての spec は、本 spec が提供する設定値・DB プール・非決定性プロバイダ・統一エラー型・ログ基盤・テストハーネスに依存する。api-foundation はエラー/レスポンス規約を本 spec の骨格から拡張する。運用設定（二層設定の DB 側）の格納先テーブルは後続 spec が所有し、本 spec はそれに依存しない。

## Requirements

### Requirement 1: アプリケーションライフサイクル

**Objective:** 一人鯖の運用者として、サーバープロセスが確実に起動し、停止要求に対して処理中の作業を取りこぼさずに安全に停止してほしい。これにより、再起動やアップデート時のデータ破損や中途半端な応答を避けられる。

#### Acceptance Criteria

1. When プロセスが起動されたとき, the Core Runtime shall 設定読み込み・DB プール確立・マイグレーション適用・HTTP リスナー待ち受け開始の順に初期化を完了し、待ち受け可能になった旨を記録する。
2. If 初期化のいずれかの段階が失敗したとき, then the Core Runtime shall HTTP リスナーを開始せず、失敗理由を診断情報とともに出力し、非ゼロの終了コードでプロセスを停止する。
3. When プロセスが OS のシャットダウンシグナル（割り込みおよび終了シグナル）を受信したとき, the Core Runtime shall 新規リクエストの受付を停止し、処理中のリクエストの完了を待ってから停止する。
4. While graceful shutdown を実行している間, the Core Runtime shall 設定された猶予時間を上限とし、猶予時間を超えても完了しないリクエストがある場合は強制的に停止する。
5. When graceful shutdown が完了したとき, the Core Runtime shall データベース接続プールを解放し、ゼロの終了コードでプロセスを停止する。

### Requirement 2: 起動設定の読み込みと検証

**Objective:** 一人鯖の運用者として、ドメイン・DB 接続先・シークレットといった起動設定を TOML ファイルまたは環境変数で与え、誤りがあれば起動時に明確に知らせてほしい。これにより、設定ミスを早期に発見できる。

#### Acceptance Criteria

1. When プロセスが起動されたとき, the Core Runtime shall TOML 設定ファイルと環境変数の双方から起動設定を読み込み、単一の検証済み設定として組み立てる。
2. When 同一の設定項目が TOML と環境変数の両方で指定されているとき, the Core Runtime shall 環境変数の値を優先して採用する。
3. If 必須の起動設定項目（少なくともサーバードメインおよびデータベース接続先）が欠落しているとき, then the Core Runtime shall どの項目が欠落しているかを示す診断メッセージを出力し、起動を中止する。
4. If 設定値が期待する形式に適合しないとき, then the Core Runtime shall どの項目が不正かを示す診断メッセージを出力し、起動を中止する。
5. The Core Runtime shall シークレットを含む設定値を平文のままログへ出力しない。
6. Where 起動設定が運用設定（DB 保存値）と概念的に区別されるべき場合, the Core Runtime shall 起動設定のみを所有し、運用設定の読み書きを行わない。

### Requirement 3: データベース接続プール

**Objective:** 後続 spec の実装者として、確立済みで再利用可能なデータベース接続プールを土台から受け取りたい。これにより、各機能が接続管理を個別に実装せずに済む。

#### Acceptance Criteria

1. When 起動設定の読み込みが完了したとき, the Core Runtime shall 設定された接続先に対してデータベース接続プールを確立する。
2. If データベースへの初回接続が確立できないとき, then the Core Runtime shall 失敗理由を診断情報とともに出力し、起動を中止する。
3. The Core Runtime shall 確立した接続プールを後続コンポーネントが共有して利用できる形で公開する。
4. While アプリケーションが稼働している間, the Core Runtime shall 接続プールのサイズおよび接続取得のタイムアウトを設定値に従って制御する。

### Requirement 4: 埋め込みマイグレーションと起動時自動実行

**Objective:** 非エンジニアの運用者として、アップデート時に手動でマイグレーションを実行せずとも、起動するだけでスキーマが最新化されてほしい。これにより、コンパイルやコマンド操作なしでサーバーを更新できる。

#### Acceptance Criteria

1. The Core Runtime shall マイグレーション定義をバイナリに埋め込んで保持し、外部のマイグレーションファイルやツールを必要としない。
2. When プロセスが起動し DB プールが確立されたとき, the Core Runtime shall 未適用のマイグレーションを適用順に自動適用する。
3. When 適用すべき未適用のマイグレーションが存在しないとき, the Core Runtime shall マイグレーションを適用せずに起動を継続する。
4. While マイグレーションを適用している間, the Core Runtime shall 既存データを保持し、適用済みマイグレーションを再適用しない。
5. If マイグレーションの適用が失敗したとき, then the Core Runtime shall HTTP リスナーを開始せず、失敗したマイグレーションを特定できる診断情報を出力し、起動を中止する。
6. If 適用済みのマイグレーション履歴がバイナリに埋め込まれた定義と不整合であるとき, then the Core Runtime shall 不整合を検出して起動を中止し、診断情報を出力する。

### Requirement 5: 注入可能な非決定性境界

**Objective:** AI 自律 TDD を行う実装者として、時刻・ID・乱数・署名鍵といった非決定的な値の供給源を差し替え可能にしたい。これにより、テストで決定的な値を注入し flaky なテストを排除できる。

#### Acceptance Criteria

1. The Core Runtime shall 現在時刻の取得を抽象境界の背後に置き、具体実装に直接依存させずに供給する。
2. The Core Runtime shall 識別子（ID）の生成を抽象境界の背後に置き、具体実装に直接依存させずに供給する。
3. The Core Runtime shall 乱数の生成を抽象境界の背後に置き、具体実装に直接依存させずに供給する。
4. The Core Runtime shall 署名鍵の供給を抽象境界の背後に置き、具体実装に直接依存させずに供給する。
5. Where テストまたは連合検証が実行される場合, the Core Runtime shall 各非決定性境界の実装を決定的な代替実装へ差し替えられるようにする。
6. While 本番構成で稼働している間, the Core Runtime shall 各非決定性境界に対して本番用の具体実装を提供する。

### Requirement 6: 統一エラー型と HTTP レスポンス変換

**Objective:** 後続 spec の実装者として、アプリケーション全体で一貫したエラー型と HTTP レスポンスへの変換骨格を土台から使いたい。これにより、各機能がエラー処理とレスポンス整形を再発明せずに済む。

#### Acceptance Criteria

1. The Core Runtime shall アプリケーション横断で利用できる統一エラー型を提供する。
2. When ハンドラ処理が統一エラー型を返したとき, the Core Runtime shall それを対応する HTTP ステータスコードと構造化されたエラー応答本文へ変換する。
3. The Core Runtime shall 利用者向けエラー（4xx）とシステムエラー（5xx）を区別し、それぞれ適切なステータスコード分類へ対応付ける。
4. If システムエラー（5xx）が発生したとき, then the Core Runtime shall 内部実装の詳細を応答本文に露出させず、かつ失敗の原因特定に十分な診断情報をログへ出力する。
5. Where 後続 spec が Mastodon 互換のエラー応答規約を必要とする場合, the Core Runtime shall その規約を拡張できる変換骨格を提供する。

### Requirement 7: 構造化ログと診断出力

**Objective:** AI 自律 TDD を行う実装者として、失敗時にリクエスト・レスポンス・実行 SQL を十分に確認できる診断情報がほしい。これにより、AI が原因を特定し自己修正できる。

#### Acceptance Criteria

1. The Core Runtime shall 構造化されたログ出力基盤を初期化し、アプリケーション全体から利用できるようにする。
2. When HTTP リクエストが処理されたとき, the Core Runtime shall 当該リクエストとレスポンスに関する診断情報を構造化ログとして出力する。
3. Where 診断レベルのログが有効な場合, the Core Runtime shall データベースに対して実行された SQL を診断情報として出力する。
4. The Core Runtime shall ログの出力レベルを起動設定から制御できるようにする。
5. The Core Runtime shall 個々のリクエスト処理に関連するログを相関できる識別子を付与する。

### Requirement 8: テストハーネスの土台

**Objective:** AI 自律 TDD を行う実装者として、DB を伴う統合テストを安定して起動できる土台がほしい。これにより、各機能 spec がエンドポイント往復の統合テストをこの土台の上で書ける。

#### Acceptance Criteria

1. The Core Runtime shall データベースを伴う統合テストのために、テスト用ランタイムインスタンスを起動できる仕組みを提供する。
2. When 統合テストがテスト用インスタンスを起動するとき, the Core Runtime shall 埋め込みマイグレーションを適用済みのデータベース状態をテストへ提供する。
3. When 統合テストがテスト用インスタンスを起動するとき, the Core Runtime shall 非決定性境界（時刻・ID・乱数・署名鍵）を決定的な実装へ差し替えた状態で起動できるようにする。
4. While 複数の統合テストが実行される間, the Core Runtime shall 各テストが互いの永続データに干渉しないように分離されたデータベース状態を提供する。
5. When 統合テストが完了したとき, the Core Runtime shall テスト用に確保したリソースを解放する。

### Requirement 9: 共有ドメインプリミティブの正準定義

**Objective:** 後続 spec の実装者として、複数の並行 spec（accounts-and-instance / statuses-core / social-graph / notifications）が共通して必要とする軽量ドメインプリミティブを、唯一の正準定義として土台から受け取りたい。これにより、各 spec が同一プリミティブを独自に再定義して型不整合・変換コスト・境界の二重所有を生むことを防げる。

#### Acceptance Criteria

1. The Core Runtime shall ローカルアクターとリモートアクターを区別する軽量参照プリミティブ `AccountRef`（`Local` / `Remote`、識別子は core-runtime の `Id` 型）を、所有者情報や Account エンティティの知識を一切持たない純粋なプリミティブとして提供する。
2. The Core Runtime shall 投稿の公開範囲プリミティブ `Visibility`（`Public` / `Unlisted` / `Private` / `Direct`）の列挙と、その serde/文字列表現の対応付けを提供する。
3. Where `Visibility` に対する公開範囲ポリシー（VisibilityPolicy 等の振る舞い）が必要な場合, the Core Runtime shall その振る舞いを所有せず、列挙と表現の対応付けのみを所有する（振る舞いは statuses-core が所有する）。
4. The Core Runtime shall これら共有プリミティブを唯一の正準定義として所有し、後続 spec はそれらを再定義せず core-runtime から取り込む。
