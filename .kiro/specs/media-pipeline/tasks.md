# Implementation Plan

- [ ] 1. 基盤の構築（永続スキーマと起動設定）
- [x] 1.1 メディアと処理ジョブのマイグレーションを追加する
  - `migrations/0004_media.sql` を連番命名・前方追加規約に従って追加し、`media`（識別子・所有アクター・種別・状態・説明・フォーカル座標・原寸法/サムネ寸法・BlurHash・原本/サムネのストレージキー・content_type・タイムスタンプ）と `media_processing_jobs`（対象メディア・状態・試行回数・`run_at`・`locked_at`・最終エラー）のテーブル・制約・インデックスを定義する
  - ジョブ取得効率のため、新規投入分（`queued` かつ `run_at` 到来）とリース期限切れの再取得対象（`processing` かつロック超過）の双方をカバーする `state`・`run_at` の複合インデックスを張り、メディアの `actor_id` にインデックスを張る
  - `spawn_test_app` 起動時にこのマイグレーションが適用され、2 テーブルが存在する状態になる
  - _Requirements: 1.2, 4.1, 4.2_
  - _Boundary: media migration_

- [x] 1.2 メディア関連の起動設定を追加する
  - core-runtime の起動設定へ、メディア保管ルート・アップロード上限サイズ・サムネイル目標寸法・対応形式一覧・ワーカー並行度/再試行上限・処理ジョブのリース期間（クラッシュしたワーカーからジョブを再取得するまでの猶予。既定は想定処理時間を十分に上回る値）を追加し、検証付きで読み込めるようにする
  - 設定読込でこれらの値が取得でき、必須/既定値の扱いが core-runtime の設定規約に沿うことを単体テストで確認できる
  - _Requirements: 1.4, 4.2, 5.2, 6.1_
  - _Boundary: core-runtime config_

- [ ] 2. ドメインモデルとストレージ/処理の抽象境界
- [x] 2.1 メディアドメインモデルを実装する
  - メディア・メディア種別・メディア状態（processing/ready/failed）・フォーカルポイント・寸法・派生メタ・処理ジョブのドメイン型を定義し、所有アクターを必須とする
  - フォーカルポイントを水平/垂直とも規約範囲（-1.0〜1.0）に制約し、未指定時の既定値を中央とする
  - 範囲内/範囲外のフォーカル値判定と既定値が単体テストで確認できる
  - _Requirements: 1.2, 4.1, 6.3, 7.1, 7.2_
  - _Boundary: model_
  - _Depends: 1.1_

- [x] 2.2 (P) ストレージ抽象境界とローカル FS 実装を実装する
  - 保管・取得・削除・公開 URL 生成を持つストレージ抽象（port）を定義し、ローカルファイルシステム実装を adapter として提供する。保管パスはメディア識別子由来で決定的にし、保管ルートは起動設定から取る
  - 公開 URL は api-foundation のプロキシ尊重ヘルパで外部ホスト名・スキームを反映した絶対 URL とする
  - ローカル FS で保管した実体が取得・削除でき、公開 URL がプロキシ情報を反映することを単体/統合テストで確認でき、呼び出し側が実装非依存であることを示せる
  - _Requirements: 5.1, 5.2, 5.3, 5.4, 5.5_
  - _Boundary: MediaStore, LocalFsStore_
  - _Depends: 1.2, 2.1_

- [x] 2.3 (P) 画像処理抽象と pure-Rust 実装を実装する（ネイティブ依存ゲート）
  - 入力バイト列からサムネイル・BlurHash・原寸法/アスペクトを生成する処理抽象（port）を定義し、pure-Rust（ネイティブ依存ゼロ）の画像処理 adapter を実装する。MVP は画像のみを対象とし、動画/音声処理のネイティブ依存を要求しない
  - 処理を抽象の背後に隔離し、将来 libvips 等のネイティブ依存実装へ呼び出し側変更なしに差し替えられる境界を維持する（pure-Rust 採用/動画後回し/抽象隔離の判断は research.md に記録済み）
  - 同一入力 + 固定パラメータで同一の BlurHash・サムネイル・寸法が再現され、未対応/破損入力が明示的エラーになることを単体テストで確認できる
  - _Requirements: 6.1, 6.2, 6.3, 6.4, 6.5, 10.1, 10.2, 10.3, 10.4_
  - _Boundary: MediaProcessor, PureRustImageProcessor_
  - _Depends: 2.1_

