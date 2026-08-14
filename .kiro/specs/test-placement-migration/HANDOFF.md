# 引き継ぎ: test-placement-migration（**全タスク完了・検証済み**）

最終更新: 2026-08-15 / 全 20 タスク完了、`/kiro-validate-impl` **GO**（`spec.json` は `ssot: "implementation"`）

> **この spec はもうログである。** 実装が真実であり、`requirements.md` / `design.md` は
> 「どう作ったか」の記録に降格する。コードとの差異は**設計文書側の陳腐化**として扱う。
> 再開すべき作業はない。

---

## 1. 何が達成されたか

steering `structure.md`「テストレイアウト」が定める「DB込みの実起動インスタンスを要する検証は
`tests/` 直下の `*_it.rs` に置く」を、規約としてだけでなく**実態として**成立させた。

| | 着手前 | 完了時 |
|---|---|---|
| 単体テスト位置の `spawn_test_app` 呼び出し | 208 | **18** |
| 内訳 | 21 ファイルに散在 | 例外 13 + 対象外 5 のみ |
| 統合テストファイル | 87 | 106 |
| テスト関数総数 | 2286 | **2286**（不変） |
| フルスイート実行時間（中央値） | 634.80 秒 | 640.89 秒（比 **1.01**） |
| フルスイート 1 回あたりの隔離スキーマ残留 | 1 | **0** |

**移設した 190 箇所すべてで、テスト関数の本体は移設前とバイト一致**（許容した差分は
統合テストクレートから到達するための `crate::` → `kawasemi::` パス接頭辞のみ、9 関数）。

### 要件の充足状況

全要件充足。特に検証機構を伴うもの:

- **要件 1.5（保存則）** — 独立な 2 経路で確認。移設元側 `208 − 18 = 190`、移設先側 `699 − 509 = 190`。
  片側だけでは取りこぼしも二重コピーも検出できない
- **要件 2.6（テスト関数総数）** — 差分の説明ではなく**差分の不在**で満たされた。
  `cargo test` の passed が移設前後とも 2286
- **要件 3.1（公開面）** — 本番モジュールの `pub` 昇格ゼロ。昇格は `src/test_harness/` 配下の
  7 項目 + モジュール宣言のみで、すべて `#[cfg(any(test, feature = "test-harness"))]` ゲート配下
- **要件 5.5（隔離スキーマ）** — 実行による新規生成 0。判定は総数ではなく集合差で行った（後述 C-9）

### 成果物

| 文書 | 内容 |
|---|---|
| `inventory.md` | 着手前に確定させた移設対象の参照表。全 21 ファイル・253 テスト関数の分類 |
| `exceptions.md` | 唯一の記録された例外（13 箇所）、Tier 1 緩和項目、対象外 5 箇所、design.md 表への実測所見 |
| `timing.md` | 移設前後の実行時間、隔離スキーマ残留の退行と真因除去 |
| `verification.md` | 最終検証の全コマンドと逐語出力、掃除の 3 欠陥の記録 |

---

## 2. 次に手を付けるならここ（後続 spec の第一候補）

**A-1〜A-3 は 1 本の spec にまとめるのが自然。** テストハーネスの解放機構と、それを説明する
steering 文言の両方が同じ問題を指している。

### A-1. リーパーの終了時破棄が未修正

`src/test_harness/reaper.rs:112-113` が明文で述べるとおり、**プロセス終了時にキューへ残った
回収要求は失われる**。`Drop` は解放をリーパーへ非同期委譲するだけなので、libtest が最後のテストの
直後に exit する以上、そのプロセスで最後に submit された 1 件は完了する時間が構造的にゼロになる。

先行 spec `test-infrastructure` の所有物であり本 spec の Out of Boundary だったため未修正。
本 spec はテスト側で `cleanup()` を徹底することで回避したが、**機構そのものは同じ状態にある**。

### A-2. 再発を検出する仕組みが存在しない

- CI なし（`.github/` 自体が存在しない）
- 該当する lint なし。`cargo clippy --all-targets` は警告ゼロで通る
- `tests/harness_release_it.rs` はハーネス自己検証だが、`wait_until_reclaimed` で
  **プロセス内待機**するため、プロセス終了時の取りこぼしという当のふるまいを検出できない

