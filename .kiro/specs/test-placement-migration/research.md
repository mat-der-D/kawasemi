# Research & Design Decisions — test-placement-migration

## Summary

- **Feature**: `test-placement-migration`
- **Discovery Scope**: Extension（既存コードベースの構造是正）
- **Key Findings**:
  1. **移設の単位はファイルではなくテスト関数である。** 対象 20 ファイルにはテスト関数が混在しており、`spawn_test_app` を呼ぶ検証（移設対象）と、親モジュールの非公開項目を直接検証する純粋な単体テスト（移設対象ではない）が同居している。前者だけを動かせば、後者が触れる非公開項目は可視性を緩和する必要がない。
  2. **可視性の壁は placement-audit.md が推定したより遥かに小さい。** 20 ファイルを機械変換して `tests/` へ丸ごと移した試行コンパイルで 689 件のエラーが出たが、その約 87% は「親モジュールの `use` グロブ経由で見えていた**公開型**（`Id` / `AccountRef` / `Arc` / `Value` / `StatusCode` など）」であり、移設先での明示 import だけで解消する。crate の可視性変更を要する真の阻害要因は **8 項目**に絞られる。
  3. **`federation::signatures` の E0603（private module）は阻害要因ではない。** `mod signer` / `mod negotiation` / `mod suite` / `mod http_client` は private だが、テストが使う項目はすべて `src/federation/signatures.rs` で `pub use` 再輸出済み。参照パスを再輸出側に変えるだけで解決する。
  4. **`query_log` の `#[cfg(test)]` は必要より狭い。** `test_harness` モジュール全体は既に `#[cfg(any(test, feature = "test-harness"))]` でゲートされているため、`query_log` を同じゲートに揃えても素の `cargo build` の成果物は変わらない。

## Research Log

### 試行移設によるコンパイル検証（本 spec で最も荷重の大きい調査）

- **Context**: placement-audit.md §2 は 3 ファイルの実移設から「移設は `pub` 化を強いる」と結論したが、対象は 20 ファイルある。要件 1.4 が求める内訳確定と、要件 3 の可視性方針を決めるには全ファイルの実測が要る。
- **手法**: 対象 20 ファイルを機械変換（`use crate::` → `use kawasemi::`、トップレベルの `use super::` → 親モジュールの絶対パス）して `tests/trial_*_it.rs` として生成し、`cargo check --tests --keep-going` を 1 回実行してエラーを収集した。検証後に生成物は削除済み（`git status` クリーンを確認）。
- **Findings**:

  | 分類 | 件数 | 内容 | crate の変更要否 |
  |---|---|---|---|
  | 公開型が glob 経由で消えた | 約 600 | `Id`(119) / `AccountRef`(68) / `Arc`(59) / `Value`(47) / `StatusCode`(40) / `Status`(32) / `Visibility`(24) 他 | **不要**（移設先で明示 import） |
  | 再輸出パスの選び直し（E0603） | 4 | `signatures::{suite, signer, negotiation, http_client}` | **不要**（`pub use` 済みパスを使う） |
  | 親モジュールの非公開項目 — **純粋テストのみ**が使用 | 8 項目 | `parse_visibility` / `parse_media_ids` / `resolve_search_limit` / `parse_search_type` / `resolve_search_offset` / `parse_optional_account_id` / `parse_follow_options` / `other_format` | **不要**（当該テストは移設対象外） |
  | 親モジュールの非公開項目 — **`spawn_test_app` テスト**が使用 | 8 項目 | 下表 | **要検討** |

  エラーが 1 件も出なかったのは 4 ファイル（`notifications/tests.rs` / `search/tests.rs` / `social_graph/tests.rs` / `accounts/account_service/tests.rs`）。この 4 つは `use super::` を持たず、`crate::` の公開パスだけで書かれている。

