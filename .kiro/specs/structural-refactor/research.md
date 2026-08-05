# Research Log — structural-refactor

## Discovery Scope

Extension（既存コードベースへの内部リファクタ）として light discovery を実行。外部依存の新規追加は無いため WebSearch/WebFetch は不要。対象は 5 項目それぞれの**現状コードの実測**であり、`docs/refactor-analysis-2026-08.md`（2026-08-04 時点の分析ログ）の指摘を設計に落とす前に再検証した。

ベースライン：`cargo check --all-targets` は exit 0（着手前）。

## 実測による再検証

### A-4：エラー制御フローの文字列一致

| 項目 | 実測 |
|---|---|
| 分岐箇所 | `src/statuses/inbound_handlers.rs:1067-1074`（`err.status == UNPROCESSABLE_ENTITY && err.public_message == "actor has already voted in this poll"`） |
| 文言の出所 | `src/statuses/poll_repository.rs:295` の `rejected("actor has already voted in this poll")`。`rejected` は `:127-129` の `AppError::client(422, message)` |
| `AppError` の形 | `src/error.rs:83-95`。`kind` / `status` / `public_message` / `source` の 4 フィールド。ドメインバリアント無し |
| 構造体リテラルでの直接構築 | `src/error.rs` 内の 2 箇所（`client` / `server` コンストラクタ）のみ。**外部にゼロ**（`grep "AppError {"` の他ヒットは全て `-> AppError {` の関数シグネチャ） |

**含意**：`AppError` にフィールドを 1 つ追加しても、コンストラクタ経由でしか構築されていないため呼び出し側は一切壊れない。`#[non_exhaustive]` 等の追加措置も不要。既存の `client`/`server` は新フィールドを既定値で埋めるだけでよい。

### B-6：定型文の重複（実測カウント）

| 対象 | レポートの主張 | 実測 |
|---|---|---|
| `parse_optional_limit` | 5 | **5**（accounts / social_graph / timelines / notifications / statuses の各 `endpoints.rs`）。バイト単位で同一 |
| `parse_loose_bool` / `parse_optional_bool_query` | 2 | `parse_loose_bool` **3**（accounts / timelines / search）、`parse_optional_bool_query` **3**（accounts / timelines / search）。レポートは search を数え落としている |
| `format_time`（RFC3339） | 4 | **4**（accounts/serializer / statuses/serializer / notifications/serializer / statuses/endpoints） |
| `ForwardedOrigin::resolve("https", &self.domain, None, None)` | 5 | **5**（notifications/service:244 / search/hydrator:278 / search/tag_serializer:88 / statuses/account_provider:220 / social_graph/follow_request_service:221）。非テストのみ |
| `build_link_header` 呼び出し | 7 | **7**（timelines ×3 / social_graph ×1 / accounts ×1 / notifications ×1 / statuses ×1） |
| `sqlx::Error -> AppError` マッパー | 「約 26 ファイル」 | **26 関数**。ただし名前は `map_server_error`（11）/ `map_query_error` / `map_insert_error` / `map_tx_error` に分かれる |

**含意**：`map_*_error` は名前が 4 系統に分かれており、単純な機械置換ではなく「同一の意味を持つものだけを寄せる」判断が要る。`map_server_error` の 11 件は同一意味と確認済み。他系統は各々の `AppError` 形状（status / public_message）が異なる可能性があるため、個別確認の上で寄せる。

### A-1：Status レンダリンググルー 5 重化 — 5 箇所は「同一ではない」

レポートは「バイト単位に同一」（`tags_json`）としているが、**グルー全体としては 5 箇所に意味のある差異がある**。これを潰すと Requirement 1（振る舞い不変）に違反する。設計はこの差異を明示的な入力として保存しなければならない。

| 差異 | statuses/endpoints | statuses/account_provider | notifications/service | timelines/hydrator | search/hydrator |
|---|---|---|---|---|---|
| `interaction_state` の viewer 型 | `Option<Id>` | `Option<Id>` | **`Id`（非 Option）** | `&FilterContext` | `Option<Id>` |
| `muted` の解決元 | 既定値（`false`） | 既定値 | 既定値 | **`ctx.muted`** | 既定値 |
| 投票の解決経路 | **`poll_service.poll()`**（可視性チェックを含む） | `poll_repository::find_poll_by_id` | 同左 | 同左 | 同左 |
| 投票行が無い場合 | **エラー**（`poll_service` 内） | **エラー**（`not_found()`） | **エラー**（`poll_not_found()`） | **`None` に縮退** | **`None` に縮退** |
| `poll_json` の戻り型 | `Result<Value>` | `Result<Value>` | `Result<Value>` | `Result<Option<Value>>` | `Result<Option<Value>>` |
| ブースト先の解決 | `status_service.show(viewer, id)` | （個別） | （個別） | `find_by_id` + `reblog_target_visible(ctx)` | `find_by_id` + `status_visible(viewer)` |
| `now` の出所 | `self.runtime.clock.now()` | 同左 | 同左 | **`ctx.now`** | `self.runtime.clock.now()` |

