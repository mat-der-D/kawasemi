# 配送ワーカーのクレーム失敗の調査記録

対象: `federation::outbound::worker::tests::run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves`
が「クレーム件数 0」になる現象（Requirements 6.1, 6.2, 6.3, 6.4 / design.md
`Investigation` → `WorkerClaimInvestigation`）。

調査日: 2026-08-14。

## 結論（先に述べる）

**テスト固有の事情**である。プロダクションの欠陥ではない。

`DbDeliveryQueue::claim_due` は仕様どおりに動作していた。クレーム件数が 0 になるのは、
`spawn_test_app` が**同じプールの上で本物の配送ワーカーのバックグラウンドループを起動している**ため、
テスト自身の `run_once` より先にそのループが同じジョブをクレームしていたからである。
`FOR UPDATE SKIP LOCKED` はまさにこの「複数のクレーム主体が競合する」状況のために存在し、
観測された挙動はその契約どおりの結果である。

`max_connections` はこの競合の**勝敗を決めるタイミング要因**にすぎず、原因ではない。

## 再現するか（2026-08-14 時点）

**する。** ただし当初の記述（「プール 1 でのみ起きる」）は不正確で、実際には
**プール 2（現行設定）でも低頻度で再現する**。

### 手順

1. `src/test_harness.rs` の `establish_isolated_db` 内（`admin_db_config` のものではない方）の
   `max_connections: 2` を `1` に変更する。
2. `cargo test --lib -- --exact federation::outbound::worker::tests::run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves`

### 観測結果

| 設定 | 試行 | 失敗 |
| --- | --- | --- |
| `max_connections: 1` | 5 | 5（決定的） |
| `max_connections: 2`（現行） | 34 | 1 |

`max_connections: 1` での失敗出力:

```
thread '...run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves' panicked at
src/federation/outbound/worker/tests.rs:297:5:
assertion `left == right` failed
  left: 0
 right: 1
```

プール 2 での 1 件の失敗は、本調査の最初の実行（コード無改変の状態）で発生したものであり、
同一の assert・同一の左辺 0 であった。その後の連続 30 回はすべて成功している。
つまり**プール 2 でも同じ競合は成立しており、単に窓が狭いだけ**である。
「フル並列実行時のみ稀に起きる」という従来の理解は誤りで、**単体実行でも起きる**。

## 仮説と検証

### 仮説 A（design.md の第一候補）: 同じプールの別経路が対象行をロックし、`SKIP LOCKED` が飛ばしている

**判定: 確認（ただし「ロックして飛ばされた」のではなく「先に UPDATE され尽くしていた」）。**

「同じプールの別経路が対象行を取っている」という第一候補の骨子は正しかった。
その別経路の正体は `spawn_test_app` が起動する**本物の配送ワーカーのバックグラウンドループ**である。

- `src/test_harness.rs` の `spawn_test_app` は `federation_background.spawn()` を呼ぶ
  （`src/test_harness.rs:823` 付近）。
- `src/federation/module.rs:645` 付近の `FederationBackgroundTasks::spawn` は、
  `worker.run_once(delivery_poll_batch_size)` を回して `TEST_DELIVERY_POLL_INTERVAL`
  （`src/test_harness.rs:225`、**200 ms**）ごとに再実行するループを `tokio::spawn` する。
- このワーカーはテストと**同一の `PgPool`・同一のスキーマ・同一の `delivery_jobs` テーブル**を見ている。
  すなわちテスト用の `DeliveryWorker` と、ハーネス自身の `DeliveryWorker` の**二者がキューを奪い合っている**。

#### 直接証拠 1: バックグラウンドループを止めると再現しなくなる

`spawn_test_app` の `federation_background.spawn()` を一時的に無効化し、`max_connections: 1` のまま実行:

```
test result: ok. 1 passed  （3 回中 3 回）
```

決定的に失敗していた条件で、この 1 行を止めるだけで決定的に成功に変わる。

#### 直接証拠 2: バックグラウンドループが「1 件クレームした」と自ら報告している

`max_connections: 1` で、テスト側とバックグラウンドループ側の双方に一時的な計測を入れた出力
（`[bg]` の時刻はループ起動時点、`[test]` の時刻はテスト開始時点を原点とする）:

```
[test] spawn_test_app done t=486.0ms
[bg]   tick-start        t=1.32ms
[test] enqueue done      t=488.4ms
[test] run_once start    t=488.4ms
[test] run_once end      t=491.0ms claimed=0
[bg]   tick-end          t=8.47ms r=Ok(1)      <-- バックグラウンドが 1 件クレーム
[bg]   tick-start        t=209.9ms
[bg]   tick-end          t=210.5ms r=Ok(0)
[test] after-wait job state = ("failed", 0, ...)
```

- バックグラウンドループの 1 回目の tick は `enqueue` より**前に開始**しているのに、
  `enqueue` が完了するまで**終わっていない**（tick-start 1.32ms → tick-end 8.47ms）。
- その tick は `claimed = 1` を返している。**テストが取り損ねた 1 件は、この tick が取っていた。**
- ジョブの最終状態は `("failed", attempts=0)` で、テストが期待するものと**完全に同一**。
  つまり処理自体は正しく一度だけ行われており、失われた仕事はない。
  テストが落ちるのは `summary.claimed` という「誰が取ったか」に依存した assert だけである。

### 仮説 B: `claim_due` の `FOR UPDATE SKIP LOCKED` がプールサイズに依存する

**判定: 否定。**

`claim_due` は単一文であり、`fetch_all(&self.pool)` が 1 本の接続を取って実行するだけで、
プールサイズを参照する箇所はない。仮説 A の直接証拠 1 のとおり、プールサイズを 1 に固定したまま
競合相手を取り除くだけでテストは成功する。したがってプールサイズは `claim_due` の正しさに影響しない。

