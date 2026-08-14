# 最終検証の記録 — test-placement-migration タスク 8.4

要件 6.3「完了を主張する場合、文書化した手順を実行した結果を証拠として示す」に対応する。
本文書は**主張ではなく実行結果そのもの**を保持する。各節はコマンドと逐語の出力を並べる。

**なぜ `exceptions.md` §5 に追記せず別文書にしたか**: `exceptions.md` は要件 4 の例外台帳であり、
その §5 は「台帳に載る件数を再現する手順」に閉じている。タスク 8.4 が要求する検証は
件数の再現に加えて公開面・ビルド・steering・lint・フルスイートを含み、台帳の主題ではない。
台帳へ混ぜると「例外の一覧」という §5 の役割が薄まるため分離した。件数の節（§1・§2）は
`exceptions.md` §5 と同じコマンドを再実行した結果であり、両者は一致する。

**実行日**: 2026-08-15（§7 の是正と §8 のフルスイート反映はレビュー round 1 後）
**実行リビジョン**: `2539bde`（`.kiro/specs/` の未コミット差分と、本タスクの doc コメント掃除
（§7）を作業ツリーに載せた状態。`git grep` は作業ツリーの追跡ファイルを走査する）
**移設前の基準リビジョン**: `5ad15a5`
**計数の単位**: 呼び出し箇所（`spawn_test_app(` というトークンの出現数）。テスト関数の本数ではない（要件 6.4）

---

## 1. 配置 — 単体テスト位置に残る呼び出し（要件 1.1）

```console
$ git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' | wc -l
18
```

内訳（行単位 `-c`）:

```console
$ git grep -c 'spawn_test_app(' -- 'src/**/tests.rs'
src/statuses/render_assembler/tests.rs:13
src/test_harness/tests.rs:5
```

内訳（出現単位 `-o`。1 行 2 呼び出しがあっても正しく数える）:

```console
$ git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' | cut -d: -f1 | sort | uniq -c
     13 src/statuses/render_assembler/tests.rs
      5 src/test_harness/tests.rs
```

**18 = 例外 13 + 対象外 5。** 他のファイルはゼロであり、要件 1.1 の
「記録された例外と対象外を除いて 0」が実測で成り立つ。

- **13** = `exceptions.md` §1.1 の記録された例外（`statuses::render_assembler::RenderContext` 連鎖、Tier 2）
- **5** = `exceptions.md` §3 の対象外（`src/test_harness/tests.rs`。先行 spec `test-infrastructure` の既決事項。
  うち L2 は `//!` doc 内の記述で、コード上の呼び出しは 4 箇所 — `inventory.md` §6.1）

## 2. 保存則（要件 1.5）

### 2.1 移設元側からの引き算

```console
$ git grep -o 'spawn_test_app(' 5ad15a5 -- 'src/**/tests.rs' | wc -l
208
```

```console
$ git grep -c 'spawn_test_app(' 5ad15a5 -- 'src/**/tests.rs' | wc -l
21
```

| 式 | 値 |
|---|---|
| 移設前の総数 | 208 箇所 / 21 ファイル |
| − 対象外（`src/test_harness/tests.rs`） | − 5 箇所 / − 1 ファイル |
| = 本 spec の対象（要件 1.4） | **203 箇所 / 20 ファイル** |
| − 記録された例外（§1） | − 13 箇所 |
| = 移設した件数 | **190 箇所** |

**203 = 190 + 13** ✅（要件 1.5）

### 2.2 移設先側からの独立検証（`exceptions.md` §5.6 の第 2 経路）

```console
$ git grep -o 'spawn_test_app(' -- 'tests/*_it.rs' | wc -l
699
```

```console
$ git grep -o 'spawn_test_app(' 5ad15a5 -- 'tests/*_it.rs' | wc -l
509
```

**699 − 509 = 190。** 移設元の減少 `208 − 18 = 190` と一致する。
片側だけでは取りこぼしも二重コピーも検出できないため、**両側の一致が要件 1.5 の実質的な証明**にあたる。

### 2.3 統合テストファイル数（要件 1.3）

```console
$ git ls-files 'tests/*_it.rs' | wc -l
106
```

```console
$ git ls-tree -r --name-only 5ad15a5 tests/ | grep '_it\.rs$' | wc -l
87
```

**87 + 19 = 106。** 新規 19 件は design.md「File Structure Plan」の移設先 19 件に対応する
（`tests/timelines_endpoints_it.rs` のみ既存と衝突したため `tests/timelines_endpoints_handler_it.rs` へ改名。
既存ファイルは無変更）。全 19 件が `_it.rs` サフィックスで命名されている。

## 3. テスト関数総数の保存（要件 2.6）

`inventory.md` §5 / §7 出力 H の方法。行コメント・ブロックコメント・doc コメント・
文字列リテラル・raw 文字列・文字リテラルを空白に置換してから `#[test]` / `#[tokio::test]` を走査する
（**生 grep は doc コメント内の記述を数えるため使わない**）。
属性の綴りはリポジトリ内に上記 2 種類しか存在しない（`inventory.md` §7 出力 I）。

移設前（`5ad15a5` を `git worktree` で切り出して同一スクリプトを実行）:

```console
$ python3 mask_count.py          # in a detached worktree at 5ad15a5
src files 320 raw 1807 masked(code only) 1779
tests files 87 raw 524 masked(code only) 507
```

移設後（作業ツリー）:

