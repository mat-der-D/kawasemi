# Research & Design Decisions

## Summary
- **Feature**: `notifications`
- **Discovery Scope**: Extension（api-foundation / statuses-core / social-graph / accounts-and-instance の確立済み境界に乗る拡張。新たに通知生成シームを導入）
- **Key Findings**:
  - Notification 契約は本 spec が新規所有するが、埋め込む Account（accounts-and-instance 所有）と Status（statuses-core 所有）は**消費**し再定義しない。両 spec のシリアライザを委譲呼び出しする。
  - 通知生成は「再検出（DB 走査）」ではなく「上流イベントの消費」で行う。上流（statuses-core / social-graph）が状態遷移の共通パス上で発生させるドメインイベントを、本 spec が所有する委譲シーム（`NotificationEventSink`、既定 no-op）へ流し込む。これは accounts-and-instance / social-graph で確立済みの「委譲 Port 実装供給」パターンと同型。
  - ブロック / ミュートのフィルタは social-graph が公開済みの関係状態問い合わせ（`FilterQuery`: blocked/blocked_by/muted（通知ミュート・期限考慮））を消費する。関係状態を本 spec で再保持しない。
  - 単一の通知生成点（`NotificationGenerator`）が「フィルタ → 重複排除 → 永続化 → 配信シーム引き渡し」を一箇所で担い、後段 streaming / web-push はこの生成点に乗る（二重発火防止）。配信手段そのものは本 spec で実装しない。

## Research Log

### マイグレーション番号の調整（Migration Numbering Coordination）
- **Context**: 先行 spec とマイグレーション番号が衝突しないようにする（タスク指示の明示要件）。
- **Sources Consulted**: 各 spec の design.md。使用済み: 0001 core-runtime / 0002 actor-model / 0003 federation+oauth（二重利用）/ 0004 media / 0005 accounts-and-instance / 0007 statuses-core。social-graph は **0006** を採用（research.md 注記）。timelines は未確定だが 0008 を想定。search spec は notifications=**0009** を前提に自身を 0010 と仮定している。
- **Findings**: 0009 は未使用かつ search の前提と整合する。0003 は federation/oauth 二重利用のため回避。
- **Implications**: 本 spec は **`migrations/0009_notifications.sql`** を採用する（search が想定する 0009 と一致、衝突なし）。統合時にグローバル連番へリナンバリングする可能性は他 spec と同様に残す。

### Notification 契約と埋め込み境界
- **Context**: Notification は通知元 Account と関連 Status を埋め込むが、両者の契約所有は別 spec。
- **Sources Consulted**: statuses-core design（`StatusSerializer.status_to_json` / Status 契約）、accounts-and-instance design（`AccountSerializer` / Account 契約・`AccountRef`）。
- **Findings**: Status / Account のシリアライズは決定的（ゴールデン固定）で、viewer 文脈（操作状態）を `SerializeContext` で受ける。通知の `status` 埋め込みは通知受信者を viewer として Status をシリアライズすべき（`favourited` 等の操作状態が受信者視点になる）。`account` は通知元アクター（ローカル/リモート）を accounts-and-instance のシリアライザで構成。
- **Implications**: 本 spec は Notification の外殻（`id` / `type` / `created_at` + `account` / `status` 埋め込み）のみを所有し、`account` / `status` の中身は上流シリアライザへ委譲。契約ハーネスには Notification の外殻ゴールデンを登録し、埋め込みの内側は上流ゴールデンに委ねる（契約ドリフト防止）。

### 通知生成イベントの消費方法（再検出の禁止）
- **Context**: タスク指示「fav/reblog/follow/mention/poll の既存イベントから生成し、再検出しない」。
- **Sources Consulted**: statuses-core design（`InteractionService` reblog/favourite、`StatusService.create_status` のメンション抽出、`PollService.vote`、受信 `InboundHandlers`）、social-graph design（`Transitions.establish_follow` / `record_pending`、受信 `InboundHandler`）。
- **Findings**: 上流はローカル発生（サービス層）と受信（InboundHandler）の双方で同一の状態遷移関数（statuses-core: Repository/Interaction、social-graph: `Transitions`）へ合流している。通知生成の正しいフックポイントは、この**合流点（状態遷移確定後）**である。ここに通知イベント発行を置けば、ローカル/リモート対称かつ二重発火しない。
- **Implications**: 本 spec は `NotificationEventSink`（trait・既定 no-op）を定義し `AppState` レジストリに置く。statuses-core / social-graph は状態遷移確定後にイベントを emit する（上流の Revalidation Trigger となる接合点）。本 spec はこの sink の本実装を登録し、イベント → `NotificationGenerator` へ流す。投票終了（`poll`）は時間ベースのため、投票ライフサイクルを所有する statuses-core が「投票終了イベント」を emit する責務を持ち、本 spec はそれを消費するのみ（終了検出機構＝sweep/scheduler は上流の関心）。