- **真の阻害要因（`spawn_test_app` を呼ぶテストが触れる非公開項目）**:

  | 項目 | 現在の可視性 | 定義位置 | 使用する移設対象テスト数 |
  |---|---|---|---|
  | `test_harness::query_log` | `#[cfg(test)] pub(crate) mod` | `src/test_harness.rs:146` | statuses 系 3 ファイル |
  | `statuses::render_assembler::RenderContext` | `pub(crate) struct` | `src/statuses/render_assembler.rs:133` | 13 |
  | `notifications::service::RequiredPolls` | private struct | `src/notifications/service.rs:457` | 2 |
  | `search::hydrator::TolerantPolls` | private struct | `src/search/hydrator.rs:452` | 1 |
  | `federation::signatures::signer::sha256_pkcs1v15_padding` | private fn | `signer.rs:197` | 2 |
  | `federation::signatures::signer::host_from_url` | private fn | `signer.rs:138` | 1 |
  | `federation::signatures::negotiation::{format_from_db, format_to_db}` | private fn | `negotiation.rs:119/108` | 1 each |
  | `federation::endpoints::webfinger::parse_acct_resource` | private fn | `webfinger.rs:177` | 1 |
  | `search::hydrator::account_ref_id` | private fn | `src/search/hydrator.rs:175` | 1 |

- **Implications**: placement-audit.md が挙げた 2 例のうち、`account_ref_id` は「テストが直接これを検証している」とされたが、その直接検証テスト（`account_ref_id_recovers_the_id_regardless_of_local_remote`, `hydrator/tests.rs:51`）は `spawn_test_app` を呼ばない純粋な単体テストであり、**移設対象ではない**。同関数を使う移設対象テストは 1 本のみで、使用箇所は `let id = account_ref_id(target);` という 1 行の `match` 相当である。阻害の深刻さは推定より一段低い。

### `query_log` のゲート幅

- **Context**: `query_log` は 3 ファイルの移設を阻害する唯一の `test_harness` 内項目。
- **Findings**: `src/test_harness.rs:140-146` のドキュメントコメントは「lib のテストを計測するためだけに存在し、出荷ライブラリに届いてはならない — `tests/*.rs` がリンクするためゲートできないこの モジュールの他の部分とは違う」と書く。しかし `src/lib.rs:49` の `pub mod test_harness;` は `#[cfg(any(test, feature = "test-harness"))]` でゲートされており（steering `structure.md`「テスト専用資産を本番成果物に入れない」）、`query_log` を同じゲートに揃えても素の `cargo build`（既定フィーチャ）からは同様に外れる。
- **Implications**: `#[cfg(test)]` → `#[cfg(any(test, feature = "test-harness"))]` かつ `pub(crate)` → `pub` への変更は、出荷成果物の公開面を 1 ミリも広げない。当該ドキュメントコメントは「出荷ライブラリに届いてはならない」という不変条件を保ったまま更新できる。

### `PollResolver` 実装体の性質

- **Context**: `RequiredPolls` / `TolerantPolls` はテストダブルかと疑ったが、そうではなかった。
- **Findings**: 両者とも本番の `PollResolver` 実装（リポジトリから直接ポールを読む実体）であり、`#[cfg(test)]` ゲートも付いていない。steering `structure.md`「集約しないものも明示する」が言う「`PollResolver` のようなポートとして注入させる」対象そのもの。テストはこれを直接構築して注入し、ふるまいを検証している。
- **Implications**: これらは「テストのためだけに存在する項目」ではなく本番コードである。可視性の緩和はテスト構成に限定する必要がある（要件 3.2）。

### 実行コストの構造

- **Context**: 要件 5 が 1.2 倍以内を求める。
- **Findings**: `tests/` 直下の各 `*.rs` は独立した統合テストバイナリとしてコンパイル・リンクされる。現在 87 本。移設で最大 20 本増える（約 23% 増）。テスト自体の実行時間は変わらない（同じ `spawn_test_app` を同じ回数呼ぶ）ため、増分はほぼリンク時間とプロセス起動オーバーヘッドに出る。
- **Implications**: 悪化した場合の是正手段は「移設先ファイルの粒度」であり（要件 5.3）、モジュールごとの新規ファイルを既存の統合テストへ統合することで binary 数を抑えられる。

## Architecture Pattern Evaluation

可視性を緩和する経路の比較。

