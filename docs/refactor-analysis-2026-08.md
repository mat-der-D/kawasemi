> [!IMPORTANT]
> **これは一時的な作業成果物であり、`docs/` の他ファイルと違って設計の一次情報ではない。**
>
> - **性質**：2026-08-04 時点のコードベースに対する分析ログ。指摘が解消されるほど内容は実態から乖離する。
>   現在のコードの説明として読んではならない（`.kiro/steering/structure.md` の「SSoT（一次情報）の所在」と同じ扱い）。
> - **役割**：リファクタ spec の入力。要件・設計を起こす材料であって、それ自体が仕様ではない。
> - **削除条件**：リファクタ実装が完了し `/kiro-validate-impl` が GO を返した時点で、
>   このファイルを**削除する**。成果は steering（コードから同期）とコードそのものに残る。
>   削除は handoff 作業の一部。

---

# kawasemi Phase 1 リファクタリング分析

7 領域を並列監査（リポジトリ/SQL・エンドポイント・DI/配線・ドメイン/シリアライザ・連合・テスト基盤・ドキュメント/命名）。
全指摘は実コードの file:line 検証済み。主要な定量値は本文中で独立に再検証した。

## 全体像

| 指標 | 値 |
|---|---|
| non-test src | 66,309 行（うちコメント **30,593 行 = 46%**、実コードは約 31,400 行） |
| テストコード | src 内 51,779 行 + `tests/` 47,397 行（84 ファイル）= 約 99,000 行（**実コード比 3:1**） |
| `cargo clippy`（既定） | **警告ゼロ** |
| `clippy::pedantic + nursery` | 5,034 件（うち doc 系 `too_long_first_doc_paragraph` 1,268 + `doc_markdown` 1,113 = **47%**） |
| task/Requirement 番号のコメント埋め込み | 4,356 件 / 293 ファイル（実装ファイルの **97.7%**） |
| 本番モジュール依存グラフ | ほぼクリーンな DAG（例外 2 件、後述） |

**結論**: コードの「正しさ」と「規約の一貫性」はむしろ良好（既定 clippy クリーン、デッドコードほぼ皆無、steering の drift ほぼ無し、連合コア設計は原則遵守）。
負債は一点に集中している — **「組み立て（assembly）コードが spec 境界ごとにコピーされた」**。Status の組み立て、モジュールの組み立て、テストの組み立て、Activity の組み立て、レスポンスの組み立て、そのすべてが N 重化している。

---

## A. 最優先（実害あり）

### A-1. Status レンダリンググルーが 5 モジュールに複製（約 750 行）★独立検証済み
`media_json` / `tags_json` / `resolve_emojis` / `interaction_state` / `poll_json` / `resolve_common` / `leaf_render_input` の一式が、以下 5 箇所に存在する。

- `src/statuses/endpoints.rs:410-598`
- `src/statuses/account_provider.rs:276-496`
- `src/notifications/service.rs:256-510`
- `src/timelines/hydrator.rs:340-519`
- `src/search/hydrator.rs:415-593`

`tags_json` の本体は 5 箇所でバイト単位に同一:
```
format!("{}://{}/tags/{}", origin.scheme, origin.host, tag.name)
```
→ `statuses/endpoints.rs:437`, `account_provider.rs:303`, `notifications/service.rs:283`, `timelines/hydrator.rs:428`, `search/hydrator.rs:499`

**すでに実装者自身が気づいている**: `src/search/hydrator.rs:59-74` に「これで 4 番目のモジュールがこのグルーを複製している。共有ヘルパーを切り出す潮時かもしれない」と CONCERN が書き残されたまま未着手。

実際に差分も発生済み: `muted` は `timelines/hydrator.rs` のみ `FilterContext` から解決し、他 4 箇所は `false` 固定。意図的な差分だが、分散しているため「意図的か直し忘れか」がコード上で判別できない。

**提案（L）**: `pool` / `accounts` / `media_store` を保持する `StatusRenderAssembler` に集約。`muted` 解決のみ `Option<&HashSet<Id>>` で注入可能にする。5 箇所は保持・呼び出しのみに縮退。