### ブロック / ミュートフィルタの委譲
- **Context**: 生成段でブロック / 通知ミュートを抑制する。
- **Sources Consulted**: social-graph design（`FilterQuery.blocked_set` = blocked/blocked_by/muted/muted_notifications、`muted_targets(notifications_only)`、期限考慮）。
- **Findings**: social-graph はタイムライン / 通知のフィルタ判定に必要な関係集合を問い合わせ可能な形で公開済み（フィルタ適用本体は各下流が実装、Req 9.4）。期限切れミュートは導出時に除外される。
- **Implications**: 本 spec は `NotificationFilter` が social-graph の `FilterQuery` を消費し、通知元が受信者にブロック/被ブロック/通知ミュートされているかを生成段で判定して抑制する。関係状態の保持・期限判定ロジックは再実装しない。

### 通知 v1 の種別集合
- **Context**: Mastodon の Notification `type` は多岐にわたるが、brief は v1（mention/follow/follow_request/favourite/reblog/poll 等）に限定し v2/policy/requests/admin を後回しにする。
- **Sources Consulted**: Mastodon API（Notification entity の `type` 列挙、一次情報は実レスポンス）。
- **Findings**: v1 標準種別のうち本 spec の上流イベントから自然に導けるのは `mention` / `favourite` / `reblog` / `follow` / `follow_request` / `poll` に加え、フォロー通知付き投稿の `status`（follow の `notifying` フラグ由来）と編集の `update`。`admin.sign_up` / `admin.report` と v2 のグループ化（`group_key`）は範囲外。
- **Implications**: `type` を `{ mention, follow, follow_request, favourite, reblog, poll, status, update }` に固定（Req 1.5）。`status` / `update` は対応する上流イベント（notify 付きフォロー対象の新規投稿 / 投稿編集）が供給された場合に生成し、なければ生成しないだけで契約は壊れない。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| イベント消費 + 単一 Generator + 委譲 Port（採用） | 上流が emit するドメインイベントを no-op 既定の sink で受け、単一 Generator がフィルタ/重複排除/永続化/配信シーム引き渡しを担う | 再検出を排除し二重発火を防ぐ。streaming/web-push が同一生成点に乗る。既存の委譲 Port パターンと同型 | 上流（statuses-core/social-graph）に emit 接合点を追加する必要（Revalidation Trigger） | brief「単一生成点」「Streaming/Push 再利用」に直結 |
| 通知を DB 走査で定期再検出 | お気に入り/フォロー等を周期的にスキャンして差分通知化 | 上流を変更しない | 検出ロジックが上流と重複し意味論が割れる。flaky・配信遅延・二重発火 | brief 制約「再検出しない」に反し却下 |
| 各エンドポイント/ハンドラで個別に通知生成 | fav/follow 各処理が直接通知を書く | 局所的 | 生成点が分散し streaming/web-push が乗る単一点が無くなる。フィルタ規則が重複 | brief「単一生成点」に反し却下 |

## Design Decisions

### Decision: 通知生成シームは「上流 emit + no-op 既定 sink」で供給する
- **Context**: 通知を上流イベントから生成しつつ、上流（statuses-core / social-graph）が notifications 未登録でも動作する必要がある。
- **Alternatives Considered**:
  1. notifications が上流の内部状態を直接 import して再検出 — 境界違反・再検出禁止違反。
  2. 上流が状態遷移確定点で `NotificationEventSink` に emit、本 spec が本実装を登録（採用）。
- **Selected Approach**: 本 spec が `NotificationEventSink`（trait・既定 `NoopSink`）と `NotificationEvent`（種別 + 受信者 + 通知元 + 対象 id）を定義。`AppState` のレジストリに既定で no-op を置き、本 spec の `NotificationModule` が本実装へ差し替え。上流は accounts-and-instance / social-graph と同じ「委譲 Port 実装供給」の逆向き（上流が呼ぶ）として emit する。
- **Rationale**: steering「意味論対称・物理配送のみ最適化」「契約の集約」と、既存の委譲 Port 文化に整合。二重発火しない単一点を構造的に保証。
- **Trade-offs**: 上流に emit 接合点が増える（Revalidation Trigger）。ただし emit は状態遷移確定後の 1 行で、上流の意味論は変えない。
- **Follow-up**: `NotificationEvent` の種別・ペイロード（受信者 / 通知元 / status_id / 種別）の最終形を実装で固定。statuses-core の投票終了 emit 接合点を確認。

