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

_タスク 8.2 で記載する（要件 3.5）。_

## 3. 対象外の 5 箇所（`src/test_harness/tests.rs`）

_タスク 8.2 で記載する（要件 4.5）。例外とは区別して扱う。_

## 4. design.md 阻害要因表の訂正と追加

_タスク 8.2 で記載する。_

## 5. 計数の再現コマンドと実行結果

_タスク 8.2 で記載する（要件 4.4 / 6.2 / 6.3）。_