| Option | 内容 | Strengths | Risks / Limitations |
|---|---|---|---|
| A. 素の `pub` 化 | 阻害項目を無条件に `pub` にする | 変更が単純 | **既定フィーチャの公開 API が広がる。要件 3.1 に反する**。採用不可 |
| B. ゲート付き `test_access` モジュールからの再輸出 | 各モジュールにゲート付きの `pub mod test_access` を置き、非公開項目を `pub use` で再輸出する | 出荷成果物は不変。緩和項目の一覧がコードになる | **コンパイルが通らない。実測で棄却**（下記） |
| C. ゲート付きラッパー関数 | `test_harness` 側にゲート付きの `pub fn` を置き、`pub(crate)` に緩めた項目を呼び出して**ふるまいだけ**公開する | 出荷成果物は不変。型名を公開しない | 型そのものを構築・命名する必要があるテストには使えない。`test_harness` が各モジュールの内部知識を溜め込む |
| D. 記録された例外 | 阻害されるテストは移設せず要件 4 の例外にする | crate を一切変更しない | 例外の分だけ要件 1 の達成範囲が狭まる |

**選択: Tier 0（crate 変更なし）を最優先、既にゲート内にある項目のゲート幅調整（`query_log`）を次に、残りは D（記録された例外）。** 詳細は下の Decision を参照。

### 棄却の実測: `pub use` は可視性を広げられない

`test_access` 案は設計として魅力的だったが、実際に書いてコンパイルした結果、Rust は非公開項目の再輸出を許さない。

```
error[E0365]: `TolerantPolls` is private, and cannot be re-exported
error[E0364]: `account_ref_id` is private, and cannot be re-exported
```

項目を `pub(crate)` に緩めてから再輸出しても同じく通らない。

```
error[E0365]: `TolerantPolls` is only public within the crate, and cannot be re-exported outside
error[E0364]: `account_ref_id` is only public within the crate, and cannot be re-exported outside
```

**結論: crate 外から項目を見せる方法は、その項目自身に `pub` を書くこと以外にない。** したがって「ゲート付きモジュールに集約して一覧性を得る」という筋は成立しない。ゲートによる封じ込めが効くのは、`test_harness` のように**項目を含むモジュール宣言そのものがゲートされている**場合に限られる。

## Design Decisions

### Decision: 移設の単位をテスト関数にする

- **Context**: placement-audit.md はファイル単位の移設を前提に阻害要因を評価していた。
- **Alternatives Considered**:
  1. ファイル単位 — `src/**/tests.rs` を丸ごと `tests/*_it.rs` へ移す
  2. テスト関数単位 — `spawn_test_app` を呼ぶ関数だけを動かし、残りは元の位置に留める
- **Selected Approach**: 2。要件 1.1 が禁じているのは「単体テスト位置に**実起動インスタンスを要する検証**があること」であって、単体テスト位置にファイルがあること自体ではない。純粋な単体テスト（パーサ・フォーマッタ・パディング等）は元の位置が正しい配置である。
- **Rationale**: 阻害要因 16 項目のうち 8 項目は純粋テストのみが使用しており、この単位を採ることで**それらの可視性を一切触らずに済む**。ファイル単位だと本来動かす必要のないテストのために本番コードの可視性を緩めることになる。
- **Trade-offs**: 移設後も同名の `tests.rs` が残るファイルが出るため、「どちらに何があるか」の判別コストが上がる。移設元・移設先の双方にモジュールドキュメントで対応関係を書いて相殺する。
- **Follow-up**: 移設対象テスト関数の確定（要件 1.4）は最初のタスクで行い、以降のタスクはその表を参照する。

### Decision: 可視性の緩和は 4 段の梯子で、最も低い段を選ぶ

- **Context**: 要件 3.1 は既定フィーチャの公開 API を広げないことを求め、要件 3.2 は緩和をテスト構成に限定することを求める。要件 4 は移設できないものを例外として記録することを許す。
- **Selected Approach**: 項目ごとに、成立する最も低い段を選ぶ。新しい可視性の慣用句は導入しない。

  | 段 | 手段 | crate への影響 | 適用先 |
  |---|---|---|---|
  | **Tier 0** | 移設先での明示 import / `pub use` 済みパスの使用 / 1 行の非公開ヘルパーのテストローカルな再定義 | なし | 阻害要因の大半（約 600 件の公開型 glob 消失、4 件の E0603、`account_ref_id` / `host_from_url` / `format_from_db` / `format_to_db` / `other_format` / `sha256_pkcs1v15_padding` / `parse_acct_resource`） |
  | **Tier 1** | 既にゲート内にある項目のゲート幅を揃える（`#[cfg(test)]` → `#[cfg(any(test, feature = "test-harness"))]`、`pub(crate)` → `pub`） | 既定フィーチャでは不変（実測済み） | `test_harness::query_log` のみ |
  | **Tier 2** | 移設せず要件 4 の例外として記録する | なし | 本番モジュールの `pub(crate)` / private な**型**を構築・命名する必要があるテスト |