### A-2. 同じ hydrator 群が N+1 クエリ源
`src/timelines/hydrator.rs:265-276` の `hydrate()` は 1 ページ分を `for ... await` で**逐次**処理。1 投稿あたり:
- `show_account`（`accounts/account_service.rs:562-609`）— followers/following/statuses カウント等で複数クエリ。**同一著者でもキャッシュされず毎回再解決**
- `media_json`（`hydrator.rs:399-415`）— `media_id` ごとに `find_by_id` を 1 件ずつ（N+1 の中の N+1）
- `interaction_state`（`hydrator.rs:460-487`）— `exists_favourite` / `exists_bookmark` / `exists_pin` / `find_reblog` を**個別 4 クエリ**
- ブーストがあれば対象投稿にも同一処理が再帰（`hydrator.rs:287-301`）

→ 20〜40 件のページで **1 投稿あたり約 10 クエリ**。低スペック VPS 運用（product.md の想定）で最初に問題化する箇所。

**提案（M〜L）**: `social_graph/repository.rs::load_states` が既に採用している「対象配列を `UNNEST` で一括 IN 検索」パターンを横展開。①`show_accounts(ids)` バッチ版、②`find_by_ids` で `WHERE id = ANY($1)`、③`favourited_status_ids(viewer, &status_ids)` 等の集合返却。A-1 の集約と同時にやるのが効率的。

### A-3. サービス層の複合書き込みが非トランザクション → カウンタ不整合
- `StatusService::create_status`（`src/statuses/status_service.rs:808-853`）: `insert_status` → `attach_media` → `insert_poll` → 親の `adjust_counts(replies+1)` → `persist_tags` → `idempotency::bind` が**すべて個別の `&self.pool` 呼び出し**。途中失敗でメディア無し／タグ無し／親カウント未更新の投稿が残る
- `InteractionService::reblog`（`interaction_service.rs:382-383`）、`favourite`（`:460-462`）、`unfavourite`（`:508`）も「レコード追加」と「カウンタ更新」が別呼び出し

対照的に `status_repository::delete_status`（`:571-635`）と `apply_edit`（`:649-692`）は**自前でトランザクションを張っており正しい**。リポジトリ層には atomicity の意識があるのに、サービス層の複合操作にだけ及んでいない。

**提案（M）**: `social_graph/repository.rs` が既に確立した `<'e, E: sqlx::PgExecutor<'e>>` ジェネリック方式に書き込み関数を揃え、サービス側を `pool.begin()` で包む。パターンは既にリポジトリ内にあるので流用可能。

### A-4. 文字列リテラル一致がエラー制御フローになっている
`src/statuses/inbound_handlers.rs:1067-1074`:
```rust
Err(err) if err.status == StatusCode::UNPROCESSABLE_ENTITY
         && err.public_message == "actor has already voted in this poll" => {
    Ok(Some(HandleOutcome::Handled))
}
```
この文字列の出所は 700 行以上離れた `src/statuses/poll_repository.rs:295`。`AppError`（`src/error.rs:83-95`）が `kind`/`status`/`public_message` の平坦構造でドメイン固有バリアントを持たないため、「この理由での失敗か」を区別する唯一の手段が文字列一致になっている。

**壊れ方**: 文言を変更（typo 修正・文言統一）すると一致が静かに外れ、「ローカル投稿主への Activity がループバックした」という**正常なべき等ケース**が投票者への**スプリアス 422** として跳ね返る。コンパイラは検証しない。

**提案（S〜M）**: `AppError` に `kind_detail: Option<&'static str>` を追加、または `record_vote` の戻り値をドメイン enum に拡張。既存コンストラクタ不変の追加のみで済む。

### A-5. モジュール配線シーケンスが 3 重化（実コード一致率 79%）
`src/bootstrap.rs:337-710`（`build_state`）/ `src/test_harness.rs:513-885`（`spawn_test_app`）/ `src/federation/test_harness.rs:247-600`（`spawn_paired_instance`）が、`build_actor_wiring` → `OauthModule::new` → `build_federation_module` → … → `build_search_module` という**同一の 11 段階**を、同一引数順・同一の暗黙的順序制約付きで独立実装。

`src/test_harness.rs:1-25` は「bootstrap と同じ構成要素を再利用する、reimplement せず」と明言しているが、実際には `build_state` 自体は再利用されず中身の呼び出し列だけがコピーされている。

