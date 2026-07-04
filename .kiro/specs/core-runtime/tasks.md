# Implementation Plan

- [ ] 1. プロジェクト基盤の構築
- [x] 1.1 Rust クレートと依存・エントリポイント骨格を作成する
  - `Cargo.toml` に axum / tokio / sqlx(PostgreSQL) / tracing / tracing-subscriber / tower-http / toml 等の依存を定義する
  - `src/main.rs` を作成し、後続で実装する `bootstrap()` を呼んで終了コードへ変換する最小骨格を置く
  - `cargo check` が通り、空のバイナリがビルドできる状態になる
  - _Requirements: 1.1_
  - _Boundary: Bootstrap_

- [x] 1.2 マイグレーションディレクトリと初期マイグレーションを用意する
  - `migrations/` を作成し、連番命名・前方追加のみの追加規約に従う初期マイグレーション `0001_init_runtime.sql` を置く
  - 後続 spec がマイグレーションを追加できる土台として、最小限（または no-op に近い）の初期化のみを含める
  - `migrations/` が存在し sqlx の埋め込み対象として認識できる状態になる
  - _Requirements: 4.1_
  - _Boundary: Migrate_

- [ ] 2. 起動設定（Config）の実装
- [x] 2.1 起動設定の読込・マージ・検証を実装する
  - TOML ファイルと環境変数から設定を読み込み、環境変数を優先してマージし、検証済み不変構造体 `AppConfig`（server / database / log）を構築する
  - 必須項目（サーバードメイン・DB 接続先）の欠落と形式不正を区別し、どの項目が問題かを示す `ConfigError` を返す
  - 単体テストで「環境変数優先のマージ」「必須欠落で起動中止」「形式不正で起動中止」が観測できる
  - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.6_
  - _Boundary: Config_

- [x] 2.2 (P) シークレットのマスク型を実装する
  - シークレット値を保持する `Secret<T>` ラッパ型を実装し、`Debug`/`Display`/ログ出力で値を露出しないようにする
  - 単体テストで `Secret<T>` をフォーマットしても平文が出力されないことを確認できる
  - _Requirements: 2.5_
  - _Boundary: Config_
  - _Depends: 2.1_

- [ ] 3. 可観測性（Telemetry）の実装
- [ ] 3.1 構造化ログ基盤を初期化する
  - tracing subscriber を初期化し、ログレベルを `LogConfig` から制御できるようにする
  - リクエスト処理に相関 ID（request_id）を付与する span 方針と、診断レベルで sqlx の実行 SQL を tracing 経由で出力する設定を組み込む
  - プロセス起動時に一度だけ呼べる `init_telemetry()` が動作し、設定レベルに応じてログ出力が変化することを確認できる
  - _Requirements: 7.1, 7.3, 7.4, 7.5_
  - _Boundary: Telemetry_
  - _Depends: 2.1_

- [ ] 4. データ層の実装
- [ ] 4.1 データベース接続プールの確立を実装する
  - `DatabaseConfig` に従い接続プール（`PgPool`）を確立し、プールサイズと接続取得タイムアウトを設定値から適用する
  - 初回接続が確立できない場合は原因を保持した `DbError` を返し、HTTP リスナー開始前に起動を中止できるようにする
  - 統合テストで「接続成功でプールが得られる」「接続不可で起動中止」が観測できる
  - _Requirements: 3.1, 3.2, 3.3, 3.4_
  - _Boundary: Db_
  - _Depends: 2.1_

- [ ] 4.2 埋め込みマイグレーションの起動時自動適用を実装する
  - `migrations/` をバイナリに埋め込み、確立済みプールに対して未適用分を適用順に自動適用する（未適用が無ければ no-op）
  - 適用失敗時は失敗マイグレーションを特定できる情報とともに起動を中止し、`_sqlx_migrations` のチェックサム不整合を検出して起動を中止する
  - 統合テストで「適用後にスキーマが最新化される」「再起動で再適用されずデータが保持される」「不整合/失敗で起動中止」が観測できる
  - _Requirements: 4.2, 4.3, 4.4, 4.5, 4.6_
  - _Boundary: Migrate_
  - _Depends: 1.2, 4.1_

