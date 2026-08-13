# Research & Design Decisions

## Summary

- **Feature**: `test-infrastructure`
- **Discovery Scope**: Extension（既存のテスト実行基盤の構造変更）
- **Key Findings**:
  - リークの本体は「`cleanup()` の呼び忘れ」ではなく「`Drop` が非同期の解放処理を実行できない」こと。
    呼び忘れ 131 箇所は増幅要因であって根本ではない
  - `TestApp` は repository テストが必要とするもの（隔離スキーマ + プール）に対して大幅に過剰。
    RSA 鍵生成・全モジュール構築・ルーター構築・TCP listener・サーバータスクまで含む
  - `src/` の `spawn_test_app` 750 回のうち大半は repository / service テストであり、
    「DB 依存を無くす」方向では解けない。「起動するものを減らす」方向が正しい

## Research Log

### 後始末を呼び忘れ不能にする方法

- **Context**: Requirement 2 は「後始末の実行をテストを書く側の記述に依存させない」ことを要求する。
  素直な案は `with_test_app(|app| async { ... })` のスコープ API だが、`spawn_test_app` の
  呼び出しは `src/` 750 + `tests/` 503 = 1,253 箇所ある
- **Sources Consulted**: `src/test_harness.rs`（`cleanup` / `Drop for TestApp` / `create_schema` /
  `drop_schema`）、sqlx 0.9.0 の `Pool` セマンティクス、実測プローブ
- **Findings**:
  - `cleanup()` は `pool.close().await` → `drop_schema` の順で正しく解放する。実測で横ばいを確認
  - `Drop` は同期コンテキストのため `pool.close()` を await できず、スキーマ削除を
    `handle.spawn` でデタッチするだけ。`#[tokio::test]` のランタイムはテスト終了で破棄されるため、
    デタッチしたタスクは走り切らない。結果として接続もスキーマも残る
  - `Pool::close()` は共有内部状態に対して作用するため、クローンを 1 つ持っていれば
    他のクローン（`AppState` / サーバータスクが保持）ごと接続を閉じられる。
    `TestApp` が持つクローン 1 つを解放処理に渡せば足りる
- **Implications**: 呼び出し側を書き換えなくても、`Drop` が「テストごとのランタイムとは独立に
  生存する実行主体」へ解放処理を委譲できれば要件を満たせる

### `TestApp` の重さと単体テストの実態

- **Context**: Requirement 4 は実インスタンス起動を要する単体テストの削減を求める
- **Findings**:
  - モジュール別 `spawn_test_app` 呼び出し数: statuses 241 / social_graph 132 / notifications 70 /
    search 63 / oauth 57 / federation 48 / accounts 48 / actor 39 / media 36 / timelines 10
  - 最も多いファイル群は `status_repository/tests.rs`(30)、`status_service/tests.rs`(29)、
    `inbound_handlers/tests.rs`(27)、`interaction_service/tests.rs`(25)、
    `interaction_repository/tests.rs`(24)、`poll_repository/tests.rs`(23) — **repository と
    service が中心**
  - repository テストは実 DB を要するため「DB 依存を無くす」方向では解けない。一方で、これらが
    必要としているのは隔離スキーマ + プールだけであり、`spawn_test_app` が追加で行う
    RSA 鍵生成・`compose_modules`・`build_router`・TCP listener bind・サーバータスク spawn は
    一切使っていない
- **Implications**: 削減対象は「DB 依存」ではなく「実インスタンス起動」。軽量フィクスチャを
  用意して repository/service テストを移すのが正しい seam

### 孤立スキーマの回収条件

- **Findings**: スキーマ名は `kawasemi_test_harness_{nanos}_{seq}`（`unique_schema_name`）。
  ナノ秒が名前に埋まっているため、経過時間による閾値判定が名前だけで可能。実行中のテストが
  使用中のスキーマを巻き込まない判定に使える
- **Implications**: 回収は名前のパースだけで完結し、DB へのメタデータ追加を必要としない

### テストハーネスの本番バイナリからの分離

- **Findings**: `Cargo.toml` に `[features]` セクション自体が存在しない。`src/lib.rs` が
  `test_harness` を無条件公開している。統合テスト（`tests/`）はこのモジュールを必要とする
- **Implications**: crate 自身への dev-dependency で feature を有効化する構成が必要。
  `[features] test-harness = []` + `[dev-dependencies] kawasemi = { path = ".", features = [...] }`

### 配送ワーカーのクレーム 0 件現象

- **Context**: プールを 1 にすると
  `federation::outbound::worker::tests::run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves`
  が決定的に claim 0 件になる
- **Findings**:
  - `DbDeliveryQueue::claim_due` は `UPDATE ... WHERE id IN (SELECT ... FOR UPDATE SKIP LOCKED)
    RETURNING ...` の単一文であり、プールサイズに依存しないはずの形をしている
  - steering `tech.md` は同じ現象を「フル並列実行時のごく稀な flake、再現手順未特定」として
    記録している。今回**プールサイズ 1 で決定的に再現する**ことが分かった。これは
    「環境要因の flake」という既存の推測を否定はしないが、**決定的な再現手順が得られた**
    という点で状況が変わっている
