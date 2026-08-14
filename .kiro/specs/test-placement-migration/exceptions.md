# 例外一覧 — test-placement-migration

移設せずに単体テスト位置 `src/**/tests.rs` に据え置いた検証を、ファイル単位・件数つきで一箇所に列挙する（要件 4.1）。
各例外には、移設が既定フィーチャのビルドにおける公開面の拡大を強いることを、具体的な項目名を挙げて示す（要件 4.2）。
根拠がこれに該当しない検証は例外とせず移設する（要件 4.3）。実際、design.md「阻害要因の実測結果」が Tier 2 と予測した
`notifications::service::RequiredPolls` と `search::hydrator::TolerantPolls` はいずれも移設対象テストから触れられておらず、
例外として記録していない（タスク 3.2 / 4.2 の実測）。

計数の単位は `inventory.md` §1 と同じく **`spawn_test_app` の呼び出し箇所**（`git grep -o`）である（要件 6.4）。

---

## 1. 記録された例外

### 1.1 `src/statuses/render_assembler/tests.rs` — 13 箇所 / テスト関数 13 本

判定: **Tier 2**（design.md「可視性の梯子」）。移設タスクを持たない唯一のファイルであり、
本 spec 全体で唯一の記録された例外である。

#### ① 据え置いたテスト関数（13 本）

`inventory.md` §4 の同ファイル節と同一。ファイル内の全テスト関数が移設対象であり、
「元位置に残す純粋単体テスト」は 0 本。したがって **呼び出し箇所 13 = テスト関数 13 本**が一致する。

| # | テスト関数 | 定義行 | `RenderContext` 構築行 |
|---|---|---|---|
| 1 | `muted_is_false_when_the_caller_supplies_no_mute_context` | L165 | L173 |
| 2 | `muted_reflects_the_supplied_mute_context` | L194 | L203 |
| 3 | `muted_is_judged_per_status_by_its_own_author` | L224 | L238 |
| 4 | `a_missing_poll_renders_as_none_under_the_tolerant_resolver` | L270 | L278 |
| 5 | `a_missing_poll_is_an_error_under_the_strict_resolver` | L300 | L308 |
| 6 | `a_present_poll_renders_its_options` | L328 | L371 |
| 7 | `assemble_many_preserves_input_order` | L397 | L409 |
| 8 | `assemble_one_matches_a_single_element_batch` | L437 | L444 |
| 9 | `resolves_registered_shortcodes_in_the_status_content` | L494 | L502 |
| 10 | `resolves_registered_shortcodes_in_poll_option_titles` | L525 | L569 |
| 11 | `a_rich_batch_keeps_every_resolved_material_and_its_order` | L702 | L794 |
| 12 | `an_unauthenticated_batch_reports_no_interactions_but_still_mutes` | L886 | L906 |
| 13 | `author_resolution_tracks_distinct_authors_and_not_list_length` | L974 | L984（+ ヘルパー `measure` L942 が `&RenderContext<'_>` を受ける） |

**13 本すべてが個別に確認済み**であり、いずれも例外なく次の 3 つを同時に行う。

- `RenderContext { viewer, now, origin, muted, polls }` を**構造体リテラルとして構築する**（上表の右列。13 箇所、1 テストにつき 1 箇所）
- テストローカルヘルパー `assembler(&app)`（L105、戻り型 `StatusRenderAssembler`）経由で `StatusRenderAssembler::new(..)` を呼び、**組み立て器を構築する**（#1–#12 は本体から直接、#13 はヘルパー `measure`（L946）経由。計 13 箇所）
- 構築した組み立て器の `assemble_one` または `assemble_many` を呼ぶ（`assemble_one` 8 本 / `assemble_many` 4 本 / 両方 1 本 = #8）

要件 4.3 の観点で「公開 API だけで書けるため移設すべき」ものは **1 本もない**。
`RenderContext` を経由しない検証経路がこのモジュールには存在しないためである
（`RenderContext` は「1 回の組み立て呼び出しのうち Status 行そのもの以外のすべて」であり、
`assemble_many`（`src/statuses/render_assembler.rs:320`）と `assemble_one`（同 :366）の
第 3 引数 `ctx: &RenderContext<'_>` として必須になっている）。

#### ② 到達不能な項目と現在の可視性

主たる阻害要因は次の 1 項目である。

| 項目 | 現在の可視性 | 定義位置 |
|---|---|---|
| `statuses::render_assembler::RenderContext<'a>` | `pub(crate) struct` | `src/statuses/render_assembler.rs:133` |

フィールド 5 つ（`viewer` / `now` / `origin` / `muted` / `polls`、L137–L152）はいずれも `pub` だが、
型自身が `pub(crate)` であるため crate 外からは名前を書くことすらできない。

**モジュール宣言そのものは既に公開されている** — `src/statuses.rs:198` は `pub mod render_assembler;`、
`src/lib.rs:39` は `pub mod statuses;`。つまりこの型を crate 内部に留めているのは**項目自身に書かれた
`pub(crate)` だけ**であり、封じ込めの手段がモジュール単位ではなく項目単位である。これが後述の ④ で
Tier 1 が使えない理由に直結する。

#### ③ 移設した場合に `pub` 化を要する項目の連鎖

13 本を `tests/*_it.rs` へ移設するには、以下 **7 項目**をすべて `pub(crate)` から `pub` へ昇格させる必要がある。
すべて `src/statuses/render_assembler.rs` の本番モジュール内にある。