- [ ] 3. データ層（メディア永続・ジョブキュー）
- [x] 3.1 メディアリポジトリを実装する
  - メディアの挿入（所有アクター必須）、所有スコープ付き取得（メディア識別子 + アクターで他者メディアを返さない）、説明/フォーカルの更新、状態とメタ（派生寸法・BlurHash）の反映を実装する
  - 識別子は core-runtime の ID 境界、時刻は時刻境界から取得する
  - 挿入したメディアが所有アクターでのみ取得でき、状態とメタの更新が反映されることを統合テストで確認できる
  - _Requirements: 1.1, 1.2, 2.2, 2.3, 2.4, 3.1, 3.3, 4.3_
  - _Boundary: MediaRepository_
  - _Depends: 1.1, 2.1_

- [x] 3.2 (P) 処理ジョブキューを実装する
  - ジョブ投入と、`FOR UPDATE SKIP LOCKED` による排他取得（新規投入分＝`queued` かつ `run_at` 到来、またはリース期限切れの再取得＝`processing` かつ `locked_at` がリース期間を超過したジョブの reclaim）、完了化、一時失敗時の試行回数加算と指数バックオフによる `run_at` 後退、再試行上限到達時の失敗化を実装する
  - reclaim（クラッシュしたワーカーからの再取得）は通常の失敗経路と同じ試行回数会計に乗せるため、reclaim 時にも試行回数を加算する
  - 時刻は時刻境界から取得し、メディア状態を真実源として冪等な取得を保証する
  - 2 つのワーカーが同一ジョブを同時取得しないこと、バックオフで再試行されること、上限到達でジョブが失敗化すること、ロック期限切れのジョブが reclaim されて試行回数が加算されることを統合テストで確認できる
  - _Requirements: 4.1, 4.2, 4.4, 4.5, 4.6_
  - _Boundary: ProcessingJobQueue_
  - _Depends: 1.1, 1.2, 2.1_

- [ ] 4. サービス・シリアライザ・ワーカー
- [x] 4.1 メディアサービスを実装する
  - 受理（形式/サイズ検証 → 原本保管 → メディア挿入を processing 状態で行い → 処理ジョブ投入）、所有スコープ付き状態取得、説明/フォーカル更新（処理中でも受付、範囲外は拒否）を業務として集約する
  - 未対応形式・上限超過・フォーカル範囲外を検証エラーとして拒否し、識別子/時刻は決定性境界から取得する
  - 有効入力で processing 状態のメディアが作られジョブが投入されること、不正入力が拒否されることを統合テストで確認できる
  - _Requirements: 1.1, 1.3, 1.4, 1.5, 1.6, 2.1, 2.2, 3.1, 3.2, 7.4_
  - _Boundary: MediaService_
  - _Depends: 2.2, 3.1, 3.2_

- [x] 4.2 (P) MediaAttachment シリアライザを実装する
  - メディア表現を Mastodon 互換の MediaAttachment JSON（識別子・種別・実体 URL・プレビュー URL・remote_url・meta(original/small/focus)・説明・BlurHash）にシリアライズし、処理中は実体 URL を null、フォーカル未指定は既定中央、remote_url は MVP 常に null とする。URL はストレージ抽象のプロキシ尊重 URL を用いる
  - この契約を api-foundation の契約テストハーネスにゴールデンとして登録できる形にする
  - 処理中で url=null・完了で実体/プレビュー URL・focus 既定中央が出力されることを単体テストで確認できる
  - _Requirements: 2.2, 7.2, 7.3, 8.1, 8.2, 8.3, 8.4_
  - _Boundary: MediaAttachmentSerializer_
  - _Depends: 2.1, 2.2_

- [ ] 4.3 (P) 処理ワーカーを実装する
  - 常駐ループでジョブを排他取得し、原本取得 → 画像処理（サムネイル/BlurHash/寸法）→ 派生物保管 → メディアを ready 化 → ジョブ完了、という流れを実行する。一時失敗は再試行/バックオフ、復号失敗や上限到達はメディアを failed 化し診断情報を出力する
  - メディア状態を真実源に、再実行時に重複派生物や不整合を生まない冪等処理とする。graceful shutdown はライフサイクルに従う一方、ワーカーが応答なく停止（クラッシュ）した場合はロック解放処理を待たず、リース期間超過後の reclaim によって別ワーカーが自動的に引き継いで復旧する
  - 投入されたジョブが処理されてメディアが ready 化し派生物が保管されること、失敗時に failed 化と再試行が機能すること、ロックされたまま応答しないジョブがリース期間経過後に別ワーカーへ再取得されることを統合テストで確認できる
  - _Requirements: 4.2, 4.3, 4.4, 4.5, 4.6, 6.1, 6.5_
  - _Boundary: ProcessingWorker_
  - _Depends: 2.3, 3.1, 3.2, 2.2_