- **Implications**: `SKIP LOCKED` は「他トランザクションがロック中の行」を飛ばす。プールが
  枯渇した状態で同一プール上の別の処理が行をロックしていれば説明がつく。調査の第一候補は
  「`run_once` の実行中に同じプールの別の接続経路が当該行を保持しているか」

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| スコープ API への全面移行 | `with_test_app(\|app\| async {...})` に 1,253 箇所を書き換え | 型で強制でき、意図が明示的 | 差分が巨大でレビュー不能。機械的置換の誤りが混入しても検出しにくい | Simplification レンズで棄却 |
| 常駐リーパーへの委譲 | プロセス常駐のバックグラウンドランタイムに解放処理を委譲し、`Drop` はそこへ送るだけ | 呼び出し側 0 箇所の変更。パニック時も効く | プロセス終了時の未処理分は残る（起動時スイープで回収） | **採用** |
| 呼び忘れ 131 箇所を埋める | `cleanup()` を機械的に追加 | 最小の変更 | 規律依存が残り Phase 2 で再発。要件 2.3 を満たさない | 棄却 |

## Design Decisions

### Decision: 解放処理を常駐リーパーへ委譲する

- **Context**: `Drop` は同期であり、テストごとの Tokio ランタイムはテスト終了時に破棄される
- **Alternatives Considered**:
  1. スコープ API への全面移行 — 1,253 箇所の書き換え
  2. `Drop` 内で `block_on` — `Drop` 内のブロッキングはランタイム上でパニックする
  3. プロセス常駐のリーパーランタイムへ委譲
- **Selected Approach**: 3。専用スレッド上の Tokio ランタイムを `OnceLock` で 1 度だけ作り、
  意図的にプロセス終了まで生かす。`Drop for TestApp` はプールのクローンとスキーマ名を
  チャネルで送るだけにする。リーパー側で `pool.close().await` → `drop_schema` を実行する
- **Rationale**: 呼び出し側を 1 箇所も変えずに Requirement 2 の全項目を満たせる。
  `cleanup()` は明示パスとして残るため、既存の 619 箇所も無変更
- **Trade-offs**: 「型による強制」ではなく「自動化」になる。呼び忘れても正しく動くため、
  規律違反がコンパイルエラーにはならない。ただし Requirement 2 の受入基準は
  ふるまいで書かれており、これで満たされる
- **Follow-up**: プロセス終了時にリーパーが処理し切れなかった分が残ることを実測で確認し、
  起動時スイープ（Requirement 3）が確実に回収することをテストで担保する

### Decision: 軽量 DB フィクスチャを導入し repository / service テストを移す

- **Context**: Requirement 4。`spawn_test_app` 750 回の大半が repository / service テスト
- **Alternatives Considered**:
  1. repository をフェイクに差し替えて DB 依存を無くす — 検証しているふるまい（実 SQL）が
     消えるため Requirement 4.4 に反する
  2. 実インスタンス起動を伴わない軽量フィクスチャへ移す
- **Selected Approach**: 2。隔離スキーマ + マイグレーション適用済みプールだけを提供する
  フィクスチャを用意し、ルーター・サーバータスク・モジュール構築・RSA 鍵生成を伴わせない
- **Rationale**: 検証範囲を狭めずに起動コストを落とせる。Requirement 4.4 を満たす唯一の方向
- **Trade-offs**: `TestApp` と軽量フィクスチャの 2 系統が並立する。どちらを使うかの判断基準を
  design で明示しないと混乱する
- **Follow-up**: 移行対象の選定基準を機械的に判定できる形にする（ルーター/トークン/HTTP を
  使っていないテストが候補）

### Decision: 起動時スイープは名前のナノ秒で判定する

- **Selected Approach**: `kawasemi_test_harness_{nanos}_{seq}` の `nanos` を閾値と比較し、
  十分に古いものだけを回収する。`OnceLock` で 1 プロセス 1 回
- **Rationale**: DB へのメタデータ追加が不要。実行中の他プロセスのスキーマを巻き込まない
- **Trade-offs**: 閾値より新しい孤立スキーマは次回実行まで残る

## Risks & Mitigations

- **リーパーが処理し切る前にプロセスが終了する** — 起動時スイープが次回実行で回収する。
  スイープの実効性をテストで担保する
- **軽量フィクスチャと `TestApp` の使い分けが曖昧になる** — design の File Structure Plan と
  判断基準を明示し、レビュー時の指摘対象にする
- **feature gate 導入で `tests/` がビルドできなくなる** — crate 自身への dev-dependency 構成を
  先に成立させてから `#[cfg]` を付ける順序にする
- **移行量が大きく、途中で緑を維持できなくなる** — モジュール単位で移行し、各段階で
  モジュール単位のテストが緑であることを確認する
- **要件 6 の調査が原因不明のまま終わる** — Requirement 6.4 が「flake として放置しない」ことを
  求めている。時間を区切り、判明しなければ「何が否定されたか」を記録する

## References

- steering `tech.md`「テスト実行基盤の既知の問題」— 孤立スキーマのスイープ未実装、
  `federation::outbound::worker` の間欠的失敗が既に記録されている
- steering `structure.md`「テストレイアウト」— 単体テストと統合テストの配置規約
- 本セッションの実測ログ（接続数サンプリング、専用プローブ）
