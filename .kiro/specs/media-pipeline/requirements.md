# Requirements Document

## Introduction

media-pipeline は、投稿（statuses-core）・アバター/ヘッダ（accounts-and-instance）・カスタム絵文字（custom_emojis）が共通して依存する「メディア添付」の土台を確立する spec である。Mastodon 互換 API のメディアは「非同期アップロード（`202` 受理 → 処理完了をポーリング）」を前提とし、添付には BlurHash・フォーカルポイント・サムネイル等の派生メタデータが伴う。これらが無いと添付を要する下流機能はいずれも完成しない。

本 spec が完了すると、標準クライアント（Ivory・Elk・Phanpy 等）がメディアを非同期アップロードでき、サーバーは DB ジョブキューで派生物（サムネイル・BlurHash・寸法メタ）を生成し、処理完了をポーリングで取得でき、フォーカルポイントと説明文を更新でき、メディア実体はストレージ抽象境界の背後に置かれた状態になる。アップロードはオーナーのアクターに結びつくが、投稿本体への紐付けは行わない（statuses-core が消費する）。

加えて本 spec は、メディア処理のネイティブ依存（libvips・ffmpeg 等）を許容する範囲を明示的に決める「判断ゲート」を持つ。MVP は画像のみを対象とし、その処理を pure-Rust で賄えるかを判断して処理抽象の背後に隔離し、配布形態（distribution）が散在するネイティブ依存に縛られないようにする。動画処理は後回しとする。

最重要の設計上の論点は (1) 非同期処理を外部ジョブブローカーに頼らず DB キューで完結させること、(2) ストレージとメディア処理をそれぞれ抽象境界の背後に置き後から差し替え可能にすること、(3) ネイティブ依存の許容範囲を配布の容易さと両立させて決定することの 3 点である。

## Boundary Context

- **In scope**: 非同期メディアアップロード API（`202` 受理 → ポーリング取得 → メタデータ更新）、DB ジョブキューによる非同期メディア処理（投入・取得・再試行・冪等・失敗状態）、ストレージ抽象境界（ローカルファイルシステム実装 + 後で差し替え可能なインターフェース）、画像派生物の生成（サムネイル・BlurHash・原寸法/アスペクトメタ）、フォーカルポイント、MediaAttachment エンティティ JSON 契約（api-foundation 契約ハーネスへの登録を含む）、メディア API への認証/スコープ/エラー/レート制限の互換適用、画像処理のネイティブ依存判断ゲートと処理抽象。
- **Out of scope**: 動画/音声処理の本格対応（後回し。判断ゲートでネイティブ依存範囲のみ決定し実処理は行わない）、配布パッケージング・単一バイナリ/Docker の実装そのもの（distribution。本 spec は依存判断結果を渡すのみ）、投稿本体・アバター/ヘッダ・カスタム絵文字へのメディア紐付けロジック（statuses-core / accounts-and-instance / custom_emojis が消費）、リモート連合経由のメディア取り込み（federation-core 以降）、未添付メディアの長期保持・クリーンアップ運用ポリシー（後続 spec/運用設定）。
- **Adjacent expectations**: 本 spec は core-runtime が提供する起動・DB プール・埋め込みマイグレーション基盤・非決定性 DI 境界（時刻 / ID / 乱数）・統一エラー型・構造化ログ・テストハーネス（`spawn_test_app`）に依存する。また api-foundation が所有する Bearer 認証ミドルウェア（単一アクター文脈 + 承認スコープの供給）、スコープ内包判定、Mastodon 互換エラー JSON 形、`X-RateLimit-*` ヘッダ規約、ページネーション規約、リバースプロキシ後段での外部ホスト名/スキーム尊重、エンティティ契約テストハーネスを消費する。本 spec はこれらの横断規約そのものを所有せず、メディア固有のエンドポイント・処理・データに適用する。下流の statuses-core / accounts-and-instance / custom_emojis は、本 spec が確立するメディア識別子・MediaAttachment 契約・処理完了状態に依存する。

## Requirements

### Requirement 1: 非同期メディアアップロードの受理

**Objective:** 標準クライアントとして、メディアファイルをアップロードして即座に受理応答を受け取りたい。これにより、処理完了を待たずに投稿作成フローを継続できる。

#### Acceptance Criteria

1. When 認証済みクライアントが有効なメディアファイルを含むアップロード要求を送信したとき, the Media Pipeline shall アップロードされた原本を保管し、メディア識別子を採番し、処理が未完了であることを示すメディア表現（メディア実体 URL が未確定の状態）とともに受理応答（`202` 相当）を返す。
2. When アップロード要求が受理されたとき, the Media Pipeline shall 当該メディアを要求元の単一アクターに結びつけて記録する。
3. If アップロード要求のファイルが欠落している、または未対応の形式であるとき, then the Media Pipeline shall メディアを保管せず、Mastodon 互換のエラー応答（入力検証失敗・422 相当）で要求を拒否する。
4. If アップロードされたファイルのサイズが規約上の上限を超えるとき, then the Media Pipeline shall メディアを保管せず、Mastodon 互換のエラー応答で要求を拒否する。
5. Where アップロード要求が説明文またはフォーカルポイントを伴う場合, the Media Pipeline shall それらをメディアに紐づけて記録する。
6. When アップロードが受理されたとき, the Media Pipeline shall 当該メディアの派生物生成を非同期処理として投入する。