| # | 項目 | 現在 | 定義行 | 昇格が要る理由 |
|---|---|---|---|---|
| 1 | `RenderContext<'a>` | `pub(crate) struct` | L133 | 13 本すべてが構造体リテラルで構築し、型名を書く |
| 2 | `StatusRenderAssembler` | `pub(crate) struct` | L281 | ヘルパー `assembler()` の戻り型として名指しされる（tests.rs:105） |
| 3 | `StatusRenderAssembler::new` | `pub(crate) fn` | L290 | 同ヘルパーが組み立て器を構築する唯一の手段（tests.rs:106） |
| 4 | `StatusRenderAssembler::assemble_many` | `pub(crate) async fn` | L320 | 一括組み立ての検証（#7, #8, #11, #12, #13） |
| 5 | `StatusRenderAssembler::assemble_one` | `pub(crate) async fn` | L366 | 単体組み立ての検証（#1–#6, #8, #9, #10） |
| 6 | `PollResolver` | `pub(crate) trait` | L123 | テストがローカル型 `TolerantPolls`（tests.rs:122）/ `StrictPolls`（tests.rs:142）にこのトレイトを実装する（`impl` は tests.rs:124 / :144）。`RenderContext::polls` が `&'a dyn PollResolver`（L152）であるため、実装なしには文脈を構築できない |
| 7 | `PollResolution<'a>` | `pub(crate) type` | L111 | 上記 2 つの `resolve_many` の戻り型として署名に現れる（tests.rs:125, 145） |

**タスク文が挙げた 5 項目（#1–#5）に加えて、`PollResolver` と `PollResolution` の 2 項目が実測で判明した。**
テストは投票解決器を注入する側なので、注入口の型だけでなく**注入するポートのトレイトとその関連型**まで
公開面に載る。連鎖は当初の想定より 2 段長い。

**7 項目はいずれも死んだコードではなく、本番から実際に呼ばれている。**
とりわけ `assemble_one`（#5）は `src/statuses/endpoints.rs:525` が呼んでおり、
同ファイルの doc（`src/statuses/endpoints.rs:512`）はこれを「the crate's last `assemble_one` call」と記している。
つまり昇格は「使われていない項目にたまたま `pub` を書く」たぐいの無害な操作ではなく、
既定フィーチャで常にビルドされ本番経路が現に使っている項目を、そのまま公開 API 契約へ載せる操作である。

なお、これら 7 項目の署名に現れる協力者型 —
`AccountService`（`src/accounts/account_service.rs:427`）・`LocalFsStore`（`src/media/local_fs.rs:37`）・
`ReqwestFederationHttpClient`（`src/federation/signatures/http_client.rs:163`）・
`Status` / `Poll`（`src/statuses/model.rs:89, 151`）・`PollTally`（`src/statuses/poll_repository.rs:415`）・
`ForwardedOrigin`（`src/api/pagination.rs:286`）— はいずれも既に `pub` であるため、
E0446（公開インターフェイス内の非公開型）による**二次的な昇格の波及はない**。
連鎖はちょうどこの 7 項目で閉じる。逆に言えば、7 項目を昇格させれば移設は成立してしまう。
成立しないから残すのではなく、**成立するが要件 3.1 が禁じているから残す**。

トレイト実装側にコヒーレンスの問題はない。`impl PollResolver for TolerantPolls` は
「外来トレイト × ローカル型」であり、タスク 5.1 / 6.1 で観測された孤児則（E0117）には当たらない。
つまりこの例外の理由は**純粋に可視性のみ**である。

#### ④ 梯子の下段がいずれも成立しないこと

- **Tier 0（明示 import / `pub use` 済みパス / テストローカル再定義）が成立しない。**
  テストは `RenderContext` を**名指しして構築する**（型名を書く）ため、import 経路の付け替えでは解けない。
  再輸出による迂回も不可能で、Rust は `pub use` による可視性の拡大を許さない
  （E0364 / E0365。`research.md` の棄却記録および `HANDOFF.md` §3「棄却した案」で決着済み。ここでは再検証しない）。
  テストローカル再定義も使えない: Tier 0 の再定義が有効なのは「非公開 fn を**期待値の構築**に使っている」場合であり
  （design.md「阻害要因の実測結果」末尾）、ここで必要なのは検証対象そのものである本番の型・組み立て器であって、
  同名の別物を定義したら検証内容が変わる。要件 2.1（対象・入力・アサーションの同一保持）に反する。

- **Tier 1（既にゲート内にある項目のゲート幅調整）が成立しない。**
  Tier 1 が `test_harness::query_log` に効いたのは、`src/test_harness.rs:164-165`（およびその親を
  ゲートする `src/lib.rs:48-49`）において**モジュール宣言そのものが
  `#[cfg(any(test, feature = "test-harness"))]` でゲートされており**、配下の項目をどれだけ公開しても
  既定フィーチャのビルドには 1 項目も出ないからである（タスク 1.3 で実測、緩和項目 7 件 + モジュール宣言）。
  `render_assembler` にはこのゲートが存在しない。②のとおり `src/statuses.rs:198` は無条件の `pub mod` であり、
  対象の 7 項目は既定フィーチャで常にビルドされる**本番モジュールの項目**である。
  ゲートの無いところでゲート幅は調整できない。7 項目に `pub` を書けば、それはそのまま既定フィーチャの公開 API になる。

- したがって成立する最も低い段が **Tier 2** である。

#### ⑤ 例外として残す判断と、その依拠するもの

steering `structure.md`「組み立て（assembly）コードを spec 境界ごとに複製しない」節は、この経路をこう定めている
（`.kiro/steering/structure.md:36`、原文）。

> - **Status 表現**：`statuses/render_assembler.rs` の `StatusRenderAssembler` が唯一の組み立て経路。投稿エンドポイント・アカウント投稿一覧・通知・タイムライン・検索の 5 経路すべてがここを通る。呼び出し側ごとの差（ミュート文脈の有無など）は `RenderContext` のフィールドとして明示する。

