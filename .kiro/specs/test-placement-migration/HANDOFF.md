# 引き継ぎ: test-placement-migration（**spec 完成・実装未着手**）

最終更新: 2026-08-14 / 作業ツリー: クリーン（spec 文書のみ追加、`src/` は無変更）
`spec.json`: `phase: "tasks-generated"`、`ready_for_implementation: true`、`ssot` フィールド不在 ＝ **spec が SSoT**。

> **この spec はまだログではない。** requirements / design / tasks が実装より上位に立つ。
> コードとの差異はコード側の不足として扱う。反転は `/kiro-validate-impl` の GO でのみ起きる。

再開: `/kiro-impl test-placement-migration`

---

## 1. 現在地

| 成果物 | 状態 |
|---|---|
| `requirements.md` | 生成・承認済み。6 要件 / 31 受け入れ基準 |
| `design.md` | 生成・承認済み |
| `tasks.md` | 生成・承認済み。8 major / 20 sub-task（うち 13 が `(P)`） |
| `research.md` | 全 20 ファイル試行コンパイルの実測記録 |
| 実装 | **未着手**。`src/` への変更は 1 行もない |

**この spec が引き取ったもの**: 先行 spec `test-infrastructure` の要件 4.5 からの明示的な繰り越し。
`src/**/tests.rs` に残る `spawn_test_app` 呼び出し **203 箇所 / 20 ファイル**を `tests/*_it.rs` へ移設する
（`git grep -o` で 208 だが、うち `src/test_harness/tests.rs` の 5 箇所は先行 spec が対象外と判断済み）。
繰り越しの決定は `../test-infrastructure/placement-audit.md` §2「決着」。

移設タスクを持つのは **190 箇所 / 19 ファイル**。残る 13 箇所（`src/statuses/render_assembler/tests.rs`）は
設計時点で例外と確定している（§3）。

---

## 2. 設計フェーズで前提が覆った（**最重要・再導出しないこと**）

先行 spec の `placement-audit.md` §2 は 3 ファイルの試行から「移設は Out of Boundary の可視性変更を強制する」と
結論し、それが本 spec の中心的論点として引き継がれた。**20 ファイル全部で検証した結果、話はもっと軽かった。**

### 検証方法（再現可能）

対象 20 ファイルを機械変換（`use crate::` → `use kawasemi::`、トップレベルの `use super::` → 親モジュールの
絶対パス）して `tests/trial_*_it.rs` として生成し、`cargo check --tests --keep-going` を 1 回実行した。
**`--keep-going` は必須** — 付けないと cargo が途中でターゲットを打ち切り、エラーの出ないファイルと
「まだ検査されていない」ファイルが区別できない（実際に 1 回目は 12 ファイル、2 回目は 16 ファイルと数が揺れた）。

### 結果

| 分類 | 件数 | crate の変更 |
|---|---|---|
| `use super::*` が隠していた**公開型**（`Id` 119 / `AccountRef` 68 / `Arc` 59 / `Value` 47 / `StatusCode` 40 …） | 約 600 | **不要**（移設先での明示 import） |
| 非公開モジュールへのパス参照（E0603 × 4） | 4 | **不要**（`pub use` 済みパスへ変更） |
| 親モジュールの非公開項目 — **純粋テストのみ**が使用 | 8 項目 | **不要**（そのテストは移設対象外） |
| 親モジュールの非公開項目 — **`spawn_test_app` テスト**が使用 | 8 項目 | 要検討（§3） |

- **エラー 0 件のファイルが 4 つ**: `notifications/tests.rs` / `search/tests.rs` / `social_graph/tests.rs` /
  `accounts/account_service/tests.rs`。`use super::` を持たず `crate::` の公開パスだけで書かれている
- **移設の単位はファイルではなくテスト関数である。** これが前提が覆った核心。非公開項目に触れるテストの
  多くは `spawn_test_app` を呼ばない純粋な単体テスト（パーサ・フォーマッタ）であり、**そもそも移設対象ではない**。
  `placement-audit.md` が阻害例として挙げた `search/hydrator.rs` の `account_ref_id` がまさにこれで、
  同関数を直接検証するテスト（`hydrator/tests.rs:51`）は `spawn_test_app` を呼ばない。移設対象で
  同関数を使うのは 1 本だけ、しかも 1 行の値取得

**実装で可視性の壁に当たったら、まず「そのテストは `spawn_test_app` を呼ぶか」を見ること。**
呼ばないなら移設対象ではないので、何も緩めなくてよい。

