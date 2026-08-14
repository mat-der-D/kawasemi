# 引き継ぎ: test-infrastructure（**完了・handoff 済み**）

最終更新: 2026-08-14 / 作業ツリー: クリーン
`spec.json`: `phase: "implemented"`、`ssot: "implementation"`。

> **この spec はもうログである。** 現状を知りたい場合は実装（`src/test_harness*`、
> `src/federation/test_harness.rs`）と steering `structure.md`「テストレイアウト」を読むこと。
> 以下は当時どう作ったかの記録であり、現状説明として読んではならない。

`/kiro-validate-impl` は一度 NO-GO を出し、その残件 4 つを解消したうえで再実行して **GO**（`f06149f`）。
handoff 3 点セットは完了している:

1. `ssot` 反転 — `f06149f`
2. トレーサビリティ参照の除去 — `613118d`
3. steering の同期 — `14391f7`

**繰り越し**: 要件 4.5 は逐語的には未充足のまま。残存 208 箇所 / 21 ファイルの移設は
後続 spec `test-placement-migration` が引き取る（決定の記録は `placement-audit.md` §2）。

---

## 1. 現在地

タスクは **全 15 件が `[x]`**、実装コミットは 13 本（`b50d8fd..f71c2de`）に
残件解消の 4 本（`aeeefc5..a1c6dd8`）。
最終ゲート `/kiro-validate-impl` を 4 次元で実行して一度 **NO-GO** と判定し、
そこで挙げた残件 4 つを解消したうえで再実行して **GO**（詳細は §2・§3）。

**本 spec の中核は達成済み**（後述の残件はいずれも中核と独立）:

| | 着手前 | 現在 |
|---|---|---|
| `cargo test` 完全実行 | 一括不可（256 失敗・全件 `PoolTimedOut`） | **2286 passed / 0 failed**（90 ターゲット） |
| `cargo test --lib` | 256 失敗 | **1779 passed / 0 failed** |
| 事前のスキーマ掃除 | 必須 | 不要 |
| モジュール分割 | 必須 | 不要 |
| 実行後の残存スキーマ | 239 個 | 0 個（`--lib`）／ 1 個（完全実行。回収漏れではなく回収の遅延、`final-metrics.md` §2.3） |
| 一括実行時間 | 891 秒 | 約 330 秒（中央値、9 回計測） |
| 実インスタンス起動を要するテスト | 750（出現回数）／745（関数） | 210 ／ 206 |
| 接続ピーク | 上限 97 に到達して停止 | 17〜23（上限は**テスト件数ではなく同時実行スレッド数**で頭打ち） |

スモークも実施済み: リリースバイナリを実起動し `/health` 200、`/api/v2/instance` 200、SIGTERM で正常終了。

---

## 2. NO-GO 残件の解消（2026-08-14・完了）

### 項目 1: steering の取り残し → 解消

`.kiro/steering/tech.md` の箇条書きが「孤立スキーマの起動時スイープ未実装」「恒久対応には
実装が必要（未着手）」と書いたままだった。タスク 2.2 で実装済み・残存 0 件を実測しているので
明確な誤り。リーパー + 起動時スイープの二段構え、真因（`Drop` が `pool.close()` を
呼んでいなかったこと）、および隔離スキーマを作るフィクスチャは必ず `establish_isolated_db` を
通す必要があることへ書き換えた（`46dae21`）。

### 項目 2: 連合ペアハーネスの回収漏れ → 解消（選択肢 (a)）

`src/federation/test_harness.rs` が持っていた schema/pool/migrate の 3 つ目の複製を削除し、
`spawn_paired_instance` を `crate::test_harness::establish_isolated_db()` 経由にした
（`establish_isolated_db` を `pub(crate)` 化）。接頭辞が `kawasemi_test_harness_` に揃い、
起動時スイープを通り、`max_connections` も 5 → 2 に揃う。削除したのは
`PAIR_TEST_DB_URL_ENV` / `DEFAULT_PAIR_TEST_DB_URL` / `base_test_db_url` /
`unique_pair_schema_name` / `admin_db_config` / `schema_scoped_url` / `create_schema`
（`aeeefc5`）。