**さらに危険なのが順序制約**: `src/bootstrap.rs:573-613` のコメントは「`statuses::register_account_ports` は accounts/media/statuses モジュールより後」「`social_graph::build_social_graph_module` はさらにその後（そうすれば自身の `AccountCountsProvider` 登録が最後になり実際に有効になる）」と説明する。つまり**複数箇所が同じレジストリスロットに `set_*` し、最後に呼ばれたものが勝つ**というセマンティクスが、型では一切強制されずコメントの中にしか存在しない。

順序を入れ替えても**コンパイルは通り、テストが偶然通れば気付かれない**（意図した `CombinedAccountCountsProvider` ではなく単体実装が静かに使われる）。これを 3 箇所で独立に守る必要がある。

**提案（L）**: 配線シーケンス本体を `compose_modules(...) -> AllModules` に切り出し、3 呼び出し元には固有部分（config の作り方・リスナー bind・shutdown signal）だけを残す。`FederationWiringConfig`（`bootstrap.rs:466`）が既に「差分だけパラメータ化する」設計を実践しているので発想を全体に広げるだけ。併せて「最後勝ち」の合成が実際に効いているかを直接アサートする統合テストを追加（現状これを検証するテストが無い）。

---

## B. 次点（保守性）

### B-1. テストフィクスチャが 84 ファイルにコピペ（約 2,000〜3,000 行）★独立検証済み
| ヘルパー | 再定義ファイル数 |
|---|---|
| `insert_actor_fixture` | 31 |
| `parse_response` | 24 |
| `body_json` | 21 |
| `issue_token` 系（`issue_test_token` / `issue_write_media_token` 含む） | 42 |
| `assert_error_shape` | 11 |
| `create_owner_with_actor` | 9 |

`tests/interactions_it.rs` と `tests/status_crud_it.rs` の `insert_actor_fixture` は `display_name` の文字列以外完全一致。

**提案（M）**: `tests/support/mod.rs`（`*_it.rs` に一致しないので新規テストバイナリと誤認されない）に集約。あるいは B-4 の `testing` feature 配下に `kawasemi::test_fixtures` として置く方が `TestApp` 内部への依存を保守しやすい。

### B-2. `spawn_test_app()` が 1,182 回 — 単体/統合/契約の 3 層が実質 1 層に収束 ★独立検証済み
`src/*/tests.rs` で 682 回、`tests/*.rs` で 500 回。`src/statuses/endpoints/tests.rs` は 18/18、`inbound_handlers/tests.rs` は 27/27、`status_service/tests.rs` は 26/26 が全て実 DB フル起動。

`spawn_test_app` はスキーマ作成 → migration → 13 モジュール wiring → 実 TCP リスナー起動という重量級処理。401/403/422 のような純粋な入力バリデーションまで実 DB を通している。同一挙動が単体・統合・契約の 3 層で重複検証されている例もある（`create_status` の 401/403/422）。

**提案（M）**: 3 層の責務を tech.md に明文化し、DB 分岐が不要なテストをモック化または統合層へ一本化。

### B-3. golden 契約テストが浸透していない ★独立検証済み
`assert_golden` を使うのは 84 ファイル中 **6 ファイル**のみ。golden ファイルは 36 個あるが、ad-hoc な `assert_eq!(body["..."], ...)` が 158 箇所。tech.md が掲げる「契約：エンティティの JSON 形をゴールデンで先に固定」という思想がリポジトリ全体には届いていない。

**提案（S〜M）**: エンティティ全体を返すテストを golden 化候補として棚卸し。新規は `assert_golden` 既定とするガイドラインを明記。

### B-4. テストハーネスが本番バイナリに同梱
`src/lib.rs:41` `pub mod test_harness;`、`src/federation.rs:148,181`（`FederationPair` を公開 API として再輸出）、`src/contract.rs` いずれも `#[cfg(test)]` なし。`Cargo.toml` に `[features]` セクション自体が存在しない。

結果として release バイナリに `TEST_KEK = [0x42; 32]` / `TEST_OWNER_PASSWORD = "test-harness-owner-passphrase"` / `DEFAULT_TEST_DB_URL`（`src/test_harness.rs:121-155`）が文字列として埋め込まれる。

