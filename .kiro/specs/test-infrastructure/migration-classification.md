# TestDb 移行対象の分類（タスク 3.2 の成果物）

対象: `src/` 配下の lib テストのうち、`spawn_test_app` で実インスタンスを起動しているもの。
判別基準は design の TestDb「使い分けの基準」に機械的に従う。

- **非対象（`TestApp` に残す）**: HTTP リクエストの送出 / ルーター / OAuth トークン /
  `AppState` 経由のモジュール参照 / `app.actor` のいずれかを使う
- **対象（`TestDb` へ移す）**: 上記のいずれも使わず、`app.pool` と `app.runtime`
  （`clock` / `ids` / `rng`）だけで SQL とドメイン型に閉じている

タスク 3.3 / 3.4 / 3.5 は下表の自グループの行をそのまま拾えばよい。

---

## 1. 総計と突き合わせ（完了状態）

| 区分 | 機械判定 | 実行可能値（§5 の除外を反映） |
|------|------|------|
| 移行対象（`TestDb`） | **540** | **539** |
| 非対象（`TestApp` のまま） | **205** | **206** |
| 合計 = 実インスタンス起動を要する lib テスト | **745** | **745** |

差の 1 件は `src/test_harness/tests.rs` のハーネス自己検証テスト。機械判定では移行対象に
出るが移行してはならない（§5 参照）。**3.3–3.5 が実際に動かすのは 539 件**。

### 745 と design の「750 件」の突き合わせ

design / requirements の 750 は **`spawn_test_app(` というテキストの出現回数**であり、
テスト関数の件数ではない。その値は現在も変わっていない。

```
$ git grep -oh "spawn_test_app(" -- src/ | wc -l
750          # design 記載値。b50d8fd 時点でも d1f3ed9 時点でも 750
```

そこから実件数への内訳は次のとおり。**差分はすべて説明可能で、テストの増減ではない。**

| 段 | 件数 | 内訳 |
|----|------|------|
| `spawn_test_app(` の生の出現回数（コメント込み） | 750 | design 記載のベースライン |
| − doc コメント中の言及 | −3 | 747 |
| − 定義そのもの（`src/test_harness.rs` の `pub async fn spawn_test_app(`） | −1 | 746 = 実コード上の呼び出し箇所 |
| → 呼び出しを含むテスト関数 | **745** | 1 関数が 2 回呼ぶ（`test_harness/tests.rs::spawn_test_app_isolates_database_state_between_instances`）ため 746 箇所 = 745 関数 |

- ヘルパー関数の中から `spawn_test_app` を呼ぶ箇所は **0**。すべてテスト関数の直下にある。
- したがって **本ドキュメントの計数単位は「`#[tokio::test]` 関数」で 745 件**、
  design の 750 は「テキスト出現回数」であり、両者は同じ実体の別の数え方である。

参考: 非コメントの `#[test]` / `#[tokio::test]` 属性は現在 **1,779** 個
（design 記載の lib テスト 1,764 件に対し、タスク 1.1–3.1 で追加されたぶん +15）。
745 / 1,779 ≈ 41.9%。

---

## 2. タスク 3.3（`statuses`, `timelines`）

| モジュール | 移行対象 | 非対象 | 合計 |
|-----------|---------|-------|------|
| `statuses` | 204 | 37 | 241 |
| `timelines` | 3 | 7 | 10 |
| **小計** | **207** | **44** | **251** |