判断の記録は `tasks.md` の Implementation Notes に移した（「一括実行前に判断が要る」と
書きながらゲートを張らなかった手続き上の反省も含む）。

### 項目 3: 要件 4.5 の解釈 → ユーザー判断で決着（選択肢 (b)）

**残存 208 箇所 / 21 ファイルの移設は別 spec として切り出す。** 要件 4.5 の文言も
steering の配置規約も書き換えず、208 箇所を本 spec からの明示的な繰り越しとして扱う。
決着と後続 spec の範囲・論点は `placement-audit.md` §2「決着」に記録した。
後続 spec `test-placement-migration` を初期化し、`roadmap.md` にも追加した（`a1c6dd8`）。

**再検証時の含意**: 逐語的に読めば要件 4.5 は本 spec の内部では未充足のままである。
GO 判定はこの繰り越しを承認したうえで下すことになる（未検出の欠落ではなく、記録された繰り越し）。

### 項目 4: `final-metrics.md` §2 のスコープ → 解消

表と結論を 3 条件（`cargo test --lib` 一括 / フィルタ / `cargo test` 完全）に分け、
完全実行の測定を §2.3 として追加した。あわせて「次回スイープで自動回収」を訂正した
（`RECLAIM_THRESHOLD` は 2 時間なので、回収されるのは作成から 2 時間以上経った後の
最初のスイープ。2 時間以内に完全実行を繰り返すと一時的に実行回数ぶん積み上がる）（`4252900`）。

### 解消後の検証結果（実測）

- `cargo test`: **2286 passed / 0 failed / 5 ignored**、exit 0（90 ターゲット）
- `federation_pair_it` 3 本 / `harness_release_it` 3 本 / `harness_sweep_it` 4 本: 全 pass、
  実行後の残存スキーマ 0 個
- `cargo clippy --all-targets`: 警告ゼロ / `cargo fmt --check`: OK
- 完全実行後の残存スキーマ: 1 個（`kawasemi_test_harness_..._1`。§1 の表と `final-metrics.md` §2.3 のとおり）

---

## 3. handoff の記録

`/kiro-validate-impl` の GO 後、委譲されていた 2 作業を実施した。

- **トレーサビリティ参照の除去**（`613118d`）: 本 spec が追加した参照のみを対象にした。
  他 spec（core-runtime / api-foundation / media-pipeline / statuses-core）が入れた参照は
  各 spec の境界に属するため残している。設計根拠と同じ文に参照が埋まっている箇所は
  行ごと削らず文を書き直した。本 spec の新規ファイル（`sweep.rs` / `reaper.rs` /
  `db_fixture.rs` とその `tests.rs`、`harness_release_it.rs` / `harness_sweep_it.rs`）は
  参照ゼロに到達。
- **steering の同期**（`14391f7`）: 実装を読んで書き起こした。フィクスチャの 2 段階・
  隔離と回収・feature ゲートの恒久パターンは `structure.md`「テストレイアウト」へ、
  コードから読み取れない事情は `tech.md` へ振り分けた。

**次の作業は `test-placement-migration` spec**
（`/kiro-spec-requirements test-placement-migration` から。Dependencies: test-infrastructure）。

---

## 4. 検証に使うコマンド（確定済み）

```
cargo test                       # 完全実行。約 19 分（コンパイル込み）、2286 passed
cargo test --lib                 # 1779 passed、約 330 秒
cargo test --lib -- <module>::   # モジュール単位。部分文字列形式（-- 無し）とは件数が違うので注意
cargo test --test <name>         # 統合バイナリ個別
cargo build --release            # 素のビルド（feature OFF）
cargo clippy --all-targets       # 警告ゼロが基準
cargo fmt --check
```

テスト DB: `postgres://kawasemi_test:kawasemi_test_pw@127.0.0.1:5432/kawasemi_test`

```
# 残存スキーマ確認（アンダースコアのエスケープ必須。_ は LIKE のワイルドカード）
psql "<url>" -c "select count(*) from information_schema.schemata where schema_name like 'kawasemi\_%'"
```

