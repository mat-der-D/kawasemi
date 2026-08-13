# Requirements Document

## Project Description (Input)

テスト基盤の構造的負債の解消。

### 誰が困っているか

このリポジトリで実装を進める開発者（人間・AI エージェントの双方）。Phase 2 以降の
実装は既存テストを回しながら進むため、テストスイートが素直に走らないことが全作業に
乗ってくる。

### 現状

2026-08-13 の実測で、`cargo test --lib` の一括実行が大量失敗する真因を特定した。
同時実行数の問題ではなく、**`TestApp` の接続リーク**である。

- `TestApp` 1 個が接続を即時に張る。`cleanup()` は正しく解放するが、**`Drop` は
  1 本も解放しない**（`pool.close()` が async で同期 `Drop` から呼べないため。doc は
  「design.md: `Drop` はベストエフォートのみに留める」と書いているが、実測値はゼロ）
- `src/` の **131 箇所が `cleanup()` を呼んでいない**（`spawn_test_app` 750 回に対し
  `cleanup()` 619 回）
- 結果、1 プロセスで上限 97 接続（`max_connections` 100 − superuser 予約 3）に詰まる。
  一括実行の失敗 256 件は**全件 `PoolTimedOut`** で、本物の失敗はゼロだった

緩和として `spawn_test_app` のプールを 5→2 に減らした（コミット `2703704`）。リーク
許容量が 19→48 個に増えたが、**リーク自体は残っている**。現状はモジュール単位に分割し
孤立スキーマを掃除するという手順書を踏むことで回避している。

派生している症状：

- CI に素直に `cargo test` と書けない
- lib テスト 1,764 件のうち **750 件が `TestApp`（＝DB）を要求**し、全体で 891 秒かかる
- 孤立スキーマが放置される（実測で 239 個の残骸）

### 何が変わるべきか

後始末を呼び出し側の規律に依存する構造をやめる。131 箇所を機械的に埋めても、Phase 2 で
新しいテストを書けば同じ比率で漏れるため、規律を要求しない形にすることが本質。

## スコープ

### 含める

1. **後始末を型で強制する** — `TestApp` の解放を呼び忘れ不能にする。`spawn_test_app`
   の呼び出しは `src/` 750 + `tests/` 503 = 1,253 箇所で、移行量は多いが機械的
2. **B-2: 単体テストの `TestApp` 依存の削減** — lib テスト 1,764 件中 750 件が DB を
   要求する。大半は純粋関数のテストで DB は不要。実行時間（891 秒）と直列性への
   唯一の効き手
3. **B-5: 孤立スキーマの起動時スイープ** — `kawasemi_test_harness_%`。パニックした
   テストの残骸は型で防げないため、安全網として 1 とは対で必要。60〜100 行で実装可能と
   監査時に見積もり済み
4. **B-4: `test_harness` を本番バイナリから外す** — `TEST_KEK` /
   `TEST_OWNER_PASSWORD` / `DEFAULT_TEST_DB_URL` が release に埋まっている。
   `Cargo.toml` に `[features]` 自体が無い。`federation → 各 feature モジュール` の
   依存エッジは全て `src/federation/test_harness.rs` 由来で、外せば本番依存グラフが
   クリーンな DAG になる。1・2 と同じファイル群を触るため同時が安い

### 含めない

- **B-1**（テストフィクスチャの 84 ファイルへのコピペ集約）— 1 の移行と同じファイルを
  触るため「ついで」の誘惑があるが、*何を*集約するかの設計判断が別軸で、タスク数が
  20 を超えて `structural-refactor` と同じ「レビュー不能」のリスクを踏む。**同じ
  ファイルを触る際の付随変更としては許容し、独立タスクには立てない**
- **B-3**（`assert_golden` が 84 ファイル中 6 ファイルのみという契約テストの薄さ）—
  「ハーネスをどう作るか」ではなく「何を assert するか」の別軸。独立に進められる

## 完了条件

- **`cargo test --lib` の一括実行が単一プロセスで通ること**（モジュール分割・スキーマ
  掃除という手順書なしで）

## 調査を要する残件

`spawn_test_app` のプールを 1 にすると
`federation::outbound::worker::tests::run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves`
が決定的に claim 0 件になる。`DbDeliveryQueue::claim_due` は `FOR UPDATE SKIP LOCKED`
の単一クエリでプールサイズに依存しないはずの形をしており、理由は未解明。プロダクションの
並行性の前提か潜在バグの可能性があるため、本 spec で調査する。

## 位置づけ

フィーチャー spec ではない構造リファクタであり、`structural-refactor` と同じく
roadmap 上は Phase 2（`streaming` / `web-push`）の前に挟む扱いとする。

## Requirements
<!-- Will be generated in /kiro-spec-requirements phase -->