| ファイル | 移行対象 | 非対象 | 合計 | 非対象の理由 |
|---------|---------|-------|------|------------|
| `src/statuses/account_provider/tests.rs` | 3 | 3 | 6 | app.state |
| `src/statuses/endpoints/tests.rs` | 0 | 21 | 21 | app.actor, app.state, router, oneshot |
| `src/statuses/idempotency/tests.rs` | 6 | 0 | 6 | — |
| `src/statuses/inbound_handlers/tests.rs` | 27 | 0 | 27 | — |
| `src/statuses/ingest_service/tests.rs` | 14 | 0 | 14 | — |
| `src/statuses/interaction_repository/tests.rs` | 24 | 0 | 24 | — |
| `src/statuses/interaction_service/tests.rs` | 25 | 0 | 25 | — |
| `src/statuses/poll_repository/tests.rs` | 23 | 0 | 23 | — |
| `src/statuses/poll_service/tests.rs` | 10 | 0 | 10 | — |
| `src/statuses/render_assembler/tests.rs` | 0 | 13 | 13 | app.state |
| `src/statuses/status_repository/tests.rs` | 30 | 0 | 30 | — |
| `src/statuses/status_service/tests.rs` | 29 | 0 | 29 | — |
| `src/statuses/tag_repository/tests.rs` | 13 | 0 | 13 | — |
| `src/timelines/endpoints/tests.rs` | 0 | 7 | 7 | app.actor, app.state, router, oneshot |
| `src/timelines/hydrator/tests.rs` | 3 | 0 | 3 | — |

**混在ファイル（テスト単位で選ぶ必要があるのはここだけ）**

`src/statuses/account_provider/tests.rs` の移行対象は次の 3 本のみ:

- `resolve_many_raises_this_modules_not_found_for_a_dangling_poll_id`
- `resolve_many_returns_every_existing_poll_in_the_requested_order`
- `resolve_many_reports_the_viewers_own_votes`

残り 3 本は `build_provider(&TestApp)`（`app.state.config()` / `app.state.accounts()` /
`app.state.media()` / `app.state.statuses()` を読む、L284 付近）を共有しているため `TestApp` 固定。

---

## 3. タスク 3.4（`social_graph`, `notifications`, `search`）

| モジュール | 移行対象 | 非対象 | 合計 |
|-----------|---------|-------|------|
| `social_graph` | 96 | 36 | 132 |
| `notifications` | 27 | 43 | 70 |
| `search` | 28 | 34 | 62 |
| **小計** | **151** | **113** | **264** |

| ファイル | 移行対象 | 非対象 | 合計 | 非対象の理由 |
|---------|---------|-------|------|------------|
| `src/notifications/endpoints/tests.rs` | 0 | 24 | 24 | app.state, router, oneshot |
| `src/notifications/filter/tests.rs` | 6 | 0 | 6 | — |
| `src/notifications/generator/tests.rs` | 4 | 0 | 4 | — |
| `src/notifications/repository/tests.rs` | 14 | 0 | 14 | — |
| `src/notifications/service/tests.rs` | 3 | 17 | 20 | app.state |
| `src/notifications/tests.rs` | 0 | 2 | 2 | app.actor, app.state, router, oneshot |
| `src/search/endpoint/tests.rs` | 0 | 10 | 10 | app.state, router, oneshot |
| `src/search/hashtag_indexer/tests.rs` | 7 | 0 | 7 | — |
| `src/search/hashtag_repository/tests.rs` | 10 | 0 | 10 | — |
| `src/search/hydrator/tests.rs` | 3 | 11 | 14 | app.state |
| `src/search/remote_resolver/tests.rs` | 8 | 0 | 8 | — |
| `src/search/service/tests.rs` | 0 | 11 | 11 | app.state |
| `src/search/tests.rs` | 0 | 2 | 2 | app.actor, app.state, router, oneshot |
| `src/social_graph/block_service/tests.rs` | 10 | 0 | 10 | — |
| `src/social_graph/endpoints/tests.rs` | 0 | 22 | 22 | app.state, router, oneshot |
| `src/social_graph/follow_request_service/tests.rs` | 0 | 8 | 8 | app.state |
| `src/social_graph/follow_service/tests.rs` | 13 | 0 | 13 | — |
| `src/social_graph/inbound/tests.rs` | 13 | 0 | 13 | — |
| `src/social_graph/mute_service/tests.rs` | 12 | 0 | 12 | — |
| `src/social_graph/providers/tests.rs` | 15 | 0 | 15 | — |
| `src/social_graph/repository/tests.rs` | 17 | 0 | 17 | — |
| `src/social_graph/tests.rs` | 0 | 6 | 6 | app.actor, app.state, router, oneshot |
| `src/social_graph/transitions/tests.rs` | 16 | 0 | 16 | — |

