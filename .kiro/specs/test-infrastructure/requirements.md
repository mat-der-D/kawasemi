# Requirements Document

## Introduction

kawasemi のテストスイートは、素直に実行できない状態にある。`cargo test --lib` を単一
プロセスで実行すると 256 件が失敗するが、その全件が `PoolTimedOut` であり、本物の失敗は
1 件も含まれていない。現状はモジュール単位に分割し、実行前に孤立スキーマを掃除する、という
手順書を人間・AI エージェントの双方が踏むことで回避している。

2026-08-13 の実測で真因を特定した。同時実行数の問題ではなく、`TestApp` の接続リークである。

- `TestApp` 1 個が接続を即時に張り、`cleanup()` は正しく解放するが、`Drop` は 1 本も
  解放しない（実測：8 個を drop し 3 秒待っても 40 接続が残存）
- `src/` の 131 箇所が `cleanup()` を呼んでいない（`spawn_test_app` 750 回に対し
  `cleanup()` 619 回）
- 結果として 1 プロセスで利用可能接続の上限に達し、以降の全テストが失敗する

緩和として `spawn_test_app` のプールサイズを縮小した（コミット `2703704`）。リーク許容量は
増えたが、リーク自体は残っている。

本 spec は、後始末を呼び出し側の規律に依存する構造そのものを解消する。131 箇所を機械的に
埋めるだけでは、Phase 2 で新しいテストを書いた時点で同じ比率で漏れが再発するため、規律を
要求しない形にすることが本質である。あわせて、テストスイートが DB に過剰依存している状態
（lib テスト 1,764 件中 750 件が実インスタンスを要求し、全体で 891 秒かかる）を是正し、
テスト専用資産が本番成果物に混入している状態を解消する。

steering `tech.md` の「テスト実行基盤の既知の問題」節に記録済みの 2 項目（孤立スキーマの
起動時スイープ未実装、`federation::outbound::worker` の間欠的失敗）は本 spec が引き取る。

## Boundary Context

- **In scope**: テストの後始末・分離・実行可能性、および本番成果物からのテスト専用資産の
  排除。テスト実行のためのビルド構成。
- **Out of scope**:
  - テストフィクスチャの重複集約（B-1）— 同じファイルを触る際の付随変更としては許容するが、
    独立した達成目標としては扱わない
  - 契約テストの網羅性（B-3、`assert_golden` の適用範囲拡大）— 「何を assert するか」の
    別軸であり、本 spec の完了とは独立に進められる
  - プロダクションの機能追加・振る舞い変更。ただし要件 6 の調査結果としてプロダクション側の
    欠陥が確定した場合は、その事実の記録までを本 spec の責務とする
- **Adjacent expectations**:
  - 本 spec はプロダクションコードの観測可能な振る舞いを変えないことを前提とする。既存の
    契約テスト（golden）の内容は本 spec の変更対象ではなく、変わらないことが成功条件の一部
  - Phase 2（`streaming` / `web-push`）は本 spec の完了後に着手する。Phase 2 で追加される
    テストが同じ負債を再生産しないことが、本 spec の存在理由である

## Requirements

### Requirement 1: テストスイートの一括実行

**Objective:** 開発者（人間・AI エージェント）として、テストスイートを単一のコマンドで
実行したい。それにより、手順書を介さずに結果を信頼でき、CI にもそのまま記述できる。

#### Acceptance Criteria

1. When `cargo test --lib` が単一プロセスで実行されたとき, the Test Suite shall 接続の
   枯渇に起因する失敗を 1 件も発生させずに完了する。
2. When テストスイートが実行されたとき, the Test Suite shall 実行前の孤立スキーマ掃除を
   前提条件とせずに完了する。
3. If テストが失敗したとき, the Test Suite shall その失敗がテスト対象の欠陥に起因する
   ものであることを保証する（実行基盤の資源枯渇に起因する失敗と混在させない）。
4. The Test Suite shall モジュール単位への分割実行と単一プロセスでの一括実行の双方で、
   同一の成否を返す。

### Requirement 2: テスト実行資源の解放保証

