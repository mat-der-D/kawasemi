# Research & Design Decisions

## Summary
- **Feature**: `api-foundation`
- **Discovery Scope**: Extension（core-runtime / actor-model の土台上に横断 API サブシステムを増設）
- **Key Findings**:
  - OAuth 認可フローの「複数アクター × 1 トークン」橋渡しは、承認画面でのアクター選択として実装する以外に Mastodon 標準と両立する方法が無い。トークンは単一アクターに不可分に結びつく。
  - エラー JSON 互換は core-runtime の `AppError` が残した「Mastodon 互換レスポンス拡張点」（core-runtime Requirement 6.5）を埋める形で実装でき、新たな並行エラー型を作る必要はない。
  - ページネーションの `since_id`（先頭固定で新しい側を埋める）と `min_id`（古い側から進む）の挙動差、およびカテゴリ毎カーソル種別（status id でない）が最大の実装リスクであり、カーソル抽象を最初に固める必要がある。

## Research Log

### OAuth 2.0 サーバーと複数アクター橋渡し
- **Context**: Mastodon API は「1 アクセストークン = 1 アカウント」前提。本プロジェクトは一人鯖だが複数ローカルアクターを持つ（product.md / mastodon-api-compat.md 3）。
- **Sources Consulted**: `docs/mastodon-api-compat.md` 2,3 / `docs/mastodon-api-estimate.md` 1.1, 2.1 / Mastodon OAuth 実挙動（apps 登録・authorize 承認画面・token/revoke・scope 体系）/ actor-model design.md（`ActorDirectory::list_actors_for_owner`）。
- **Findings**:
  - 必要面: `POST /api/v1/apps`・`GET /api/v1/apps/verify_credentials`・`GET /oauth/authorize`（承認画面）・`POST /oauth/token`・`POST /oauth/revoke`・Bearer ミドルウェア・スコープ検証。
  - アクター選択は `authorize` の承認段階で行い、選択アクター ID を認可コードへ、さらにアクセストークンへ伝播させる。
  - actor-model は `list_actors_for_owner(owner_id) -> Vec<ActorSummary>` を提供。これを選択候補供給に使う。トークンに載るアクター ID の正当性は actor-model のアクター解決に従う。
- **Implications**: トークン・認可コードのデータモデルに `actor_id` を必須カラムとして持たせる。承認画面はオーナー認証を前提とする（下記決定参照）。

### エラー JSON / HTTP ステータス互換
- **Context**: クライアントはエラー形が崩れると「不明なエラー」表示になる（estimate 2.5）。
- **Findings**: Mastodon のエラー本文は `{"error": "...", "error_description": "..."}`。`401/403/404/422/429` の出し分けが契約。core-runtime `AppError` は 4xx/5xx 分類と `IntoResponse` 骨格を持ち、本文表現を差し替え可能な拡張点を残している。
- **Implications**: api-foundation は本文表現を Mastodon 互換へ拡張する変換層を提供し、全エンドポイントに横断適用する。core-runtime のエラー型を再定義しない。

### ページネーション規約
- **Context**: 全リスト系で一貫させないと無限ループ/歯抜けが起きる（estimate 2.4）。
- **Findings**: `Link` ヘッダ（`rel=next/prev`）+ `max_id`/`since_id`/`min_id`/`limit`。`since_id` は最新固定で新しい側を埋める、`min_id` は古い側から進む。bookmarks/favourites/notifications はカーソルが status id でない。`Link` URL は `X-Forwarded-*` を尊重した絶対 URL。
- **Implications**: カーソルをカテゴリ毎に差し替え可能な抽象（カーソル種別）として設計し、リンク生成を 1 箇所に集約する。

### レート制限ヘッダ互換
- **Findings**: `X-RateLimit-Limit/Remaining/Reset`。一人鯖では実値は緩くてよいが形は厳守（estimate 2.5）。Reset 時刻は時刻境界から算出。
- **Implications**: ミドルウェアでヘッダ付与。実カウンタは軽量（DB 不要のインメモリで足りる）でよいが、決定性のため時刻は `Clock` 経由。

### 契約テストハーネス
- **Findings**: エンティティ JSON をゴールデン/スナップショットで先に固定し実装を合わせる（compat 4.2）。決定的境界で再現可能ゴールデンを得る。実クライアントのキャプチャをフィクスチャ化（compat 4.1）。
- **Implications**: ハーネスは「決定的 RuntimeContext での応答生成 + ゴールデン比較 + 差分報告 + フィクスチャ登録」を提供。個別エンティティ契約は下流が足す。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| Tower ミドルウェア層 + ドメインサービス | 横断関心（認証・エラー変換・RL・ページネーション）を tower レイヤー/抽出器に、OAuth 業務を Repository+Service に分離 | core-runtime の axum/tower と整合、各エンドポイントが「乗るだけ」、並行実装しやすい境界 | レイヤー順序の規律が必要 | **採用**。core-runtime の Composition Root に配線 |
| 各 spec で個別実装 | 横断土台を作らず各機能 spec が再発明 | 初期着手が速い | 互換ドリフト・契約基準不在（本 spec の存在理由に反する） | 却下 |
| 外部 OAuth ライブラリ全面採用 | 既製 OAuth サーバー crate を丸ごと採用 | 実装量減 | Mastodon 独自挙動（scope 体系・アクター選択・承認画面）と適合せず適応コスト大 | 部分採用（後述） |

