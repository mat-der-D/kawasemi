# Requirements Document

## Project Description (Input)

**誰の問題か**: このリポジトリでテストを読み書きする開発者と、テスト配置規約を維持する立場。

**現状**: 実起動インスタンスを要する検証（`spawn_test_app` を呼ぶもの）が、統合テストの配置規約に反して単体テスト位置 `src/**/tests.rs` に **208 箇所 / 21 ファイル**残っている。これは test-infrastructure spec の要件 4.5 からの**明示的な繰り越し**であり、完全な既存債務である（同 spec 着手前は 748 箇所 / 71 ファイル。208 / 21 まで削減済みで、新規追加はゼロ）。test-infrastructure spec ではこの残りを扱わないと決めた（判断の記録と論証は `.kiro/specs/test-infrastructure/placement-audit.md` §2「決着」）。

**何が変わるべきか**: 残る 208 箇所 / 21 ファイルを `tests/` 直下の `*_it.rs` へ移設し、「実起動インスタンスを要する検証は統合テストの配置規約に従って配置する」が実態としても成り立つようにする。

**中心的な設計判断**: 移設は Out of Boundary の可視性変更を強制する（`pub(crate) mod query_log` の公開化、private な `account_ref_id` の公開化など）。3 ファイルを実際に移設して確認済み。この可視性をどこまで緩めることを許容するか — 移設のためだけに内部を公開してよいか、`#[cfg(any(test, feature = "test-harness"))]` ゲート越しの限定公開に留めるか、あるいは移設できないものを例外として明示的に残すか — が本 spec の中心的な設計判断になる。

**非目標**: 実行時間の短縮。移設は配置の問題であってフィクスチャの起動コストの問題ではないため、移設のみでは実行時間は縮まない（test-infrastructure の `TestDb` 移行とは性質が異なる）。

**参照**:
- `.kiro/specs/test-infrastructure/placement-audit.md` — 配置監査。§2 に要件 4.5 の 2 通りの読みと決着、§3 に規約の適用例、§5 に件数の再現手順
- `.kiro/specs/test-infrastructure/migration-classification.md` — 745 件の分類。§5 に移設・移行の対象外と判断した自己検証テスト

## Introduction

steering `structure.md`「テストレイアウト」は「DB込みの実起動インスタンスを要する検証は `tests/` 直下の `*_it.rs` に置く」と定めているが、実態はこれを満たしていない。単体テスト位置 `src/**/tests.rs` に `spawn_test_app` の呼び出しが 208 箇所 / 21 ファイル残っている。

このうち `src/test_harness/tests.rs` の 5 箇所は `spawn_test_app` 自体のふるまいを検証する自己検証テストであり、test-infrastructure spec が既に移設・移行の対象外と判断し、統合側の対応物も `tests/test_harness_lifecycle_it.rs` に存在する。したがって本 spec が実際に扱う対象は **203 箇所 / 20 ファイル**である。

本 spec の目的は、この残存債務を解消して規約と実態を一致させることにある。ただし移設は一部のファイルで内部項目の可視性緩和を強制する（`src/test_harness.rs` の `pub(crate) mod query_log`、`src/search/hydrator.rs` の完全に非公開な `account_ref_id` など、3 ファイルの実移設で確認済み）。テストの配置を直すために本番ライブラリの公開面を広げるのは、解こうとした問題より大きな問題を作る。そのため本 spec は「配置を直す」と「本番の公開面を広げない」を同時に満たすことを求め、両立しない検証については**記録された例外**として単体テスト位置に残すことを許す。例外が単なる逃げ道にならないよう、その正当化条件と網羅性の検証を要件として明示する。

## Boundary Context

- **In scope**:
  - 単体テスト位置 `src/**/tests.rs` に残る `spawn_test_app` 呼び出し 203 箇所 / 20 ファイルの `tests/` 直下 `*_it.rs` への移設
  - 移設に伴い必要となる可視性の調整（テスト構成に限定される範囲で）
  - 移設できない検証の例外としての記録と正当化
  - 移設前後での検証範囲の同一性、およびフルスイート実行時間・リソース消費の非悪化の確認
- **Out of scope**:
  - `src/test_harness/tests.rs` の 5 箇所（ハーネス自己検証テスト。test-infrastructure spec が対象外と判断済み）
  - フィクスチャの起動コスト削減およびテスト実行時間の短縮（本 spec は配置の問題のみを扱う）
  - `TestApp` から `TestDb` への追加のフィクスチャ移行（test-infrastructure spec で完了済み）
  - 検証内容そのものの追加・拡張・改善
  - 配置規約を将来にわたり自動的に強制する仕組み（lint・CI ゲート等）の導入
  - steering `structure.md`「テストレイアウト」の規約文言の緩和
- **Adjacent expectations**:
  - test-infrastructure spec が確立した隔離機構（1 フィクスチャにつき 1 スキーマ、`Drop` 経由の常駐リーパー回収、起動時スイープ）は本 spec が所有せず、そのまま動作し続けることを前提とする
  - `tests/*.rs` から `test_harness` が見えるのは `Cargo.toml` の自己 dev-dependency（`features = ["test-harness"]`）経由であり、この経路の存在を前提とする
  - 既存の `tests/*_it.rs` 87 本が検証している「モジュール横断の配線・契約・ライフサイクル」は本 spec の移設対象ではなく、移設物と重複させない

## Requirements

### Requirement 1: 実起動インスタンスを要する検証の配置

**Objective:** テストを読み書きする開発者として、実起動インスタンスを要する検証がすべて統合テスト位置にあってほしい。そうすれば「どこを見ればその検証があるか」を規約から一意に導けるようになる。