```console
$ python3 mask_count.py
src files 311 raw 1614 masked(code only) 1589
tests files 106 raw 714 masked(code only) 697
```

| 対象 | 移設前 | 移設後 | 差 |
|---|---|---|---|
| `src/**` | 1779 | **1589** | − 190 |
| `tests/**` | 507 | **697** | + 190 |
| **合計** | **2286** | **2286** | **0** |

**要件 2.6 は「差分が説明可能であること」を求めるが、差分そのものが存在しない**（合計 2286 が不変）。
`src` の減少 190 と `tests` の増加 190 が §2 の呼び出し箇所ベースの 190 と一致する。
移設を理由とした削除・無効化・`#[ignore]` 付与は 1 件もない（要件 2.2）。

計数スクリプトは `inventory.md` §7 出力 H の記述をそのまま実装したものである。

## 4. 本番成果物の公開面（要件 3.1 / 3.2 / 3.4 / 3.5）

### 4.1 既定フィーチャのビルド

```console
$ cargo build
   Compiling kawasemi v0.1.0 (/home/smoothpudding/Documents/dev/github/kawasemi)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.64s
```

### 4.2 公開へ昇格した項目の全数

spec 全体の差分から、`src/` に追加された `pub` 項目を機械的に抽出する。

```console
$ git diff 5ad15a5..HEAD -- src/ | awk '/^diff --git/{f=$3} /^\+[[:space:]]*pub (fn|struct|enum|trait|mod|const|type)/{print f"  |"$0}'
a/src/test_harness.rs  |+pub mod query_log;
a/src/test_harness/query_log.rs  |+pub enum QueryKind {
a/src/test_harness/query_log.rs  |+pub struct QueryLog {
a/src/test_harness/query_log.rs  |+    pub fn per_statement(&self) -> BTreeMap<String, usize> {
a/src/test_harness/query_log.rs  |+    pub fn count(&self, kind: QueryKind) -> usize {
a/src/test_harness/query_log.rs  |+    pub fn count_matching(&self, needle: &str) -> usize {
a/src/test_harness/query_log.rs  |+    pub fn require_kinds(&self, kinds: &[QueryKind]) {
```

`record_queries` はシグネチャが 1 行にまとまったため上の正規表現から漏れる。差分本体で確認する:

```console
$ git diff 5ad15a5..HEAD -- src/ | grep -n 'record_queries' | tail -2
13872:-pub(crate) async fn record_queries<F: Future>(
13876:+pub async fn record_queries<F: Future>(pool: &sqlx::PgPool, fut: F) -> (F::Output, QueryLog) {
```

`pub(crate)` から昇格した項目の全数（削除側から見る）:

```console
$ git diff 5ad15a5..HEAD -- src/ | awk '/^diff --git/{f=$3} /^-[[:space:]]*pub\(crate\)/{print f"  |"$0}'
a/src/test_harness.rs  |-pub(crate) mod query_log;
a/src/test_harness/query_log.rs  |-pub(crate) enum QueryKind {
a/src/test_harness/query_log.rs  |-pub(crate) struct QueryLog {
a/src/test_harness/query_log.rs  |-    pub(crate) fn per_statement(&self) -> BTreeMap<String, usize> {
a/src/test_harness/query_log.rs  |-    pub(crate) fn count(&self, kind: QueryKind) -> usize {
a/src/test_harness/query_log.rs  |-    pub(crate) fn count_matching(&self, needle: &str) -> usize {
a/src/test_harness/query_log.rs  |-    pub(crate) fn require_kinds(&self, kinds: &[QueryKind]) {
a/src/test_harness/query_log.rs  |-pub(crate) async fn record_queries<F: Future>(
```

`src/test_harness/` の外に追加された `pub`（あらゆる綴り）:

```console
$ git diff 5ad15a5..HEAD -- src/ | awk '/^diff --git/{f=$3} /^\+[[:space:]]*pub[ (]/{if (f !~ /test_harness/) print f"  |"$0}'
（出力なし）
```

**昇格は 7 項目 + モジュール宣言 1 件の計 8 件、すべて `src/test_harness/` 配下。
本番モジュールの項目を公開へ昇格させる変更は差分に 1 件も含まれない。** ✅（要件 3.1）

これらはすべて `src/lib.rs` の `#[cfg(any(test, feature = "test-harness"))]` 付きモジュール宣言の
配下にあり、可視性の緩和はテスト構成に限定されている（要件 3.2）。
一覧と各項目の根拠は `exceptions.md` §2.2（要件 3.5）。

### 4.3 ビルド成果物にテスト専用資産が入っていないこと（要件 3.4）

**注意**: `cargo build --tests` / `--all-features` は `target/debug/libkawasemi.rlib` を
feature ON のビルドで上書きする（`HANDOFF.md` §5。先行 spec が実際に踏んだ罠）。
以下は素の `cargo build`（§4.1）**直後**に、`--tests` を挟まずに実行した。

```console
$ nm target/debug/libkawasemi.rlib | grep -i 'spawn_test_app\|test_harness'
（出力なし）
```

```console
$ strings target/debug/libkawasemi.rlib | grep -c 'query_log'
0
```

`strings` は `spawn_test_app` を 38 件拾うが、いずれも本番モジュールの doc コメント本文
（rmeta に載る文字列）であってシンボルではない。`nm` にハーネス由来のシンボルは 1 つも現れない。

