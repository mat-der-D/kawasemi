# Research & Design Decisions

## Summary
- **Feature**: `social-graph`
- **Discovery Scope**: Extension（federation-core / accounts-and-instance の確立済み委譲境界に乗る拡張）
- **Key Findings**:
  - 関係状態の真実源は本 spec が新規所有するが、Relationship の JSON 契約は accounts-and-instance が所有済み。本 spec は `RelationshipView` / `RelationshipSerializer` を**消費**し再定義しない。
  - フォロー・ブロックの連合往復は federation-core の `DeliveryService`（共通配送パス）と `InboundActivityHandler` 登録レジストリにそのまま乗る。本 spec は Activity 種別の意味論（状態遷移）のみを追加し、署名・配送キュー・double-knock には触れない。
  - ブロック先署名拒否は federation-core が用意済みの `BlockPolicy` 委譲境界（既定 no-op）に実装を差し込むだけで成立する。
  - 「同一サーバー承認スキップ」は steering（structure.md / tech.md）が明示する「明示的な管理者特権を一箇所に定義」原則の唯一の具体例であり、意味論対称の共通パス上に乗る単一の例外として閉じ込める。

## Research Log

### マイグレーション番号の非衝突確定
- **Context**: 先行 spec とマイグレーション番号が衝突しないようにする必要がある（タスク指示の明示要件）。
- **Sources Consulted**: 各 spec の design.md（`grep migrations/00NN`）。core-runtime=`0001_init`、actor-model=`0002`（慣例）、federation-core=`0003_federation`、api-foundation=`0003_oauth`（federation と同番号を二重利用）、media-pipeline=`0004`（慣例）、accounts-and-instance=`0005_accounts`、statuses-core=`0007_statuses`（仮番号）。
- **Findings**: 使用済み番号は 0001 / 0002 / 0003 / 0004 / 0005 / 0007。`0006` は未使用かつ accounts-and-instance（0005）と statuses-core（0007）の間で明確に非衝突。
- **Implications**: 本 spec は **`migrations/0006_social_graph.sql`** を採用する。0003 は federation/oauth の二重利用があるため回避（accounts-and-instance design.md の注記と同方針）。

### Relationship 契約の所有境界
- **Context**: follow / block / mute 応答は Relationship JSON を返すが、契約所有は accounts-and-instance。
- **Sources Consulted**: accounts-and-instance requirements Req 5 / design `RelationshipView`・`RelationshipSerializer`・`RelationshipStateProvider`。
- **Findings**: `RelationshipView` は全フラグ（following/showing_reblogs/notifying/languages/followed_by/blocking/blocked_by/muting/muting_notifications/requested/requested_by/domain_blocking/endorsed/note）を持つ。`RelationshipSerializer.build_relationship(view)` が JSON を生成。`RelationshipStateProvider` は閲覧者 + 対象群 → `Vec<RelationshipView>` を返す委譲 trait（既定: 全 false）。
- **Implications**: 本 spec は (1) 関係状態 → `RelationshipView` を構築するマッパを所有し、(2) `RelationshipSerializer` を呼んで JSON を生成し、(3) `RelationshipStateProvider` の本実装を accounts-and-instance のレジストリへ登録する。Relationship のフィールド定義・シリアライズ規律は本 spec で再定義しない。`domain_blocking` は本 spec 範囲外のため常に false（Req 8.5）。

### 連合配送・受信境界の消費方法
- **Context**: Follow/Accept/Reject/Block/Undo の往復を意味論対称で実現する。
- **Sources Consulted**: federation-core design `DeliveryService.deliver(DeliveryRequest)`・`InboundActivityHandler`/`InboundActivityDispatcher.register`・`InboundContext { signer: VerifiedSigner }`・`BlockPolicy.is_blocked(actor_uri)`。
- **Findings**: `DeliveryService` が共通部（正規 Activity 生成・検証・宛先解決）を担い、local は in-process、remote はキュー投入へ分岐する。受信は署名検証→ブロック判定→重複排除→ディスパッチの順で `InboundActivityHandler` に委譲される。重複排除は federation-core の `ReceivedActivityStore` が担う。
- **Implications**: 本 spec は Activity の論理生成（JSON 構築）→ `DeliveryService.deliver` 呼び出しのみ行い、署名・キュー・double-knock には触れない。受信ハンドラは `ParsedActivity` + `InboundContext.signer`（検証済み署名者 URI）を受け取り状態遷移を起こす。Activity id レベルの重複は federation-core が排除するが、本 spec も関係状態遷移を冪等に保つ（Req 7.7）。

