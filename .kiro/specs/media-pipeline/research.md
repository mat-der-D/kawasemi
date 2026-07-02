# Research & Design Decisions

## Summary

- **Feature**: `media-pipeline`
- **Discovery Scope**: New Feature（グリーンフィールド。core-runtime / api-foundation の土台上に新規モジュールを追加）
- **Key Findings**:
  - Mastodon 互換のメディアは「非同期アップロード（`POST /api/v2/media` → `202`）→ `GET /api/v1/media/:id` で `206`/`200` ポーリング → `PUT /api/v1/media/:id` でメタ更新」という確立した契約を持つ。これをそのまま採用する。
  - 外部ジョブブローカー禁止の制約下では、PostgreSQL の `SELECT ... FOR UPDATE SKIP LOCKED` による単一テーブルのジョブ取得が定石であり、新規依存を増やさずワーカー競合のない排他取得を実現できる。
  - 画像処理（復号・縮小・符号化・BlurHash）は pure-Rust（`image` + `blurhash` 系 crate）で MVP を賄える見込みであり、これにより musl static 単一バイナリ配布が維持可能。動画はネイティブ依存（ffmpeg）を要するため MVP から外し、依存判断を distribution へ引き渡す。

## Research Log

### Mastodon メディア API の非同期契約

- **Context**: 非同期アップロード（202→poll）の具体的なステータスコードとエンティティ形を確定する必要がある。
- **Sources Consulted**: `docs/mastodon-api-compat.md` / `docs/mastodon-api-estimate.md`（プロジェクト一次情報）、Mastodon `media` API の実挙動（ドキュメント < 実レスポンス方針）。
- **Findings**:
  - `POST /api/v2/media`: 受理時 `202`、本体は MediaAttachment で処理中は `url` が `null`。multipart で `file`・任意 `thumbnail`・`description`・`focus`。
  - `GET /api/v1/media/:id`: 処理中は `206 Partial Content`、完了で `200`。
  - `PUT /api/v1/media/:id`: `description`・`focus` を更新し `200`。
  - MediaAttachment: `id` / `type` / `url` / `preview_url` / `remote_url` / `meta`(`original`,`small`,`focus`) / `description` / `blurhash`。
  - `focus`: `x`,`y` ともに `-1.0`〜`1.0`、既定は中央 `(0.0, 0.0)`。
- **Implications**: エンドポイントは v2 アップロード + v1 取得/更新の 3 本。MVP は常時非同期（`202`）に単純化し、ポーリングで `206→200` を返す。MediaAttachment 契約は本 spec が所有し、api-foundation の契約ハーネスに登録する。`remote_url` は連合取り込み（範囲外）用フィールドのため MVP では常に `null`。

### DB ジョブキューの実現方式

- **Context**: 「外部ジョブブローカー不使用（DB キュー）」制約と「非決定性は DI 境界の背後」「flaky 禁止」を満たす非同期処理が必要。brief は core-runtime を「DB キュー基盤」と記すが、確定済みの core-runtime design には汎用ジョブキューは含まれない。各 spec が自身のキューを所有する方針（federation-core も独自の「DB 配送キュー」を所有）に倣う。
- **Sources Consulted**: PostgreSQL `FOR UPDATE SKIP LOCKED` パターン、core-runtime design（`PgPool`・`RuntimeContext`・テストハーネス）、roadmap の境界戦略。
- **Findings**:
  - 単一テーブル `media_processing_jobs`（状態・試行回数・`run_at`・`locked_at`）を用い、ワーカーは `FOR UPDATE SKIP LOCKED` で 1 件を排他取得 → 処理 → 完了/失敗更新。
  - 再試行は `attempts` と指数バックオフで `run_at` を後退。上限到達で失敗状態へ。
  - 冪等性は「メディア状態 + 派生物の有無」を真実源とし、再実行時は既存派生物を上書き/スキップして不整合を防ぐ。
- **Implications**: メディア処理キューは media-pipeline が所有する。汎用化はインターフェース（投入/取得/完了/失敗）レベルに留め、実装はメディア処理専用にスコープする（過度な共通化を避ける）。時刻は `Clock`、ジョブ/メディア識別子は `IdGenerator` から取得し決定的にする。

### ネイティブ依存判断ゲート（最重要決定）

- **Context**: メディア処理のネイティブ依存（libvips/ffmpeg 等）の許容範囲が配布形態を左右する。tech.md は「真の要件はインストールが簡単。単一バイナリには固執しない。必要なら Docker に切替可」とする。
- **Sources Consulted**: tech.md「配布形態」「主要な技術的判断」、brief.md「Constraints」、Rust 画像処理エコシステム（`image`・`fast_image_resize`・`blurhash`）。
- **Findings**:
  - 画像（JPEG/PNG/GIF/WebP 等）の復号・縮小・符号化・BlurHash 生成は pure-Rust crate で実用範囲をカバーでき、ネイティブ依存なしで musl static 単一バイナリを維持できる。
  - libvips はメモリ効率・速度で優位だが C ライブラリへの動的/静的リンクを要し、配布の単純さと衝突しうる。一人鯖・低スペック VPS・画像 MVP の前提では pure-Rust の性能で十分。
  - 動画/アニメーション最適化は ffmpeg 等のネイティブ依存をほぼ不可避とし、これは Docker 配布への切替を促す重い判断。MVP から外す。