**混在ファイル**

`src/notifications/service/tests.rs` の移行対象は次の 3 本のみ（ファイル末尾の
`RequiredPolls` 系。L1273 付近）:

- `resolve_many_raises_this_modules_not_found_for_a_dangling_poll_id`
- `resolve_many_returns_every_existing_poll_in_the_requested_order`
- `resolve_many_reports_the_viewers_own_votes`

残り 17 本は `build_service(&TestApp)`（L150、`app.state.*` を 3 箇所読む）を共有。

`src/search/hydrator/tests.rs` の移行対象は次の 3 本のみ:

- `resolve_many_drops_a_dangling_poll_id_instead_of_failing`
- `resolve_many_returns_polls_in_the_requested_order`
- `resolve_many_reports_the_viewers_own_votes`

残り 11 本は `build_hydrator(&TestApp)`（L142、`app.state.*` を 5 箇所読む）を共有。
なお `hydrate_accounts_following_only_narrows_to_followed_accounts` はテスト本体でも
直接 `app.state` を触っている（L249）。

---

## 4. タスク 3.5（`oauth`, `accounts`, `actor`, `media`, `federation`）

| モジュール | 移行対象 | 非対象 | 合計 |
|-----------|---------|-------|------|
| `oauth` | 50 | 7 | 57 |
| `accounts` | 32 | 16 | 48 |
| `actor` | 39 | 0 | 39 |
| `media` | 36 | 0 | 36 |
| `federation` | 24 | 23 | 47 |
| **小計** | **181** | **46** | **227** |

| ファイル | 移行対象 | 非対象 | 合計 | 非対象の理由 |
|---------|---------|-------|------|------------|
| `src/accounts/account_service/tests.rs` | 0 | 16 | 16 | app.actor |
| `src/accounts/emoji_repository/tests.rs` | 5 | 0 | 5 | — |
| `src/accounts/emoji_service/tests.rs` | 3 | 0 | 3 | — |
| `src/accounts/instance_service/tests.rs` | 3 | 0 | 3 | — |
| `src/accounts/profile_repository/tests.rs` | 6 | 0 | 6 | — |
| `src/accounts/remote_fetcher/tests.rs` | 8 | 0 | 8 | — |
| `src/accounts/remote_repository/tests.rs` | 3 | 0 | 3 | — |
| `src/accounts/settings_repository/tests.rs` | 4 | 0 | 4 | — |
| `src/actor/directory/tests.rs` | 12 | 0 | 12 | — |
| `src/actor/keys/repository/tests.rs` | 7 | 0 | 7 | — |
| `src/actor/keys/service/tests.rs` | 5 | 0 | 5 | — |
| `src/actor/owner/tests.rs` | 3 | 0 | 3 | — |
| `src/actor/repository/tests.rs` | 7 | 0 | 7 | — |
| `src/actor/service/tests.rs` | 5 | 0 | 5 | — |
| `src/federation/endpoints/document/tests.rs` | 9 | 0 | 9 | — |
| `src/federation/endpoints/webfinger/tests.rs` | 0 | 5 | 5 | app.actor |
| `src/federation/inbound/dedup/tests.rs` | 3 | 0 | 3 | — |
| `src/federation/outbound/queue/tests.rs` | 7 | 0 | 7 | — |
| `src/federation/outbound/worker/tests.rs` | 0 | 4 | 4 | app.actor, app.state |
| `src/federation/signatures/key_resolver/tests.rs` | 5 | 0 | 5 | — |
| `src/federation/signatures/negotiation/tests.rs` | 0 | 6 | 6 | app.actor, app.state |
| `src/federation/signatures/signer/tests.rs` | 0 | 8 | 8 | app.actor, app.state |
| `src/media/job_queue/tests.rs` | 8 | 0 | 8 | — |
| `src/media/media_repository/tests.rs` | 13 | 0 | 13 | — |
| `src/media/service/tests.rs` | 8 | 0 | 8 | — |
| `src/media/worker/tests.rs` | 7 | 0 | 7 | — |
| `src/oauth/app_repository/tests.rs` | 8 | 0 | 8 | — |
| `src/oauth/code_repository/tests.rs` | 7 | 0 | 7 | — |
| `src/oauth/middleware/tests.rs` | 0 | 7 | 7 | router, oneshot |
| `src/oauth/owner_gate/tests.rs` | 4 | 0 | 4 | — |
| `src/oauth/service/tests.rs` | 22 | 0 | 22 | — |
| `src/oauth/token_repository/tests.rs` | 9 | 0 | 9 | — |