### Requirement 2: 処理状態のポーリング取得

**Objective:** 標準クライアントとして、アップロードしたメディアの処理状態をポーリングで取得したい。これにより、処理完了後に投稿へ添付できる。

#### Acceptance Criteria

1. When クライアントが処理未完了のメディアの状態取得を要求したとき, the Media Pipeline shall まだ処理中であることを示す応答（`206` 相当）を、現時点で確定しているメタデータとともに返す。
2. When クライアントが処理完了済みのメディアの状態取得を要求したとき, the Media Pipeline shall メディア実体 URL・プレビュー URL・派生メタデータを含む完成したメディア表現を成功応答（`200` 相当）として返す。
3. If クライアントが存在しないメディア識別子の状態取得を要求したとき, then the Media Pipeline shall Mastodon 互換の未検出エラー応答（404 相当）を返す。
4. If クライアントが自身に結びつかないメディアの状態取得を要求したとき, then the Media Pipeline shall そのメディアを参照させず、未検出または権限エラーの Mastodon 互換応答を返す。

### Requirement 3: メディアメタデータの更新

**Objective:** 標準クライアントとして、アップロード済みメディアの説明文とフォーカルポイントを更新したい。これにより、投稿前に表示位置や代替テキストを調整できる。

#### Acceptance Criteria

1. When 認証済みクライアントが自身のメディアの説明文またはフォーカルポイントの更新を要求したとき, the Media Pipeline shall 指定された値で当該メディアのメタデータを更新し、更新後のメディア表現を返す。
2. If 更新要求のフォーカルポイント座標が許容範囲外であるとき, then the Media Pipeline shall 更新を行わず、Mastodon 互換のエラー応答で要求を拒否する。
3. If クライアントが自身に結びつかないメディアの更新を要求したとき, then the Media Pipeline shall 更新を行わず、未検出または権限エラーの Mastodon 互換応答を返す。
4. While メディアが処理中であっても, the Media Pipeline shall 説明文およびフォーカルポイントの更新を受け付ける。

### Requirement 4: DB ジョブキューによる非同期メディア処理

**Objective:** 一人鯖の運用者として、外部のジョブブローカーを導入せずにメディア処理を非同期で安定して実行したい。これにより、ランタイムを「アプリ + PostgreSQL」のみに保てる。

#### Acceptance Criteria

1. The Media Pipeline shall メディア処理ジョブをデータベース上のキューで管理し、外部のジョブブローカーやメッセージキューを必要としない。
2. When メディア処理ジョブが投入されたとき, the Media Pipeline shall ワーカーが当該ジョブを他のワーカーと競合せずに排他的に取得して処理する。
3. When メディア処理ジョブが正常に完了したとき, the Media Pipeline shall 当該メディアを処理完了状態に遷移させ、生成した派生物とメタデータを反映する。
4. If メディア処理ジョブが一時的な失敗で終了したとき, then the Media Pipeline shall 規約に従った再試行を行い、再試行間隔を後退（バックオフ）させる。
5. If メディア処理ジョブが再試行上限に達しても完了しないとき, then the Media Pipeline shall 当該メディアを処理失敗状態に遷移させ、原因特定に十分な診断情報を出力する。
6. While 同一ジョブが再実行される間, the Media Pipeline shall 重複した派生物や不整合な状態を生まないよう冪等に処理する。

### Requirement 5: ストレージ抽象境界

**Objective:** 後続 spec の実装者および運用者として、メディア実体の保管先を抽象境界の背後に置きたい。これにより、初期はローカルファイルシステムで動作させ、後から別のストレージへ差し替えられる。

#### Acceptance Criteria

1. The Media Pipeline shall メディア実体の保管・取得・削除を抽象インターフェースの背後に置き、呼び出し側を特定のストレージ実装に直接依存させない。
2. The Media Pipeline shall 抽象インターフェースの具体実装としてローカルファイルシステム実装を提供する。
3. When メディアまたは派生物が保管されたとき, the Media Pipeline shall 当該実体へアクセスするための URL を生成して提供する。
4. While 公開 URL を生成する間, the Media Pipeline shall リバースプロキシ後段での外部ホスト名・スキームを尊重した絶対 URL を生成する。
5. Where 将来別のストレージ実装へ差し替える場合, the Media Pipeline shall 呼び出し側の変更なしに具体実装を入れ替えられる境界を維持する。

### Requirement 6: 画像派生物の生成

**Objective:** 標準クライアントのユーザーとして、アップロードした画像にサムネイルとプレースホルダ表現が付与されてほしい。これにより、タイムライン上で軽量・低遅延に画像を表示できる。