タスク 8.3 の退行（残留 1 → 4）はこの空白で起きた。移設そのものが原因ではなく、
**規約違反のテストがプロセス分割によって初めて顕在化した**というのが正確な理解。

### A-3. steering の記述が再発を誘発する向きになっている

`.kiro/steering/structure.md:81` は「**呼ばなくてもリークしない** — `Drop` がプールとスキーマ名を
常駐リーパーへ渡す」と記しているが、**プロセス終了時については不正確**。この記述に従って
`cleanup()` を省いたテストを書くと、そのバイナリは確実に 1 個漏らす。

要件 6.1 が本 spec での steering 変更を禁じたため未修正。

---

## 3. 本 spec が意図的に残した判断

### B-4. 別 spec 起因の陳腐化 2 件（未修正・報告のみ）

本移設が原因ではないため掃除の対象外とした。

- `src/notifications/endpoints.rs:147-165`「Not wired into the module tree yet」
  — notifications spec のタスク 4.2 が配線を完了させたことで陳腐化
- `src/social_graph/endpoints.rs`「nothing mounts this module onto it yet」
  — `src/server.rs:447` が 9 ハンドラを mount 済み

### B-5. 実質的な冗長 2 例（現状維持）

移設物が既存テストに包含され、しかも**既存側のほうが証明力が強い**組が 2 つある。

- `tests/search_module_it.rs::search_end_to_end_through_the_real_router_with_the_default_pg_backend`
  ⊂ `tests/search_type_scope_it.rs::type_statuses_narrows_to_statuses_only`
  （既存側は競合する account と hashtag を実在させたうえで空配列を主張しており narrowing の証明が厳密）
- `tests/timelines_endpoints_handler_it.rs::tag_timeline_any_filter_requires_at_least_one_additional_tag`
  ⊂ `tests/timelines_endpoints_it.rs:965` の `any` 節（クエリ文字列・フィクスチャ・アサーションが一致）

**要件 2.2 が削除を禁じ、要件 2.4 の規範動詞が「新たに作らない」である**ため現状維持とした。
**要件 4 の例外台帳には載せていない** — 4.2 の「公開面の拡大を強いる」に該当せず、
載せると台帳の意味が壊れるため。整理するなら後続 spec で。

### B-6. ユーザー承認による境界拡張が 1 件ある

`tests/social_graph_visibility_query_it.rs` への `cleanup()` 追加は、design.md
「Boundary Commitments」が既存 `tests/*_it.rs` を Out of Boundary と定めているため**形式上は境界外**。
ユーザーの明示的な判断（「小さい修正なので合わせて修正してください」）に基づいて実施した。
加えたのは teardown のみで検証内容は不変。

### B-6b. 配送ダブルを誰も観測していないファイルが 2 つある

孤児則を避けるテストローカル newtype を 3 ファイルに入れたが、**共有セマンティクスを
テストが実際に強制しているのは 1 ファイルだけ**である。

- `tests/social_graph_follow_request_service_it.rs` — `calls().len() == 1` を主張し直後に `[0]` を索引する。
  共有が壊れれば必ず落ちる
- `tests/social_graph_endpoints_it.rs` / `tests/statuses_endpoints_it.rs` — **どのテストも `calls` を読まない**
  （全呼び出し箇所が `_local` / `_http` とアンダースコア束縛）。共有が壊れても検出されない

移設前から同じ性質で本 spec が作ったものではないが、**配送ダブルを持ちながらその挙動を何も主張していない**
状態なので、静かな弱体化が起きうる唯一の場所である。

### B-7. 唯一の例外はコンパイラの制約ではなく方針判断

`src/statuses/render_assembler/tests.rs` の 13 箇所を移設するには 7 項目
（`PollResolution` / `PollResolver` / `RenderContext` / `StatusRenderAssembler` / `new` /
`assemble_many` / `assemble_one`、すべて `pub(crate)`）の公開が要る。

**連鎖は 7 項目で閉じており E0446 の波及もない。つまり公開すれば移設は実際にコンパイルが通る。**
残した理由は要件 3.1 が禁じていること、その根拠は steering `structure.md:36` の
「`StatusRenderAssembler` が唯一の組み立て経路」。**方針が変われば解消できる。**