このグループに混在ファイルはない。**すべてファイル単位で全移行か全据え置き**。

---

## 5. 3.3–3.5 のいずれにも入らないもの（要注意）

| ファイル | 移行対象 | 非対象 | 合計 |
|---------|---------|-------|------|
| `src/test_harness/tests.rs` | 1 | 2 | 3 |

**タスク 3.3–3.5 のどの `_Boundary:_` にも `test_harness` は含まれていない。**
ただしここは移行してはならない。3 本とも `spawn_test_app` 自体のふるまいを検証する
ハーネスの自己テストであり、`TestDb` に置き換えると検証対象が消える。

- `spawn_test_app_boots_with_applied_migrations_and_deterministic_runtime` — 非対象（`app.address`）
- `cleanup_releases_pool_listener_and_isolated_schema` — 非対象（`app.address` / listener）
- `spawn_test_app_isolates_database_state_between_instances` — **機械判定では「移行対象」に
  出るが、実質は非対象**。`spawn_test_app` の隔離性そのものを検証しているため、移行すると
  要件 4.4（検証範囲を狭めない）に反する。

したがって **実際に移行してよいのは 540 − 1 = 539 本**、`TestApp` に残るのは 206 本。
グループ別の実行可能件数は 3.3=207 / 3.4=151 / 3.5=181（合計 539）。

`src/federation/test_harness.rs`（連合ペアハーネス）には `#[test]` 関数がなく、
利用元は `tests/` 配下の統合テストのみ。745 件には含まれず、本タスクの範囲外。

---

## 6. モジュールごとの移行時の注意

### 全般
- `TestDb` は `pool` / `runtime` の 2 フィールドしか持たない。`app.address` / `app.state` /
  `app.actor` を使うテストはコンパイルが通らない（design の Risks が言う「良い失敗様式」）。
  逆に言うと、下表の「移行対象」がもし誤っていても**ほとんどの場合**静かには壊れない。
  例外は下の 2 つ（`runtime.keys` とバックグラウンドループの不在）で、どちらも両フィクスチャで
  コンパイルが通る。
- `app.cleanup().await` は `TestDb::cleanup` にそのまま置き換わる。
- 変数束縛名は `app` に統一されている。`let TestApp { .. } = app` 形式の分解は **0 件**。

### 静かに壊れうる経路 その 1: `runtime.keys` の意味的な差
`TestApp.runtime.keys` は実 DB 由来の `DbSigningKeyProvider`、`TestDb.runtime.keys` は
`FixedSigningKeyProvider`。**フィールド自体は両方にあるのでコンパイルは通る。**
`runtime.keys` に触れているのは次の 5 ファイルのみ:

| ファイル | 用途 | 判定 |
|---------|------|------|
| `src/federation/signatures/signer/tests.rs` | `RequestSigner::new` に渡す＝実鍵に依存 | 非対象（`app.actor` / `app.state` でも非対象） |
| `src/federation/signatures/negotiation/tests.rs` | 同上 | 非対象 |
| `src/federation/outbound/worker/tests.rs` | 同上 | 非対象 |
| `src/media/worker/tests.rs` (L217 `runtime_with_clock`) | `RuntimeContext` を clock 差し替えで再構築するために `keys` を横流しするだけ。`ProcessingWorker` は keys を読まない | **移行対象** |
| `src/oauth/service/tests.rs` (L625, L646) | 同じく `RuntimeContext` 再構築のための横流し。`OauthService` は keys を読まない | **移行対象** |