---

## 3. 中心的な設計判断: 可視性の梯子

項目ごとに、成立する**最も低い段**を選ぶ。新しい可視性の慣用句は導入しない。

| 段 | 手段 | 既定フィーチャのビルド | 適用先 |
|---|---|---|---|
| **Tier 0** | 明示 import / `pub use` 済みパス / 1 行ヘルパーのテストローカル再定義 | 影響なし | 阻害要因の大半 |
| **Tier 1** | 既にゲート内にある項目のゲート幅調整 | 影響なし（実測済み） | `test_harness::query_log` **のみ** |
| **Tier 2** | 移設せず例外として記録 | 影響なし | 本番モジュールの非公開**型**を構築・命名する検証 |

### 棄却した案（**書いてコンパイルして棄却した。推論ではない**）

「各モジュールにゲート付き `pub mod test_access` を置き、非公開項目を `pub use` で再輸出して
一覧性を得る」案は成立しない。**Rust は `pub use` による可視性の拡大を許さない。**

```
error[E0365]: `TolerantPolls` is private, and cannot be re-exported
error[E0364]: `account_ref_id` is private, and cannot be re-exported
```

項目を `pub(crate)` に緩めてから再輸出しても同じ。

```
error[E0365]: `TolerantPolls` is only public within the crate, and cannot be re-exported outside
```

**crate 外へ項目を見せる方法は、その項目自身に `pub` を書くこと以外に存在しない。**
したがって封じ込めが効くのは、`src/test_harness.rs` のように**項目を含むモジュール宣言そのものが
ゲートされている**場合に限られる。これが Tier 2 を「妥協」ではなく「構造上の終着点」にしている。

### 真の阻害要因（実測）

| 項目 | 現在の可視性 | 対象テスト数 | 段 |
|---|---|---|---|
| `test_harness::query_log` | `#[cfg(test)] pub(crate) mod` | statuses 系 3 ファイル | Tier 1 |
| `statuses::render_assembler::RenderContext` | `pub(crate) struct` | 13 | **Tier 2** |
| `notifications::service::RequiredPolls` | private struct | 2 | Tier 2 見込み |
| `search::hydrator::TolerantPolls` | private struct | 1 | Tier 2 見込み |
| `search::hydrator::account_ref_id` | private fn | 1 | Tier 0 |
| `signatures::signer::{sha256_pkcs1v15_padding, host_from_url}` | private fn | 2, 1 | Tier 0 |
| `signatures::negotiation::{format_from_db, format_to_db}` | private fn | 各 1 | Tier 0 |
| `federation::endpoints::webfinger::parse_acct_resource` | private fn | 1 | Tier 0 |

**`src/statuses/render_assembler/tests.rs` の 13 本は最初から移設しない。** 移設するには
`RenderContext` に加えて `StatusRenderAssembler` 本体・`new`・`assemble_many`・`assemble_one` を
すべて `pub` にする必要がある（全部 `pub(crate)`）。steering が「唯一の組み立て経路」と定める
crate 内部の要を、テストの配置を直すために公開 API 契約へ昇格させることになる。要件 3.1 が禁じている。

Tier 0 の非公開 fn はいずれも、移設対象テストでは**ふるまいの検証ではなく期待値の構築**に使われている
（ふるまいを検証している純粋テストは元位置に残る）。だからテストローカルな再定義は検証内容を変えない。

---

## 4. 着手順（守るべき順序）

```
1.1 インベントリ確定 ─┬─→ 2〜5, 7 の移設波（並列可）─┐
1.2 ベースライン計測 ─┤                              ├→ 8.1/8.2 例外確定 → 8.3 再計測 → 8.4 最終検証
1.3 query_log ゲート ─┴─→ 6 statuses 系（1.3 必須）──┘
```

- **タスク 1.2（移設前のフルスイート計測）は移設を 1 件でも始めたら取り返しがつかない。** 要件 5.1 が
  前後比較を求めており、後から before を測る手段はない。最初に必ず実施すること
- **タスク 1.3 は statuses 系 2 ファイル（6.1 / 6.2）の前提**。両者ともクエリ計測モジュールを使う。
  それ以外の波とは独立で並行実施できる
- 移設の単位はファイル。ロールバックは移設先を削除して移設元を復元すれば済む
- 13 の `(P)` タスクは触るファイル集合が互いに素であることを確認済み

---

## 5. 検証に使うコマンド