なお `assemble_one` は `src/statuses/endpoints.rs:525` から本番で呼ばれており、
dead code ではない。公開は実質的な公開面の拡大にあたる。

---

## 4. 計数・検証の落とし穴（再現するときに必読）

### C-8. 計数単位を取り違えない

- **要件 1.1 の判定は生 `git grep` 基準の 18 で行う。** うち 1 件（`src/test_harness/tests.rs` L2）は
  doc コメント内の記述で、コード上の呼び出しは 17。この取り決めは `inventory.md` §6.1 にある
- **doc コメントに `spawn_test_app(`（開き括弧つき）を書くと計数が黙って狂う。**
  括弧なしの `` `spawn_test_app` `` と書くこと
- **要件 2.6 のテスト関数総数は逆にマスク後の値で比較する。** 生 grep は doc コメント内の
  `#[tokio::test]` を拾う。マスク後の基準値は移設前 src 1779 / tests 507

### C-9. 要件 5.5 の判定は総数ではなく集合差で行う

```bash
comm -13 <(sort pre.txt) <(sort post.txt)   # この実行が新たに残したもの
```

起動時スイープの 2 時間閾値により、実行前後の**総数は本実行と無関係に増減する**。
最終実測は 24 → 20 と減っているが、これはスイープが過去の残留 4 件を回収したためで、
**本実行の新規残留は 0**。この区別を失うと判定が無意味になる。

タスク 8.3 では実際に総数の差分（+4 / +4 / +3）を見て「3〜4 件」と誤読し、
「変動がある」かのように退行を過小評価しかけた。集合差では一定して 4 件だった。

### C-10. `design.md`「阻害要因の実測結果」表は信用しない

実装で以下が判明した。

- **過大計上 6 項目** — 表が「移設対象テスト N 本が使う」としているが、実際には
  元位置に残る純粋テストだけが使っていた（2.1 ×3、2.2 ×1、3.2 ×1、4.2 ×1）
- **欠落 1 系統** — 表は**可視性しか模していない**。実際にはコヒーレンス由来の阻害
  （孤児則 E0117。`impl ForeignTrait for Arc<TestDouble>` が別クレートで不成立）が 3 タスクで発生。
  解法はテストローカル newtype で Tier 0
- **原因の誤分類 1 件** — `design.md:328` は E0599 を列挙しているが原因を
  「非公開項目への到達不能」に帰している。実際はトレイトが既に `pub` で、
  必要だったのはスコープへの import だけだった

**Tier 2 予測の的中率は 3 行中 1 行**（`RenderContext` のみ成立、`RequiredPolls` と
`TolerantPolls` は移設候補ですらなかった）。表を根拠に判断する前に実使用箇所を必ず自分で確認すること。

なお表が**正しかった**箇所もある（`design.md:107` の Tier 1 行は正確で、
昇格 7 項目のうち 6 つが実使用されていた）。

---

## 5. 横断的な doc コメント掃除の知見

### D-11. 掃除スクリプトには独立した 3 つの欠陥がある

タスク 8.4 は**3 回「網羅した」と書いて 3 回とも漏れた**。レビューの各ラウンドで 1 つずつ
別種の欠陥が顕在化した。最終的に 36 ファイル・77 件。

- **(a) 突合をファイル単位で行わない。** 「新規作成した移設先だから自分の移設元を名指しするのは
  正当」とファイルごと除外すると、同じ `//!` ブロック内に同居する**別の**移設対象への
  陳腐化参照を巻き添えで落とす。突合はマッチ単位で行い、行番号はコメントブロックの開始行ではなく
  **マッチ行**を出すこと
- **(b) 針はパス修飾・モジュール修飾・裸の 3 形すべてを張る。** 参照の主語が数行上の散文にしか
  ないケースがあり、裸の `` `tests.rs` `` はトークンだけを見る正規表現では原理的に到達できない。
  **本番モジュールの doc（`cargo doc` 描画対象）に多い**
- **(c) 針に閉じ記号を要求しない。** `` `tests\.rs` `` と閉じバッククォート込みで書くと
  `` `tests.rs::item` `` を落とす。(b) の是正だけでは不十分だった

### D-12. 検算は「項目」の実在で行う

