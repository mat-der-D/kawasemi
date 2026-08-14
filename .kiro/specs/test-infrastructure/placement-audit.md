# 配置規約の監査（タスク 3.6 の成果物）

対象要件: 4.5「Where 実起動インスタンスを要する検証である場合, the Test Suite shall それを
統合テストの配置規約（steering `structure.md`「テストレイアウト」）に従って配置する」

**結論: 移動は行わない。** 4.5 は本 spec の変更が持ち込む配置についての要件であり、
Phase 1 から steering が許容してきた既存配置の一括移設を求めるものではない。以下、根拠。

---

## 1. 事実（計測値）

計測は `git grep -o 'spawn_test_app(' -- 'src/**/tests.rs'`（呼び出し箇所の数。テスト関数の
本数ではない）。

| 時点 | commit | 単体テスト位置の `spawn_test_app(` |
|---|---|---|
| 本 spec 着手前 | `b50d8fd` | **748**（71 ファイル） |
| 現在 | `b012f1f` | **208**（21 ファイル） |
| 差分 | | **−540**（増加ゼロ・新規ファイルゼロ） |

- 現在の 21 ファイルはすべて `b50d8fd` 時点にも存在し、いずれも件数が同数か減少している。
  本 spec が単体テスト位置に新たな実インスタンス起動を **1 件も追加していない**。
- `src/` 内の `spawn_test_app(` 呼び出しで `tests.rs` 以外にあるのは
  `src/test_harness.rs`（定義）と `src/federation/module.rs`（doc）のみ。
- `spawn_federation_pair(` は `src/**/tests.rs` に **0 件**。連合ペアはすべて
  `tests/federation_pair_it.rs` / `tests/statuses_federation_pair_it.rs` 側にある。

内訳（上位）: `notifications/endpoints` 24 / `social_graph/endpoints` 22 /
`statuses/endpoints` 21 / `notifications/service` 17 / `accounts/account_service` 16 /
`statuses/render_assembler` 13 / `search/service` 11 / `search/hydrator` 11 /
`search/endpoint` 10 / 以下 8 以下が 12 ファイル。

### `tests/` 側

`tests/*_it.rs` は 87 本（`b50d8fd` 時点の 85 本 + 本 spec が追加した 2 本）。うち
84 本が `spawn_test_app` / `spawn_federation_pair` /
`spawn_test_db` を使う。残り 3 本は
`bootstrap_fail_fast_it.rs` / `bootstrap_lifecycle_it.rs`（`bootstrap()` を直に起動する＝
ハーネスを経由しない実起動）と `media_store_it.rs`。

分業は明確で、重複ではない。単体テスト位置に残った 208 は
**モジュール内部のふるまい**（ルーター経由の HTTP、`AppState` 経由のモジュール参照、
`app.actor` の実鍵）を実インスタンスを足場にして検証するもの。`tests/` 側は
**モジュール横断の配線・契約・ライフサイクル**を検証する。同じ対象を二重に検証している
組は見つからなかった（網羅的な突き合わせではなく、ファイル名と対象モジュールの対応を
見た範囲での所見）。

## 2. 4.5 の読み方

### 逐語的な読み（対立仮説）を先に置く

タスク本文は「実起動インスタンスを要する検証が単体テスト側に**残っていないか**確認し、
**残っていれば**…移す」と書き、完了状態は「**すべて**…配置規約に沿って置かれている」と書く。
4.5 自体も EARS の `Where <条件>` 節であり、これは通常「移行の引き金」ではなく
**状態不変条件**として読む。主語も「the Test Suite」でスイート全体を指す。
素直に読めば、残存 208 箇所すべてが違反であり移設対象になる。**これが最も強い対立仮説である。**

以下はこれを退ける論証であり、対立仮説を回避したものではない。

### 逐語的な読みを退ける根拠

1. **design.md「Requirements Traceability」の 4.5 行の Summary** — 「**統合テストは**配置規約に
   従う」。承認済み design 自身による 4.5 の言い換えが、主語を「統合テスト」に限定している。
   スイート内のあらゆる検証を対象にしていない。
2. **要件 4.1 との整合** — 4.1 は「実インスタンスの起動を要する単体テストの件数を…**削減する**」
   と書く。逐語的読みでは 4.5 がその件数を構造的にゼロに強制するため、4.1 は完全に包含され
   「削減」という語が誤導になる。どの条項も冗長にならない読みを採る。