③の 7 項目は、この「唯一の組み立て経路」と「呼び出し側ごとの差を明示する `RenderContext`」そのものである。
5 経路が共有する crate 内部の要を、**テストの配置を直すためだけに**公開 API 契約へ昇格させることになり、
これは要件 3.1（既定フィーチャのビルド成果物の公開 API に、本 spec の移設のためだけに公開された項目を含めない）が
正面から禁じている。design.md「Boundary Commitments / Out of Boundary」も
`RenderContext` / `StatusRenderAssembler` を名指しで「`pub` へ昇格させない。これらを要する検証は例外として残す」としている。

要件 2.3 が定める出口はここに正確に適合する — 「移設先で検証が成立しない場合、検証を弱めることなく要件 4 の例外として扱う」。
13 本は 1 本も削除・無効化・`#[ignore]` 付与・アサーション削減を受けておらず（要件 2.2）、
単体テスト位置で移設前と同一の内容のまま実行され続ける。

失われるのは配置の一貫性だけであり、それは要件 4 が「規約違反」ではなく「記録された判断」として
明示的に受け入れることを認めた範囲にある。

**この例外が何に依拠しているかを正確に述べておく。** ③のとおり連鎖は 7 項目で閉じ、E0446 の波及もない。
すなわち 7 項目を `pub` にすれば移設はコンパイルが通る。この例外はコンパイラが課した不可能性ではなく、
**要件 3.1 がその昇格を禁じているという方針判断**（steering `structure.md:36`「唯一の組み立て経路」に裏打ちされたもの）
に立っている。Tier 0 / Tier 1 が使えないことは実際に不可能性の問題だが、Tier 2 を選ぶ最終的な理由は方針である。
この区別を曖昧にしないことが、後から「本当に移設できなかったのか」を再検討可能にしておく条件になる。

---

## 2. Tier 1 で緩和した項目

**これは例外ではない。** §1 と混同しないよう節を分けている。ここに挙げるのは
「移設できたが、そのために可視性を緩めた項目」であり、要件 3.5（緩和した項目とその適用範囲を
一覧できる形で残す）に対応する。緩和はタスク 1.3 の 1 回だけで、対象は
`test_harness::query_log` の 1 モジュールに閉じている。**本番モジュールの昇格は 1 件もない。**

### 2.1 緩和の内容

| | 緩和前 | 緩和後 | 位置 |
|---|---|---|---|
| モジュール宣言 | `#[cfg(test)] pub(crate) mod query_log;` | `#[cfg(any(test, feature = "test-harness"))] pub mod query_log;` | `src/test_harness.rs:164-165` |

`cfg` の条件を親（`src/lib.rs:48-49` の `#[cfg(any(test, feature = "test-harness"))] pub mod test_harness;`）に
揃えたうえで、`pub(crate)` を `pub` にしている。**適用範囲は `query_log` モジュールとその配下だけ**であり、
`test_harness` の他の子（`sweep` / `db_fixture` / `reaper`）には触れていない。
緩和の理由はモジュール自身の doc（`src/test_harness.rs:160-163`）に記録されている —
`tests/*.rs` は別クレートなので `pub` しか見えず、`pub use` による再輸出では代替できない。

### 2.2 昇格した項目（7 件）

定義はすべて `src/test_harness/query_log.rs`。**消費側は統合テスト 2 ファイル**であり、
以下では次のように略記する（要件 3.5 が求める「適用範囲」はこの 2 ファイルで閉じている）。

- **[A]** = `tests/statuses_account_provider_it.rs`（タスク 6.2 の移設先）
- **[B]** = `tests/statuses_endpoints_it.rs`（タスク 6.1 の移設先）

| # | 項目 | 現在の可視性 | 定義行 | 実使用 [A] | 実使用 [B] |
|---|---|---|---|---|---|
| 1 | `QueryKind` | `pub enum` | L121 | :580-585（`ANCILLARY_KINDS`）、:647, :711, :716, :733 | :2366-2369, :2372-2375, :2393, :2401, :2409 |
| 2 | `QueryLog` | `pub struct` | L232 | 型名の直接記述はなく、doc リンク `[`QueryLog::require_kinds`]`（:595）のみ | 直接記述なし |
| 3 | `QueryLog::per_statement` | `pub fn` | L257 | :638-639（差分時の診断出力） | :2385-2386, :2398 |
| 4 | `QueryLog::count` | `pub fn` | L279 | :631-632, :636-637, :647-648, :711, :716, :733 | :2378-2379, :2383-2384, :2393, :2401, :2409 |
| 5 | `QueryLog::count_matching` | `pub fn` | L288 | :664, :669 | **未使用** |
| 6 | `QueryLog::require_kinds` | `pub fn` | L300 | :628 | :2365 |
| 7 | `record_queries` | `pub async fn` | L418 | :615, :624, :704, :726 | :2357, :2363 |

import は **2 箇所**で、どちらも同一の行である —
`[A]:37` と `[B]:64` の `use kawasemi::test_harness::query_log::{QueryKind, record_queries};`。
`git grep -n 'query_log' -- tests/` が返すのはこの 2 行だけであり、`query_log` の適用範囲は
統合テストクレートのこの 2 ファイルに限られる。

**7 項目のうち 6 つがコード上で実際に呼ばれている**（`QueryLog` は型名としては書かれず、
メソッドの受け皿および doc リンクとしてのみ現れる）。**うち 5 つは 2 ファイル両方から呼ばれており**、
片側だけなのは `count_matching`（[A] のみ）である。消費側が 1 ファイルではなく 2 ファイルあることは、
この緩和が死んだ公開面を作っていないという論証をむしろ強める。

### 2.3 昇格しなかった項目（2 件）と、その判断