## Design Decisions

### Decision: アクター選択は認可コードへバインドし、トークンへ伝播する
- **Context**: 複数アクター × 1 トークン（独自設計点）。
- **Alternatives Considered**:
  1. トークン発行後に別 API でアクター切替 — 標準クライアントが知らないため非互換。
  2. authorize 承認時にアクターを選択し、コード→トークンへ不可分に伝播 — 標準フロー内で完結。
- **Selected Approach**: 2。承認画面で `list_actors_for_owner` の一覧から 1 アクターを選択 → 認可コードに `actor_id` を保持 → トークン交換で `actor_id` をトークンへ移送 → Bearer ミドルウェアが「現在の単一アクター文脈」を確定。
- **Rationale**: Mastodon 標準フローを壊さず独自要件を満たす唯一現実的な接合。
- **Trade-offs**: 1 トークンで複数アクター同時操作は不可（統合操作は独自フロントで担保＝product.md）。
- **Follow-up**: トークン文脈のアクター無効化時の扱いを統合テストで確認。

### Decision: エラー互換は core-runtime AppError の拡張点を埋める
- **Selected Approach**: 新エラー型を作らず、`AppError` のレスポンス本文表現を Mastodon 互換 JSON に差し替える変換層 + ステータス対応表を提供。
- **Rationale**: core-runtime Requirement 6.5 が明示的に拡張点を残している。二重のエラー体系を避ける。
- **Trade-offs**: core-runtime のエラー骨格契約に従属（変更時は再検証トリガ）。

### Decision: オーナー認証ゲートは最小・単一オーナー前提
- **Context**: 承認画面の前に「人間オーナー」を認証する必要があるが、オーナー資格情報の本格管理を所有する spec が無い。
- **Selected Approach**: 一人鯖前提で、起動設定（core-runtime の `Secret<T>`）由来の単一オーナー資格情報による最小ログインゲートを api-foundation が持ち、認証後に短命のオーナーセッションを確立してから承認画面を出す。アクター候補は `list_actors_for_owner` で取得。
- **Rationale**: 標準クライアントのログイン実通過点（承認画面）に必要な最小限。フル機能のアカウント/資格情報管理は admin-frontend / 将来へ委譲。
- **Trade-offs**: 資格情報ローテーション等の高度な管理は本 spec 範囲外。スコープ越境を避けるため境界に明記。

### Decision: OAuth プリミティブは標準 crate を部分採用、サーバー編成は自前
- **Selected Approach**: PKCE 検証・トークン乱数生成・定数時間比較等の暗号プリミティブは確立 crate（注入 `Rng` を受ける）を採用。apps/authorize/token/scope/承認画面/アクター選択の編成は Mastodon 互換要件に合わせ自前。
- **Rationale**: Build vs Adopt: プリミティブは枯れた解を採用、Mastodon 独自挙動は自前が適合。
- **Trade-offs**: 編成コードのテスト責任は自前で負う（契約テストで担保）。

### Decision: ページネーションのカーソルをカテゴリ毎に差し替え可能な抽象にする
- **Selected Approach**: カーソル種別を trait 化し、`max_id/since_id/min_id` の解釈と `Link` URL 生成を 1 箇所へ集約。bookmarks/favourites/notifications が status id 以外のカーソルを差し込めるようにする。
- **Rationale**: estimate 2.4 のカテゴリ毎カーソル差異リスクを構造で吸収。

## Risks & Mitigations
- `since_id` と `min_id` の取り違え → 共通カーソルロジックの単体テストで両向きを固定。
- アクター選択を載せ忘れたトークン → コード/トークンの `actor_id` を NOT NULL とし、ミドルウェアで必須解決。
- `Link` URL の host/scheme 誤り（プロキシ後段）→ `X-Forwarded-*` 尊重を 1 箇所に集約しテスト。
- スコープ細分化の後付け破壊 → 上位/細分の内包判定を最初に確定し全段階で共有。
- アクセストークン/オーナー資格情報のログ漏洩 → `Secret` ラッパとマスクを徹底。

## References
- `docs/mastodon-api-compat.md` — 互換方針・横断土台・契約テスト指針
- `docs/mastodon-api-estimate.md` 1.1, 2.1, 2.4, 2.5 — OAuth/ページネーション/エラー・RL の範囲とリスク
- `.kiro/specs/core-runtime/design.md` — `AppError` 拡張点・`RuntimeContext`・`spawn_test_app`
- `.kiro/specs/actor-model/design.md` — `ActorDirectory::list_actors_for_owner` / アクター解決