**依存グラフへの副作用**: `federation → {statuses, social_graph, search, timelines, notifications, accounts, server, state}` という広範なエッジは**すべて `src/federation/test_harness.rs:90-115` 由来**。これを除けば本番依存グラフはクリーンな DAG になる。

**提案（M）**: `testing` feature を追加し `#[cfg(any(test, feature = "testing"))]` でゲート。自己参照 dev-dependency（`[dev-dependencies] kawasemi = { path = ".", features = ["testing"] }`）を使えば `cargo test` / `cargo build` のコマンドは変えずに済む。CI 設定ファイルが無いため影響範囲は `Cargo.toml` + mod 宣言 2 箇所のみ。

### B-5. 孤立スキーマ回収（既知課題）は見た目ほど大きくない
`Drop for TestApp`（`src/test_harness.rs:445-489`）は Tokio runtime handle が取れた場合のみ detached task で `drop_schema` を投げ、取れなければ eprintln のみ。スキーマ名は `kawasemi_test_harness_<nanos>_<seq>` / `kawasemi_federation_pair_<nanos>_<seq>` とナノ秒を含む規則的な命名なので「古いものだけ安全に消す」判定が容易。

**提案（M、60〜100 行）**: `spawn_test_app` 先頭で `OnceCell` により 1 プロセス 1 回だけ、①`information_schema.schemata` を prefix で検索 → ②名前のタイムスタンプが閾値（例 2 時間）より古いもののみ `DROP SCHEMA IF EXISTS ... CASCADE`。冪等なので並列プロセスでも安全。既存の `create_schema`/`drop_schema`（`:249-288`）を流用できる。

### B-6. エンドポイント定型文の重複
- `parse_optional_limit`: バイト単位で同一のものが 5 箇所（`accounts/endpoints.rs:568`, `social_graph/endpoints.rs:369`, `timelines/endpoints.rs:212`, `notifications/endpoints.rs:364`, `statuses/endpoints.rs:1205`）
- Link ヘッダー組み立て（`RequestUriContext::new` → `with_query` → `build_link_header` → `insert`）が 7 箇所（`accounts:640`, `statuses:1258`, `notifications:467`, `social_graph:536`, `timelines:304/387/499`）
- `parse_loose_bool` / `parse_optional_bool_query` が accounts と timelines に完全複製
- `ForwardedOrigin::resolve("https", &self.domain, None, None)` が 5 箇所（`social_graph/follow_request_service.rs:221`, `search/hydrator.rs:278`, `notifications/service.rs:244`, `search/tag_serializer.rs:88`, `statuses/account_provider.rs:220`）★独立検証済み
- `format_time`（RFC3339）が 4 箇所（`accounts/serializer.rs:292`, `statuses/serializer.rs:358`, `notifications/serializer.rs:185`, `statuses/endpoints.rs:322`）
- `map_server_error`（`sqlx::Error → AppError`）が約 26 ファイル

いずれも `src/api/` 配下へ寄せるだけの **S サイズ**。`api::pagination` / `api::error` / `api::ratelimit` / `oauth::middleware` という一元化の受け皿は既に良く出来ているので、「その道具を呼ぶ最後の定型文」だけを追加で寄せればよい。

### B-7. Activity 組み立てヘルパーの二重実装
- `mint_activity_id` と `const ACTIVITY_OBJECT_KIND` が `statuses/activity_builder.rs:351,234` と `social_graph/activity_builder.rs:319,192` に同一実装
- Activity エンベロープ（`id`/`type`/`actor`/`published`/`to`/`cc`）の手書き組み立てが `statuses/activity_builder.rs` に 7 回（`:453,480,509,549,576,624,674`）、`social_graph/activity_builder.rs` に 4 回（`:356,416,440,480`）— 計 11 箇所
- `activity_map` / `malformed()` が `statuses/inbound_handlers.rs:542,620` と `social_graph/inbound.rs:278,269` に同一（後者の doc が「mirrors ... identical defensive helper」と自認）
- `ProdRemoteActorResolver`（`src/statuses.rs:357-420`）と `ProdActorUriResolver`（`src/social_graph/inbound.rs:314-383`）の `local_handle` が一字一句同一（doc が「verbatim copy」と自認）
- signature suite 選択 `match` が `signatures/signer.rs:351` と `verifier.rs:346` に重複（ただし**署名対象文字列の構築自体は `SignatureSuite::build_signing_input` で正しく一本化**されている）

