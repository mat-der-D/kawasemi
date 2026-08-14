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

## Requirements
<!-- Will be generated in /kiro-spec-requirements phase -->