- [ ] 5. 非決定性注入境界（RuntimeContext）と共有ドメインプリミティブの実装
- [ ] 5.1 (P) 共有ドメインプリミティブの正準定義を実装する
  - `src/domain/primitives.rs` に、並行 spec 横断で共有される軽量プリミティブを唯一の正準定義として実装し、`src/domain/mod.rs` から再公開する
  - 識別子 `Id`（64bit 符号付き整数の newtype、生成時刻順に単調増加、serde は 10 進文字列表現、DB は `BIGINT` 列対応）を定義する。この `Id` は `IdGenerator`（5.3）や `KeyRef`（5.5）など後続コンポーネントが取り込む唯一の正準表現となる
  - `AccountRef`（`Local(Id)`/`Remote(Id)`）を、所有者情報を露出せず Account エンティティの知識を持たない純粋プリミティブとして定義する
  - `Visibility`（`Public`/`Unlisted`/`Private`/`Direct`）の列挙と serde/文字列表現の対応付けのみを定義し、公開範囲ポリシー（振る舞い）は含めない（statuses-core が所有）
  - 単体テストで「`Id` の serde 文字列表現と内部表現が可逆変換できる」「`Visibility` の serde/文字列表現が各バリアントで安定」「`AccountRef` がローカル/リモートを区別する」ことを確認できる
  - _Requirements: 9.1, 9.2, 9.3, 9.4_
  - _Boundary: DomainPrimitives_

- [ ] 5.2 (P) Clock 境界を実装する
  - `Clock` trait と本番実装（システム時刻）・決定的実装（固定時刻）を実装する
  - 単体テストで決定的実装が常に同一時刻を返すことを確認できる
  - _Requirements: 5.1_
  - _Boundary: RuntimeContext/clock_

- [ ] 5.3 (P) IdGenerator 境界を実装する
  - `IdGenerator` trait と本番実装・決定的実装（シード由来の連番）を実装し、5.1 で定義した正準 `Id` 型を払い出す
  - 単体テストで決定的実装が同一シードで同一 ID 列を再現することを確認できる
  - _Requirements: 5.2_
  - _Boundary: RuntimeContext/ids_
  - _Depends: 5.1_

- [ ] 5.4 (P) Rng 境界を実装する
  - `Rng` trait と本番実装・決定的実装（シード固定）を実装する
  - 単体テストで決定的実装が同一シードで同一バイト列を再現することを確認できる
  - _Requirements: 5.3_
  - _Boundary: RuntimeContext/rng_

- [ ] 5.5 (P) SigningKeyProvider 境界を実装する
  - 対象アクターの `Id`（5.1）を直接ラップする newtype `KeyRef(Id)` を single-key-per-actor 前提（鍵バージョン/世代を区別しない）で定義する
  - `SigningKeyProvider` trait（`KeyRef` を受け取り鍵を返す）とテスト用固定鍵実装を実装し、本番実装を actor-model が差し込む拡張点を残す
  - 単体テストでテスト用実装が固定鍵を再現的に返すことを確認できる
  - _Requirements: 5.4_
  - _Boundary: RuntimeContext/signing_key_
  - _Depends: 5.1_

- [ ] 5.6 RuntimeContext 集約を実装する
  - 4 つの境界を保持する `RuntimeContext` と、`production()`（本番実装）・`deterministic(seed)`（決定的実装）の構築関数を実装する
  - 単体テストで `deterministic` が同一シードで時刻/ID/乱数/鍵を再現し、`production` が本番実装で構築されることを確認できる
  - _Requirements: 5.5, 5.6_
  - _Boundary: RuntimeContext_
  - _Depends: 5.2, 5.3, 5.4, 5.5_

- [ ] 6. エラー基盤（Error）の実装
- [ ] 6.1 (P) 統一エラー型と HTTP レスポンス変換骨格を実装する
  - 横断利用する `AppError`（4xx=Client / 5xx=Server の分類）を定義し、axum `IntoResponse` でステータスと構造化本文へ変換する
  - 5xx では内部 source を本文に露出させず相関 ID 付きでログにのみ出力し、本文表現を api-foundation が拡張できる拡張点を残す
  - 単体テストで「4xx は public_message を返す」「5xx 本文に内部詳細が出ない」ことを確認できる
  - _Requirements: 6.1, 6.2, 6.3, 6.4, 6.5_
  - _Boundary: Error_
  - _Depends: 1.1_

- [ ] 7. サーバとライフサイクルの実装
- [ ] 7.1 AppState 共有ハンドルを実装する
  - `PgPool` + `RuntimeContext` + 設定参照を束ねた不変の共有ハンドル `AppState` を実装し、axum ステートとして共有できるようにする
  - 下流がプール・注入境界・設定値を `AppState` から取得できることを確認できる
  - _Requirements: 1.1, 3.3, 5.5, 5.6_
  - _Boundary: AppState_
  - _Depends: 4.1, 5.6_

