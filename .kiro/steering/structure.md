# Project Structure

> Phase 1（MVP）の全 11 spec（core-runtime / actor-model / api-foundation / federation-core / media-pipeline / accounts-and-instance / statuses-core / social-graph / timelines / notifications / search）が実装済み。以下は実コードから確認された構成原則。

## Organization Philosophy

**レイヤー分離を最優先**。とりわけ「管理層の概念（同一オーナー）」を「プロトコル層（ActivityPub）」に漏らさないことを構造で担保する。機能は**垂直スライス**（投稿する → TL に出る → 通知される）で 1 本ずつ通す。

## Core Structural Principles

### プロトコル層とローカル特権の分離
ローカル宛・リモート宛で Activity の生成・可視性判定・状態遷移は**共通コードパス**。分岐は配送手段の選択（in-process 関数呼び出し or HTTP 送信）のみに閉じ込める。ローカル特権的な「振る舞い」を作らない。明示的例外（例：同一サーバー内のフォロー承認スキップ）は、サーバー層の**明示的な管理者特権**として一箇所に定義する。

### 契約（スキーマ）の集約
Mastodon 互換エンティティ（Status / Account / Notification / Poll …）の JSON 契約を、実装に先んじてゴールデン／スナップショットとして一箇所に固める。実装側はこの契約に従う。

### 注入可能な非決定性境界
clock / id generator / RNG / 署名鍵は具体実装に直接依存させず、差し替え可能な境界（trait 等）の背後に置く。テスト・連合検証で差し替えられること。

### 検索の抽象境界
検索は抽象インターフェースの背後に置き、呼び出し側（Mastodon 互換 API 等）を特定エンジン実装に依存させない。後から日本語対応に差し替え可能なマイグレーション経路を確保する。

### 独自連合方言の正規化境界
絵文字リアクション・引用は実装ごとに方言が乱立する（Misskey `Like`+`_misskey_reaction` / Pleroma `EmojiReact` / `quoteUrl` / FEP-e232 等）。**受信時の正規化／送信時の出し分け**を専用の境界に集約し、コア状態モデルから方言を隔離する。

### Ports & Adapters（差し替え境界）の置き場
下流所有情報や外部副作用への依存を抽象境界の背後に隔離する spec（accounts-and-instance / media-pipeline / notifications / search 等）は、各モジュールの `<module>/ports.rs` にトレイト定義・既定実装（no-op や PostgreSQL 標準実装）・swap-in テストダブルをまとめる（例：`search/ports.rs` の `SearchBackend` + `StubSearchBackend`、`accounts/ports.rs` の `*Provider` 群、`notifications/ports.rs` の `*Sink` + `NoopSink`）。design.md 側では該当コンポーネントを「Port」と呼ぶ。ただし境界が 1 モジュールに閉じ単純な場合（`media/store.rs` の `MediaStore`、`runtime/signing_key.rs` の `SigningKeyProvider`）はトレイトを実装ファイルに直接置いてよく、`ports.rs` への分離は必須ではない。

### バックエンド一体配信
フロント（React）のビルド済み資産・DB マイグレーション・SPA 配信はバックエンドバイナリに同梱する前提で構成する（別 Web サーバーを構造に持ち込まない）。

## テストレイアウト

- **単体テスト**：実装ファイルと同階層の `tests.rs` サブモジュールに置く（例：`src/actor/service.rs` → `src/actor/service/tests.rs`）。`#[cfg(test)] mod tests;` で親から宣言する。
- **統合テスト**：`tests/` 直下、ファイル名は `_it.rs` サフィックス（例：`tests/actor_lifecycle_it.rs`）。DB込みの実起動インスタンスを要する検証はここに置く。
- **TestHarness**：`src/test_harness.rs` の `spawn_test_app` が bootstrap と同じ構成要素（`db::establish_pool` / `migrate::apply_migrations` / `runtime::RuntimeContext::deterministic` / `state::AppState::new` / `server::build_router`）を再利用して実インスタンスを起動する。`bootstrap::bootstrap` 自体を直接再利用しないのは、待受アドレスを呼び出し側に返さないため。統合テストは `bootstrap` を再実装せず、この harness 経由で実体を起動する。

## Spec & Steering Layout

- ステアリング（プロジェクト全体ルール）：`.kiro/steering/`
- 仕様（個別フィーチャー）：`.kiro/specs/`
- 設計の一次情報：`docs/`（`fediverse-design.md` / `mastodon-api-compat.md` / `mastodon-api-estimate.md`）