```console
$ strings target/debug/libkawasemi.rlib | grep 'spawn_test_app' | head -3
E Postgres instance (via `spawn_test_app`) is this crate's established
K therefore requires a running instance (`spawn_test_app`); they all live in
F `spawn_test_app`) is this crate's established verification method for
```

## 5. steering の不変性と実態との一致（要件 6.1 / 6.5）

```console
$ git diff 5ad15a5..HEAD -- .kiro/steering/
（出力なし）
```

```console
$ git diff 5ad15a5 -- .kiro/steering/ | wc -l
0
```

**規約文言は 1 文字も変更されていない**（作業ツリーを含めて差分ゼロ）。✅（要件 6.1）

規約本文（`.kiro/steering/structure.md:59-60`）:

> - **単体テスト**：実装ファイルと同階層の `tests.rs` サブモジュールに置く（例：`src/actor/service.rs` → `src/actor/service/tests.rs`）。`#[cfg(test)] mod tests;` で親から宣言する。
> - **統合テスト**：`tests/` 直下、ファイル名は `_it.rs` サフィックス（例：`tests/actor_lifecycle_it.rs`）。DB込みの実起動インスタンスを要する検証はここに置く。

**規約と実態の一致（要件 6.5）**:

- 「DB込みの実起動インスタンスを要する検証は `tests/` 直下に置く」— §1 の実測 18 箇所が
  記録された例外 13 と対象外 5 のみであり、それ以外の単体テスト位置には 1 箇所も残っていない
- 「ファイル名は `_it.rs` サフィックス」— §2.3 の新規 19 ファイルすべてが `_it.rs`
- 「`tests.rs` があるなら単体テストがある」— 移設でテスト関数が 0 本になった 9 ファイルは
  ファイルごと削除し `#[cfg(test)] mod tests;` の宣言も外した（tasks.md 実装ノート 2.2 の方針）

## 6. 静的検査

```console
$ cargo fmt --check
（出力なし・rc=0）
```

```console
$ cargo clippy --all-targets --keep-going
    Checking kawasemi v0.1.0 (/home/smoothpudding/Documents/dev/github/kawasemi)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.87s
```

```console
$ cargo clippy --all-targets --keep-going 2>&1 | grep -c '^warning\|^error'
0
```

**警告ゼロ。**（いずれも §7 の是正を載せた最終状態に対する再実行）

`cargo doc --no-deps` は crate 全体で 334 件の rustdoc 警告を出すが、**すべて本タスク以前から存在するもの**で、
§7 で編集した doc コメントの行はいずれも警告を出していない（`src/timelines/endpoints.rs` の唯一の警告は
:90 の `parse_tag_query_pairs` へのリンクで、本タスクが触れた :152-157 とは別の箇所）。
既存の警告の解消は本 spec の Boundary 外である。

## 7. 移設で陳腐化した相互参照の掃除

移設は他ファイルの doc コメントに書かれた相互参照（移設元ファイル・そのテスト・ヘルパーを
名指しするもの）を偽にする。各移設タスクの境界外に落ちるため、本タスクで横断的に掃除した。

### 7.1 探索の方法

> **⚠ 以下は当初の方法で、これだけでは不十分だった。** レビュー round 1 / round 2 で計 6 件の
> 取りこぼしが判明し、手順 4（針の形）に 2 種類、手順 5（突合の粒度）に 1 種類、
> 計 **3 つの独立した欠陥**があることがわかった。
> **必ず §7.3 の 3 つの原因・検算の原則・是正後の照合結果まで読むこと。**

行単位 grep では不十分である（tasks.md 実装ノート 2.2）。以下を行うスクリプトで走査した:

1. `src/**/*.rs` と `tests/*.rs` の全追跡ファイルからコメント連なり（`//` `///` `//!` `/* */`）を抽出
2. 改行と行頭のコメントマーカーを潰して 1 本の文字列にする（`tests/webfinger_nodeinfo_it.rs:22-23` は
   移設元パスが行をまたいで折り返されており、この平坦化なしには検出できない）
3. さらに `\s*/\s*` を `/` に縮約する（折り返しがパス区切りに割り込む場合に効く）
4. `(?:[A-Za-z_*]+/)*[A-Za-z_*]+/tests\.rs` と `(?:[a-z_]+::)+tests\b` で照合する。
   `*` を文字クラスに含めるのでグロブ表記（`src/federation/endpoints/*/tests.rs`）も拾う
5. 得られた参照を移設対象 20 パスの完全形・末尾 1〜2 セグメント形・モジュールパス形と突き合わせる

**是正後は、これに加えて** (a) 突合をファイル単位ではなく**マッチ単位**で行い、
(b) パス修飾のない裸の `` `tests.rs` `` を別途走査し、
(c) 針に閉じバッククォートを要求せず `` `tests.rs::item` `` 形も拾い、
(d) 参照先**ファイル**の実在ではなく名指しされた**項目**の実在で全件検算する — §7.3 参照。

**参照先がまだ存在するもの（部分移設で `tests.rs` が残ったケース）は、名指しされた
ヘルパー・テストが実際に残存側にあるかを個別に確認した。** 例:

- `tests/statuses_account_provider_it.rs` → `render_assembler/tests.rs` の
  `create_test_actor` / `seed_custom_emoji` / `material_fingerprint` / クエリ計測節はすべて残存 → **無変更**
- `tests/search_service_it.rs` → `search/hydrator/tests.rs` の `create_test_actor` は
  `tests/search_hydrator_it.rs` へ移動済み → **修正**