共通なのは：`media_json`（`media_ids_for_status` → `find_by_id` ループ）、`tags_json`（`tags_for_status` → `format!("{}://{}/tags/{}")`）、`resolve_emojis`（`extract_content_tokens` → `emoji_repository::resolve_emojis`）、`account_json`（`accounts.show_account(id, None, origin)`）、`StatusRenderInput` の組み立て（`mentions: Vec::new()` 固定）、1 段のみのブースト入れ子。

**設計判断**：共通部分（`resolve_common` の 6 要素の解決と `leaf_render_input` の組み立て）を集約し、**差異は 2 つの注入点に押し出す**。
1. `muted` の解決 → `Option<&HashSet<Id>>`
2. 投票の解決 → `PollResolver` ポート（3 実装：`PollService` 経由 / 行必須（エラー種別は各モジュールが供給）/ 行任意）

ブースト先の解決と可視性判定は**集約しない**。各モジュールで使う協調オブジェクト（`StatusService` / `FilterContext` / `SearchHydrator::status_visible`）が本質的に異なり、押し込めばそれ自体が新しい暗黙分岐になる。集約するのは「解決済みのブースト先を受け取ってから」の組み立てのみ。

`src/search/hydrator.rs:59-74` の CONCERN（「4 番目の複製、切り出す潮時」）は本 spec で解消される。

### A-2：N+1 の実測構成と、バッチ化の現実的な上限

`StatusHydrator::hydrate`（`src/timelines/hydrator.rs:265-276`）は `for ... await` の逐次処理。1 投稿あたりの内訳：

- `account_json` → `AccountService::show_account`（`src/accounts/account_service.rs:562-609`）。内部で `resolve_local` + `ports.counts()` + `emoji_candidates()`。**同一著者でもキャッシュされない**
- `media_json` → `media_ids_for_status` 1 回 + `media_repository::find_by_id` を **media 件数だけ**（N+1 の中の N+1）
- `tags_json` → `tags_for_status` 1 回
- `resolve_emojis` → `resolve_emojis` 1 回
- `interaction_state` → `exists_favourite` / `exists_bookmark` / `exists_pin` / `find_reblog` の**個別 4 クエリ**
- `poll_json` → `find_poll_by_id` + `tally`
- ブーストがあれば対象にも同一処理が再帰（`:287-301`）

**バッチ化の現実的な上限**：`show_account` の完全なバッチ化は `AccountService` / `AccountPortsRegistry` / `AccountSerializer` にまたがり、accounts-and-instance の内部に踏み込む。本 spec の境界外（Out of Boundary）とし、代わりに **1 回の一覧組み立て内での著者単位メモ化**を採る。これは Requirement 5.2 / 5.3 を満たす（K 種類の著者に対して K 回）。Requirement 5.1 は「Status 固有の付随データ」に限定して書き直した（当初の「全クエリが N に比例しない」は `show_account` を書き換えない限り達成できないため）。

横展開元のパターンは `src/social_graph/repository.rs::load_states`：`WHERE (kind_col, id_col) IN (SELECT * FROM UNNEST($1::text[], $2::bigint[]))` で `targets.len()` によらず 7 クエリ固定（同ファイル `:114-126` の doc）。単一カラム版の先例は `emoji_repository.rs::resolve_emojis` の `= ANY($1)`。

### A-3：非トランザクションな複合書き込み

| 箇所 | 個別 `&self.pool` 呼び出しの列 |
|---|---|
| `StatusService::create_status`（`status_service.rs:808-853`） | `insert_status` → `attach_media` → `insert_poll` → `persist_tags` → `adjust_counts(Replies,+1)` |
| `InteractionService::reblog`（`:382-383`） | `insert_status` → `adjust_counts(Reblogs,+1)` |
| `InteractionService::favourite`（`:460-462`） | `add_favourite` → `adjust_counts(Favourites,+1)` |
| `InteractionService::unfavourite`（`:508`） | `remove_favourite` → `adjust_counts(Favourites,-1)` |

**重要な制約**：これらの書き込みの**直後に Activity 配送**（`deliver_create` / `deliver_announce` / `deliver_like` / `deliver_undo`）が続く。配送はネットワーク I/O であり、**トランザクション内に含めてはならない**（DB 接続を保持したまま外部 HTTP を待つことになり、配送失敗で正常なローカル書き込みが巻き戻る）。トランザクション境界は「DB 書き込み群のみ、commit 後に配送」とする。