## SSoT（一次情報）の所在

**`.kiro/specs/<feature>/spec.json` の `ssot` フィールドで判定する。** spec が常に正しいわけでも、実装が常に正しいわけでもない。フィーチャーのライフサイクル上のどこにいるかで一次情報が移る。

| `ssot` | 状態 | 一次情報 |
|---|---|---|
| `"spec"`（またはフィールド不在） | 実装中 | **その spec**。コードは spec に従う。design.md が正であり、コードとの差異はコード側の不足として扱う |
| `"implementation"` | handoff 済み | **実装**。spec は当時のログ。現状説明として読んではならない |

- **不在時は `"spec"` として扱う**（保守的側）。`/kiro-spec-init` で作られた新規 spec は自動的に正しい状態から始まる。
- **遷移は `/kiro-validate-impl` の GO でのみ発生する。** 手で書き換えない。GO は requirements カバレッジ・design の Boundary Commitments 整合・フルスイート通過を検査済みであり、「実装と spec に差異がない」ことの確認そのものである。
- `phase` の完了状態は `"implemented"`。`ready_for_implementation` は spec の承認状態という歴史的事実であり、現在の SSoT を表さない。

### handoff で行うこと（`/kiro-validate-impl` GO 時の 3 点セット）

1. `spec.json` の `ssot` を `"implementation"` に反転し、`handoff` を記録する — **`/kiro-validate-impl` 自身が実行**
2. 用済みになったトレーサビリティ参照を実装コードから除去する（下記）— **委譲**。独立したコミットとしてレビューする
3. steering を**コードから**同期する（`/kiro-steering`）— **委譲**。spec や計画から書き下ろさない。そうすると steering 自体が予言＝ログになる

2 と 3 を gate 自身にやらせない理由は、**検証対象を書き換えられる gate は gate ではない**から。`/kiro-validate-impl` の書き込み権限は自身の判定を `spec.json` に記録することだけに限る。GO に到達するためにソースの変更が要ると感じたら、それは GO ではなく NO-GO である。

### コード内コメントの扱い

実装が SSoT である以上、実装ファイルは自分を説明できなければならない。spec ログの残骸を SSoT に混入させない。

| コメント | 扱い |
|---|---|
| `task 5.2 で追加` / `task 7.3 までは main.rs にあった` | **除去**（履歴は git の仕事） |
| `design.md の X コンポーネントに従う` | **除去**（外部文書への従属は SSoT の否定） |
| `Requirements 1.1 を満たす` | **除去**（ふるまいの根拠は契約テストが持つ） |
| なぜ generic ではなく boxed future か | **保持**（コードから読み取れない設計判断は SSoT の一部） |
| なぜこの lint を抑制するか | **保持**（同上） |

トレーサビリティ参照は**実装中は残してよい**（`/kiro-validate-impl` が requirements → implementation 行列を組むのに使う）。除去するのは handoff 時。実装中は足場、handoff 後は負債。

### ふるまいの仕様は契約テストが持つ

requirements.md も handoff 後はログである。ふるまいの一次情報は `src/contract.rs` の golden 契約テスト — 実行可能であり、腐れば落ちるので検証機構が内蔵されている。「requirements.md を読め」ではなく「契約テストを読め」と言える状態を維持する。

## Naming Conventions

- Rust：標準慣習（モジュール/関数 `snake_case`、型/トレイト `PascalCase`）。
- TypeScript/React：標準慣習（コンポーネント `PascalCase`）。
- 統合テストファイル：`<対象>_it.rs`（例：`bootstrap_lifecycle_it.rs` / `owner_actor_boundary_it.rs`）。
- フロント実装はまだ無し。着手時に確立した命名パターンをここに追記する。

## Rust コーディング規約

- **Edition**：2024 を使用する（`Cargo.toml` の `edition = "2024"`）。
- **モジュール定義**：`mod.rs` は使わない。ディレクトリ名と同名の `.rs` ファイルでモジュールを定義する（Rust 2018+ の方式）。

  ```
  src/
    foo.rs        // `mod bar;` などサブモジュール宣言を書く
    foo/
      bar.rs
      baz.rs
  ```

  NG例（旧方式・使用しない）：

  ```
  src/
    foo/
      mod.rs
      bar.rs
      baz.rs
  ```

---
_ファイルツリーではなくパターンを記載する。パターンに従う新規ファイルの追加では更新不要。_