**提案（S〜M）**: `federation` 側に共通ヘルパー（`inbound/support.rs`、`ActorUrls::mint_activity_id`、`SignatureFormat::suite()`）を置く。3 つ目の spec が来ると三重化する見込みが高いので今のうちが安い。

### B-8. レジストリパターンが 7 回独立実装
`Arc<RwLock<Arc<dyn Trait>>>`（単一スロット）が 5 つ — `AccountPortsRegistry`（`accounts/ports.rs:264`）、`NotificationPortsRegistry`（`notifications/ports.rs:134`）、`NotificationSinkRegistry`（`statuses/notification_sink.rs:142`）、`RelationshipQueryRegistry`（`statuses/visibility.rs:263`）、`BlockPolicyRegistry`（`federation/inbound/block_policy.rs:168`）。
`Arc<RwLock<Vec<Arc<dyn Trait>>>>`（ファンアウト）が 2 つ — `ObjectDocumentRegistry` / `OutboxSourceRegistry`（`federation/endpoints/document.rs:184,267`）。

すべてが「`write().expect("...must not be poisoned")` で差し替え、`read()` して Arc を clone しガードを解放してから await」という**微妙な安全性判断まで手で複製**している。`statuses/visibility.rs:257-259` の doc が「`BlockPolicyRegistry`/`NotificationSinkRegistry`/`AccountPortsRegistry` と同じ形」と自認しており、これ自体が抽象化すべき合図。

**提案（M）**: `ReplaceableSlot<T: ?Sized>` と `FanoutRegistry<T: ?Sized>` に統合。ドメイン固有のメソッド名（`emit`/`deliver`/`counts`）は各モジュールの薄いラッパーに残す。

### B-9. `src/server.rs`（1,091 行）の構造的肥大
全 feature モジュールが `pub fn router()` を持たない規約が意図的に貫かれた結果、`server.rs` が 13 個の `FromRef`（`:193-437`）、7 個のモジュール別 router 関数、全パス定数（`:102-174`）、`.merge()` チェーン（`:816-863`）を一手に引き受けている。エンドポイントを 1 つ追加するのに **5 箇所**の編集が必要。

**提案（L）**: 各モジュールに `pub(crate) fn router() -> Router<AppState>` と `FromRef` を移し、`server.rs` は薄い合成のみに。

### B-10. `src/statuses/inbound_handlers.rs`（1,682 行）の分割
実コードは 410 行目以降（1-409 行はドキュメント）。410-970 が共有ヘルパー、973-1682 が 6 つの独立した `InboundActivityHandler` 実装。god-file ではなく各ハンドラは自己完結しているが、単一ハンドラの修正でも 1,600 行超を再コンパイルすることになる。

**提案（M、低リスク）**: `inbound_handlers/{shared, create_note, announce, like, delete, update, undo}.rs` に純粋分割。

### B-11. ジェネリック引数爆発と、同一問題に対する解の二重化
`StatusesEndpointsState<A,D,L,H,R,M>`（6 個）、`SocialGraphEndpointsState<AL,AR,D,LS,HS>`（5 個）、`SearchEndpointsState<B,H,R,M>`（4 個）が存在するが、`server.rs:350-354,419-422,655-660` の型エイリアスが示す通り**起動時に 1 つの具体型にしか単相化されず、実行時の柔軟性は使われていない**。`statuses/endpoints.rs` は 18 関数すべてに同一の 6 行 `where` 節を反復。

一方、同じ「async trait が dyn 非互換」という問題に対し `accounts/ports.rs`・`notifications/ports.rs`・`federation/endpoints/document.rs` 等は boxed future 方式で `Arc<dyn Trait>` にし、ジェネリックをゼロにしている。

**提案（M）**: 既存の書き換えは急がないが、「AppState 構築後に差し替えが要る → boxed future、起動時 1 回選ぶだけ → generic」という判断基準（`search/ports.rs:160-164` が既に明文化している）を `structure.md` に一文追記し、今後のブレを止める。