実際に `runtime.keys` を消費する本番配線は `src/federation/module.rs:497` だけである。
上記 2 ファイル（`media/worker`, `oauth/service`）の移行では `keys: db.runtime.keys.clone()` に
そのまま置き換えてよい。

### 静かに壊れうる経路 その 2: バックグラウンドループの不在

`spawn_test_app` は `federation_background.spawn()` と `media_background.spawn(pending)` を
起動する（`src/test_harness.rs:823-824`）。配送ポーリング 200ms 間隔、プルーニング 5 秒間隔、
メディアワーカー並列度 2。**`spawn_test_db` はこれらを一切起動しない。**
フィールドの有無ではないのでコンパイルは両方通る。

移行時の意味: 対象テストは「同じスキーマの同じキューテーブルから行を奪い合う生きた競合者」を
失う。方向としては決定性が上がる（移行前のほうが非決定的だった）。現時点の 540 件を調査した
範囲では、これによって空虚に通るようになるテストは無い — `federation/inbound/dedup` は
`prune_expired()` を自分で駆動して `deleted == 1` を主張し、`federation/outbound/queue` /
`media/job_queue` / `media/worker` はいずれも `claim_due` / `run_once` を明示的に駆動して
戻り値を検証している。いずれも「静かに通る」ではなく「大きな音を立てて落ちる」側に倒れる。

ただし 3.3–3.5 の完了条件は「成否が移行前と一致」なので、`federation/outbound/queue`（7 本）と
`media/*`（36 本）の移行後に挙動が変わった場合は、まずこの差を疑うこと。要件 4.4 の
「検証しているふるまいの範囲を狭めない」の判断材料でもある。

なお `SeqIdGenerator` は共有カウンタなので、`TestApp` ではバックグラウンドループが ID を
並行に消費しうる。`TestDb` ではテストが唯一の消費者になる。リテラルな ID 値を主張している
テストがあれば落ちるが、移行前の時点で既に非決定的なので、そのようなテストは現存しない。

### `statuses`
- 移行対象 204 本のうち 201 本はファイル丸ごと移行。混在は
  `account_provider/tests.rs` のみ（3/6）。
- `endpoints` / `render_assembler` は丸ごと `TestApp` 据え置き。

### `timelines`
- `endpoints/tests.rs`（7 本）は router + oneshot で据え置き。移行対象は
  `hydrator/tests.rs` の 3 本のみ。

### `social_graph`
- `follow_request_service/tests.rs` の 8 本は、`app.state.accounts().service()` と
  `app.state.config().server.domain` の 2 箇所（L222–223）だけで `TestApp` に縛られている。
  この 2 引数を素の値に差し替えればグループ内で最大の追加移行余地になるが、
  **本 spec の機械的基準では非対象**。3.4 の範囲外の作業なので勝手にやらないこと。

### `notifications`
- `endpoints/tests.rs`（24 本）が最大の据え置き塊。ルーター経由の HTTP テスト。
- `service/tests.rs` は 3/17 の混在。上記 §3 の 3 本だけを動かす。

### `search`
- `hydrator/tests.rs` は 3/11 の混在。`service/tests.rs` と `endpoint/tests.rs` は丸ごと据え置き。

### `oauth`
- 50/57 が移行可能。据え置きは `middleware/tests.rs`（7 本、ルーターに tower layer を
  被せて oneshot する）のみ。
- `service/tests.rs` は 22 本すべて移行可能だが、上記 `runtime.keys` の再構築パターンがある。

### `accounts`
- `account_service/tests.rs`（16 本）の据え置き理由は `app.actor.directory()` の 1 パターンだけ
  （L210, L239, L266）。`ActorDirectory::new(pool)` で置き換え可能に見えるが、
  基準どおり非対象として扱う。それ以外の 7 ファイル 32 本は丸ごと移行。

### `actor`
- 39 本すべて移行対象。`actor/service/tests.rs` は `SigningKeyService` を
  pool + runtime + 自前の cipher/KeyCache から組み立てており、`app.actor` を使っていない
  （＝移行後も鍵生成の実経路をそのまま検証できる）。この組み立て方が他モジュールの
  お手本になる。