- **Implications**: **MVP 決定 = 画像処理は pure-Rust、ネイティブ依存ゼロ**。処理は `MediaProcessor` 抽象の背後に隔離し、将来 libvips 実装や動画対応をネイティブ依存込みで差し込めるよう境界を維持する。この決定（pure-Rust 採用 / 動画＝ネイティブ依存で後回し / 抽象境界で隔離）を distribution へ引き渡す成果物とする。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| Ports & Adapters（採用） | ストレージと処理を trait（port）で抽象化し、ローカル FS / pure-Rust 画像処理を adapter として差し込む。非同期は DB キュー + ワーカー。 | 差し替え可能境界が明確、決定性テスト容易、ネイティブ依存を 1 箇所に隔離 | port を増やしすぎると過抽象 | steering「検索の抽象境界」「注入可能な非決定性境界」と同型。port は Store と Processor の 2 つに限定 |
| 同期処理（POST で完結） | アップロード時に処理を同期実行 | 実装単純・ポーリング不要 | 大きい画像でリクエストが長時間化、Mastodon 契約（202→poll）から逸脱 | 却下。契約非互換 |
| 外部ジョブブローカー | Redis/RabbitMQ 等でキュー | スケール容易 | 「ランタイムはアプリ + PostgreSQL のみ」制約違反 | 却下。制約違反 |

## Design Decisions

### Decision: 非同期処理は DB ジョブキュー + ポーリング（202→206→200）

- **Context**: Requirement 1/2/4。外部ブローカー禁止・契約互換・決定性。
- **Alternatives Considered**:
  1. 同期処理 — 契約非互換でリクエスト長時間化。
  2. 外部ブローカー — 制約違反。
- **Selected Approach**: アップロードで原本保管 + ジョブ投入 + `202` 返却。ワーカーが `FOR UPDATE SKIP LOCKED` でジョブを排他取得し派生物生成、状態を `processing→ready/failed` に遷移。`GET` は状態に応じ `206`/`200`。
- **Rationale**: 制約・契約・決定性をすべて満たす最小構成。
- **Trade-offs**: ポーリング遅延が生じるが Mastodon クライアントは前提済み。
- **Follow-up**: 再試行バックオフ・冪等性を統合テストで検証。

### Decision: 画像処理は pure-Rust（ネイティブ依存ゼロ）を MVP 採用

- **Context**: Requirement 6/10。配布の容易さとの両立。
- **Alternatives Considered**:
  1. libvips（ネイティブ）— 高性能だが配布が複雑化。
  2. pure-Rust（`image` + `blurhash`）— 単一バイナリ維持。
- **Selected Approach**: pure-Rust 実装を `MediaProcessor` port の adapter として実装。動画はネイティブ依存を要するため MVP 範囲外。
- **Rationale**: 一人鯖・画像 MVP では pure-Rust の性能で十分。musl static 単一バイナリを維持でき distribution を縛らない。
- **Trade-offs**: 大量・大サイズ画像での処理コストは libvips に劣るが許容範囲。
- **Follow-up**: 決定内容を distribution へ引き渡す。`MediaProcessor` 境界を将来の libvips/動画実装の差込点として維持。

### Decision: ストレージは MediaStore port + ローカル FS adapter

- **Context**: Requirement 5。後から差し替え可能に。
- **Selected Approach**: `MediaStore` trait（put/get/delete + URL 生成）を定義し、ローカル FS 実装を提供。公開 URL は api-foundation のプロキシ尊重規約に合わせて絶対 URL 化。
- **Rationale**: steering の抽象境界方針に整合。S3 等は将来 adapter 追加で対応。
- **Trade-offs**: URL 生成規約を Store と API の双方で一貫させる必要がある（プロキシ情報の供給）。

## Risks & Mitigations

- メディア識別子・保管パスの非決定性 — `IdGenerator` 採番に統一し、保管パスを ID から導出して決定的ゴールデンを担保。
- 二重処理・孤児派生物 — ジョブ排他取得 + 冪等処理 + メディア状態を真実源にして防止。
- pure-Rust 画像処理の対応形式・性能不足 — 対応形式を受理時に検証して未対応は 422 で拒否。性能は `MediaProcessor` 境界で将来差し替え可能に。
- core-runtime に汎用キューが無い齟齬（brief 表現との差） — media-pipeline が自身のメディア処理キューを所有することで解消（federation-core の配送キュー所有と同型）。
- マイグレーション番号の競合（既存に 0003 重複あり） — `0004_media.sql` を採用し、連番・前方追加規約に従う。実装時に未使用番号と齟齬があれば調整する。

## References

- `docs/mastodon-api-compat.md` / `docs/mastodon-api-estimate.md` — メディア API 互換の一次情報。
- `.kiro/specs/core-runtime/design.md` — `PgPool` / `RuntimeContext`（Clock/Id/Rng）/ `AppError` / テストハーネス。
- `.kiro/specs/api-foundation/design.md` — Bearer 認証・スコープ内包・Mastodon 互換エラー・`X-RateLimit-*`・契約ハーネス・プロキシ尊重 URL。
- PostgreSQL `FOR UPDATE SKIP LOCKED` — DB ジョブキューの排他取得パターン。
