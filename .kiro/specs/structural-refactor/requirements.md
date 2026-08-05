# Requirements Document

## Introduction

Phase 1（全 11 spec）完了後のコードベースを対象とした構造リファクタリング。`docs/refactor-analysis-2026-08.md`（7 領域の並列監査、全指摘 file:line 検証済み）が示す通り、コードの正しさと規約の一貫性は良好（既定 clippy 警告ゼロ、デッドコードほぼ皆無、steering drift ほぼ無し）である一方、負債は一点に集中している — **組み立て（assembly）コードが spec 境界ごとに複製された**。加えて、型で守られるべき不変条件が文字列リテラルやコメントの中に退避している箇所がある。

この構造は Phase 2 以降の spec を足すたびに複製が増える方向に働く。本 spec は同レポート「E. 推奨着手順」の 1〜5、すなわち **Phase 2 に進む前にやるべき最小セット**を実施する。

本 spec は**振る舞いを変えない内部リファクタリング**である。したがって受け入れ基準の多くは「新しく何ができるようになるか」ではなく「何が変わらないことが保証されるか」「何が自動的に検出されるようになるか」を規定する。

## Boundary Context

- **In scope**:
  - 投票の重複に対するべき等な受信処理を、エラーメッセージ文言に依存せず判定できるようにする（分析レポート A-4）
  - エンドポイント横断の定型処理（`limit` 解釈、`Link` ヘッダー、時刻表記、真偽値解釈、サーバーエラー変換、公開オリジン解決）の挙動を全経路で統一する（B-6）
  - Status の JSON 表現の組み立てを単一の経路に統合する（A-1）
  - 一覧取得時のデータベースクエリ数を件数に比例させない（A-2）
  - 投稿作成・ブースト・お気に入りの複合書き込みを原子的にする（A-3）
  - 本番起動・テストアプリ起動・連合ペア起動が共有するモジュール配線シーケンスを一本化し、暗黙の順序制約を自動検証する（A-5）
- **Out of scope**:
  - テストフィクスチャの集約、テスト階層の再設計、golden 契約テストの浸透、`testing` feature 化、孤立スキーマ回収（B-1〜B-5）
  - 連合ヘルパー統合、レジストリパターンの抽象化、`server.rs` / `inbound_handlers.rs` の分割、ジェネリック引数の整理（B-7〜B-11）
  - ドキュメント圧縮、CONCERN 109 件の棚卸し、命名統一、`error ↔ api` 循環の解消（C 群）
  - 分析レポート「D. 良好で触るべきでない箇所」に列挙された機構（`api::pagination` / `api::error` / `api::ratelimit` / `oauth::middleware` / 連合コア設計 / contract ハーネス / エンティティ JSON 契約定義そのもの / ページネーション規約 / モジュールバンドルのボイラープレート）
  - Mastodon 互換 API の機能追加および API 契約そのものの変更
- **Adjacent expectations**:
  - 既存の 11 spec は `ssot: "implementation"`（handoff 済み）であり、それらの `design.md` はログである。本リファクタで生じるコードと旧 design.md の乖離は不整合ではない。
  - 回帰検知は既存テスト資産（`src/*/tests.rs` 約 51,779 行 + `tests/` 47,397 行、84 統合テストファイル）に依存する。本 spec はこの網の拡充を目的としないが、A-5 の順序制約のように**現状テストが存在しない不変条件**については新規テストを追加する。
  - 入力とした `docs/refactor-analysis-2026-08.md` は分析ログであって仕様ではない。本 spec の実装完了（`/kiro-validate-impl` の GO）時点で削除される。削除は handoff 作業の一部であり、本 spec の実装タスクではない。

## Requirements

### Requirement 1: 振る舞いの不変性

**Objective:** As kawasemi の実装者, I want リファクタリングの前後で外部から観測可能な振る舞いが一切変わらないこと, so that クライアント互換性と連合互換性を壊さずに内部構造だけを改善できる

#### Acceptance Criteria