### `media`
- 36 本すべて移行対象。`worker/tests.rs` の `runtime_with_clock` は上記の `keys` 注意点。

### `federation`
- 24/47。移行できるのは `endpoints/document` (9) / `outbound/queue` (7) /
  `signatures/key_resolver` (5) / `inbound/dedup` (3)。
- 署名まわり 3 ファイル（signer 8 / negotiation 6 / outbound/worker 4）と
  `endpoints/webfinger`(5) は据え置き。webfinger は
  `insert_actor_fixture` が `app.actor.actor_service().create_actor(...)` を使っているのが理由
  （L70）で、実インスタンスの HTTP は使っていない。

---

## 7. 分類方法（再現手順）

### ベースラインの計数

```bash
# design 記載の 750（テキスト出現回数、コメント込み・定義込み）
git grep -oh "spawn_test_app(" -- src/ | wc -l      # => 750
```

### 分類ルール（機械適用）

1. Rust のコメント（`//` / `/* */`）と文字列リテラル（raw / byte raw の
   `r#"…"#` `br#"…"#` を含む）を除去する。**`br#"{...}"#` を素の文字列として扱うと
   波括弧の対応が壊れ、関数本体の切り出しが後続の関数まで飲み込む**（実際に
   `federation/signatures/signer/tests.rs` で踏んだ）。
2. `fn <name>` から波括弧対応で本体を切り出す。直前の `#[...]` 属性列を関数に紐づける。
3. `#[test]` / `#[tokio::test]` を持つものをテスト関数とする。
4. 同一ファイル内の呼び出し関係（本体中に識別子が現れるか）で推移閉包を取り、
   ヘルパー経由の間接利用を本体に含める。
5. 閉包テキストに `spawn_test_app` が現れるものを「実インスタンス起動を要するテスト」とする。
6. そのうち、次のいずれかに当たるものを **非対象** とする。
   - 文脈非依存マーカー: `reqwest` / `.oneshot(` / `ServiceExt` / `build_router` /
     `Router` / `AppState` / `axum::http::Request` / `http::Request::builder`
   - `TestApp` 束縛（`let x = spawn_test_app` および `x: &TestApp` 引数）に対する
     フィールドアクセスのうち、`pool` / `runtime` / `cleanup` 以外のもの
     （= `address` / `state` / `actor` / `schema`）
   - `TestApp { .. }` の分解束縛で `pool` / `runtime` 以外を取り出すもの
7. `runtime.keys` は **非対象の理由にしない**（`TestDb` にも同名フィールドがあり
   コンパイルは通る）。代わりに §6 の注意点として扱う。

### 検証したこと

- 移行対象 540 本すべてについて、`TestApp` 束縛が 1 つ以上検出されていることを確認（0 件の漏れ）。
  つまりフィールドアクセス走査は全件で実際に走っている。
- 移行対象 540 本の閉包テキストに対する再走査で、`build_*_module` / `AppState` /
  `TcpListener` / `hyper` / `SigningKeyProvider` / `RequestSigner` / `spawn_paired*` /
  `app.<pool|runtime|cleanup 以外>` の出現は **0 件**。

### 目視で確認した件数と結果

個別に本文を読んで検証したのは **テスト 14 本 + 共有ヘルパー 6 本**。
その過程で判定方法を 3 回修正しており、最終結果は修正後のもの。

| 発見 | 対処 |
|------|------|
| `br#"{…}"#` の解析崩れで 1 ファイルの関数境界が壊れていた | raw/byte-raw 文字列のスキップを追加。再走査で「関数外の呼び出し箇所」が 0 になり整合 |
| `axum::http::StatusCode` / `summary.state` / `resolved.state` を非対象と誤判定（レシーバ無視の正規表現） | `TestApp` 束縛に対するフィールドアクセスに限定。253 本 → 208 本に縮小 |
| `runtime.keys` の横流しを非対象と誤判定（`media/worker` 2 本、`oauth/service` 1 本） | `runtime.keys` を非対象の理由から外し、注意点に降格。205 本に確定 |

最終サンプルでの誤分類は 0 件。