**Objective:** テストを書く開発者として、後始末を明示的に書かなくても資源が解放される
ようにしたい。それにより、呼び忘れという失敗様式そのものを無くせる。

#### Acceptance Criteria

1. When 1 つのテストが終了したとき, the Test Harness shall そのテストが確保した接続を
   解放する。
2. When テストがパニックにより異常終了したとき, the Test Harness shall そのテストが
   確保した接続を解放する。
3. The Test Harness shall 後始末の実行を、テストを書く側の記述に依存させない。
4. If テストの記述が後始末を省略したとき, the Test Harness shall 資源が解放されない状態を
   生じさせない。
5. While 1 プロセス内で多数のテストが逐次実行されている間, the Test Harness shall 同時に
   生存しているテスト数に比例する量を超える接続を保持しない。

### Requirement 3: 孤立スキーマの回収

**Objective:** 開発者として、過去の異常終了が残した残骸に後続の実行が影響されないように
したい。それにより、手動での掃除を不要にできる。

#### Acceptance Criteria

1. When テスト実行基盤が起動したとき, the Test Harness shall 過去の実行が残した孤立
   スキーマを回収する。
2. While 現在実行中のテストが使用しているスキーマが存在する間, the Test Harness shall
   それらを回収対象に含めない。
3. If 回収処理が失敗したとき, the Test Harness shall テスト自体の実行を妨げない。
4. The Test Harness shall 回収を 1 プロセスにつき 1 回だけ実行する。

### Requirement 4: 単体テストの実インスタンス非依存化

**Objective:** 開発者として、単体テストを DB なしで高速に実行したい。それにより、変更の
フィードバックが速くなり、テストが直列実行に縛られなくなる。

#### Acceptance Criteria

1. The Test Suite shall 実インスタンスの起動を要する単体テストの件数を、現状（1,764 件中
   750 件）から削減する。
2. Where 検証対象が外部状態を持たない純粋なロジックである場合, the Test Suite shall
   その検証を実インスタンスの起動なしで実行する。
3. When 単体テストが実行されたとき, the Test Suite shall 現状（全体 891 秒）より短い時間で
   完了する。
4. The Test Suite shall 削減の前後で、検証しているふるまいの範囲を狭めない。
5. Where 実起動インスタンスを要する検証である場合, the Test Suite shall それを統合テストの
   配置規約（steering `structure.md`「テストレイアウト」）に従って配置する。

### Requirement 5: 本番成果物からのテスト専用資産の排除

**Objective:** 運用者として、配布される成果物にテスト専用の資格情報や接続先が含まれない
ようにしたい。それにより、事前ビルド済みバイナリを安全に配布できる。

#### Acceptance Criteria

1. The Release Artifact shall テスト専用の固定鍵・固定パスフレーズ・テスト用データベース
   接続先を含まない。
2. Where テスト構成が有効化されていない場合, the Build Configuration shall テストハーネスを
   成果物に含めない。
3. When テストがビルドされたとき, the Build Configuration shall テストハーネスを利用可能に
   する。
4. The Production Dependency Graph shall テストハーネスに起因する依存エッジを含まない。

### Requirement 6: 配送ワーカーのクレーム失敗の決着

**Objective:** 開発者として、接続資源の逼迫時に配送キューが黙ってジョブを取得しなくなる
現象の正体を知りたい。それにより、テストの問題なのかプロダクションの欠陥なのかを判断できる。

#### Acceptance Criteria

1. The Test Infrastructure shall 接続プールを 1 に縮小した際に
   `federation::outbound::worker` のクレーム件数が 0 になる現象について、再現条件を特定する。
2. If 当該現象がプロダクションの欠陥に起因すると判明したとき, the Test Infrastructure shall
   その事実と影響範囲を記録する。
3. If 当該現象がテスト固有の事情に起因すると判明したとき, the Test Infrastructure shall
   その根拠を記録し、steering `tech.md` の既知の問題の記述を実態に合わせて更新する。
4. The Test Infrastructure shall 本現象の調査結果を、原因不明のまま「flake」として放置しない。