---

## C. ドキュメント/命名（量は最大、実害は小）

### C-1. コメント 46%・task 番号 4,356 箇所
- `src/oauth/service.rs`（536 行）: **冒頭 152 行が連続する `//!`**（最初の `use` は 153 行目）。`##` 見出し 6 個のエッセイ
- `src/federation.rs`（182 行）: **140 行が `//!`**（77%）で実体は 8 行の `pub mod`/`pub use`
- pedantic clippy 5,034 件のうち doc 系が 2,381 件（47%）

スポットチェック 6 件中 5 件は `.kiro/specs/` と整合しており「嘘」ではない。ただし **rot の実例が 1 件確認された**: `src/media.rs:120-121` が「task 5.2 が対処すべき CONCERN」と未来形で書く一方、`src/media.rs:142` は「task 5.1 が文書化した CONCERN を解決済み」と書いており、同一ファイル内で矛盾したまま放置されている。タスク単位で追記し前のブロックを更新しない方式は、単一ファイル内でさえ陳腐化する。

**提案（M〜L、2 段階）**: ①新規コードでは task/Requirement 番号をモジュール doc の先頭要約のみに限定し関数単位の反復埋め込みをやめる。②将来的にトレーサビリティ表（spec 側の逆引き）へ委譲し、コードは「何を・なぜ」に絞る。

### C-2. `TODO`/`FIXME` はゼロ、代わりに `CONCERN` が 109 件
標準的な TODO/FIXME/HACK/XXX は src 全体で **0 件**。代わりに `CONCERN`（実装者がレビュー確認待ちとして明記した判断）が 109 件で事実上の負債トラッカーになっている。集中箇所は `oauth/`（15 件超）、`search/`（13 件超）、`statuses/`、`notifications/`、`social_graph/`。

また「このタスクの境界外だから触れない」「task 7.3 まで main.rs 内だった」式の歴史的記述が 162 件。これは git blame / commit message が担うべき情報。

**提案（M）**: 109 件の CONCERN を「恒久的な設計トレードオフ（残す）」と「一時的な作業分担事情（削除）」に仕分け。A-1 の CONCERN のように**実際に着手すべき負債が埋もれている**のが最大の損失。

### C-3. 命名の不整合
| パターン | 多数派 | 少数派 | 判定 |
|---|---|---|---|
| エンドポイント集約 | `endpoints.rs` 7 件 | `endpoint.rs` 1 件（search） | 真の不整合（S でリネーム可） |
| Module 構造体名 | 9/10 がモジュール名と一致 | `NotificationModule`/`build_notification_module`（**単数形**、ファイルは `notifications.rs`） | 真の不整合（S） |
| `*_service.rs` vs `service.rs` | prefixed 11 / bare 8 | — | **実質一貫**（同一ディレクトリに 1 個なら bare、複数なら prefixed。混在ディレクトリ 0 件） |
| `*_repository.rs` vs `repository.rs` | prefixed 14 / bare 4 | — | **同上、問題なし** |
| Port trait 接尾辞 | `*Provider`/`*Sink`/`*Backend`/`*Store`/`*Policy`/`*Query`/`*Source` の 7 種 | — | 命名から dyn 差し替え可否が判別できない。structure.md にガイド追記（S） |

### C-4. `#[allow(...)]` 34 件 / steering drift
- `clippy::too_many_arguments` 19 件（`accounts/serializer.rs:530,635,664` の 3 件はビルダー化の余地あり、他は DI コンストラクタで妥当）
- `async_fn_in_trait` 13 件（言語制約への妥当な対処。ただし同一の正当化文が doc コメント 26 箇所で反復説明されており、これ自体がドキュメント重複）
- `dead_code` 2 件（両方とも理由付きで正当）

steering の構造的主張（`mod.rs` 不使用、`ports.rs` 規約とその例外、統合テスト 84/84 が `_it.rs`）は**すべて実コードと整合、drift なし**。唯一のグレーゾーンは `tech.md` の「Key Technical Decisions」が Streaming API / Web Push / 内蔵 ACME / rust-embed を確定事項として書いているが、これらは Cargo.toml・src に一切存在しない点（`roadmap.md` では Phase 2〜4 として未着手と明記されており矛盾はないが、tech.md 単体だと実装済みと誤読され得る）。**提案（S）**: 該当箇条書きに「(Phase 2/4 予定・未実装)」を追記。