| 項目 | 可視性 | 定義行 |
|---|---|---|
| `QueryLog::statements` | `pub(crate) fn` | L238 |
| `QueryLog::per_kind` | `pub(crate) fn` | L270 |

この 2 つは当初「feature ON・非 test ビルドで `dead_code` になるから一緒に昇格すべき」という
理由で昇格候補に挙がったが、**タスク 1.3 のレビューで clippy 実測により反証された** —
どちらも `per_statement` / `count` / `require_kinds` から crate 内で到達可能であり、
`pub` にしなくても `dead_code` にはならない。したがって差し戻して `pub(crate)` のまま据え置いた。

**タスク 6.1 / 6.2 の実移設でも再昇格の必要は生じなかった。** §2.2 の [A] [B] 両ファイルを走査しても
この 2 つの呼び出しは 1 件もない（`git grep -n 'per_kind\|\.statements()' -- tests/` は 0 件を返す）。
すなわち差し戻しの判断は、clippy による理屈の反証だけでなく、
**その後の実移設 2 件でも需要が現れなかった**という実測で二重に裏づけられている。
対照的に `count_matching` は [A] からのみ呼ばれており（[B] では未使用）、
「1 ファイルでしか使わない項目でも実使用があれば昇格する」ことと
「2 ファイルとも使わない項目は昇格しない」ことの境界が、実例として揃っている。

**可視性は「そのテストが実際に呼ぶ項目」に限る** — これが本 spec で緩和に適用した規則である。

### 2.4 なぜこれが Tier 1 であり、公開面の拡大ではないのか

要件 3.1 が禁じているのは「**既定フィーチャでのビルド成果物**の公開 API に、移設のためだけに
公開された項目を含めること」である。ここで昇格した 7 項目は、モジュール宣言そのものが
`#[cfg(any(test, feature = "test-harness"))]` でゲートされた配下にある
（`src/lib.rs:48-49` が `test_harness` を、`src/test_harness.rs:164-165` が `query_log` を、二重にゲートしている）。
`cargo build` の既定フィーチャでは `test-harness` が立たず `test` も立たないため、
**モジュールごとコンパイル単位に出ない**。タスク 1.3 は素の `cargo build` が生成した rlib に対して
`nm` / `strings` を実行し、7 項目のシンボルも文字列も現れないことを確認している。
配下の項目をどれだけ `pub` にしても既定フィーチャの成果物は 1 バイトも変わらない。

これが §1 ④で「`render_assembler` には Tier 1 が使えない」と述べた理由の裏返しである。
**Tier 1 はゲートが既に存在するところでしか使えない。** `query_log` にはあり、`render_assembler` にはない。

要件 3.4（テスト専用資産が既定フィーチャのビルドから外れている状態の維持）も、
ゲートの条件を緩めるのではなく親と揃えただけなので影響を受けていない。

## 3. 対象外の 5 箇所（`src/test_harness/tests.rs`）

**これは例外ではなく、本 spec の判断でもない。** 先行 spec `test-infrastructure` の既決事項を
引き継いでいるだけである（`../test-infrastructure/migration-classification.md` §5、
および `../test-infrastructure/placement-audit.md` §2「決着（2026-08-14・ユーザー判断）」）。
本 spec の requirements「Boundary Context / Out of scope」第 1 項および要件 4.5 が、
これを例外と区別して記載することを定めている。

### 3.1 5 箇所の内訳

`git grep -n 'spawn_test_app(' -- 'src/test_harness/tests.rs'` の 5 行は次のとおり。

| 行 | 種別 | 属する要素 |
|---|---|---|
| L2 | **`//!` ドキュメントコメント内の記述** | モジュール doc（L1-L5 の「proving `spawn_test_app()` boots a real, connectable instance ...」） |
| L89 | コード上の呼び出し | `spawn_test_app_boots_with_applied_migrations_and_deterministic_runtime`（fn 定義 L82） |
| L142 | コード上の呼び出し | `spawn_test_app_isolates_database_state_between_instances`（fn 定義 L136） |
| L143 | コード上の呼び出し | 同上（この 1 関数だけが 2 回呼ぶ） |
| L193 | コード上の呼び出し | `cleanup_releases_pool_listener_and_isolated_schema`（fn 定義 L188） |

**したがって「呼び出し箇所 5」「コード上の呼び出し 4」「テスト関数 3 本」がすべて食い違う。**
本 spec の 21 ファイル中、grep 値とコード上の値が食い違うのはこのファイルだけである
（`inventory.md` §2 の表で「呼び出し箇所（grep）」列と「うちコード上」列が一致しない唯一の行）。

### 3.2 それでも「5」が正しい数である理由

要件 1.1 の完了判定は `git grep` の出現数を単位とする（要件 6.4、`inventory.md` §1）。
`placement-audit.md` が挙げた 208 / 21 も同じ単位で数えられている。
**単位を揃えないと保存則が合わない**ため、要件 1.1・1.5 の照合には一貫して **grep 基準の 5** を使う。
対象 203 の側はどちらの単位でも変わらない（208 − 5 = 207 − 4 = 203）。

なお、モジュール doc に `spawn_test_app(` と書かれた文字列が計数へ混入するというこの現象は、
タスク 2.2 で「テスト関数が 0 本になった `tests.rs` を削除し、移設先へのポインタを本番モジュールの
`//!` doc に畳み込む」方針を決めた際の根拠のひとつになっている
（スタブ doc に `spawn_test_app(` と書いた瞬間に計数照合が黙って狂う。L2 がその実例）。

### 3.3 移設しない理由