3. **`migration-classification.md` §1 / §4 / §5** — 残った 205–206 本（呼び出し 208 箇所）を
   明示的に「非対象（`TestApp` の**まま**）」と分類し、その分類は検証・承認済み。4.5 が移設を
   命じているなら「`TestApp` のまま」という分類自体が成立しない。3.3–3.5 も移設を一度も
   指示していない。
4. **design.md「Boundary Commitments → Out of Boundary」との実証的な衝突** — 逐語的に従うと、
   承認済み design が境界外と定めた変更が**実際に**強制される。3 ファイルを実際に `tests/` へ
   移して確かめた結果:

   | ファイル | 結果 |
   |---|---|
   | `src/notifications/endpoints/tests.rs`（24 箇所・最大） | import 5 行の追加で**コンパイルが通る**。阻害要因なし |
   | `src/statuses/render_assembler/tests.rs`（13 箇所） | 阻害。`src/test_harness.rs:147` の `pub(crate) mod query_log` が統合バイナリから見えない。加えて対象 6 項目が `pub(crate)` |
   | `src/search/hydrator/tests.rs`（11 箇所） | 阻害。`src/search/hydrator.rs:184` の `fn account_ref_id` が**完全に非公開**で、テストが直接これを検証している |

   つまり「移設は `pub` 化を強いる」は**一部のファイルについて真**であって全部ではない。
   ただし阻害されるファイルでは、テスト用アサーション対象を crate の公開 API に昇格させる
   必要が生じ、これは Out of Boundary の中心にある。要件を、自身の design が除外した変更を
   命じる形には読めない。
5. **design.md「Non-Goals」** — 本 spec の達成目標は起動コストの階層化であってテスト配置の
   再編ではない。

なお、タスク本文の「残っていれば…移す」は**検証タスク＋条件付きの是正措置**という構造であり、
その引き金は `_Requirements: 4.5_` に委ねられている。したがってタスク本文が 4.5 の意味を
確定することはできない（循環する）。また本タスクの検証は空振りではなかった — ファイル別の
単調減少チェックは反証可能であり、`reaper/tests.rs` の判定は実際に判断を要した。

したがって 4.5 は「本 spec が実インスタンス起動を要する検証を**新たに置く**とき、および
既存の検証を**動かすとき**、その行き先は `tests/*_it.rs` でなければならない」という
前向きの制約として読む。既存配置は Phase 1 以来 steering が許容してきた状態であり、
その是正は本 spec の責務ではない。

**この読みの下で完了状態が何を意味するかを明示しておく**: 完了状態「実インスタンス起動を
要する検証がすべて統合テストの配置規約に沿って置かれている」は、**本 spec が所有する配置に
ついて**充足されている。残存 208 箇所は充足範囲の外にあり、逐語的な読みの下では未充足である。
この 208 箇所の扱いは follow-up の判断事項として残す（別 spec を切るか、既存配置を追認して
steering の「テストレイアウト」を実態に合わせるか）。

### 決着（2026-08-14・ユーザー判断）

**残存 208 箇所 / 21 ファイルの移設は、本 spec の外に別 spec として切り出す。**

タスク 3.6 のレビュアー（逐語的な読みを退ける）と最終ゲートの被覆検証（文言どおりには
未充足）の対立は、どちらの読みも要件文言そのものを書き換えずに済ませることを選んで
決着させた。すなわち:

- **要件 4.5 の文言は改訂しない。** 承認済み要件を後から狭めない。
- **steering の「テストレイアウト」も実態に合わせない。** 規約側を実態に寄せると
  既存債務が正式に追認されてしまう。
- **本 spec の完了判定に対しては、208 箇所を明示的な繰り越しとして扱う。**
  本 spec は 4.5 を「自身が新たに置く／動かす配置」について充足しており（§3）、
  着手前 748 箇所 / 71 ファイルを 208 / 21 へ削減し、新規追加はゼロ。
  残りは完全な既存債務であり、後続 spec が引き取る。

後続 spec が扱う範囲と、そこで避けられない論点:

- `src/**/tests.rs` に残る `spawn_test_app` 208 箇所 / 21 ファイルの `tests/` への移設。
- 移設は Out of Boundary の可視性変更を強制する（`pub(crate) mod query_log` の公開化、
  private な `account_ref_id` の公開化など）。3 ファイルを実際に移設して確認済み。
  この可視性をどこまで緩めるかが後続 spec の中心的な設計判断になる。