- [ ] 5. エンドポイントと配線
- [ ] 5.1 メディアエンドポイントを実装する
  - アップロード（multipart 受理、`write:media` スコープ要求、受理で 202・実体 URL null）、状態取得（`write:media` スコープ要求、処理中 206 / 完了 200 / 処理失敗は互換エラー本文で 422、未存在・非所有は 404）、メタ更新（範囲検証後 200、範囲外 422）の HTTP 表層を実装し、失敗は api-foundation の Mastodon 互換エラー本文で返す
  - 認証は Bearer 認証ミドルウェアと共通スコープ内包判定を再利用し、欠如で 401・スコープ不足で 403 とする（アップロード・取得・更新のいずれも `write:media` を要求する）
  - 各エンドポイントが規定の応答コードと互換エラー本文を返すこと、処理失敗状態のメディアの取得が `{"error": "..."}` 形の 422 を返すことを統合テストで確認できる
  - _Requirements: 1.1, 2.1, 2.2, 2.3, 3.1, 3.2, 6.5, 9.1, 9.2, 9.3, 9.4_
  - _Boundary: MediaEndpoints_
  - _Depends: 4.1, 4.2_

- [ ] 5.2 メディアモジュールをランタイムへ配線する
  - core-runtime の Composition Root（状態・起動・サーバ）を拡張し、メディアのリポジトリ/キュー/ストア/プロセッサ/サービスを構築して共有状態に格納し、常駐ワーカーを起動し、メディアエンドポイントを mount して api-foundation の横断レイヤー（認証・エラー変換・レート制限）が適用される装着点に乗せる
  - これは複数コンポーネントとワーカーを束ねる明示的な統合タスクであり、レート制限ヘッダがメディア応答にも一貫適用される
  - 起動後にメディアエンドポイントが到達可能で、ワーカーが稼働し、アップロード→ポーリングで派生物が反映され、`X-RateLimit-*` が付与されることを統合テストで確認できる
  - _Requirements: 1.1, 4.1, 9.5_
  - _Boundary: MediaModule wiring_
  - _Depends: 4.3, 5.1_

- [ ] 6. 検証（統合・契約テスト）
- [ ] 6.1 アップロード・ポーリング・更新の統合テストを実装する
  - 有効画像のアップロードで 202（url=null）と所有アクター結びつけ、未対応形式/上限超過で 422、処理後のポーリングで 206→200、処理失敗状態のメディアの取得で互換エラー本文の 422、未存在で 404、他アクターからの不可視、説明/フォーカル更新の反映と範囲外 422・処理中更新・非所有 404 を検証する
  - 受理・取得・更新の一連の利用者操作が期待ステータスと表現で応答することを確認できる
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 2.1, 2.2, 2.3, 2.4, 3.1, 3.2, 3.3, 3.4, 6.5_
  - _Depends: 5.2_

- [ ] 6.2 (P) 処理・キュー・ストレージの統合テストを実装する
  - ワーカーによる派生物生成と ready 化、一時失敗時の再試行とバックオフ、上限到達時の failed 化、再実行で重複派生物を生まない冪等性、ロックされたままリース期間を超過したジョブが別ワーカーに reclaim（試行回数加算込み）されること、ローカル FS の保管/取得/削除とプロキシ尊重 URL を検証する
  - 非同期処理とストレージ抽象が期待どおりに動作し、失敗・再試行・reclaim・冪等が成立することを確認できる
  - _Requirements: 4.2, 4.3, 4.4, 4.5, 4.6, 5.1, 5.2, 5.3, 5.4, 6.1, 6.5_
  - _Boundary: ProcessingWorker, ProcessingJobQueue, MediaStore_
  - _Depends: 5.2_

- [ ] 6.3 (P) MediaAttachment 契約テストを実装する
  - 決定的な非決定性境界の上で、処理中（url=null）と完了の双方の MediaAttachment JSON ゴールデンを固定し、focus 既定中央・meta（original/small）・BlurHash・寸法・null 規律が再現可能に一致することを契約ハーネスで検証する
  - 同一入力で安定したゴールデン比較が成立し、契約のドリフトが検出されることを確認できる
  - _Requirements: 6.2, 6.3, 6.4, 7.1, 7.2, 7.3, 8.1, 8.2, 8.3, 8.4, 10.1, 10.3_
  - _Boundary: MediaAttachmentSerializer_
  - _Depends: 5.2, 4.2_