**参照先ファイルの実在を見ても意味がない。** 部分移設ではパスは有効なまま中身だけが移設先へ移る。
名指しされた**項目**がそのファイルに実際にあるかを検算すること。

本 spec で実際に起きた 3 型:
ヘルパー関数（`create_test_actor`）、テスト内の構築（`Router::new().route(...)`）、
ハンドラ double（`scoped_probe` / `test_router`）。

### D-12b. 本 spec で確定した 2 つの運用規約（task ノートにしか無い）

どちらも spec 中盤で決めて以降の全タスクに適用した。**再利用可能な規約なので steering 候補**。

- **空になった `tests.rs` は削除し、`#[cfg(test)] mod tests;` も外す。** 移設先へのポインタは
  本番モジュール自身の `//!` doc に畳み込む（`#[cfg(test)]` 配下の doc は `cargo doc` に描画されないため
  案内板として機能しない）。worked example: `src/search.rs` / `src/accounts/account_service.rs`
- **部分移設で `tests.rs` が残る場合も、親 `.rs` の doc が偽になっていないか確認して同タスク内で直す。**
  D-11 は掃除の欠陥を記録しているが、この規約は**そもそも陳腐化を作らないための予防**にあたる

### D-12c. 未処理のまま残った軽微な項目

- `tests/*_it.rs` の**コメント内**に `crate::` 表記が 105 行残っている。タスク 5.2 のノートは
  8.4 の掃除対象として挙げていたが、8.4 の実際の職掌は陳腐化した相互参照になったため未実施。
  既存からの慣行で無害だが、ノートの期待は満たされていない
- `research.md` にも事実誤認が 1 件ある（「エラー 0 件だった 4 ファイルは `use super::` を持たない」— 実際は
  `account_service/tests.rs:39` が持つ）。C-10 は `design.md` の訂正だけを扱っており `research.md` に触れていない

### D-13. 参照の絶対数が大きいのは慣行の裏返し

このリポジトリは「隣接テストモジュールの慣行を明示的に引用する」文書化された慣行を持つ。
`/` 修飾形だけで 399 ヒット、`` `tests.rs::項目` `` 形で 63 ヒットある。
横断掃除のコストはこの慣行とセットで見積もること。

---

## 6. 検証の再現

```bash
# 配置（要件 1.1）— 18 = 例外 13 + 対象外 5
git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' | wc -l
git grep -c 'spawn_test_app(' -- 'src/**/tests.rs'

# 保存則（要件 1.5）— 独立な 2 経路がともに 190
git grep -o 'spawn_test_app(' 5ad15a5 -- 'src/**/tests.rs' | wc -l   # 208 → 208 - 18 = 190
git grep -o 'spawn_test_app(' 5ad15a5 -- 'tests/*_it.rs' | wc -l     # 509
git grep -o 'spawn_test_app(' -- 'tests/*_it.rs' | wc -l             # 699 → 699 - 509 = 190

# 公開面（要件 3.1）— ヒットはすべて src/test_harness/ 配下
git diff 5ad15a5..HEAD -- src/ | grep -E '^\+\s*pub (fn|struct|enum|trait|mod|const|type)'

# steering 無変更（要件 6.1）— 空であること
git diff 5ad15a5..HEAD -- .kiro/steering/

# フルスイート（要件 2.5 / 5.4 / 5.5）
psql "$URL" -Atc "select schema_name from information_schema.schemata \
  where schema_name like 'kawasemi\_test\_harness\_%' order by 1" > pre.txt
cargo test
psql "$URL" -Atc "..." > post.txt
comm -13 <(sort pre.txt) <(sort post.txt)   # 空であること
```

全コマンドの逐語出力は `verification.md` にある。

**落とし穴**（先行 spec と本 spec で実際に踏んだもの）:

- `cargo check --tests` に `--keep-going` を付けないとターゲットが打ち切られ、
  「エラー 0」と「未検査」が区別できない
- `cargo build --tests` と `--all-features` は `target/debug/libkawasemi.rlib` を feature ON 版で
  上書きする。rlib を検査するなら直前に素の `cargo build` を打つこと
- フルスイートの出力を `tail` で切らない。失敗理由が失われる

テスト DB: `postgres://kawasemi_test:kawasemi_test_pw@127.0.0.1:5432/kawasemi_test`
