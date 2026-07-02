# Research & Design Decisions

## Summary

- **Feature**: `core-runtime`
- **Discovery Scope**: New Feature（グリーンフィールド・最上流 spec）
- **Key Findings**:
  - 技術スタック（Rust / axum / tokio / sqlx / PostgreSQL）はステアリング（`tech.md`）で確定済みのため再検討しない。discovery の価値は責務境界の確定と注入境界の形状にある。
  - 非決定性の注入は本プロジェクトの自律 TDD の生命線であり、clock / id / rng / 署名鍵を trait 境界に置き、本番実装とテスト用決定的実装を差し替え可能にすることが土台の中核要件。
  - マイグレーションは `sqlx::migrate!` による埋め込み + 起動時自動実行が確定方針。失敗時は HTTP リスナーを開始せず安全停止する。

## Research Log

### マイグレーションの埋め込みと起動時自動実行

- **Context**: 非エンジニアでも「起動するだけ」でスキーマが最新化される必要がある（product.md の価値提案）。
- **Sources Consulted**: `docs/fediverse-design.md`（6.1 マイグレーション）、`tech.md`（DB マイグレーション方針）、sqlx の `migrate!` マクロと `Migrator` の一般的挙動。
- **Findings**:
  - sqlx は `migrate!()` マクロで `migrations/` ディレクトリの SQL をコンパイル時にバイナリへ埋め込み、`Migrator::run(&pool)` で起動時適用できる。
  - sqlx は `_sqlx_migrations` テーブルで適用履歴とチェックサムを管理し、適用済みマイグレーションの内容変更（チェックサム不一致）を検出して失敗させる。
  - 適用は冪等（未適用分のみ適用）であり、データ保持を満たす。
- **Implications**: Requirement 4 全体を sqlx の標準機構で満たせる。カスタムのマイグレーションランナーを自作しない（build-vs-adopt → adopt）。

### 非決定性境界（clock / id / rng / 署名鍵）

- **Context**: flaky テスト禁止（tech.md「決定性の強制」）。時刻・ID・乱数・署名鍵を DI 可能にする。
- **Findings**:
  - 4 つの関心はいずれも「本番では実環境、テストでは固定値」という同型の問題であり、共通の「Provider trait + 本番実装 + 決定的実装」パターンで一般化できる。
  - これらをまとめて 1 つの `RuntimeContext`（または同等の集約）として下流へ手渡すと、各 spec は個別配線を意識せずに注入を受けられる。
  - 署名鍵プロバイダは本 spec では「鍵を供給する境界（trait）」のみを所有し、鍵の生成・保管・ローテーション運用は actor-model が所有する。本 spec はテスト用に固定鍵を返す実装を持てば足りる。
- **Implications**: Requirement 5・8 を満たす。trait は 4 つだが、集約の手渡し方を統一する（generalization）。

### 設定の二層化（起動設定側のみ）

- **Context**: 起動設定（ドメイン・DB 接続・シークレット）= TOML/環境変数、運用設定 = DB 保存（後続 spec 所有）。
- **Findings**:
  - TOML + 環境変数のマージ（環境変数優先）と必須項目検証を起動時に行い、検証済みの不変設定構造体を構築する。
  - シークレットはログ出力時にマスクする必要がある（Requirement 2.5）。`Debug` 実装をカスタムするか専用ラッパ型でマスクする。
- **Implications**: 本 spec は運用設定テーブルに一切触れない（境界）。

### エラー型と HTTP レスポンス変換

- **Context**: api-foundation が Mastodon 互換エラー規約をここから拡張する（roadmap の Adjacent）。
- **Findings**:
  - axum の `IntoResponse` を実装した統一エラー型を中核に置く。4xx/5xx を区別し、5xx は内部詳細を本文に出さずログにのみ出す。
  - Mastodon の実エラー本文形（`{ "error": "...", "error_description": "..." }` 等）の確定は api-foundation の所有。本 spec は「変換骨格」（トレイト/分類/ステータス対応）に留め、具体的な JSON 形は拡張点として開けておく。
- **Implications**: Requirement 6。骨格のみ。過剰一般化を避ける（simplification）。

### 可観測性（構造化ログ・SQL ログ・相関 ID）

- **Findings**:
  - `tracing` + `tracing-subscriber` がデファクト。HTTP は `tower-http` の `TraceLayer` でリクエスト/レスポンス span を付与できる。
  - sqlx はクエリログを `log`/`tracing` 経由で出力でき、診断レベルで実行 SQL を観測できる。
  - 相関 ID はリクエスト span のフィールド（request_id）として付与する。