### 仮説 C: 同一接続上でロックを保持したトランザクションが自分自身の `SKIP LOCKED` を飛ばしている

**判定: 否定。**

`claim_due` は明示トランザクションを開かない単一の autocommit 文であり、`enqueue` も同様である。
テストの経路上、対象行のロックを保持したまま開きっぱなしになるトランザクションは存在しない。
また証拠 2 の `r=Ok(1)` が示すとおり、行は「飛ばされた」のではなく**別主体に取られていた**。

### 仮説 D: プール枯渇による `acquire` タイムアウトが握り潰されている

**判定: 否定。**

`acquire_timeout` 超過は `sqlx::Error::PoolTimedOut` として `claim_due` から `AppError` で返り、
テストは `.expect("run_once must succeed")` で panic する。実際には `Ok` が返って
`claimed == 0` だったので、エラーの握り潰しではない。

## なぜプールサイズが勝敗を変えるのか

`claim_due` の正しさとは無関係に、プールサイズは**バックグラウンドループの 1 回目の tick が
いつ実際に SQL を発行するか**を決める。

- **`max_connections: 2`**: バックグラウンドループは起動直後（tick-start ≈ 1.3ms）に
  自分用の接続をすぐ確保でき、まだ空の `delivery_jobs` に対して `claim_due` を実行し、
  `Ok(0)` を返して 200 ms 眠る（計測でも tick-end ≈ 2.4ms で完了している）。
  テストの `enqueue` → `run_once` はその後の数 ms の窓で完結するため、通常はテストが勝つ。
  次の tick がちょうどこの窓に重なったときだけ失敗する（実測 34 回中 1 回）。
- **`max_connections: 1`**: 接続は 1 本しかない。バックグラウンドループは起動直後に
  `claim_due` の `acquire` を要求するが、その接続は `spawn_test_app` の残りの処理と
  テストの `enqueue` が使っている。プールの待ち行列に**先に並んでいる**のはバックグラウンド側なので、
  `enqueue` が接続を手放した瞬間にバックグラウンドの `claim_due` が実行され、
  テスト自身の `claim_due`（さらに後から並ぶ）より必ず先に行を取る。
  結果として「enqueue の直後・テストの claim_due の直前」という最悪の順序が**毎回**成立する。

要するに、プールサイズ 1 は競合の窓を「稀」から「必ず」に変える増幅器であって、原因ではない。

## プロダクションへの影響（Requirement 6.2 の判定）

**プロダクションの欠陥は見つかっていない。**

- プロダクションで `claim_due` を呼ぶのは `FederationBackgroundTasks` の配送ループ**ただ一つ**である。
  テストで観測された「二者がキューを奪い合う」状況は、ハーネスがテスト用ワーカーを
  追加で立てていることによって初めて生じる。
- 仮に将来ワーカーを複数プロセスに増やしたとしても、`FOR UPDATE SKIP LOCKED` は
  「片方が取ったら他方は 0 件」を返すのが**正しい設計**であり、ジョブが失われることはない。
  実際、本現象でもジョブは正しく一度だけ処理され `failed` に落ちている。
- 接続が逼迫した場合に起きるのは「`claim_due` が黙って 0 を返す」ではなく
  「`acquire` が `acquire_timeout` で `PoolTimedOut` エラーになる」であり、こちらは
  握り潰されずログに出る（`module.rs` の `tracing::error!` 経路）。

### 本 spec の範囲外だが記録しておく観察

`claim_due` が行を `'in_progress'` にした後、そのクレーム主体がクラッシュ／再起動した場合に
`'in_progress'` のまま取り残された行を回収する仕組み（stale claim の再取得）はコードベースに存在しない。
これは本現象とは別の話であり、本調査で欠陥として確認したものではないが、
配送キューの堅牢性を扱う将来の spec が検討すべき既知のギャップとして残す。

## テスト側の問題の所在（Requirement 6.3）

問題はテストの前提にある。
`run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves` は
`summary.claimed == 1`、すなわち「**このワーカーが**取ったこと」を assert しているが、
`spawn_test_app` は同じキューを消費する別のワーカーを常時走らせている。
共有キューに対して「誰が取ったか」を assert する以上、この競合は原理的に避けられない。

修正方針の候補（**本 spec の Out of Boundary。実施しない**）:

- この 4 テストを `TestDb` 系フィクスチャに移す。`db_fixture` はバックグラウンドループを起動しないため
  競合相手が存在しなくなる（ただし本テストは `app.actor` / `app.state.config()` を使うため、
  モジュール配線の入手手段が別途必要）。
- あるいは `spawn_test_app` に配送ループを起動しない選択肢を設ける。
- あるいは assert を「誰が取ったか」ではなく最終状態（`status = 'failed'`, `attempts = 0`）のみに緩める。

## 再現に用いた一時計測（すべて撤去済み）

- `src/federation/module.rs`: 配送ループの `run_once` 前後に `eprintln!` を挿入し、
  tick の開始／終了時刻とクレーム件数を出力。
- `src/federation/outbound/worker/tests.rs`: `spawn_test_app` / `enqueue` / `run_once` の
  前後に `eprintln!` を挿入。加えて assert 前に 400 ms の `sleep` を置き、
  バックグラウンド tick の完了を観測できるようにした。
- `src/test_harness.rs`: `federation_background.spawn()` の一時無効化、および `max_connections` の 1 への変更。

いずれも調査後に `git checkout` で復元済み。恒久的な変更は
`src/test_harness.rs` の `max_connections: 2` を説明するコメントの更新のみである。