### 7.2 修正前の状態（RED — 陳腐化していた参照）

**36 ファイル・77 件の置換。** 参照先ファイルが削除されたもの（移設で空になり削除された 9 ファイル）と、
参照先ファイルは残るが名指しされたヘルパー・テストが移設先へ移ったものの両方を含む。

**うち末尾 6 行はレビューで追加した分である** — round 1 で 5 件（4 件は指摘そのもの、
1 件は指摘の原因を追ううちに同じ欠陥で見つけたもの）、round 2 でさらに 1 件。
走査がなぜ 2 度も取りこぼしたかは §7.3 に 3 つの欠陥として記録した。

| 参照元 | 陳腐化した参照先 | 訂正後 |
|---|---|---|
| `src/accounts/instance_service/tests.rs:10` | `src/accounts/account_service/tests.rs` | `tests/accounts_account_service_it.rs` |
| `src/social_graph/follow_service/tests.rs:8-9, 38` | `account_service/tests.rs`（折り返し） | 同上 |
| `src/search.rs:275` | `search::service::tests` | `tests/search_service_it.rs` |
| `src/test_harness.rs:438` | `federation::outbound::worker::tests::run_once_...` | `tests/federation_outbound_worker_it.rs` |
| `src/statuses/render_assembler/tests.rs:471` | `statuses::endpoints::tests` | `tests/statuses_endpoints_it.rs` |
| `src/federation/endpoints/ap_get/tests.rs:6` | `webfinger/tests.rs` | `tests/federation_webfinger_endpoint_it.rs` |
| `src/federation/endpoints/nodeinfo/tests.rs:5` | `webfinger/tests.rs` | 同上（履歴として旧位置も残す） |
| `tests/signatures_it.rs:8, 108, 486` | `signatures/*/tests.rs`（グロブ）・`negotiation/tests.rs` | `tests/federation_signatures_{signer,negotiation}_it.rs` |
| `tests/webfinger_nodeinfo_it.rs:22-23, 45` | `webfinger/tests.rs`（折り返し）・`endpoints/*/tests.rs`（グロブ） | `tests/federation_webfinger_endpoint_it.rs` |
| `tests/federation_signatures_negotiation_it.rs:39, 71` | `signer/tests.rs::create_signable_actor` / `signer_for` | `tests/federation_signatures_signer_it.rs` |
| `tests/inbox_delivery_it.rs:13, 41, 53, 58, 90, 96, 533, 715, 751` | `outbound/worker/tests.rs` | `tests/federation_outbound_worker_it.rs` |
| `tests/auth_scope_it.rs:8, 56` | `src/oauth/middleware/tests.rs` | `tests/oauth_middleware_it.rs` |
| `tests/media_endpoints_it.rs:51, 60, 90` | `oauth/middleware/tests.rs::{test_token_hash_key, issue_test_token}` | 同上 |
| `tests/social_graph_endpoints_it.rs:397, 420` | `oauth::middleware::tests::{register_test_app, issue_test_token}` | 同上 |
| `tests/social_graph_endpoints_it.rs:121, 874` | `follow_request_service/tests.rs` | `tests/social_graph_follow_request_service_it.rs` |
| `tests/notifications_service_it.rs:62` | `follow_request_service/tests.rs::create_test_actor` | 同上 |
| `tests/notifications_service_it.rs:671` | `statuses/account_provider/tests.rs` の `seed_custom_emoji` | `tests/statuses_account_provider_it.rs` |
| `tests/notification_list_it.rs:8, 11` | `src/notifications/endpoints/tests.rs` | `tests/notifications_endpoints_it.rs` |
| `tests/notification_show_dismiss_it.rs:8, 14` | 同上 | 同上 |
| `tests/search_endpoint_it.rs:221, 243` | `notifications::endpoints::tests::{register_test_app, issue_test_token}` | 同上 |
| `tests/notification_contract_it.rs:52` | `src/notifications/tests.rs` | `tests/notifications_module_it.rs` |
| `tests/search_module_it.rs:5, 8, 72` | `kawasemi::notifications::tests` / `src/notifications/tests.rs` | 同上 |
| `tests/notifications_endpoints_it.rs:60, 159` | `notifications/service/tests.rs::build_service` | `tests/notifications_service_it.rs` |
| `tests/notifications_endpoints_it.rs:198-199, 221-222` | `social_graph::endpoints::tests` / `timelines::endpoints::tests` | `tests/social_graph_endpoints_it.rs` / `tests/timelines_endpoints_handler_it.rs` |
| `tests/timelines_endpoints_handler_it.rs:195, 218` | `social_graph::endpoints::tests` | `tests/social_graph_endpoints_it.rs` |
| `tests/search_hydrator_it.rs:19, 67` | `notifications/service/tests.rs::create_test_actor` | `tests/notifications_service_it.rs` |
| `tests/search_service_it.rs:15` | `search/hydrator/tests.rs` の `create_test_actor` | `tests/search_hydrator_it.rs` |
| `tests/search_accounts_it.rs:31, 58` | `src/search/hydrator/tests.rs` | 同上 |
| `tests/search_statuses_it.rs:36` | 同上 | 同上 |
| `tests/search_hashtags_it.rs:43` | `src/search/service/tests.rs` | `tests/search_service_it.rs` |
| `tests/search_resolve_it.rs:29-30, 54-55, 223` | `search/{endpoint,service}/tests.rs`・`crate::search::endpoint::tests` | `tests/search_{endpoint,service}_it.rs` |
| `tests/search_backend_swap_it.rs:40-41` | `src/search/endpoint/tests.rs` の `build_router` | `tests/search_endpoint_it.rs` |
| `tests/search_type_scope_it.rs:30` | `src/search/endpoint/tests.rs` | 同上 |
| `tests/remote_account_fetch_it.rs:35, 74, 97` | `src/accounts/account_service/tests.rs` | `tests/accounts_account_service_it.rs` |
| `tests/timelines_endpoints_it.rs:10, 15, 50, 265` | `src/timelines/endpoints/tests.rs` | `tests/timelines_endpoints_handler_it.rs` |
| `tests/timelines_bootstrap_wiring_it.rs:14, 150` | 同上 | 同上 |
| `tests/notifications_service_it.rs:31-32` ⚠ | `src/social_graph/follow_request_service/tests.rs` | `tests/social_graph_follow_request_service_it.rs` |
| `tests/notifications_module_it.rs:34` ⚠ | `src/social_graph/tests.rs` | `tests/social_graph_module_it.rs` |
| `tests/notifications_module_it.rs:54` ⚠ | 同上 | 同上 |
| `src/timelines/endpoints.rs:154` ⚠ | 裸の `` `tests.rs` ``（主語は 3 行上の `social_graph::endpoints`）。`Router::new().route(...)` の組み立ては移設済み | `tests/social_graph_endpoints_it.rs` |
| `src/oauth/middleware.rs:285` ⚠ | 裸の `` `tests.rs` `` の `test_router` | `tests/oauth_middleware_it.rs` |
| `src/oauth/middleware.rs:130` ⚠⚠ | `` `tests.rs::scoped_probe` ``（閉じバッククォートが続かない綴り）。同ファイル L141-142 と自己矛盾していた | `tests/oauth_middleware_it.rs` |