1. When 同一のリクエストをリファクタリングの前後で送信した, the kawasemi Server shall 同一の HTTP ステータスコード、同一のレスポンスボディ、同一のレスポンスヘッダーを返す
2. When 連合相手へ Activity を送出する, the kawasemi Server shall リファクタリング前と同一の Activity JSON 表現を生成する
3. If リファクタリング後に既存テストが失敗した, then the 実装者 shall テストの期待値ではなく実装側を修正する
4. Where 既存テストの期待値を変更する場合, the 実装者 shall その変更が仕様変更ではなく「テスト側が誤っていた」ことの修正であることを、根拠とともに記録する
5. When リファクタリングの各項目が完了した, the kawasemi Server shall `cargo clippy`（既定設定）で警告ゼロを維持する
6. When リファクタリングの各項目が完了した, the テストスイート shall その時点でグリーンである

### Requirement 2: 投票の重複に対するべき等な受信処理

**Objective:** As 連合相手およびローカルオーナー, I want 既に投票済みのアクターからの投票 Activity が重複到着しても正しくべき等に扱われること, so that エラーメッセージの文言を変更しただけで正常系が静かに壊れることがない

#### Acceptance Criteria

1. When 既に投票済みのアクターからの投票 Activity を受信した, the Inbound Activity Handler shall その Activity を処理済みとして扱い、エラーを返さない
2. When ローカル投稿主へ自身の投票 Activity がループバックした, the Inbound Activity Handler shall スプリアスな 422 応答を発生させない
3. If 「重複投票」を表すエラーメッセージの文言が変更された, then the Inbound Activity Handler shall べき等判定を従来どおり維持する
4. If 「重複投票」という失敗理由を識別する手段がコード変更によって失われた, then the ビルド shall それをコンパイル時に検出する
5. When 重複投票以外の理由で処理不能エラーが発生した, the Inbound Activity Handler shall それをべき等ケースとして扱わず、エラーとして伝播する
6. When 重複投票を表す失敗が発生した, the kawasemi Server shall API クライアント向けのエラーメッセージ文言をリファクタリング前と同一に保つ

### Requirement 3: エンドポイント横断の定型処理の一貫性

**Objective:** As API クライアント, I want すべてのエンドポイントでページネーション・時刻表記・真偽値解釈・サーバーエラー応答が同一に振る舞うこと, so that エンドポイントごとの微妙な差異を気にせずクライアントを実装できる

#### Acceptance Criteria

1. The kawasemi API shall `limit` クエリパラメータの解釈（省略時の既定値、範囲外の値、非数値）を、`limit` を受け付ける全エンドポイントで同一に扱う
2. The kawasemi API shall ページネーション可能な全エンドポイントで同一形式の `Link` ヘッダーを生成する
3. The kawasemi API shall すべてのエンティティの時刻フィールドを同一の形式で出力する
4. The kawasemi API shall 真偽値クエリパラメータの解釈を、それを受け付ける全エンドポイントで同一に扱う
5. When データストアの障害が発生した, the kawasemi API shall どのエンドポイントであっても同一形式のサーバーエラー応答を返す
6. The kawasemi Server shall 公開 URL のオリジン解決を全モジュールで同一の規則により行う
7. When 新しいエンドポイントが追加される, the 実装者 shall これらの定型処理を再実装することなく、既存の共通手段を呼び出すだけで済む

### Requirement 4: Status 表現の単一の組み立て経路

**Objective:** As API クライアント, I want 単体取得・アカウント別一覧・通知・タイムライン・検索のどの経路から取得しても Status の JSON 表現が一致すること, so that 経路ごとの差異による表示の不整合が起きない

#### Acceptance Criteria

1. When 同一の Status を異なる API 経路から取得した, the kawasemi Server shall メディア・タグ・絵文字・投票・インタラクション状態について同一の JSON 表現を返す
2. When Status の JSON 表現の仕様を変更する, the 実装者 shall 単一箇所の変更で全経路に反映できる
3. Where 経路ごとに意図的な差異が必要な場合, the kawasemi Server shall その差異を呼び出し側からの明示的な入力として受け取り、経路ごとの暗黙の既定値による分岐を持たない
4. When ミュート判定に必要な閲覧者コンテキストが与えられない経路から Status を取得した, the kawasemi Server shall `muted` を `false` として出力する
5. When ミュート判定に必要な閲覧者コンテキストが与えられる経路から Status を取得した, the kawasemi Server shall そのコンテキストに基づく `muted` の値を出力する
6. If Status の組み立て処理が新たな経路で必要になった, then the 実装者 shall 既存の組み立て処理を複製することなく再利用できる