3 本とも `spawn_test_app` **自体のふるまい**を検証するハーネスの自己検証テストである
（実起動・マイグレーション適用済み・決定的 `RuntimeContext`・隔離性・`cleanup()` の解放）。
検証対象がハーネス境界そのものなので、統合テスト位置へ移しても検証内容は変わらず、
「単体テスト位置に置くべきでない」という規約の趣旨に該当しない。
加えて統合側の対応物は `tests/test_harness_lifecycle_it.rs` に既に存在する
（同ファイル L8 以降の「Why this lives in its own `tests/*.rs` binary, not `src/test_harness/tests.rs`」節が
両者の役割分担を記している）。

**この 5 箇所は要件 4.2 の判定を受けていない。** 公開面の拡大を強いるかどうかを検討した結果ではなく、
そもそも本 spec の対象 203 に含まれていないためである。§1 の例外台帳とは性質が異なる。

## 4. design.md 阻害要因表の訂正と追加

design.md「阻害要因の実測結果」の表（design.md:105-116。見出しは :101、前文は :103）は、
実移設の前に予測として書かれたものである。
波 2〜7 の実測でこの表には **3 系統の所見**が出た。表は「可視性の一覧」としては概ね正しいが、
「移設の阻害要因の一覧」としては過大かつ不足している。以下は後続 spec が同じ表を読む際の訂正である。

### 4.1 過大計上 — 移設対象テストが触れていない項目（6 件）

表の「対象テスト数」列は、**そのモジュールのテスト全体**が非公開項目を使っている件数を数えており、
**移設対象（`spawn_test_app` を呼ぶもの）に限った件数ではない**。実際には、以下 6 項目を使っているのは
いずれも元位置に残る純粋単体テストだけで、移設対象からは 1 本も触れられていない。

| # | 表の項目 | 表の段 | 実測 | 現に使っているのは |
|---|---|---|---|---|
| 1 | `signatures::signer::host_from_url` | Tier 0 / 1 本 | 移設対象からの使用 0 | `src/federation/signatures/signer/tests.rs:26`（`host_from_url_extracts_the_authority_without_scheme_or_path`。呼び出しは :28, :31, :33）。定義は `src/federation/signatures/signer.rs:218`、本番使用は同 :323 |
| 2 | `signatures::negotiation::format_to_db` | Tier 0 / 1 本 | 同 0 | `src/federation/signatures/negotiation/tests.rs:19-20`。定義 `src/federation/signatures/negotiation.rs:108`、本番使用 同 :202 |
| 3 | `signatures::negotiation::format_from_db` | Tier 0 / 1 本 | 同 0 | `src/federation/signatures/negotiation/tests.rs:22-26`。定義 同 :119、本番使用 同 :185 |
| 4 | `federation::endpoints::webfinger::parse_acct_resource` | Tier 0 / 1 本 | 同 0 | `src/federation/endpoints/webfinger/tests.rs:16, 24, 29, 34, 39` の純粋単体テスト 5 本。定義 `src/federation/endpoints/webfinger.rs:177`、本番使用 同 :128 |
| 5 | `search::hydrator::TolerantPolls` | **Tier 2** / 1 本 | 同 0 | `src/search/hydrator/tests.rs:154, 185, 213` の 3 本。いずれも `spawn_test_db`（同 :26 で import、:153, :184, :212 で使用）。定義 `src/search/hydrator.rs:452`、本番使用 同 :402 |
| 6 | `notifications::service::RequiredPolls` | **Tier 2** / 2 本 | 同 0、かつ**本数も誤り（実際は 3 本）** | `src/notifications/service/tests.rs:111, 138, 166` の 3 本。いずれも `spawn_test_db`（同 :22 で import、:110, :137, :165 で使用）。定義 `src/notifications/service.rs:466`、本番使用 同 :356 |

**#5 と #6 は「予測された Tier 2 例外が存在しなかった」ケースである。** design.md:109-110 は
`RequiredPolls`（:109）と `TolerantPolls`（:110）を移設できない例外として見込んでいたが、実際にはどちらも
最初から移設対象ではない（軽量な `spawn_test_db` フィクスチャを使う純粋寄りのテストであり、
`inventory.md` §4 / §6.3 が実装前から独立に「元位置に残す」と分類していた）。
要件 4.3 は「例外の根拠が要件 4.2 に該当しないなら例外とせず移設する」と定めており、これらは
そもそも 203 に含まれていないので**例外として数えてはならない**。とりわけ design.md:109 は
「`RequiredPolls` / 対象テスト 2 本 / Tier 2」と**二重に誤っている**（本数が 3、かつ移設候補ですらない）。

この結果、**本 spec 全体で成立した Tier 2 例外は §1 の 1 件のみ**になった。
表が Tier 2 と予測した 3 行のうち、実際に成立したのは `RenderContext` の行（design.md:108）だけであり、
残る 2 行（:109, :110）は蒸発した。**予測の的中率は Tier 2 に限れば 3 分の 1 である。**

### 4.2 欠落 — 表がまったく模していない阻害要因（2 系統）

design.md の表も「可視性の梯子」の判定フロー（design.md:84-99 の mermaid。判定は :86 `Q0` から :98 まで）も、
阻害要因を**可視性だけ**でモデル化している。
実測では可視性に還元できない阻害要因が 2 系統出た。

#### (a) コヒーレンス — 孤児則（E0117）

単体テスト位置では `impl DeliverySink for Arc<RecordingSink>` と書けるが、統合テストクレートからは書けない。
`DeliverySink` も `Arc` も当該クレートから見て外来であり、`Arc` は `#[fundamental]` ではないためである。
**これは可視性の問題ではないので、梯子のどの問いにも引っかからない。** 表に載っていないのは当然だが、
実測では 3 タスクで再発した。