- [ ] 7.2 土台ルータと TraceLayer を実装する
  - 最小の土台ルート（ヘルス確認）と後続が拡張する装着点を持つ axum ルータを組み立て、リクエスト/レスポンス診断と相関 ID を付与する TraceLayer を装着する
  - 統合テストでヘルス確認が応答し、リクエスト/レスポンスのログが request_id 付きで出力されることを確認できる
  - _Requirements: 1.1, 7.2_
  - _Boundary: Server_
  - _Depends: 7.1, 3.1, 6.1_

- [ ] 7.3 graceful shutdown を実装する
  - シグナル受信で新規受付を停止し、処理中リクエストの完了を猶予時間まで待ち、猶予超過時は強制停止してからプールを解放する
  - 統合テストで「in-flight リクエストが猶予内に完了する」「猶予超過で強制停止する」「停止後にプールが解放される」ことを確認できる
  - _Requirements: 1.3, 1.4, 1.5_
  - _Boundary: Server_
  - _Depends: 7.2_

- [ ] 7.4 Bootstrap composition root を実装する
  - config → telemetry → pool → migrate → runtime context → AppState → serve の順に依存を組み立て、いずれの初期化失敗も HTTP リスナー開始前に診断出力 + 非ゼロ終了に変換する
  - 統合テストで「正常時は待ち受け可能になる」「初期化失敗時は HTTP を開始せず非ゼロ終了する」ことを確認できる
  - _Requirements: 1.1, 1.2_
  - _Boundary: Bootstrap_
  - _Depends: 2.1, 3.1, 4.1, 4.2, 5.6, 7.3_

- [ ] 8. テストハーネスの土台
- [ ] 8.1 DB 込み統合テストハーネスを実装する
  - テストごとに分離された DB 状態（一意 DB/スキーマ名 or テンプレート DB からの作成）を用意し、埋め込みマイグレーションを適用済みの状態で、決定的 `RuntimeContext` を用いてテスト用インスタンスを起動する `spawn_test_app()` を実装する
  - 正規の解放経路として明示的な非同期 `cleanup()` を実装し、テストコードが必ずこれを呼んでプール解放・分離 DB 破棄・listener 停止を行える状態にする
  - `Drop` はベストエフォートのみに留める（分離 DB 破棄をデタッチしたバックグラウンドタスクへ委ねる、または次回起動時の起動時スイープに委ねる）。tokio ランタイム内の同期 `Drop::drop` から async 解放処理を `block_on` して panic させない
  - 統合テストハーネス自体を用いた最小テストが起動でき、適用済み DB と決定的注入が得られ、`cleanup()` 呼び出し後にリソースが解放されることを確認できる
  - _Requirements: 8.1, 8.2, 8.3, 8.4, 8.5_
  - _Boundary: TestHarness_
  - _Depends: 7.4_

- [ ] 9. 統合と検証
- [ ] 9.1 ライフサイクルとマイグレーション安全性の統合テストを追加する
  - `spawn_test_app` による起動でマイグレーション適用済み DB が得られヘルス確認が応答すること、テストコードが明示的に呼ぶ `TestApp::cleanup()` でリソースが解放されることを検証する
  - 既存データ保持・再適用なし（4.4）、適用失敗/チェックサム不整合での起動中止（4.5, 4.6）を検証する
  - テストがグリーンになり、起動〜停止の往復とマイグレーション安全性が観測できる
  - _Requirements: 1.1, 1.5, 4.4, 4.5, 4.6, 8.1, 8.2_
  - _Depends: 8.1_

- [ ] 9.2 起動失敗とエラー応答の統合テストを追加する
  - 必須設定欠落・DB 接続不可で HTTP リスナーを開始せず非ゼロ終了すること（1.2, 2.3, 3.2）を検証する
  - ハンドラが `AppError(Server)` を返したとき本文に内部詳細が出ず、相関 ID 付きでログに出ること（6.4, 7.5）を検証する
  - テストがグリーンになり、安全停止とエラー秘匿が観測できる
  - _Requirements: 1.2, 2.3, 3.2, 6.4, 7.5_
  - _Depends: 8.1_

- [ ] 9.3 テスト分離と graceful shutdown の検証テストを追加する
  - 2 つの統合テストが分離 DB で互いの永続データに干渉しないこと（8.4）を検証する
  - in-flight リクエストが猶予内に完了し取りこぼされないこと、猶予超過で強制停止すること（1.3, 1.4）を検証する
  - テストがグリーンになり、分離性と shutdown 挙動が観測できる
  - _Requirements: 1.3, 1.4, 8.4_
  - _Depends: 8.1_