### Requirement 5: 一覧取得時のクエリ効率

**Objective:** As 低スペック VPS でサーバーを運用するオーナー, I want 一覧取得のデータベースクエリ数が件数に比例して増えないこと, so that ページサイズが大きい場合でも応答性能が劣化しない

#### Acceptance Criteria

1. When N 件の Status を含む一覧を返す, the kawasemi Server shall Status 固有の付随データ（メディア・タグ・絵文字・インタラクション状態・投票）の取得回数を N に依存しない一定回数に収める
2. When 一覧内の複数の Status が同一の著者を持つ, the kawasemi Server shall その著者の情報を一覧あたり 1 回だけ解決する
3. When 一覧内に K 種類の異なる著者が含まれる, the kawasemi Server shall アカウント解決の回数を K に比例させ、N には比例させない
4. When 一覧内の Status が複数のメディアを持つ, the kawasemi Server shall メディア情報を個別ではなく一括で取得する
5. When 閲覧者コンテキスト付きで一覧を返す, the kawasemi Server shall お気に入り・ブックマーク・ピン・ブーストの各状態を個別ではなく一括で取得する
6. When ブーストを含む一覧を返す, the kawasemi Server shall ブースト元の Status についても同じ一括取得の対象に含める
7. When 一覧取得をバッチ化した, the kawasemi Server shall 各 Status の JSON 表現および一覧の並び順をバッチ化の前後で変えない

### Requirement 6: 複合書き込みの原子性

**Objective:** As サーバーのオーナー, I want 投稿作成・ブースト・お気に入りの操作が途中で失敗しても中途半端な状態が残らないこと, so that カウンタの値が実体と食い違ったまま運用を続けることがない

#### Acceptance Criteria

1. If 投稿の作成が途中で失敗した, then the kawasemi Server shall 投稿本体・メディア添付・投票・タグ・親投稿の返信カウントを含め、その操作による変更を一切残さない
2. If ブースト、お気に入り、またはお気に入り解除が途中で失敗した, then the kawasemi Server shall レコードとカウンタのいずれか一方だけが更新された状態を残さない
3. When 複合書き込みが成功した, the kawasemi Server shall カウンタの値と実体レコードの数が一致する状態にする
4. If 複合書き込みの一部が失敗した, then the kawasemi Server shall 呼び出し元にエラーを返し、成功したかのように振る舞わない
5. When 複合書き込みが成功パスを通った, the kawasemi Server shall レスポンスおよびカウンタ値をリファクタリング前と同一に保つ

### Requirement 7: モジュール配線の単一実装と順序制約の保証

**Objective:** As kawasemi の実装者, I want 本番起動・テストアプリ起動・連合ペア起動が同一の配線シーケンスを共有し、暗黙の順序制約が自動検証されること, so that 新しい spec を足すたびに配線を 3 箇所へコピーし、その都度順序制約を手で守らなくて済む

#### Acceptance Criteria

1. When 本番サーバー、テストアプリ、連合ペアのいずれかが起動する, the kawasemi Server shall 同一のモジュール配線シーケンスを実行する
2. When モジュール配線に新しい段階が追加される, the 実装者 shall 単一箇所の変更で 3 つの起動経路すべてに反映できる
3. When 複数の実装が同一のレジストリスロットへ登録する, the kawasemi Server shall 意図した合成実装が実際に有効になる状態で起動する
4. If 配線の順序が壊れて意図しない実装が有効になった, then the テストスイート shall それを失敗として検出する
5. Where 起動経路ごとに固有の処理（設定値の生成、リスナーの bind、シャットダウン信号の扱い）が必要な場合, the kawasemi Server shall その差分のみを各経路に残し、共通部分を重複させない
6. When 配線シーケンスを一本化した, the kawasemi Server shall 本番起動経路の観測可能な挙動をリファクタリング前と同一に保つ
7. Where 一本化の前に 3 経路の間に配線の食い違いが既に存在する場合, the 実装者 shall 一本化によって解消される差異を明示的に記録し、それに伴って期待値が変わるテストについて正当性を示す