| タスク | 移設元の `impl` | 移設先の解法 |
|---|---|---|
| 5.1 | `impl DeliverySink for Arc<NoopSink>`（移設前の `src/social_graph/endpoints/tests.rs:160`。`git show e80ee62^:…` で確認） | `tests/social_graph_endpoints_it.rs:182` `struct SharedNoopSink(Arc<NoopSink>);` / `impl DeliverySink for SharedNoopSink`（同 :184） |
| 5.2 | `impl DeliverySink for Arc<RecordingSink>`（移設前の `src/social_graph/follow_request_service/tests.rs:175`。`git show d773357^:…` で確認） | `tests/social_graph_follow_request_service_it.rs:193` `struct SharedRecordingSink(Arc<RecordingSink>);` / `impl`（同 :205） |
| 6.1 | `src/statuses/status_service/tests.rs:146` ほか | `tests/statuses_endpoints_it.rs:174` `struct SharedRecordingSink(Arc<RecordingSink>);` / `impl`（同 :176） |

解法は**テストローカルな newtype で `Arc` を包み、`Clone` を `Arc` の clone に委譲する** — すなわち **Tier 0**
（本番クレートに一切変更を加えない）。表へは「コヒーレンス（孤児則 E0117）→ テストローカル newtype = Tier 0」の行として
追記すべきである。

**この解法には静かに壊れる経路がある。** newtype の `Clone` が `Arc` の clone でなく内側の実体の複製に
なっていると、共有が切れて「配送回数」のアサーションが黙って弱まる。タスク 5.2 は
「壊れていたら落ちるアサーションが実在するか」で共有を確認する方法を採った（`build_service` が
クローンを `DeliveryService` へ渡しクローン元をテストへ返すため、分岐していれば `calls().len() == 1` が
0 を観測して必ず落ち、直後の `[0]` 索引も panic する）。
なお同型の `impl DeliverySink for Arc<RecordingSink>` は移設対象外のファイルにまだ 7 箇所残っており
（`src/social_graph/block_service/tests.rs:134`、`src/social_graph/follow_service/tests.rs:155`、
`src/social_graph/inbound/tests.rs:207`、`src/statuses/activity_builder/tests.rs:237`、
`src/statuses/interaction_service/tests.rs:127`、`src/statuses/poll_service/tests.rs:129`、
`src/statuses/status_service/tests.rs:146`）、これらを将来移設するなら同じ論点が再発する。

#### (b) 表に無い非公開 const

`JRD_MEDIA_TYPE`（`src/federation/endpoints/webfinger.rs:77` の非公開 `const`、本番使用は同 :192）は
表に 1 行も無いが、タスク 2.2 の移設で実際に阻害要因になった。
解法はテストローカルな再定義（`tests/federation_webfinger_endpoint_it.rs:39`、使用は同 :77、
再定義であることの明示は同 :34 の doc）で **Tier 0**。値が本番と一致することを確認済み。

表は網羅ではなくサンプルとして読むべきである、というのがこの 2 系統の含意である。

### 4.3 原因分類の誤り — E0599 の帰属先

design.md:328 の行「非公開項目への到達不能 | E0364 / E0365 / E0603 / E0599」は、E0599 を
**可視性の問題として分類している**。タスク 6.2 で観測された E0599 の実際の原因は可視性ではなく、
**トレイトメソッドの呼び出しにトレイト自身がスコープに要ること**だった。

- 対象メソッド: `AccountStatusesProvider::list_statuses`
- トレイトの可視性: **既に `pub`**（`src/accounts/ports.rs:116`）
- 移設前に見えていなかった理由: `use super::*` が親モジュールの `use` ごとトレイトを引き込んでいた
- 解法: 移設先で明示 import（`tests/statuses_account_provider_it.rs:22` の
  `use kawasemi::accounts::ports::{AccountStatusesProvider, StatusesQuery};`）— **Tier 0、crate 無変更**

**誤っているのはエラーコードの一覧ではなく、原因の分類（taxonomy）である。**
E0599 は「非公開項目への到達不能」の欄ではなく、その 1 つ上の行「グロブ import が隠していた依存の未解決」
（design.md:326、E0425 / E0433 / E0412）と同じ性質のものとして扱うのが正しい。
この区別を誤ると、Tier 0 で済む案件を Tier 2 の例外候補として扱ってしまう。

### 4.4 逆に、実測で裏付けが取れた記述

表を全面的に疑うべきではない。以下は予測が正しかったものである。

- **design.md:107 の Tier 1 行（`test_harness::query_log`）は正確だった。** §2.2 のとおり、
  昇格した 7 項目のうち 6 つが統合テストで実使用されている。予測どおりの段（Tier 1）で解け、
  予測どおりの範囲（`query_log` のみ）に収まった。
- **`research.md` のゼロエラー予測は結果として当たった。** 試行コンパイルでエラー 0 件と報告された
  `src/search/tests.rs` と `src/accounts/account_service/tests.rs` は、実移設でも Tier 0 で完了し
  crate 側の変更を要していない。
- ただし **`research.md:28` の前提記述には誤りがある。** 同行は「エラーが 1 件も出なかった 4 ファイルは
  `use super::` を持たず、`crate::` の公開パスだけで書かれている」とするが、
  移設前の `src/accounts/account_service/tests.rs`（現在は削除済み。`git show 3b3aa9d^:src/accounts/account_service/tests.rs` で確認）
  は **L39-L42**（4 行にまたがる `use super::{ … };`）に
  `AccountService, MAX_PROFILE_FIELDS, MediaUploadInput, ProfileFieldInput, StatusesQueryInput, UpdateCredentialsInput` を持つ。
  **結論（Tier 0・crate 無変更）は無傷である** — 6 項目はいずれも既に `pub` だからである
  （`src/accounts/account_service.rs` の `StatusesQueryInput`:245 / `ProfileFieldInput`:261 / `MediaUploadInput`:275 /
  `UpdateCredentialsInput`:309 / `MAX_PROFILE_FIELDS`:331 / `AccountService`:427）。
  誤っているのは前提であって結論ではない、という区別のために記録する。