- **Rationale**: `pub use` が可視性を広げられない以上（上記の実測）、本番モジュールにある非公開の**型**を crate 外へ見せるには、その型自身を `pub` にするしかない。`RenderContext` の場合それは `StatusRenderAssembler` ごと公開することを意味し（`struct` / `new` / `assemble_many` / `assemble_one` がすべて `pub(crate)`）、要件 3.1 が禁じる公開面の拡大そのものになる。**Tier 2 は妥協ではなく、この構造から導かれる正しい終着点である。**
- **Trade-offs**: 例外が残るため要件 1 は「例外を除いてゼロ」でしか達成されない。これはユーザーが要件フェーズで選択した受け入れ基準と一致する。
- **Follow-up**: Tier 0 の「テストローカルな再定義」を適用する際、それが検証内容を変えていないこと（要件 2.1）をタスクごとに確認する。再定義したヘルパーが本体からドリフトしうる点は、当該テストが本体の**ふるまい**を検証していない（値の構築にのみ使っている）場合に限り許容する。

### Decision: 移設先はモジュール単位の新規ファイル、粒度は計測後に調整する

- **Context**: 要件 5.2 が実行時間 1.2 倍以内を求める一方、統合テストは 1 ファイル = 1 バイナリ。
- **Alternatives Considered**:
  1. モジュールごとに新規ファイル（最大 20 本増）
  2. 既存の `tests/*_it.rs` へ最初から統合する
- **Selected Approach**: 1 を既定とし、要件 5.2 を破った場合にのみ 2 へ寄せる。
- **Rationale**: 移設元と移設先が 1:1 に対応するほうがレビューが容易で、要件 1.5 の件数の保存則も検証しやすい。既存ファイルへの混入は最初からやると差分が読めなくなる。
- **Trade-offs**: バイナリ数が 87 → 最大 107 になる。リンク時間が増える可能性がある。これは計測して判断する（要件 5.1）。

## Risks & Mitigations

- **`use super::*` のグロブ import が隠していた依存が移設先で表面化する** — 試行コンパイルで既に全量を把握済み（約 600 件、すべて明示 import で解消）。実装時は同じ試行手順を繰り返して残余を洗い出す。
- **例外を減らしたい圧力から、本番の `pub(crate)` 項目が `pub` に緩められる** — 要件 3.1 の検査（既定フィーチャのビルドで公開項目が増えていないこと）を各タスクの完了条件に置く。`pub use` による迂回が不可能であることは実測済みなので、緩和は必ず項目自身の `pub` として差分に現れる。
- **移設で 20 本のバイナリが増えリンク時間が悪化する** — 要件 5 の計測で検知し、粒度統合で是正する。
- **`query_log` の計測が移設先で機能しなくなる** — `record_queries` は「測定対象の future に per-future な subscriber を張る」実装であり、テストがどのバイナリにあるかに依存しない。ただし doc コメントが述べる interest cache のウォームアップは並行実行の状況に依存するため、移設後に該当テストを繰り返し実行して安定性を確認する。
- **移設対象テストの取りこぼし・二重計上** — 要件 1.5 の保存則（203 = 移設 + 例外）を各タスクの完了条件に置く。

## References

- `.kiro/specs/test-infrastructure/placement-audit.md` — 配置監査。§2「決着」が本 spec への繰り越しを定義。§5 に件数の再現手順
- `.kiro/specs/test-infrastructure/migration-classification.md` — §5 が `src/test_harness/tests.rs` を対象外と判断した根拠
- `.kiro/steering/structure.md`「テストレイアウト」「テスト専用資産を本番成果物に入れない」 — 本 spec が満たすべき規約と、既存のゲート機構
- `src/test_harness/query_log.rs` — モジュールドキュメントが per-future subscriber を選んだ理由と interest cache のウォームアップを説明する