⚠ = レビュー round 1 で判明した取りこぼし / ⚠⚠ = round 2 で判明した取りこぼし。
最後の 3 件はいずれも `cargo doc` に描画される**本番モジュール doc**であり、うち 2 件は同一ファイル（`src/oauth/middleware.rs`）である。

### 7.3 走査の欠陥 — 「網羅した」と書いて漏れていた事実

**本節は「網羅した」と 2 回書いて、2 回とも漏れていた。**

- round 1 前: 「残る参照はすべて次の 3 分類のいずれかである」と断定 → **⚠ 4 件で反証**
- round 2 前: 原因 (b) の是正として走査コマンドを記録 → **そのコマンド自体が第 3 の綴りを拾えず ⚠ 1 件で反証**

最終検証タスクの証拠文書が未達成状態を GREEN と記述していたことになるため、断定を削除し、
**なぜ漏れたか**を次に同じ掃除をする人のために記録する。これが本節の主題である。
**この経過そのもの — 網羅を主張するたびに別種の綴りが残っていたこと — が、
この種の横断掃除の難しさを示す一次資料である。**

漏れの原因は 1 つではなく、**独立した 3 つの欠陥**だった。(a) は突合の、(b)(c) は走査の欠陥である。

#### 原因 (a) 突合の欠陥 — ファイル単位で切り捨てた（⚠ 3 件）

`tests/notifications_module_it.rs` と `tests/notifications_service_it.rs` の該当箇所は、
**走査自体は正しく検出していた。** 当初の走査出力（`all_refs.txt`）には最初から載っている:

```
323:tests/notifications_module_it.rs:1	src/notifications/tests.rs	[flat]
325:tests/notifications_module_it.rs:1	src/social_graph/tests.rs	[flat]
327:tests/notifications_module_it.rs:54	src/social_graph/tests.rs	[flat]
331:tests/notifications_service_it.rs:1	src/social_graph/follow_request_service/tests.rs	[flat]
```

落としたのは突合の段階である。**「この 19 ファイルは本 spec が新規作成した移設先だから、
自分の移設元を名指しするのは正当な由来の記述だ」とファイル単位で判断し、ファイルごと切り捨てた。**
実際には 1 つの `//!` ブロックの中に、

- 正当な由来の記述（`src/notifications/tests.rs` から移設してきた ← 正しい）
- **別の**移設対象モジュールへの陳腐化した参照（`src/social_graph/tests.rs` のヘルパーを真似た ← 偽）

が同居していた。移設先ファイルは自分の移設元だけを参照するとは限らない。

**この誤りを増幅したのがスクリプトの行番号の出し方である。** コメントの連なりを 1 本に平坦化して
照合したため、報告した行番号が**マッチ行ではなくコメントブロックの開始行**（`:1`）だった。
その結果、同じ `:1` に由来の記述と陳腐化参照が並んで見え、区別がつかなかった。
`:54` は独立した別のヒットだったが、同じファイル単位の切り捨てに巻き込まれた。

**教訓: 突合はマッチ単位で行い、行番号はマッチ行を出すこと。ファイル単位の除外規則を作らないこと。**

#### 原因 (b) 走査の欠陥 その 1 — 裸の `` `tests.rs` `` が針に掛からない（⚠ 2 件）

`src/timelines/endpoints.rs` は当初の走査結果に**一度も現れていない**
（`grep -c '^src/timelines/endpoints.rs' all_refs.txt` → 0）。
針が `(?:[A-Za-z_*]+/)*[A-Za-z_*]+/tests\.rs` と `(?:[a-z_]+::)+tests\b` の 2 形だったため、
**パス修飾もモジュール修飾もない裸の `` `tests.rs` `` を拾えない。** 当該箇所（:152-154）は