## 付記. 判断記録 — 例外台帳に載せないが記録するもの

要件 4.2 は例外を「移設が既定フィーチャのビルドにおける公開面の拡大を強いる」場合に限定し、
要件 4.3 はそれに該当しないものを例外とせず移設せよと定めている。
したがって以下は **§1 の例外台帳に載せてはならない**（載せると台帳の意味が壊れる）。
しかし判断としては残す必要があるため、ここに記録する。

### 付記 1. 実質的な冗長 2 件（後続 spec の候補）

波全体で要件 2.4 の近接ペアを多数検証した結果、**真に包含関係にあるペアは 2 組**だった。

| # | 移設した側 | 包含する既存側 | 判定 |
|---|---|---|---|
| 1 | `tests/search_module_it.rs:212` `search_end_to_end_through_the_real_router_with_the_default_pg_backend`（タスク 3.3） | `tests/search_type_scope_it.rs:327` `type_statuses_narrows_to_statuses_only` | 既存側が**厳密に強い**。競合する account / hashtag を実在させたうえで空配列を主張している |
| 2 | `tests/timelines_endpoints_handler_it.rs:419` `tag_timeline_any_filter_requires_at_least_one_additional_tag`（タスク 7.2） | `tests/timelines_endpoints_it.rs:965` `tag_timeline_any_all_none_combination_via_real_router` の `any` 節 | クエリ文字列・フィクスチャ・アサーションのすべてで既存側が上位集合 |

**どちらも現状維持とした。** 根拠は 2 つある。

- 要件 2.2 が「移設を理由としたテスト関数の削除」を明示的に禁じている。
- 要件 2.4 の規範動詞は「重複する検証を**新たに作らない**」であり、移設は既存検証の再配置であって
  新規作成ではない。したがって移設は 2.4 に違反しない（付記 2 参照）。

整理・統合は本 spec の Boundary Context「Out of scope / 検証内容そのものの追加・拡張・改善」に当たるため、
**後続 spec の候補**として `HANDOFF.md` に引き継ぐ。

### 付記 2. 要件 2.4 の解釈（タスク 3.3 で確定）

要件 2.4 を「移設後の `tests/` に重複が存在してはならない」と読むと、**要件集合が充足不能になる**。

- 要件 1.1 は移設を要求する
- 要件 2.1 は対象・入力・アサーションの同一保持を要求する（統合による書き換えができない）
- 要件 2.2 は削除・無効化・アサーション削減を禁じる

すなわち、移設した結果として既存と重なるものが出た場合、それを解消する手段が要件集合に存在しない。
spec が用意した出口は要件 2.3 →要件 4 の例外（弱めずに元位置へ残す）だけであり、
重複を理由とした統合・削除を認める条項はどこにもない。
したがって 2.4 は規範動詞どおり「**新たに作らない**」と読むのが唯一の整合的な解釈である。
本 spec はこの解釈のもとで、新規の重複検証を 1 件も作っていない。

### 付記 3. 要件 5.5 の判定基準（**未決 — 人間承認待ち**）

要件 5.5 は「移設後も実行終了時に隔離スキーマを残さない」と定めるが、
タスク 1.2 の移設前ベースライン（`timing.md` §3）は **フルスイート 1 回につき 1 個残る**である。
これは移設前から存在する既存挙動であり、本 spec が作り出したものではない。しかも 2 段階の構造的理由がある。

1. **なぜ残るか** — `src/test_harness/reaper.rs` のリーパーはプロセス終了時のベストエフォートで、
   終了時点でキューに残っていた回収要求は失われ、次回のスイープに委ねられる。
   この主張を直接支えているのは `HarnessReaper::global` の doc **`src/test_harness/reaper.rs:112-113`**
   （"Requests still queued when the process exits are simply lost, and the startup sweep reclaims their
   schemas on a later run"）である。**設計上そう書かれている**のであって、観測された不具合ではない。
   なお同ファイルの L1-L21（モジュール doc）と L227-L230 はそれぞれ
   「なぜ回収を呼び出し側のランタイムから追い出す必要があるか（`Drop` と死にゆくランタイム）」と
   「スキーマ削除自体が `TestApp::cleanup` と同じベストエフォートの経路を再利用していること」を述べており、
   背景としては関係するが上記の「取りこぼしが失われる」という主張の出典ではない。
2. **なぜ後続の実行で回収されないか** — `src/test_harness/sweep.rs:99` の `RECLAIM_THRESHOLD` が **2 時間**で、
   スイープは埋め込み時刻が閾値以上過去にあるスキーマしか落とさない。並行実行中の別プロセスのスキーマを
   誤って落とさないことを優先した設計である。

`timing.md` §3 は、要件 5.5 の判定を絶対値ゼロではなく **「1 件から悪化しないこと」（非regression）** と
読み替えることを**暫定提案**している。要件 5 の表題が「非悪化」であり 5.4 / 5.5 とも「移設後**も**」と
書かれていることとは整合するが、**これは承認済み要件の判定基準を計測文書が単独で緩めるものであり、
承認ゲートを通っていない。**

**したがってこれは決定事項ではなく未解決の問いである。** タスク 8.4 でこの基準を適用する前に、
要件 5.5 の改訂か、要件 4 と同様の「記録された判断」としての人間承認を得ること。
なお、移設によって残留が 1 → 20 のように増えたのであれば、その分は移設に起因する退行として扱う
（テストバイナリが 87 → 106 に増えるため、リーパーが取りこぼす窓も増える）。

## 5. 計数の再現コマンドと実行結果

要件 4.4（記録時点で確認された件数と、その件数を再現できる手順）・6.2（数える手順のコマンド化）・
6.3（実行結果を証拠として示す）に対応する。