### Decision: 通知受信者はローカルアクターに限定
- **Context**: 通知はサーバー上のローカルアクターの受信箱。リモートアクターの通知は相手サーバーが持つ。
- **Selected Approach**: `NotificationGenerator` は受信者がローカルアクターのイベントのみ通知化（Req 5.3）。通知元（`account`）はローカル/リモート双方を許容し `AccountRef` 相当で保持。
- **Rationale**: Mastodon 実挙動準拠。リモート宛通知を作っても配送先が無い。
- **Trade-offs**: なし（明確な境界）。

### Decision: 重複排除は (受信者, 種別, 通知元, 対象) の一意制約で行う
- **Context**: 上流イベントの再送・再受信、同一お気に入りの再記録などで通知が重複しうる。
- **Selected Approach**: `notifications` テーブルに (recipient, type, account, status) を識別キーとする部分一意制約（status が無い種別は status=NULL を含む一意化）を置き、生成は upsert/ON CONFLICT DO NOTHING で冪等化（Req 8.1, 8.2）。
- **Rationale**: federation-core の Activity 重複排除と二重で生成段も冪等にし、flaky を避ける。
- **Trade-offs**: 「再フォロー後の再通知」など意図的再通知は MVP では非対応（v2/policy 範囲）。許容。
- **Follow-up**: follow/follow_request の重複規律（解除→再フォローで再通知するか）は MVP では「同一キーは再生成しない」で固定。

### Decision: Notification の `status` 埋め込みは受信者視点でシリアライズ
- **Context**: 通知一覧の各 status の操作状態（`favourited` 等）は通知受信者から見た状態であるべき。
- **Selected Approach**: `status` 埋め込み時は通知受信者を viewer として statuses-core の `StatusSerializer` を呼ぶ。`account` は通知元を accounts-and-instance の `AccountSerializer` で構成。
- **Rationale**: Mastodon 実挙動準拠（自分宛通知の status は自分視点）。
- **Trade-offs**: シリアライズ時に受信者文脈を伝播する必要。`SerializeContext` で吸収。

## Risks & Mitigations
- リスク: 上流 emit 接合点が statuses-core / social-graph の境界に未定義で、結線できない — 対策: 本 spec が `NotificationEventSink` を所有して既定 no-op を供給し、上流は「確定後 emit」のみを追加（最小接合）。design に接合点と Revalidation Trigger を明記。
- リスク: 生成点が分散し streaming/web-push が二重発火 — 対策: 生成・フィルタ・永続化・配信シーム引き渡しを `NotificationGenerator` 一箇所に集約し、配信は post-persist の `NotificationDeliverySink`（既定 no-op）経由でのみ供給。
- リスク: ブロック/ミュート判定を本 spec で再実装し social-graph と乖離 — 対策: `FilterQuery` を消費し関係判定を委譲（Req 7.4）。
- リスク: Notification 埋め込みの Account/Status 契約ドリフト — 対策: 埋め込み内側は上流ゴールデンに委ね、本 spec は外殻のみゴールデン登録（Req 1.6）。
- リスク: 投票終了通知（`poll`）の時間依存で flaky — 対策: 投票終了検出は statuses-core 所有、emit イベントを消費するのみ。時刻は注入 `Clock` で決定的に検証。
- リスク: マイグレーション番号衝突 — 対策: 0009 を採用（search の前提と整合・既使用と非衝突）。

## References
- Mastodon API: Notifications（`GET /api/v1/notifications`, `/:id`, `POST /clear`, `/:id/dismiss`、Notification entity の `type`/`account`/`status`）。一次情報は実レスポンス。
- 依存 spec: `.kiro/specs/api-foundation/design.md`（Bearer/Scope/Pagination/MastodonError/ContractHarness）、`.kiro/specs/statuses-core/design.md`（Status 契約・`StatusSerializer`・interaction/poll/inbound イベント源）、`.kiro/specs/social-graph/design.md`（`FilterQuery`・フォロー/フォローリクエストのイベント源・ブロック/ミュート関係状態）、`.kiro/specs/accounts-and-instance/design.md`（Account 契約・`AccountSerializer`・`AccountRef`）。