### 落とし穴（実際に踏んだもの）

- **`cargo build --tests` と `--all-features` は `target/debug/libkawasemi.rlib` を feature ON 版で上書きする。** 成果物の中身を検査するときは直前に必ず素の `cargo build` / `--release` を打つこと。さもないと偽の失敗を見る
- **共有テスト DB を測るテストはテスト間分離を自前で持つこと。** `information_schema` をサーバ全体で舐めると兄弟テストの生存スキーマを自分の残骸と誤検出して flaky になる（実際に 1 回目 FAIL / 2 回目 PASS を踏んだ）
- **実行時間は単発測定で判断しない。** 9 回計測で 323〜560 秒。効果より測定間のばらつきが大きい
- **フルスイートの出力を `tail` で切らない。** 失敗理由が失われる

---

## 5. 判断の根拠が要るときに読む文書

| 文書 | 内容 |
|---|---|
| `tasks.md` の `## Implementation Notes` | 全タスクの申し送りと発見（最も情報密度が高い） |
| `migration-classification.md` | 745 件の分類。§6 に「両フィクスチャでコンパイルが通るのに挙動が変わる 2 経路」 |
| `placement-audit.md` | 要件 4.5 の配置監査。§2 に対立仮説も含めた論証 |
| `worker-claim-investigation.md` | 配送ワーカーのクレーム 0 件現象の根本原因（解決済み） |
| `final-metrics.md` | 3 指標の測定値と 4.3 の判定機序 |

---

## 6. 覚えておくべき結論（再導出しないこと）

- **接続リークの真因**: `Drop for TestApp` が `pool.close()` を一度も呼んでいなかった。スキーマ削除のデタッチはしていたので「スキーマは消えるが接続は残る」状態だった。破棄ごとに約 2 接続ずつ積み上がる
- **`TestDb` 移行は実行時間を縮めない**（要件 4.1 は満たすが 4.3 には効かない）。支配的なのは両フィクスチャが共有する `establish_isolated_db` のマイグレーション適用（フィクスチャ 1 個ごとに 11 本）。さらに短縮するにはテンプレートスキーマ複製等が要るが、design の Non-Goals にあたる
- **要件 4.3 が達成された機序**はリーパー + 起動時スイープによるリーク解消。正確には「スイートが速くなった」のではなく「**ベースラインが病的だった**」。1 件あたりのコストはほぼ不変
- **891 秒のベースラインはプール 5 で測られている**（`2703704` は spec 初期化の 84 秒前）。厳密な like-for-like ではないが、当時プール 2 では構造的に完走しないので判定は覆らない
- **配送ワーカーのクレーム 0 件**は解決済み。`spawn_test_app` が同じプール・同じスキーマで本物の配送ループを走らせていた競合。テスト固有でプロダクション欠陥ではない。**プール 2 でも低頻度（約 1/35）で再現するので、今後この 1 件が落ちても資源枯渇ではない**
- **要件 4 の Objective「DB なし」は未達**（`TestDb` も隔離スキーマ + 11 本のマイグレーションを要する）。ただし Objective は受入基準ではないので 4.1〜4.5 の被覆には影響しない
- **`LIKE 'kawasemi_test_harness_%'` を使わない。** `_` は LIKE のワイルドカードなので見た目より広く一致する
- **起動時スイープは「次回実行で必ず消す」機構ではない。** `RECLAIM_THRESHOLD` は 2 時間（`src/test_harness/sweep.rs:99`）で、生存中のフィクスチャを誤射しないために埋め込み時刻がそれより古いものだけを回収する。完全実行後に残る 1 個は、直後に再実行しても残ったままになる
- **隔離スキーマを作るフィクスチャは必ず `establish_isolated_db` を通す。** 回収の 2 経路（`HarnessReaper` と起動時スイープ）はどちらも接頭辞 `kawasemi_test_harness_` だけを手がかりに対象を決めるので、独自の接頭辞を持つ複製はどの回収経路からも見えなくなる。連合ペアハーネスがまさにこれだった