> `social_graph::endpoints`'s task 5.1 ... defines no `router()` function of its own either
> — its `tests.rs` builds a test-only router ...

と書かれており、**参照の主語は 3 行上の散文にしかない。** 参照トークンだけを見る正規表現では
原理的に到達できない。この形を狙って走査し直したところ `src/oauth/middleware.rs:285` の
`` `tests.rs` `` の `test_router` も同じ欠陥で漏れており（`test_router` は
`tests/oauth_middleware_it.rs:164` へ移設済み）、これが ⚠ の 5 件目だった。

#### 原因 (c) 走査の欠陥 その 2 — 閉じバッククォートを要求する針（⚠ 1 件）

**原因 (b) の是正として本節に記録した走査コマンド自体が未達だった。** レビュー round 2 が
実際に実行して反証した。当初記録していたのは次の形である:

```console
$ ... | xargs grep -n '^\s*\(//!\|///\|//\).*`tests\.rs`' | grep -v '[/:]tests\.rs`'
```

針が `` `tests\.rs` `` と**閉じバッククォート込み**なので、`` `tests.rs::scoped_probe` `` のように
**バッククォートが `tests.rs` の直後で閉じない第 3 の綴り**に届かない。
これで漏れていたのが `src/oauth/middleware.rs:130`:

> `require_scope` likewise stays a plain function callers invoke inside their own handler
> body (as `tests.rs::scoped_probe` does) rather than a third extractor type

`scoped_probe` は `tests/oauth_middleware_it.rs:155` にのみ存在し、
`src/oauth/middleware/tests.rs` に残るのは `require_scope` の純関数 2 本だけである。
**同じファイルの L141-142 が正しくそう述べており、L130 と自己矛盾していた。**

閉じを要求しない形に差し替えると、当初 19 行だった出力が **22 行**になる
（差の 3 行はいずれも `tests.rs` が行頭に折り返した参照）:

```console
$ git ls-files 'src/**/*.rs' 'src/*.rs' 'tests/*.rs' \
    | xargs grep -nE '^[[:space:]]*(//!|///|//).*(^|[^/:[:alnum:]_])tests\.rs' \
    | grep -vE '[/:]tests\.rs' | wc -l
22
```

**（本節は当初「13 箇所が出た」と記していたが、記載したコマンドの実際の出力は 19 行だった。
証拠文書の数値が再現しないという指摘を受け、上の是正済みコマンドの実測値 22 に置き換えた。）**

#### 検算の原則 — ファイルの実在ではなく「項目」の実在で検算する

3 つの欠陥に共通する根がこれである。**部分移設では参照先のパスは有効なまま、
名指しされた中身だけが移設先へ移る。** パスの実在だけを見ると全部 OK に見えてしまう。

本 spec で実際に起きたのは 3 型:

| 型 | 例 | パスの実在 | 項目の実在 |
|---|---|---|---|
| ヘルパー関数 | `notifications/service/tests.rs::create_test_actor` | ✅ 残る | ❌ 移設先へ |
| テスト内の構築 | `social_graph/endpoints/tests.rs` の `Router::new().route(...)` | ✅ 残る | ❌ 移設先へ |
| ハンドラ double | `oauth/middleware/tests.rs` の `scoped_probe` / `test_router` | ✅ 残る | ❌ 移設先へ |

**教訓（3 本立て）:**

- **(a) 突合はマッチ単位で行う。** ファイル単位の除外規則を作らない。行番号はコメントブロックの
  開始行ではなく**マッチ行**を出す。移設先ファイルは自分の移設元だけを参照するとは限らない。
- **(b) 針はパス修飾・モジュール修飾・裸の 3 形すべてを張る。** 裸の `` `tests.rs` `` は
  本番モジュール doc（`cargo doc` 描画対象）に多い。
- **(c) 針に閉じ記号を要求しない。** `` `tests.rs::item` `` のように修飾が続く綴りを落とす。
- **そして、参照先ファイルの実在ではなく、名指しされた項目の実在で検算する。**

#### 是正後の照合結果 — 4 綴りすべてで残余ゼロ

**綴り 1: `/` 修飾パス形** — 削除済み 9 ファイルへの参照を明示的な綴りで走査:

```console
$ git ls-files 'src/**/*.rs' 'src/*.rs' 'tests/*.rs' | xargs grep -nE \
   'account_service/tests\.rs|outbound/worker/tests\.rs|notifications/endpoints/tests\.rs|src/notifications/tests\.rs|search/service/tests\.rs|src/search/tests\.rs|follow_request_service/tests\.rs|src/social_graph/tests\.rs|timelines/endpoints/tests\.rs' \
   | wc -l