- 移設のみでは実行時間は縮まない（`migration-classification.md` の `TestDb` 移行と違い、
  配置の問題であってフィクスチャのコストの問題ではない）。

## 3. 4.5 の充足確認

本 spec が追加・移動した配置は次のとおりで、すべて規約に沿っている。

| 追加物 | 位置 | 判定 |
|---|---|---|
| `tests/harness_release_it.rs` | `tests/` 直下・`_it.rs` | ✅ `spawn_test_app` で実インスタンスを起動する検証。規約どおり |
| `tests/harness_sweep_it.rs` | `tests/` 直下・`_it.rs` | ✅ 同上（起動時スイープを実起動経路で検証） |
| `src/test_harness/sweep/tests.rs` | 単体テスト位置 | ✅ 10 本すべて純粋関数（名前解析・回収可否判定）。DB にも触れない |
| `src/test_harness/db_fixture/tests.rs` | 単体テスト位置 | ✅ `spawn_test_db` のみ。**実インスタンスを起動しない**（起動しないことの検証そのもの） |
| `src/test_harness/reaper/tests.rs` | 単体テスト位置 | ✅ 実 DB 接続は張るが、ルーター・サーバ・`AppState` は起動しない |

`db_fixture` と `reaper` の単体テストが実 DB に触れる点は規約違反ではない。規約の文言は
「DB込みの**実起動インスタンス**を要する検証」であり、本 spec が導入した 2 段階の階層
（design「テスト用フィクスチャの起動コスト階層（実インスタンス起動と DB のみの 2 段階）」）の
下段＝ DB のみは、その条件に当たらない。

`src/test_harness/tests.rs` の 5 箇所は `spawn_test_app` の隔離性そのものを検証する
自己検証テストで、`migration-classification.md` §5 が移設・移行の対象外と明記している。
統合側の対応物は `tests/test_harness_lifecycle_it.rs` に別途ある。

## 4. 意図的に手を付けなかったもの

- **単体テスト位置に残る 208 箇所** — 上記 §2 の理由。着手前から存在し、本 spec は
  1 件も増やしていない。
- **`tests/media_store_it.rs`** — 逆方向の所見。`LocalFsStore` をテンポラリディレクトリ上で
  検証するだけで、DB も実インスタンスも要さない。単体テスト位置に置ける。ただし 4.5 は
  一方向の要件であり、これは違反ではない。記録のみ。

## 5. 再現手順

```
git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' | wc -l          # 208
git grep -o 'spawn_test_app(' b50d8fd -- 'src/**/tests.rs' | wc -l  # 748
git grep -o 'spawn_federation_pair(' -- 'src/**/tests.rs' | wc -l   # 0
git ls-files 'tests/*_it.rs' | wc -l                                # 87
git ls-files 'tests/*_it.rs' | xargs grep -l 'spawn_test_app\|spawn_federation_pair\|spawn_test_db' | wc -l   # 84
cargo test --lib test_harness                                        # 18 passed
```

「新規に増やしていない」の検証（本文書で最も荷重の大きい主張。ファイル別に突き合わせる）:

```
git grep -c 'spawn_test_app(' -- 'src/**/tests.rs' | sort > /tmp/after.txt
git grep -c 'spawn_test_app(' b50d8fd -- 'src/**/tests.rs' \
  | sed 's/^b50d8fd://' | sort > /tmp/before.txt
# 現在の各ファイルが before に存在し、かつ件数が増えていないことを確認する
join -t: /tmp/after.txt /tmp/before.txt | awk -F: '$2 > $3 {print "INCREASED: " $0}'
# 出力が空であること。また after 側のファイル集合が before 側の部分集合であること:
comm -23 <(cut -d: -f1 /tmp/after.txt) <(cut -d: -f1 /tmp/before.txt)
# 出力が空であること（＝新規ファイルなし）
```

上記 2 つの確認はいずれも出力が空になる（新規ファイル 0、件数の増加 0）。

**計数単位の注意**: `src/` 全体を対象にすると 210 になる。差の 2 は
`src/test_harness.rs` の定義そのものと `src/federation/module.rs` の doc コメント言及で、
`src/**/tests.rs` に限れば行単位・出現単位のどちらで数えても 208 で一致する。
また 205–206 は**テスト関数**の数、208 は**呼び出し箇所**の数であり、単位が異なる。