### C-5. 依存グラフ上の唯一の真の循環
`src/error.rs:190` が `crate::api::error::mastodon_error_body` を呼ぶ一方、`src/api/{pagination,error,ratelimit}.rs` は `crate::error` に依存 → **`error ↔ api` の循環**。小さいが、`error.rs` と `api/error.rs` の統合か trait による反転で解消できる（S）。
それ以外の feature モジュール間循環は B-4 の通りすべて `federation/test_harness.rs` 由来。

---

## D. 良好で触るべきでない箇所

- **横断機構の一元化**: `api::pagination`（`PageParams`/`Cursor`/`paginate`/`build_link_header`）、`api::error`、`api::ratelimit`、`oauth::middleware`（`OptionalActor`/`RequiredActor`/`require_scope`）は全モジュールから一貫して再利用され、エラー → HTTP 変換は `AppError::into_response_with`（`error.rs:183-191`）で完全に一元化。**模範パターン**
- **ハンドラ引数順序**（`State` → `RequiredActor`/`OptionalActor` → `ResolvedOrigin` → `Path` → `Query`/`Body`）が全モジュールで一貫
- **連合コア設計**: `DeliveryService::deliver` での物理配送分岐の一本化、`InboxService::process_inbound`/`process_local` の `process_verified` への収斂、単一の `InboundActivityDispatcher` マルチマップ。steering の「ローカル/リモート単一経路」原則に忠実で、**Activity 構築・可視性・状態遷移レベルでのローカル特殊化の漏れは発見されなかった**
- **contract ハーネス**: `src/contract.rs` は完全にジェネリックな golden 比較機構で、型定義を持たない。golden は実サーバー経由の実出力から生成されており手書き JSON との照合ではない（当初懸念した「パラレル型定義層への劣化」は該当せず）
- **エンティティ JSON 契約定義そのもの**（`AccountJson`/`StatusJson`/`NotificationJson`）は各 1 箇所に単一定義。問題は定義ではなく「組み立てグルー」側
- **ページネーション規約**、**DB 由来 enum のパース時 panic 規約**、**論理 FK 方針**、**モジュールバンドル（`XModule`）のボイラープレート**（アクセサ 2〜4 行のみで抽象化する共通ロジックが無い）
- **`AppState::new` の 13 引数**: `src/state.rs:138-153` の正当化は妥当。ただし「lint を抑制したから設計は完了」ではなく、真のコストは A-5 の**組み立て側**にある

---

## E. 推奨着手順

| 順 | 項目 | 規模 | 理由 |
|---|---|---|---|
| 1 | A-4 文字列エラー一致の解消 | S〜M | 単独で完結、静かに壊れるバグの芽 |
| 2 | B-6 エンドポイント定型文の `api/` 集約 | S | 受け皿が既にある、低リスク、即効 |
| 3 | A-1 + A-2 `StatusRenderAssembler` 集約＋N+1 バッチ化 | L | **最大の負債。同時にやるのが効率的** |
| 4 | A-3 サービス層トランザクション化 | M | パターンはリポジトリ内に既存 |
| 5 | A-5 `compose_modules` 抽出＋順序の検証テスト | L | 次の spec を足す前にやるべき |
| 6 | B-1 テストフィクスチャ集約 + B-4 `testing` feature | M | Phase 2 のテスト量増加前に |
| 7 | B-7 / B-8 連合ヘルパー・レジストリ統合 | S〜M | 3 つ目の spec が来る前が安い |
| 8 | C-2 CONCERN 109 件の棚卸し | S | 実着手すべき負債が埋もれている |
| 9 | B-9 / B-10 ファイル分割 | M〜L | 純粋分割、いつでも可 |
| 10 | C-1 ドキュメント圧縮 | L | 量は最大だが実害は最小。他が済んでから |

**Phase 2 に進む前にやるべき最小セット**: 1・2・3・4・5。
とくに A-5 は「次の spec がまた 3 箇所に配線をコピーする」構造なので、Phase 2 着手前が最も安い。