## Implementation Notes

- 1.1: マイグレーションファイル名はタスク本文記載の `0004_media.sql` ではなく `0005_media.sql` を使用した。`0001`〜`0004` は実装済み（`0004` は federation-core が先に確保）で、design.md のファイル番号は `/kiro-spec-batch` 生成時に spec 毎に独立採番されたものであり実装順を反映しない（`migrations/0004_federation.sql` のヘッダコメントに同様の説明が既にある）。今後 media-pipeline の設計文書を参照するタスクは、マイグレーション番号のみ実ファイル `0005_media.sql` に読み替えること。
- 2.1: このサンドボックスでは PostgreSQL が既定で起動していない（`pg_isready` が無応答）。DB 依存テスト（`spawn_test_app` 経由の統合テスト等）を含むフルスイートを実行する前に `service postgresql start` が必要。`media::model` のような DB 非依存の単体テストはこれと無関係に通る。design.md の `model` 型定義（抜粋）は `Focus { pub x, pub y }` と公開フィールドで示すが、範囲外構築を型で拒否するには `x`/`y` を非公開にし `Focus::new(x, y) -> Result<Focus, FocusRangeError>` のフォールブルコンストラクタを介す必要がある（抜粋は API を厳密に強制するものではない）。今後 `Focus` を消費するタスク（2.2 のシリアライズ、4.1 のサービス等）はこのフォールブルコンストラクタ/アクセサ経由で扱うこと。
- 2.2: design.md の `public_url(&self, key: &ObjectKey, req_uri: &RequestUriContext) -> String` 型シグネチャの `RequestUriContext`（`src/api/pagination.rs`）はフィールド非公開でページネーション `Link` ヘッダ専用の私有 `url_with` しか持たず、単純な絶対 URL 生成には使えない。代わりに同ファイルの公開型 `ForwardedOrigin::resolve(...)` （プロキシ転送ヘッダ由来の scheme/host 解決のみを担う分離されたプリミティブ）を使用した。今後 `MediaStore::public_url` や同種のプロキシ尊重 URL 生成を消費/拡張するタスクはこの `ForwardedOrigin` を再利用すること。また `MediaStore` トレイトはこのクレートの既存慣例（`ReceivedActivityStore` 等）に倣い `#[allow(async_fn_in_trait)]` のネイティブ async fn を用いており、`dyn` オブジェクト安全ではない（`Arc<dyn MediaStore>` が必要になった場合は later task 側でボクシングを追加する）。
- 2.3: 画像処理は `image = "0.25.10"`（`default-features = false`, `features = ["jpeg", "png", "gif", "webp"]`。`avif` 等の重い/不要フォーマットを明示的に除外）と `blurhash = "0.2.3"`（既定機能のみ。`gdk-pixbuf`/`image` 統合機能は任意でオフ）を採用した。両クレートとも `cargo tree` で確認済みでネイティブ/`*-sys` 依存を持たない（Requirement 10.2 の pure-Rust ゲートを満たす）。`ThumbnailSpec` は design.md の抜粋にフィールドが無いため `MediaConfig::thumbnail_target_width`/`thumbnail_target_height` から `target_width`/`target_height: u32` を推論した。`ProcessedImage::content_type` は常に `"image/png"`（サムネイル自体のエンコード形式。原本の content_type とは別物で、アップロード検証側が別途保持する）。サムネイル生成は原寸を超えて拡大しない（`fit_within` ヘルパ、`DynamicImage::resize`/`thumbnail` は使わない）。今後 `MediaProcessor`/`PureRustImageProcessor` を消費するタスク（4.1 のサービス、4.3 のワーカー等）はこれらの型/挙動を前提にすること。
- 3.1: design.md の `MediaRepository` Service Interface 抜粋 `insert_media(pool: &PgPool, media: &Media) -> Result<(), AppError>` は単一の `&Media` 引数のみだが、`migrations/0005_media.sql` の `media.object_key`/`media.content_type` はいずれも `NOT NULL` であり、`Media`（task 2.1、`src/media/model.rs`、本タスクでは変更していない）には `object_key`/`thumb_key`/`content_type` に相当するフィールドが一切存在しない（task 2.1 自身のモジュールコメントが明言する通り、ストレージ層の関心はドメイン型に持ち込まない設計判断）。`&Media` 単体ではこれら `NOT NULL` 列を埋められないため、`insert_media` は `object_key: &str, content_type: &str` の 2 引数を追加した拡張シグネチャとした（`code_repository.rs::insert_code`/`consume_code` が `token_hash_key: &TokenHashKey` を追加した前例、および task 2.2 が `MediaStore::public_url` の引数を `&RequestUriContext` から実際に使える `&ForwardedOrigin` に差し替えた前例と同じ「抜粋が不完全/実態と齟齬がある場合はスキーマと後続タスクの必要に沿って拡張し、理由を記録する」方針を踏襲）。`thumb_key` は挿入時点では常に `NULL`（挿入直後の `processing` メディアにサムネイルはまだ存在しない）とし、生成後に埋めるのは `set_ready` の役目とした。同じ理由で `set_ready` は design.md の抜粋 `set_ready(pool, media_id, meta, blurhash) -> Result<(), AppError>` に `thumb_key: &str` を追加している（`thumb_key` 列を埋める手段が他に無いため）。また `update_metadata`/`set_ready`/`set_failed` はいずれも design.md の抜粋に無い `now: OffsetDateTime` 引数を追加した：`media.updated_at`（`NOT NULL`）を本タスクの受け入れ文言「時刻は時刻境界から取得する」に従い `Clock` 境界から供給する必要があり、`actor/repository.rs::update_state` が既に確立した同じ規約（呼び出し側が `RuntimeContext.clock` 由来の `now` を渡す）を転用した。`object_key`/`thumb_key`/`content_type` はいずれもストレージ層の関心のまま `Media` ドメイン型には持ち込んでおらず、`find_owned`/`update_metadata` が返す `Media` にもこれらは現れない。今後 `MediaService`（4.1）・`ProcessingWorker`（4.3）・`MediaEndpoints`（5.1）等 `MediaRepository` を消費するタスクは、この拡張シグネチャ（`insert_media` の `object_key`/`content_type`、`update_metadata`/`set_ready`/`set_failed` の `now`、`set_ready` の `thumb_key`）を前提にすること。`find_owned` はオーナースコープの `SELECT ... WHERE id = $1 AND actor_id = $2` を単一クエリで行い、「存在しない」と「存在するが他者所有」を区別可能な形では返さない（両方とも `Ok(None)`）ことで design.md の postcondition（「他アクターのメディアを返さない」）を構造的に満たしている。
- 3.2: design.md の `ProcessingJobQueue` Service Interface 抜粋は `fail_or_retry` の戻り値を裸の `JobOutcome`（コメント `// Retried | Failed`）とだけ記すのみで、この型の定義・所属モジュールをどこにも明示していない（他コンポーネントからの参照/再エクスポートも無い）。`fail_or_retry` 自身の結果報告以外に用途が無いため、`src/media/job_queue.rs` にペイロード無しの 2 バリアント enum（`Retried`/`Failed`）としてその場で定義した（抜粋の裸の `Retried | Failed` 表記に忠実。呼び出し側が `run_at`/状態の実値を要るときは `claim_due` 等で再取得する想定）。`attempts` 会計は「reclaim 時のみ `claim_due` が加算し、新規 `queued` ジョブの初回取得では加算しない／`fail_or_retry` は呼ばれる度に必ず 1 加算する」という規約に確定した（design.md の "reclaim... の会計と整合させる" 文言と、本タスク本文の "reclaim 時にも試行回数を加算する" の両方を満たす最小の解釈）。`claim_due` の `lease_duration: Duration` は Postgres `INTERVAL` としてバインドせず、Rust 側で `now - lease_duration` を計算して `OffsetDateTime` として直接バインドした（sqlx 経由の `INTERVAL` エンコードを新たに導入する理由が無く、減算はどちらの側で行っても同じ結果になるため）。`claim_due` のクエリは `src/federation/outbound/queue.rs::DbDeliveryQueue::claim_due` と同型の単一原子的 `UPDATE ... WHERE id IN (SELECT ... FOR UPDATE SKIP LOCKED) RETURNING ...` とし、`WHERE (state='queued' AND run_at<=$1) OR (state='processing' AND locked_at<$2)` の述語形を `migrations/0005_media.sql` の `media_jobs_due_idx`（`(state, run_at) WHERE state IN ('queued','processing')`）がそのまま活用できる形に合わせた。バックオフは Requirements が具体的な式/上限を指定していないため、同ファイルの `backoff_delay` 前例（倍加・上限あり）を踏襲しつつ、media 独自の定数 `DEFAULT_MEDIA_BASE_DELAY`（15 秒）・`DEFAULT_MEDIA_MAX_DELAY`（30 分）を新設した（federation の配送再試行 30 秒/6 時間よりも短く設定：メディア処理はローカル完結でありポーリングで結果を待つ利用者体験を考慮）。`complete` は `media_processing_jobs.state` に `done` 相当の値が無い（`JobState` に `Completed`/`Done` バリアントが無いという task 2.1 の確定済み設計）ため、行を `DELETE` する形で完了化を実装した。`fail_or_retry`/`complete` は呼び出し元がすでに排他保持しているジョブに対してのみ呼ばれる前提のため、`FOR UPDATE`/サブクエリを使わない単純な `UPDATE ... WHERE id = $n` とした（`DbDeliveryQueue::mark_done`/`reschedule`/`mark_failed` と同じ理由）。今後 `MediaService`（4.1）・`ProcessingWorker`（4.3）は `enqueue`/`claim_due`/`complete`/`fail_or_retry` をこの拡張済みシグネチャ・会計規約のまま消費すること。
- 3.2 レビュー所見: `fail_or_retry` は `media_processing_jobs.last_error` を書き込まない（design.md のシグネチャにもエラーメッセージ引数が無い）。Requirement 4.5 の「原因特定に十分な診断情報を出力する」は design.md 上 `ProcessingWorker`（task 4.3）の責務として設計されているため 3.2 の受け入れ基準違反ではないが、4.3 実装時にこの列への書き込み経路が実際に用意されることを確認すること（未着手のまま見落とされるとこの要件が永久に未充足のままになる）。
- 4.1: design.md は `UploadInput`/`MetadataPatch` のフィールドを定義していないため、`MediaService`（`src/media/service.rs`）実装時に以下で確定した: `UploadInput { bytes: Vec<u8>, content_type: String, description: Option<String>, focus: Option<(f32, f32)> }`、`MetadataPatch { description: Option<String>, focus: Option<(f32, f32)> }`。`focus` はどちらも未検証の生座標 `(f32, f32)` であり、`Focus::new` によるフォールブル検証と範囲外エラーの `AppError`（422）への変換は `MediaService` 自身が一元的に担う（`accept_upload`/`update_metadata` 双方で同じ `validate_focus` ヘルパを経由）。今後 `MediaService` を消費するタスク（4.2 のシリアライズ、5.1 のエンドポイント等）はこの契約を前提に、事前検証済み `Focus` ではなく生座標を渡すこと。また `MediaService<S: MediaStore>` は `MediaStore` が `#[allow(async_fn_in_trait)]` で `dyn` 安全でない（2.2 の既存知見）ため `Arc<dyn MediaStore>` ではなくジェネリック型パラメータとした（`src/federation/` の `DeliveryWorker<Q, H>` 等の前例を踏襲）。5.2（ランタイム配線）で `AppState` に格納する際は具体型（`LocalFsStore` 想定）を選ぶ必要がある。`media_type` は `UploadInput` の明示フィールドではなく `content_type` から推論する（`image/*` → `MediaType::Image`、それ以外は `Unknown`）。
- 4.2: `MediaAttachmentSerializer`（`src/media/serializer.rs`）は `url` を `state == Ready` のみで確定させ、`preview_url`/`meta.small` は別途 `meta.small.is_some()` を条件にした（`state` の再チェックではなく、確定済みメタの有無で判定する方が Requirement 8.2「確定済みのメタデータのみを含める」の文言に忠実で、`Ready` かつ `meta.small` 未確定という型上は許容される不変条件違反状態でも panic せず `preview_url=null` を返せるため）。`meta.original`/`meta.small` は未確定時 `null` ではなくフィールド省略（`skip_serializing_if`）とした。本 spec は `src/contract.rs` の契約ハーネス（`assert_golden`、`tests/golden/<spec>/*.json` を仕様側が所有）の最初の実利用例であり、ゴールデン（`tests/golden/media/media_attachment_{processing,ready}.json`）は `KAWASEMI_UPDATE_GOLDEN=1` を対象 2 テストのみに絞った一回限りの手動実行で生成し、通常のテスト実行では環境変数トグルを埋め込まない（`tests/contract_harness_it.rs` が明文化する「この方式は隔離された単体テストバイナリでのみ安全」という方針を踏襲）。今後 `MediaService` 経由でこのシリアライザを消費するタスク（6.3 の契約統合テスト等）はこの規約を前提にすること。