14
```

**14 件すべてが「由来の記述」または「不在の記述」であり、陳腐化参照はゼロ。** 内訳:

- **不在の記述 2 件** — `src/accounts/account_service.rs` / `src/social_graph/follow_request_service.rs` の
  「There is no `X/tests.rs`」（空ソース方針で削除したことを本番モジュール doc に記したもの）
- **由来の記述 12 件** — "moved here from" / "Relocated from" / "themselves moved out of" など。
  移設先ファイルが自分の出自を記しているもので、削除済みパスを指しているのは正しい

除外キーワードで由来・不在を落とすと残りが見える（陳腐化があればここに出る）:

```console
$ ... | grep -vE 'moved|Relocated|formerly|There is no' | wc -l
3
```

**残る 3 件も由来の記述**で、キーワードが前行に折り返しているために除外できなかっただけである
（`tests/federation_outbound_worker_it.rs:3` / `tests/search_resolve_it.rs:33` /
`tests/search_service_it.rs:10`。3 件とも原文を読んで確認した）。
**この「折り返しで除外キーワードを取り逃す」性質が、行単位のフィルタを信用できない理由そのものである。**

**綴り 2: `::` 修飾モジュールパス形** — 13 ヒットあるが、参照先はいずれも
`social_graph::providers` / `accounts::endpoints` / `accounts::remote_fetcher` /
`statuses::ingest_service` / `statuses::activity_builder` / `federation::outbound::delivery` /
`federation::outbound::target` / `accounts::ports` / `search::remote_resolver` /
`statuses::notification_sink` / `accounts::emoji_repository` で、
**移設対象 20 モジュールは 1 つも含まれない。ゼロ。**

**綴り 3: 裸 `tests.rs` 形** — 22 行を全件分類。内訳は
「A module with no `tests.rs` therefore means ...」の定型（7 件）、
自モジュールの `tests.rs` への自己言及（9 件）、由来の記述（2 件。
`src/timelines/endpoints.rs:117, 157` で、いずれも本タスクが訂正した箇所でもある）、
行頭に折り返した未移設モジュールへの参照（4 件、
`src/migrate/tests.rs` / `src/api/pagination/tests.rs` / `src/search/{tag,result}_serializer/tests.rs` /
`search/remote_resolver/tests.rs`）。**陳腐化ゼロ。**

**綴り 4: `tests.rs::項目` 形** — 63 ヒット・一意 48 組。各組について
参照先ファイルを解決したうえで**名指しされた項目が実際にそのファイルに存在するか**を機械的に検算した。
手順は (1) 全追跡 `.rs` から `([A-Za-z_/*]*tests\.rs)::([A-Za-z_][A-Za-z0-9_]*)` を抽出、
(2) 左辺を末尾一致で実在する `*/tests.rs` に解決、(3) 右辺を単語境界つきでそのファイル本文に照合:

```console
unique (file,item) pairs = 48
MISS = 0
```

**MISS ゼロ**（`src/oauth/middleware.rs:130` の `scoped_probe` を訂正した後の状態）。
訂正前はこの 1 組だけが MISS だった。

**綴り 1 の総ヒットは 399、綴り 4 の総ヒットは 63。** 参照の絶対数がこの規模になるのは、
このリポジトリが「隣接テストモジュールの慣行を明示的に引用する」という文書化された慣行を
持っているためで、横断掃除のコストはこの慣行の裏返しである。

#### 差分の性質（機械的確認）

```console
$ git diff -U0 -- src/ tests/ | grep -E '^[+-]' | grep -vE '^(\+\+\+|---)' | grep -vE '^[+-]\s*(//!|///|//)'
（出力なし）
```

**コード行・アサーション・テスト名は 1 行も変えていない。**

計数の汚染を避けるため、追加した doc コメントに `spawn_test_app` を**開き括弧つきで**書いていないことも確認した
（`src/test_harness/tests.rs` L2 が、doc コメント内の記述が grep 計数に混入する実例である — `inventory.md` §6.1）:

```console
$ git diff -U0 -- src/ tests/ | grep -E '^\+' | grep 'spawn_test_app('
（出力なし）
```

掃除後の §1 の計数（round 3 の是正後に再実行）:

```console
$ git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' | wc -l
18
$ git grep -c 'spawn_test_app(' -- 'src/**/tests.rs'
src/statuses/render_assembler/tests.rs:13
src/test_harness/tests.rs:5
```

**18 のまま変わらない。**

### 7.4 本タスクで直さなかった陳腐化（本 spec の移設に起因しないもの）

要件の対象は「移設が falsify した参照」である。以下は**別の spec の完了によって**陳腐化した
記述であり、本タスクは報告のみ行い修正しない（tasks.md 実装ノート 4.1 / 5.1）。

| 箇所 | 内容 | 陳腐化の原因 |
|---|---|---|
| `src/notifications/endpoints.rs:147-165` | 「Not wired into the module tree yet」 | notifications spec タスク 4.2 の配線完了 |
| `src/social_graph/endpoints.rs` | 「nothing mounts this module onto it yet」 | `src/server.rs:447` が 9 ハンドラを mount 済み |

いずれも `HANDOFF.md` へ後続 spec の候補として引き継ぐ。

## 8. フルスイート（要件 2.5 / 5.4 / 5.5）

§7 の doc コメント掃除が載った**最終状態**に対して、親コントローラが 1 回だけ実行した。
生ログは `final_suite.log`（239 KB）、スキーマ集合は `final_pre.txt` / `final_post.txt`。
以下の集計値は本タスクの実装者がその生ログから独立に再計算したものである。

### 8.1 通過本数（要件 2.5 / 2.6）

```console
$ head -n 2990 final_suite.log | grep -h '^test result:' \
    | awk '{p+=$4; f+=$6; i+=$8} END {print "passed="p" failed="f" ignored="i" (lines="NR")"}'
binaries: passed=2286 failed=0 ignored=0 (lines=108)
```

```console
$ grep -c '^     Running ' final_suite.log
108
$ grep -c 'test result: FAILED' final_suite.log
0
```

doc テストは別枠で、`#[test]` 側には `#[ignore]` が 1 件もない:

```console
$ tail -n +2991 final_suite.log | grep '^test result:'
test result: ok. 0 passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

**rc=0 / 108 バイナリ / passed 2286 / failed 0 / ignored 0。** ✅（要件 2.5）

- **passed 2286 は §3 のマスク後テスト関数総数 2286 と完全に一致する**（1589 + 697）。
  移設前のフルスイート通過数も 2286 であり（`timing.md` §6）、**移設前後で 1 本も増減していない。**
  要件 2.6 は「差分が説明可能であること」を求めるが、**差分そのものが存在しない**。
- **バイナリ側の `ignored` が 0** であることが、要件 2.2 の「移設を理由とした `#[ignore]` 付与」が
  1 件も起きていないことをスイート実行の側から裏づける。
  記録された 5 件の `ignored` はすべて doc テスト（`no_run` 相当）であって `#[test]` ではない。

### 8.2 隔離スキーマの残留（要件 5.5）

```console
$ wc -l < final_pre.txt        # 実行前に存在した隔離スキーマ
24
$ wc -l < final_post.txt       # 実行後に存在した隔離スキーマ
20
$ comm -13 <(sort final_pre.txt) <(sort final_post.txt) | wc -l   # 実行が新たに残したもの
0
$ comm -23 <(sort final_pre.txt) <(sort final_post.txt) | wc -l   # 起動時スイープが回収したもの
4
```

**本実行が新たに残した隔離スキーマは 0 件。** ✅（要件 5.5「移設後も実行終了時に隔離スキーマを残さない」）

判定は総数の増減ではなく**集合差**で行っている。総数が 24 → 20 と減っているのは、
起動時スイープ（`test_harness/sweep.rs`、閾値 2 時間）が過去の実行の置き土産 4 件を回収したためであり、
本実行の産物ではない。残る 20 件も本実行以前から存在したもので、2 時間閾値を越え次第スイープが片付ける。

**タスク 8.3 が真因（`cleanup()` を呼んでいなかった 4 ファイル）を除去した効果が、
4 / 108 バイナリではなくスイート全体で確認された**（`timing.md` §7.4 / §7.5）。

### 8.3 上限への非到達（要件 5.4）

108 バイナリすべてが rc=0 で完走し、`test result: FAILED` は 0 行。
接続数・スキーマ数の上限に起因する失敗（プール取得失敗・`too many connections`・スキーマ作成失敗）は
ログに 1 件も現れていない。✅（要件 5.4）

### 8.4 `exceptions.md` 付記 3（要件 5.5 の判定基準）の解消

付記 3 は「フルスイート 1 回につき隔離スキーマが 1 個残る」という移設前ベースラインを前提に、
要件 5.5 の判定を絶対値ゼロではなく「1 件から悪化しないこと」へ読み替えることを暫定提案し、
承認ゲートを通っていない単独判断であることを明示して**未決**としていた。

**この読み替えは不要になった。** タスク 8.3 が真因を除去し（`timing.md` §7.4）、
本節 §8.2 の実測で**新規残留 0 件**が確定したため、要件 5.5 は**元の文言のまま、絶対値 0 で充足**している。
`exceptions.md` 付記 3 は「解消済み」として経緯ごと残してある（削除していない）。

なお真因そのもの（リーパーのキューがプロセス終了時に破棄される、`src/test_harness/reaper.rs:112-113`）は
Out of Boundary のため未修正であり、再発を検出する仕組みも存在しない（`timing.md` §7.6）。
`cleanup()` を呼ばないテストが今後追加されれば同じ残留が再発する。**HANDOFF 必須。**

## 付録. 判定の要約

| 要件 | 判定 | 根拠 |
|---|---|---|
| 1.1 配置 | ✅ | §1（18 = 例外 13 + 対象外 5、他はゼロ） |
| 1.3 `_it.rs` 命名 | ✅ | §2.3（新規 19 件すべて） |
| 1.5 保存則 | ✅ | §2.1 / §2.2（独立な 2 経路がともに 190） |
| 2.2 無効化・`#[ignore]` 禁止 | ✅ | §8.1（バイナリ側 `ignored` が 0） |
| 2.5 フルスイート通過 | ✅ | §8.1（108 バイナリ・passed 2286・failed 0・rc=0） |
| 2.6 テスト関数総数 | ✅ | §3（合計 2286 不変）・§8.1（通過数 2286 が一致） |
| 3.1 公開面の保全 | ✅ | §4.1 / §4.2（本番モジュールの昇格ゼロ） |
| 3.2 テスト構成への限定 | ✅ | §4.2（8 件すべて `src/test_harness/` 配下） |
| 3.4 テスト専用資産の分離 | ✅ | §4.3（`nm` にハーネスのシンボルなし） |
| 5.4 上限超過なし・完走 | ✅ | §8.3（108 バイナリ完走、上限起因の失敗ゼロ） |
| 5.5 隔離スキーマの残留なし | ✅ | §8.2（集合差で新規残留 0 件。読み替え不要 — §8.4） |
| 6.1 steering 不変 | ✅ | §5（差分ゼロ） |
| 6.3 証拠の提示 | ✅ | 本文書そのもの |
| 6.5 規約と実態の一致 | ✅ | §5（3 項目それぞれを実測と突合） |