### 同一サーバー承認スキップの一箇所定義
- **Context**: steering が「明示的例外を一箇所に定義」と要求（structure.md / tech.md / brief.md Constraints）。
- **Findings**: フォロー確立可否の判定は「宛先がロック済みか」だけでなく「送信元・宛先がともに同一サーバーローカルか」を加味する。これを各経路（API 経由フォロー / 受信 Follow ハンドラ）に分散させると特例が漏れる。
- **Implications**: 承認要否判定を単一の関数 `FollowApprovalPolicy::requires_approval(source, target)` に集約し、API フォロー経路・受信 Follow 経路の双方がこれを呼ぶ。同一サーバー特権はこの関数内の単一分岐としてのみ存在する（Req 3.3）。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| Service + Repository + 委譲 Port（採用） | 関係状態を Repository、操作を Service、連合は ActivityFactory + DeliveryService 消費、フィルタ/関係供給を Port 実装で外部へ供給 | 上流の確立済み境界に素直に乗る。意味論対称が共通 Service に集約される | Service が肥大化しうる → operation 単位で分割 | accounts-and-instance / federation-core と同型 |
| Activity 種別ごとの独立ハンドラに状態遷移を散在 | 各 Activity に閉じた処理 | 局所的 | 承認スキップ・ブロック解消などの横断規則が重複し意味論対称が壊れる | 却下 |
| 関係状態を accounts-and-instance に同居 | 1 テーブルに集約 | 参照が単純 | 書き込み所有が分裂し境界が崩れる（brief の In/Out 違反） | 却下 |

## Design Decisions

### Decision: 関係状態テーブルの分離（follows / follow_requests / mutes / blocks）
- **Context**: フォロー（確立）・保留フォローリクエスト・ミュート・ブロックは生存期間・属性（reblogs/notify/languages、通知ミュート/期限、被ブロック方向）が異なる。
- **Alternatives Considered**:
  1. 単一 `relationships` テーブルに状態カラムを集約 — 疎なフラグで NULL 多発、被ブロック方向や保留の表現が苦しい。
  2. 関係種別ごとにテーブル分離（採用）。
- **Selected Approach**: `follows`（reblogs/notify/languages 属性付き）、`follow_requests`（送信中/受信保留を方向で表現）、`mutes`（notifications・expires_at）、`blocks` の 4 テーブル。被ブロックは「相手→自分の blocks 行」として一意に表現し、`blocked_by` は逆引きで導出。
- **Rationale**: 各関係の属性差を素直に表現でき、`RelationshipView` 導出は 4 テーブルの逆引き合成で決定論的に行える。
- **Trade-offs**: `RelationshipView` 構築は複数テーブル参照になるが、対象 id バッチ問い合わせで吸収する。
- **Follow-up**: フォロー/被フォロー・保留方向の一意制約とインデックスを設計（実装で検証）。

### Decision: 連合 Activity 生成は ActivityBuilder に集約し DeliveryService へ委譲
- **Context**: Follow/Accept/Reject/Block/Undo を意味論対称に生成する。
- **Selected Approach**: `ActivityBuilder` が各 Activity の正規 JSON を生成し、`DeliveryService.deliver` に渡す。ローカル/リモートの分岐は federation-core 側（sink）に閉じ、本 spec は recipient を確定して渡すのみ。
- **Rationale**: tech.md「意味論対称・物理配送のみ最適化」に整合。本 spec は配送手段を意識しない。
- **Trade-offs**: Undo は元 Activity の参照（id）を保持・再構築する必要がある → 送信 Activity の id を記録。
- **Follow-up**: Undo 対象 Activity id の保持方法（follows/blocks 行に活動 id を持たせる）を実装で確定。

### Decision: ミュートはローカル限定・連合配送なし
- **Context**: Mastodon の mute は連合しないローカル表示制御。
- **Selected Approach**: mute/unmute は Activity を生成せず DB 状態のみ更新（Req 4.5）。期限付きミュートは `expires_at` を持ち、フィルタ供給・`RelationshipView` 導出時に期限切れを除外（Req 4.3, 9.3）。
- **Rationale**: Mastodon 実挙動準拠（ドキュメント < 実レスポンス）。
- **Trade-offs**: 期限切れの遅延クリーンアップが必要だが、導出時フィルタで論理的には即時に解除扱いにできる。

## Risks & Mitigations
- リスク: 受信 Follow とローカル発フォローで承認スキップ判定が二重化し意味論が割れる — 対策: `FollowApprovalPolicy` 単一関数に集約し両経路から呼ぶ（Req 3.3）。
- リスク: ブロック成立時の関係解消（双方向フォロー・保留）漏れ — 対策: block 操作を単一トランザクションで follows/follow_requests を両方向削除（Req 5.2）。連合/受信双方で同一の状態遷移関数を共有。
- リスク: Undo(Follow/Block) で対象 Activity id を再構築できず相手が解釈できない — 対策: 送信時に Activity id を関係行へ保存し Undo 時に参照。
- リスク: `domain_blocking` を将来実装する際に契約が割れる — 対策: 現状は常に false を供給し、契約フィールドは accounts-and-instance 所有のまま温存（Req 8.5）。
- リスク: 期限付きミュートの flaky テスト — 対策: 期限判定は core-runtime の注入 `Clock` のみを用い決定的に検証。

## References
- Mastodon API: accounts follow/unfollow/mute/block, follow_requests（一次情報は実レスポンス、ゴールデンは accounts-and-instance が所有）。
- 依存 spec: `.kiro/specs/federation-core/design.md`（DeliveryService / InboundActivityHandler / BlockPolicy）、`.kiro/specs/accounts-and-instance/design.md`（RelationshipView / RelationshipSerializer / RelationshipStateProvider）、`.kiro/specs/api-foundation/design.md`（Bearer / Scope / Pagination / MastodonError）。