正しい先例はリポジトリ層に既にある：`status_repository::delete_status`（`:571-635`）と `apply_edit`（`:649-692`）が自前で `pool.begin()` を張っている。エグゼキュータ・ジェネリック方式の先例は `src/social_graph/repository.rs:371, 442, 655` の `<'e, E: sqlx::PgExecutor<'e>>`。同ファイル `:168-180` の doc が「`&PgPool` 自身が `PgExecutor<'_>` を満たすので既存呼び出し側は無変更」と明記しており、**この変換は後方互換**。

`idempotency::bind`（`src/statuses/idempotency.rs:136`）はレポートが挙げていたが、`create_status` の該当範囲（`:808-853`）内には現れない。実際の呼び出し位置を実装時に確認し、トランザクション内に含めるべきかを判断する。

### A-5：3 重化した配線と、**既に発生している食い違い**

11 段階の配線を 3 箇所が独立実装：`src/bootstrap.rs:337-710`（`build_state`）/ `src/test_harness.rs:513-885`（`spawn_test_app`）/ `src/federation/test_harness.rs:247-600`（`spawn_paired_instance`）。

**最大の発見：`src/federation/test_harness.rs` は `statuses::register_account_ports` を呼んでいない。**
`grep -rn "register_account_ports"` の呼び出し箇所は `src/bootstrap.rs:584` と `src/test_harness.rs:792` の **2 箇所のみ**。`spawn_paired_instance` では `statuses_module`（`:507`）の直後に `social_graph_module`（`:520`）が来ており、その間の登録が欠落している。

**帰結**：連合ペアテストの各インスタンスでは、`accounts_module` が組み込み既定の `EmptyStatusesProvider` / `ZeroCountsProvider` を保持したままになる。さらに `social_graph::build_social_graph_module` の `CombinedAccountCountsProvider` は、statuses-core の実装ではなく**この既定値と合成**される。つまり連合ペアテストの Account JSON は本番と異なる（statuses カウントが 0、statuses プロバイダが空）。

これは A-5 が「順序を入れ替えてもコンパイルは通り、テストが偶然通れば気付かれない」と予測した事象が、**順序ではなく段階の欠落という形で実際に起きていた**ケースである。一本化すればこの差異は自動的に解消されるが、それは連合ペアテストの観測結果を変える（Requirement 7.7 が要求する記録対象）。

**3 経路の実際の差分**（一本化後に各経路へ残すべきもの）：

| 段階 | bootstrap | test_harness | federation/test_harness |
|---|---|---|---|
| config | `config::load_config()` | テスト用 `Config` | ペア用 `Config` |
| pool | 本番プール + マイグレーション | スキーマ隔離プール | スキーマ隔離プール |
| runtime / actor | `build_actor_wiring`（`RuntimeContext::production`） | `RuntimeContext::deterministic` + `keys` 差し替え | 同左 |
| HTTP クライアント | `ReqwestFederationHttpClient::new()` を **4 箇所で個別に生成** | 同左（4 箇所） | **caller 供給の `insecure_loopback` を 1 つ共有** |
| `OauthModule::new` の末尾フラグ | `false` | `false` | `false`（差異なし） |
| `media_background.spawn` の signal | `server::os_shutdown_signal` | `std::future::pending::<()>` | `std::future::pending::<()>` |
| `federation_background.spawn()` | あり | あり | あり（差異なし） |
| `register_account_ports` | **あり** | **あり** | **欠落（上記）** |
| `AppState::new` の 13 引数 | 同一 | 同一 | 同一 |
| 後段 | 実リスナー + `serve_with_shutdown` | エフェメラルバインド + `TestApp` | エフェメラルバインド + `TestApp` |

→ 一本化の入力は `pool` / `runtime` / `actor_module` / `config` / `http_client` / `media_shutdown_signal`。出力は 9 モジュール + 2 つの background ハンドル。config の作り方・リスナー bind・shutdown 信号の扱いは各経路に残る（Requirement 7.5）。

`FederationWiringConfig`（`src/federation/module.rs:203`）が既に「差分だけをパラメータ化する」形を実践しており、これを配線全体に広げる。

## Design Decisions

### 1. Generalization