#### Acceptance Criteria

1. When 画像メディアの処理が実行されたとき, the Media Pipeline shall 規約上の寸法に縮小したサムネイル（プレビュー）を生成して保管する。
2. When 画像メディアの処理が実行されたとき, the Media Pipeline shall 当該画像の BlurHash を生成して記録する。
3. When 画像メディアの処理が実行されたとき, the Media Pipeline shall 原画像の幅・高さ・アスペクト比およびサムネイルの寸法メタデータを記録する。
4. While 同一の入力画像と同一の決定的境界で処理する間, the Media Pipeline shall 同一の BlurHash・サムネイル・寸法メタデータを再現可能に生成する。
5. If 画像の復号または派生物生成が失敗したとき, then the Media Pipeline shall 当該メディアを処理失敗状態に遷移させ、診断情報を出力する。

### Requirement 7: フォーカルポイント

**Objective:** 標準クライアントのユーザーとして、画像のフォーカルポイント（注目点）を指定したい。これにより、クライアント側のトリミング表示で重要な被写体が残るようにできる。

#### Acceptance Criteria

1. The Media Pipeline shall フォーカルポイントを水平・垂直それぞれ規約範囲の座標値として保持する。
2. When フォーカルポイントが指定されていないメディアの表現を返すとき, the Media Pipeline shall 規定の既定値（中央）をフォーカルポイントとして返す。
3. When フォーカルポイントが指定されたメディアの表現を返すとき, the Media Pipeline shall 記録された座標値をメディア表現に含めて返す。
4. If 指定されたフォーカルポイント座標が許容範囲外であるとき, then the Media Pipeline shall その指定を受理せず、Mastodon 互換のエラー応答で拒否する。

### Requirement 8: MediaAttachment エンティティ JSON 契約

**Objective:** AI 自律 TDD を行う実装者および下流 spec として、メディア添付の JSON 形を固定したい。これにより、添付を消費する下流機能が安定した契約に乗れる。

#### Acceptance Criteria

1. The Media Pipeline shall メディア表現を、識別子・種別・メディア実体 URL・プレビュー URL・寸法メタ・フォーカルポイント・説明文・BlurHash を含む Mastodon 互換の MediaAttachment JSON 形として返す。
2. When メディアが処理未完了であるとき, the Media Pipeline shall メディア実体 URL を未確定（null 相当）として表現し、確定済みのメタデータのみを含める。
3. The Media Pipeline shall この MediaAttachment 契約を、共通のエンティティ契約テストハーネスにゴールデンとして登録し、出力ドリフトを検出可能にする。
4. While 決定的な非決定性境界の上で契約テストを実行する間, the Media Pipeline shall 再現可能なゴールデンを生成する。

### Requirement 9: 認証・スコープ・エラー・レート制限の互換適用

**Objective:** 標準クライアントとして、メディア API でも他の API と同じ認証・エラー・レート制限の規約が一貫して適用されてほしい。これにより、クライアントが特別扱いなしにメディア機能を利用できる。

#### Acceptance Criteria

1. When クライアントがメディアのアップロードまたは更新を要求したとき, the Media Pipeline shall 書き込み系メディアスコープを要求し、共通のスコープ内包判定で権限を検証する。
2. If メディア要求が有効な認証を欠く、または提示トークンが無効・失効済みであるとき, then the Media Pipeline shall 共通の認証エラー応答（401 相当）を返す。
3. If 提示トークンの承認スコープが要求スコープを内包しないとき, then the Media Pipeline shall 共通の権限不足エラー応答（403 相当）を返す。
4. When メディア API がエラーを応答するとき, the Media Pipeline shall 共通の Mastodon 互換エラー JSON 形（`error` フィールドを含む）でエラーを返す。
5. When メディア API がレート制限対象の応答を返すとき, the Media Pipeline shall 共通の `X-RateLimit-*` ヘッダ規約に従ったヘッダを付与する。

### Requirement 10: ネイティブ依存判断ゲートと処理抽象

**Objective:** 配布（distribution）の判断者として、メディア処理が要求するネイティブ依存の範囲を明示的に決め、配布の容易さと両立させたい。これにより、配布形態が散在するネイティブ依存に縛られないようにできる。

#### Acceptance Criteria

1. The Media Pipeline shall 画像の復号・縮小・符号化・BlurHash 生成を処理抽象の背後に隔離し、ネイティブ依存の有無を呼び出し側へ波及させない。
2. The Media Pipeline shall MVP の画像処理が要求するネイティブ依存の範囲（pure-Rust で賄える範囲と、許容するネイティブ依存）を明示的に決定し、配布判断へ引き渡せる形で記録する。
3. Where MVP が画像のみを対象とする場合, the Media Pipeline shall 動画・音声処理のためのネイティブ依存を MVP では要求しない。
4. Where 将来別の処理実装（ネイティブ依存を含む実装等）へ差し替える場合, the Media Pipeline shall 呼び出し側の変更なしに処理実装を入れ替えられる境界を維持する。