#### Acceptance Criteria

1. The Test Suite shall 単体テスト位置 `src/**/tests.rs` に `spawn_test_app` の呼び出しを持たない。ただし要件 4 に従って記録された例外と、対象外である `src/test_harness/tests.rs` を除く。
2. When 検証が実起動インスタンス（`spawn_test_app` が返す `TestApp`）を必要とする場合, the Test Suite shall その検証を `tests/` 直下に配置する。
3. When 移設先のファイルを新規に作成する場合, the Test Suite shall そのファイル名を `_it.rs` サフィックスで命名する。
4. The Migration shall 移設対象を 203 箇所 / 20 ファイルとして着手し、その内訳を移設前に確定させる。
5. The Migration shall 移設前の対象件数 203 が「移設した件数」と「記録された例外の件数」の合計に一致することを示す。

### Requirement 2: 検証範囲の保全

**Objective:** 開発者として、移設によって検証していた内容が失われないと保証されてほしい。そうすれば配置の変更を安心して受け入れられる。

#### Acceptance Criteria

1. When 検証を移設する場合, the Migration shall その検証の対象・入力・アサーションを移設前と同一に保つ。
2. The Migration shall 移設を理由としたテスト関数の削除、無効化、`#[ignore]` 付与、アサーションの削減を行わない。
3. If 移設先で検証が成立しない場合, then the Migration shall 検証を弱めることなく、その検証を要件 4 の例外として扱う。
4. The Migration shall 移設物が既存の `tests/*_it.rs` の検証と重複する検証を新たに作らない。
5. When 移設が完了した場合, the Test Suite shall フルスイート（lib テストと統合テストの双方）を通過する。
6. The Migration shall 移設前後でテスト関数の総数を比較し、差分が説明可能であることを示す。

### Requirement 3: 本番成果物の公開面の保全

**Objective:** このリポジトリを利用・保守する立場として、テストの都合で本番ライブラリの公開 API が広がらないでほしい。そうすればテスト配置の是正が意図せぬ API 契約の追加にならない。

#### Acceptance Criteria

1. The Library Build shall 既定フィーチャでのビルド成果物の公開 API に、本 spec の移設のためだけに公開された項目を含めない。
2. When 移設が非公開項目（private または crate 内限定）へのアクセスを必要とする場合, the Migration shall その項目の可視性の緩和をテスト構成に限定する。
3. If 可視性の緩和をテスト構成に限定できない場合, then the Migration shall その検証を移設せず、要件 4 の例外として記録する。
4. The Migration shall テスト専用資産（固定鍵・固定パスフレーズ・テスト DB 接続先を含むハーネス一式）が既定フィーチャのビルドから外れている状態を維持する。
5. When 可視性を緩和した場合, the Migration shall 緩和した項目とその適用範囲を一覧できる形で残す。

### Requirement 4: 移設できない検証の記録と正当化

**Objective:** 配置規約を維持する立場として、単体テスト位置に残る検証がすべて意図された例外だと確認できてほしい。そうすれば「規約違反」と「記録された判断」を区別できる。

#### Acceptance Criteria

1. Where 検証が移設されずに単体テスト位置に残る場合, the Migration shall その検証をファイル単位で件数とともに一箇所に列挙する。
2. Where 例外が記録される場合, the Migration shall 各例外について、移設が既定フィーチャのビルドにおける公開面の拡大を強いることを、具体的な項目名を挙げて示す。
3. If 例外の根拠が前項に該当しない場合, then the Migration shall その検証を例外とせず移設する。
4. The Migration shall 例外の一覧に、記録時点で確認された件数と、その件数を再現できる手順を含める。
5. The Migration shall 対象外である `src/test_harness/tests.rs` の 5 箇所を、例外とは区別して記載する。

### Requirement 5: 実行コストとリソース消費の非悪化

**Objective:** テストを日常的に実行する開発者として、配置を直したことでスイートが目に見えて遅くならないでほしい。そうすれば移設が開発サイクルの負担にならない。

#### Acceptance Criteria

1. The Migration shall 移設前と移設後のフルスイート実行時間を同一条件で測定し、両方の値を記録する。
2. The Test Suite shall 移設後のフルスイート実行時間を、移設前の 1.2 倍以内に収める。
3. If 移設後の実行時間が前項の範囲を超える場合, then the Migration shall 原因を特定し、移設先ファイルの粒度を調整して範囲内に収める。
4. The Test Suite shall 移設後もフルスイート実行中に同時に存在する隔離スキーマ数がテスト DB の接続・スキーマ上限を超えず、完走する。
5. The Test Suite shall 移設後も実行終了時に隔離スキーマを残さない。

### Requirement 6: 規約との整合と検証の再現性

**Objective:** 配置規約を維持する立場として、完了状態を後から誰でも再現して確認できてほしい。そうすれば「達成した」という主張を検証機構つきで残せる。

#### Acceptance Criteria

1. The Migration shall steering `structure.md`「テストレイアウト」の規約文言を、実態に合わせて緩和しない。
2. The Migration shall 単体テスト位置に残る `spawn_test_app` 呼び出しの件数を数える手順を、コマンドとして文書化する。
3. When 完了を主張する場合, the Migration shall 文書化した手順を実行した結果を証拠として示す。
4. The Migration shall 件数を数える際の単位（呼び出し箇所かテスト関数か）を明示し、参照する既存文書と同じ単位で比較する。
5. When 移設が完了した場合, the Test Suite shall steering の記述と実態が一致した状態になり、その一致が前項までの手順で確認できる。