- **`AppError` のドメイン識別（A-4）は、投票専用の仕組みにしない。** 「特定の理由での失敗か」を型で問える手段は、A-4 以外でも将来必要になるクラスの問題。ただし実装スコープは現在の要件が要求する範囲（投票の重複 1 種類）に留め、**インターフェースだけを一般化する**（`ErrorTag` を持てる `AppError`）。ドメインごとの enum バリアント追加は行わない — `AppError` は横断型であり、そこに個別ドメインの語彙を積むと横断型でなくなる。
- **`StatusRenderAssembler`（A-1）と一括取得（A-2）は同一コンポーネント。** 「1 件の組み立て」と「N 件の組み立て」を別コンポーネントに分けると、また 2 つの実装が乖離する。1 件は N=1 の特殊ケースとして同じ経路を通す。

### 2. Build vs. Adopt

- **一括 IN 検索は自作しない**：`social_graph/repository.rs::load_states` の `UNNEST` 並列配列バインドと `emoji_repository::resolve_emojis` の `= ANY($1)` が既にこのリポジトリの確立パターン。新しい抽象を導入せず、同じ SQL イディオムを対象リポジトリへ横展開する。
- **エグゼキュータ・ジェネリックは自作しない**：`sqlx::PgExecutor<'e>` が標準の解。`social_graph/repository.rs` が既に採用しており、`&PgPool` が同トレイトを満たすため既存呼び出し側は無変更で済む（同ファイル doc で明記済み）。
- **トランザクション管理ライブラリを導入しない**：`pool.begin()` / `tx.commit()` の素の sqlx で足りる。`status_repository::delete_status` に先例がある。

### 3. Simplification

- **ブースト先の解決と可視性判定を `StatusRenderAssembler` に取り込まない。** 5 経路で協調オブジェクトが本質的に異なるため、取り込めば「どの経路か」の暗黙分岐がアセンブラ内部に生まれ、除去した複製を分岐として再導入することになる。アセンブラは**解決済みのブースト先**を受け取る。
- **`interaction_state` の viewer 型を統一する（`Option<Id>`）。** notifications だけが `Id` を取るが、`Some(viewer)` を渡せば挙動は同一（`notifications/service.rs:292-296` は viewer 有り前提の直列 4 クエリで、`Option` 版の `Some` 分岐と同じ）。型の差異は呼び出し規約の差であってふるまいの差ではない。
- **`compose_modules` に trait 抽象を被せない。** 起動時に 1 回だけ選ぶ配線であり、差分は具体値（config・クライアント・シャットダウン信号）に過ぎない。`search/ports.rs:160-164` が明文化している判断基準（差し替えが要る → boxed future、起動時 1 回 → generic/具体値）に従い、**プレーンな関数 + 入力構造体**にする。
- **`PollResolver` は boxed future の `dyn` トレイトにする。** 呼び出し側ごとに実装が異なり、かつ `StatusesEndpointsState` は既にジェネリック引数 6 個を抱えている（B-11）。ここでジェネリックを足すのは既知の負債を悪化させる。

## Risks

| リスク | 影響 | 緩和 |
|---|---|---|
| A-1 の集約で 5 経路のいずれかの差異を潰す | Requirement 1 違反（振る舞い変更） | 上表の差異を設計に明記し、`PollResolver` と `muted` 注入で保存。各経路の既存テストを段階ごとにグリーン維持 |
| A-2 のバッチ化で並び順や欠損時の縮退が変わる | 一覧のレスポンスが変わる | `media_json` の「解決できない media id は黙って省く」規約と `poll` の縮退規約を一括版でも保存。並び順は入力 `&[Status]` の順を維持 |
| A-3 のトランザクション化で配送をトランザクション内に含めてしまう | 接続枯渇・配送失敗による正常書き込みの巻き戻り | 「commit 後に配送」を設計の Boundary Commitment として明記 |
| A-5 の一本化で連合ペアテストの期待値が変わる | 既存テストの失敗 | 既知の差異（`register_account_ports` 欠落）として事前に記録済み。失敗したテストは「これまで誤った前提でグリーンだった」ものとして個別に正当性を示す（Requirement 1.4 / 7.7） |
| HTTP クライアントの共有化（4 インスタンス → 1） | 接続プールの共有 | `federation/test_harness.rs` が既に 1 インスタンス共有で動作しており実証済み。送出される HTTP リクエスト自体は不変 |
| 5 項目を一度に進めて回帰の原因切り分けが困難になる | デバッグコスト | 項目ごとに独立したタスク群とし、各項目の完了時点でフルスイートをグリーンにする（Requirement 1.6） |

## Open Questions

- `idempotency::bind` の実際の呼び出し位置が `create_status` のトランザクション境界に含まれるべきか（実装時に該当箇所を確認して判断）。
- `map_query_error` / `map_insert_error` / `map_tx_error`（`map_server_error` 以外の 15 件）が `map_server_error` と同一の `AppError` を返すか。同一でないものは B-6 の集約対象から外す。