```
# 単体テスト位置の残存（完了時は「例外の件数 + 対象外 5」に一致すべき）
git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' | wc -l          # 現在 208
git grep -c 'spawn_test_app(' -- 'src/**/tests.rs' | sort -t: -k2 -rn   # ファイル別内訳

# 統合テストバイナリ数（移設で増える。87 → 最大 106）
git ls-files 'tests/*_it.rs' | wc -l                                # 現在 87

# 試行移設の検証（--keep-going が必須）
cargo check --tests --keep-going --message-format=short

# 公開面が広がっていないこと
cargo build                      # 既定フィーチャ。成功すること
git diff | grep -E '^\+\s*pub (fn|struct|enum|trait|mod|const)'   # 本番モジュールの昇格がないこと

cargo test                       # フルスイート
cargo clippy --all-targets       # 警告ゼロが基準
cargo fmt --check
```

テスト DB: `postgres://kawasemi_test:kawasemi_test_pw@127.0.0.1:5432/kawasemi_test`

```
# 残存スキーマ確認（アンダースコアのエスケープ必須。_ は LIKE のワイルドカード）
psql "<url>" -c "select count(*) from information_schema.schemata where schema_name like 'kawasemi\_%'"
```

### 落とし穴

- **`cargo check --tests` に `--keep-going` を付けないと結果が不完全になる。** ターゲットが途中で
  打ち切られ、「エラー 0 件」と「未検査」が区別できない
- **`cargo build --tests` と `--all-features` は `target/debug/libkawasemi.rlib` を feature ON 版で
  上書きする。** 成果物を検査する前に必ず素の `cargo build` を打つこと（先行 spec で実際に踏んだ）
- **実行時間は単発測定で判断しない。** 先行 spec の計測で 9 回中 323〜560 秒の幅があった。
  要件 5.2 の 1.2 倍判定は前後で試行回数を揃えた中央値で行う
- **フルスイートの出力を `tail` で切らない。** 失敗理由が失われる
- **移設後のテスト失敗は、まず移設が原因かを切り分ける。** `TestApp` のバックグラウンドループも
  `runtime.keys` も移設で変わらない。疑うべきは import の取り違えか、移設元に残したヘルパーとの不整合

---

## 6. 判断の根拠が要るときに読む文書

| 文書 | 内容 |
|---|---|
| `research.md` | 試行コンパイルの全結果、棄却した `test_access` 案とそのエラー出力、実行コストの構造 |
| `design.md`「可視性の梯子」 | 段の定義と判定フロー図 |
| `design.md`「阻害要因の実測結果」 | 8 項目の一覧と適用する段 |
| `../test-infrastructure/placement-audit.md` §2 | 繰り越しの決定と、要件 4.5 の 2 通りの読みの論証 |
| `../test-infrastructure/migration-classification.md` §5 | `src/test_harness/tests.rs` を対象外とした根拠 |

---

## 7. 未確定のまま実装に渡すもの

- **例外の最終件数**。確実なのは `render_assembler` の 13 本のみ。`TolerantPolls` / `RequiredPolls` を
  使う 3 本は Tier 2 見込みだが、実装時に Tier 0（テストローカル再定義）で解けるか再判定する余地がある。
  残りは各移設タスクの判定に委ねている
- **実行時間の閾値 1.2 倍**は要件フェーズで Claude が設定しユーザーが承認したもの。工学的な相場観であって
  計測に基づく数字ではない。1.2 を超えた場合の是正は移設先ファイルの粒度統合（要件 5.3）
- **移設先ファイル名**は `design.md`「File Structure Plan」に案を置いたが、名前の重複は実装時に再確認すること。
  より重要なのは**検証内容の重複**（要件 2.4）で、既存 87 本には同じ領域を扱うものが既にある
  （`notification_list_it.rs` / `notification_show_dismiss_it.rs` / `search_statuses_it.rs` など）。
  先行 spec の監査では二重検証の組は見つからなかったが、それはファイル名と対象モジュールの対応を見た
  範囲の所見であり、網羅的な突き合わせではない（`placement-audit.md` §1）

## 8. ユーザーが要件フェーズで決めたこと（勝手に変えないこと）

- **例外は許す。** ただし「移設が既定フィーチャの公開面拡大を強いる」場合に限り、項目名つきで列挙する
- **`src/test_harness/tests.rs` の 5 箇所は対象外**（先行 spec の既決事項を踏襲）
- **steering「テストレイアウト」の規約文言は緩和しない**（要件 6.1）。規約を実態に寄せると
  既存債務を正式に追認することになる