**実行日**: 2026-08-14 / **実行リビジョン**: `bfda759`（`src/` `tests/` は作業ツリー無変更。
未コミットの差分は `.kiro/specs/` 配下のみ）/ **移設前の基準リビジョン**: `5ad15a5`

**計数の単位は「呼び出し箇所」**（`spawn_test_app(` というトークンの出現数）であり、
テスト関数の本数ではない（要件 6.4）。`placement-audit.md`・`requirements.md`・`design.md`・
`HANDOFF.md`・`inventory.md` が挙げる 208 / 203 / 190 / 13 / 5 はすべてこの単位である。
`git grep -o` は 1 行に複数の出現があっても正しく数えるため、行単位の `-c` ではなく `-o | wc -l` を基準とする
（`src/test_harness/tests.rs` L142-L143 のように 1 関数で 2 回呼ぶ例があるため、この区別は実際に効く）。

### 5.1 単体テスト位置に残る総数（要件 1.1）

```console
$ git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' | wc -l
18
```

### 5.2 その内訳（要件 4.1 / 4.5）

```console
$ git grep -c 'spawn_test_app(' -- 'src/**/tests.rs'
src/statuses/render_assembler/tests.rs:13
src/test_harness/tests.rs:5
```

- `src/statuses/render_assembler/tests.rs` の **13** = §1 の記録された例外（Tier 2）
- `src/test_harness/tests.rs` の **5** = §3 の対象外（先行 spec の既決事項）
- **他のファイルはゼロ。** 移設前の 21 ファイルのうち 19 ファイルが完全に消えている
  （残る 12 ファイルに `spawn_test_app` の言及があるが、すべて doc コメント内の括弧なし表記で計数に混入しない）

### 5.3 対象外 5 箇所の位置（§3 の根拠）

```console
$ git grep -n 'spawn_test_app(' -- 'src/test_harness/tests.rs'
src/test_harness/tests.rs:2://! (task 8.1, Requirements 8.1-8.5): proving `spawn_test_app()` boots a real,
src/test_harness/tests.rs:89:    let app = spawn_test_app().await;
src/test_harness/tests.rs:142:    let app_a = spawn_test_app().await;
src/test_harness/tests.rs:143:    let app_b = spawn_test_app().await;
src/test_harness/tests.rs:193:    let app = spawn_test_app().await;
```

L2 が `//!` doc、残り 4 がコード上の呼び出し（§3.1）。

### 5.4 統合テストファイル数（要件 1.3）

```console
$ git ls-files 'tests/*_it.rs' | wc -l
106
```

```console
$ git ls-tree -r --name-only 5ad15a5 tests/ | grep '_it\.rs$' | wc -l
87
```

87 + 19（新規作成）= 106。新規 19 件は `design.md`「File Structure Plan」の移設先 19 件に対応する
（`tests/timelines_endpoints_it.rs` のみ既存ファイルと衝突したため `tests/timelines_endpoints_handler_it.rs` へ改名。
既存ファイルは無変更）。

### 5.5 保存則の検証（要件 1.5）

移設前の基準値:

```console
$ git grep -o 'spawn_test_app(' 5ad15a5 -- 'src/**/tests.rs' | wc -l
208
```

```console
$ git grep -c 'spawn_test_app(' 5ad15a5 -- 'src/**/tests.rs' | wc -l
21
```

これより:

| 式 | 値 | 出典 |
|---|---|---|
| 移設前の総数 | 208 箇所 / 21 ファイル | 5.5 の上記 2 コマンド |
| − 対象外（`src/test_harness/tests.rs`） | − 5 箇所 / − 1 ファイル | 5.2 / 5.3 |
| = **本 spec の対象** | **203 箇所 / 20 ファイル** | 要件 1.4 |
| − 記録された例外 | − 13 箇所 | 5.2、§1 |
| = **移設した件数** | **190 箇所** | |

**要件 1.5 の保存則: 203 = 190（移設）+ 13（例外）** ✅

**要件 1.1 の残存内訳: 18 = 13（記録された例外）+ 5（対象外）** ✅
（5.1 の実測値 18 と一致。要件 1.1 の「例外と対象外を除いて 0」を満たす）

### 5.6 移設先側からの独立検証

上記は移設**元**からの引き算である。移設**先**からも独立に数えると同じ 190 が出る。

```console
$ git grep -o 'spawn_test_app(' -- 'tests/*_it.rs' | wc -l
699
```

```console
$ git grep -o 'spawn_test_app(' 5ad15a5 -- 'tests/*_it.rs' | wc -l
509
```

**699 − 509 = 190。** 移設元から消えた 190 箇所が、そのまま移設先に現れている。
2 つの独立した経路（`src/` 側の減少 208 − 18 = 190、`tests/` 側の増加 699 − 509 = 190）が
一致することで、移設中の取りこぼしも重複追加も起きていないことが示せる。

### 5.7 注意点

- **`git grep -c` は行単位**のため、1 行に 2 呼び出しがあると過少計数する。上記の内訳表示に使っているが、
  総数の判定には必ず `-o | wc -l` を使うこと。現状 21 ファイルすべてで両者は一致している。
- **`git grep` はコメント・文字列を区別しない。** §3.1 の L2 がその実例である。
  grep 基準の数と「コード上の呼び出し」の数が食い違うのは 21 ファイル中このファイルだけだが、
  今後 doc コメントに `spawn_test_app(` と書けば黙って混入する（`inventory.md` §2 / §6.1）。
- 上記コマンドはすべて **`git grep`**（ワークツリーではなくインデックスを走査）である。
  未追跡ファイルは数えない。再現時はワークツリーがクリーンであることを先に確認すること。