- **Implications**: Requirement 7 を既存クレートの組み合わせで満たす（adopt）。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| Composition Root + 注入境界 | 起動時に全依存を組み立て、非決定性を trait 背後に置いて下流へ手渡す | 自律 TDD に必須の差し替え性、明確な依存方向 | 集約構造体が肥大化しやすい | 採用。集約は最小限に保つ |
| ports & adapters（全面ヘキサゴナル） | 全 I/O をポート抽象化 | 究極の差し替え性 | 土台段階では過剰、YAGNI | 不採用（simplification） |
| 各 spec で個別配線 | 共通土台を作らない | 初期コスト低 | 不整合・flaky の温床（brief の問題そのもの） | 不採用 |

## Design Decisions

### Decision: 非決定性は単一の集約コンテキストで下流へ手渡す

- **Context**: clock / id / rng / 署名鍵を各 spec が個別に注入配線すると不整合が起きる。
- **Alternatives Considered**:
  1. 4 つの trait を個別に引数で配り回す — 配線が散らばる。
  2. 1 つの集約（RuntimeContext）にまとめて手渡す — 配線が一点に集約。
- **Selected Approach**: 4 つの Provider trait を定義し、それらを保持する単一の `RuntimeContext` を Composition Root で構築して共有する。
- **Rationale**: 下流 spec は集約を 1 つ受け取れば全注入境界にアクセスでき、テストでは集約を決定的版に差し替えるだけでよい。
- **Trade-offs**: 集約が成長しうる。各 trait は本 spec が所有する 4 つに限定し、無関係な依存を集約に混ぜない規律で対処。
- **Follow-up**: actor-model が署名鍵プロバイダの本番実装を差し込む際の拡張点を壊さないこと。

### Decision: マイグレーションは sqlx 標準機構を採用（自作しない）

- **Context**: 埋め込み + 自動適用 + 履歴整合チェックが要件。
- **Selected Approach**: `sqlx::migrate!` でコンパイル時埋め込み、起動時に `Migrator::run`。`_sqlx_migrations` のチェックサム不整合検出を Requirement 4.6 に充てる。
- **Rationale**: 要件を標準機構が過不足なく満たす。自作はリスクのみ。
- **Trade-offs**: sqlx の挙動に従属するが、ステアリングがすでに sqlx を確定済み。

### Decision: エラー/レスポンスは骨格のみ、Mastodon 互換形は api-foundation へ委譲

- **Context**: 境界の漏れを防ぐ（上流に下流仕様を埋め込まない）。
- **Selected Approach**: 統一エラー型 + 分類 + `IntoResponse` 変換の骨格を提供し、具体的な互換 JSON 形は拡張点として残す。
- **Rationale**: roadmap の境界戦略（エンティティ契約は各機能 spec が所有、ハーネスは上流が用意）に整合。

## Risks & Mitigations

- **Risk: 集約コンテキストが「何でも入れ物」になり境界が崩れる** — 本 spec が所有する 4 つの注入境界 + プール + 設定 + ログに限定し、ドメイン依存を入れない規律を design の Boundary に明記。
- **Risk: 統合テストの DB 分離が不十分で flaky 化** — テストごとに分離された DB 状態（テンプレート DB からの作成 or 一意スキーマ/DB 名）を提供し、終了時に解放する方針を Testing Strategy に固定。
- **Risk: graceful shutdown が処理中リクエストを取りこぼす / 無限待機する** — 受付停止 → 完了待ち → 猶予タイムアウトで強制停止、の三段で明確化（Requirement 1.3/1.4）。
- **Risk: シークレットのログ漏洩** — シークレットは専用ラッパ型でマスクし、設定全体の Debug 出力時も露出させない。

## References

- `docs/fediverse-design.md` — 6.1 マイグレーション、6.2 設定の二層化、6.4 HTTPS（distribution へ委譲する範囲の確認）
- `.kiro/steering/tech.md` — 技術スタック確定、決定性の強制、可観測性
- `.kiro/steering/structure.md` — 注入可能な非決定性境界、レイヤー分離
- sqlx `migrate!` マクロ / `Migrator`（埋め込みマイグレーションと履歴管理）
- `tracing` / `tracing-subscriber` / `tower-http` TraceLayer（構造化ログ・リクエスト span）
