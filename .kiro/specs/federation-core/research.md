# Research & Design Decisions

## Summary

- **Feature**: `federation-core`
- **Discovery Scope**: Complex Integration（上流 core-runtime / actor-model に乗る新規連合基盤。外部 ActivityPub 実装との相互運用を伴う）
- **Key Findings**:
  - HTTP Signatures は draft-cavage（Mastodon 系の事実上の標準）と RFC 9421（新標準）が混在し、相手の対応形式が事前に分からない。double-knocking（片方で失敗したらもう片方で再送し、成功形式を host 単位で記憶）が相互運用の現実解。
  - 「意味論は対称・物理配送のみ最適化」は、配送の共通部（Activity 生成・検証・宛先解決）と分岐部（in-process / HTTP）を型と関数境界で分離し、ローカル経路も同一の受理・ディスパッチ経路を通すことで担保できる。これを連合テスト（2 インスタンス往復）で結果同値検証する。
  - 本 spec は Activity 種別の意味論を持たない。受信はディスパッチ境界（trait レジストリ）へ、送信は配送境界へ業務側を接続する。ブロック判定・可視性判定も委譲境界として外出しし、コア状態モデルを汚さない。

## Research Log

### HTTP Signatures の二系統と double-knocking

- **Context**: brief が draft-cavage + RFC 9421 + double-knocking フォールバックを必須としている。両形式は署名対象ヘッダの選び方・署名ヘッダ名・メタデータ表現が異なる。
- **Sources Consulted**: draft-cavage-http-signatures、RFC 9421（HTTP Message Signatures）、Mastodon / Mastodon 系実装の相互運用慣行、`docs/fediverse-design.md`（一次情報）。
- **Findings**:
  - draft-cavage は `Signature` ヘッダに `keyId` / `headers` / `signature` を持ち、`(request-target)` 疑似ヘッダと `Date` / `Host` / `Digest` を署名対象にするのが一般的。
  - RFC 9421 は `Signature-Input` / `Signature` を分離し、署名対象を構造化フィールドで宣言する。
  - 受信側は両形式を検出して検証できる必要があり、送信側は相手の対応が不明なため double-knocking が必要。成功形式を host 単位でキャッシュすると再交渉コストを抑えられる。
- **Implications**: 署名は「アルゴリズムスイート（形式差を吸収）」「署名器（送信）」「検証器（受信）」「形式交渉（double-knock + host 能力記憶）」に分解する。検証・公開鍵取得・ネットワークはモック可能境界にし、決定性テストを可能にする。

### 「意味論対称・物理配送のみ最適化」の構造化

- **Context**: steering tech.md / structure.md / roadmap.md が本制約を最重要シームと明記。ショートカット（in-process）が JSON-LD/署名往復を通らずローカルだけ通るバグを生む点が懸念。
- **Sources Consulted**: `.kiro/steering/tech.md`、`.kiro/steering/structure.md`、roadmap.md「Shared seams to watch」。
- **Findings**:
  - 共通部: Activity の正規 JSON-LD 生成・検証・宛先（recipient → 物理ターゲット）解決。
  - 分岐部: 物理配送のみ（local in-process sink / remote HTTP sink）。
  - ローカル in-process でも、リモート受信と同一の受理・ディスパッチ経路（署名検証を除く意味論処理）を通すことで結果同値を担保。
- **Implications**: `DeliveryService` が共通部を持ち、`DeliverySink`（local/remote 実装）に分岐を閉じ込める。連合テストで「同一 Activity → ローカル配送結果 == HTTP 配送結果」を検証する。

### 受信ディスパッチ・ブロック・可視性の委譲境界

- **Context**: 本 spec は Activity 種別の業務処理を持たない（Out of Boundary）。一方でブロック先署名拒否は本 spec の責務。
- **Sources Consulted**: brief.md Scope/Out of Boundary、actor-model design.md（ActorDirectory）、roadmap.md（social-graph がブロックを所有）。
- **Findings**:
  - 受信 Activity 処理は trait レジストリ（種別→ハンドラ）で下流が登録する。
  - ブロック判定は `BlockPolicy` 委譲境界（既定は拒否なし、social-graph が実装供給）。
  - 公開鍵供給（ローカル）・ハンドル解決は actor-model `ActorDirectory` を消費。署名生成鍵は core-runtime `SigningKeyProvider`。
- **Implications**: federation-core は「連合プロトコルの配管」に徹し、意味論・ブロック実体・アクター鍵保管を持たない。境界 trait を本 spec が定義し、下流が埋める。

### リモート公開鍵取得の最小範囲

- **Context**: 署名検証にはリモートアクターの公開鍵が要る。一方リモートアクターの完全プロフィール永続化は accounts-and-instance の責務。
- **Findings**: 本 spec は署名検証に必要な公開鍵素材（keyId・公開鍵 PEM・所有アクター URI）のみを取得・キャッシュする。完全なリモートアクターモデル化は行わない。
- **Implications**: `remote_public_keys` キャッシュテーブルと `PublicKeyResolver` 境界（ネットワーク取得をモック可能に）を本 spec が所有。プロフィール永続化は下流へ。

### マイグレーション番号の割り当て（クロス spec 衝突回避）

- **Context**: 当初 federation-core の連合用マイグレーションは `0003_federation.sql` を予定していたが、api-foundation が同じ `0003` スロットを `0003_oauth.sql` で確定使用しており、番号が衝突する。クロス spec レビューで検出。
- **Findings**:
  - api-foundation は `0003_oauth.sql` を維持する（federation-core が譲る）。
  - `0008` スロットは従来 timelines spec が「マイグレーション無し予約」として確保していたが、当該予約は解放された。
  - federation-core の連合テーブル（`delivery_jobs` / `received_activities` / `remote_public_keys` / `instance_signature_capabilities`）への他 spec からの FK は存在しないため、federation DDL を `0008` で適用しても適用順序は安全（order-safe）。
- **Decision**: federation-core のマイグレーションを `0003_federation.sql` から `0008_federation.sql` へ改番する。**`0008` スロットは以後 federation-core が所有する。**
- **Implications**: design.md / tasks.md / research.md 上の番号表記を `0008_federation.sql` に統一。実テーブル定義・索引・制約の内容は変更しない。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| Ports & Adapters（採用） | プロトコル配管をコアに、ネットワーク/署名/ブロック/ディスパッチ/鍵供給を port に | モック可能境界・決定性・委譲境界が自然に表現でき、意味論対称を構造で担保 | port 数が多く配線が増える | steering「注入可能な非決定性境界」「レイヤー分離」に合致 |
| 直接結合（ハンドラ内で署名・配送を直書き） | エンドポイント実装に処理を集約 | 初期実装が速い | モック不能・ローカル/HTTP 分岐が散在し対称性が壊れる | 却下 |
| 外部メッセージブローカで配送 | 配送を外部キューへ | スケール容易 | steering「外部ブローカー非依存・DB 完結」に違反 | 却下（DB キュー採用） |

## Design Decisions

### Decision: 署名形式差をスイート境界で吸収し double-knock を host 能力キャッシュで最適化

- **Context**: draft-cavage / RFC 9421 双方対応 + 相手形式不明。
- **Alternatives Considered**:
  1. 常に draft-cavage 送信のみ — RFC 9421 専用実装と相互運用できない。
  2. 双方を並行送信 — 二重配送・冪等性リスク。
- **Selected Approach**: `SignatureSuite`（draft/rfc9421）を抽象化し、`SignatureNegotiator` が host 能力を `instance_signature_capabilities` に記録。未知 host は片方→拒否なら他方で再送し、成功形式を記録。
- **Rationale**: 相互運用と再交渉コスト削減を両立。
- **Trade-offs**: 初回送信で 1 往復余分になりうる。記録により以降は解消。
- **Follow-up**: 署名関連拒否（401/403 や署名エラー）と一般失敗の区別を実装時に厳密化。

### Decision: 配送は共通部 + DeliverySink 分岐、ローカルは受信経路を再利用

- **Context**: 意味論対称・物理配送のみ最適化。
- **Selected Approach**: `DeliveryService` が正規 Activity 生成・検証・宛先解決を共通実行し、`LocalDeliverySink` は `InboxService` の意味論処理（署名検証を除く）を in-process 呼び出し、`HttpDeliverySink` は署名付き HTTP 送信を DB キュー経由で実行。
- **Rationale**: ローカルだけ通るバグを構造で排除し、連合テストで結果同値を担保。
- **Trade-offs**: ローカル配送も JSON-LD 生成・検証コストを払う（意図的。対称性の代償）。
- **Follow-up**: 連合テストでローカル/HTTP の業務処理結果同値を必ず assert。

### Decision: 受信 Activity 処理・ブロック・可視性を委譲境界として外出し

- **Context**: Activity 種別意味論・ブロック実体は下流所有。
- **Selected Approach**: `InboundActivityDispatcher`（種別→`InboundActivityHandler` レジストリ）、`BlockPolicy` trait（既定 no-op）、可視性/addressing は配送依頼時に呼び出し側が確定した recipient 集合を渡す形で受け取る。
- **Rationale**: コア状態モデルへ意味論・ブロックを漏らさない。
- **Trade-offs**: 下流が境界を埋めるまで実機能は不完全（テストはスタブハンドラで成立）。
- **Follow-up**: 下流 spec の再検証トリガを Revalidation Triggers に明記。

## Risks & Mitigations

- 署名形式の検出誤りで誤検証 — 形式ごとの検証単体テスト + 既知実装のフィクスチャ再生で担保。
- in-process 配送がリモート経路と乖離 — ローカル/HTTP 結果同値の連合テストを必須化。
- 公開鍵キャッシュの陳腐化（鍵ローテーション後の検証失敗）— 検証失敗時にキャッシュを無効化し再取得する経路を用意。
- 配送キューの無限再試行 — 再試行上限と恒久失敗記録で抑止。
- shared inbox の二重配送 — 宛先解決時に共有 inbox 単位で重複排除。

## References

- [RFC 9421 HTTP Message Signatures](https://www.rfc-editor.org/rfc/rfc9421) — RFC 9421 署名形式の一次情報。
- [draft-cavage-http-signatures](https://datatracker.ietf.org/doc/html/draft-cavage-http-signatures-12) — draft-cavage 署名形式（Fediverse の事実上標準）。
- [ActivityPub W3C Recommendation](https://www.w3.org/TR/activitypub/) — inbox/outbox/配送・コレクションのプロトコル定義。
- [WebFinger RFC 7033](https://www.rfc-editor.org/rfc/rfc7033) — `acct:` 解決と JRD。
- [NodeInfo schema](http://nodeinfo.diaspora.software/) — NodeInfo ディスカバリとドキュメント。
- `.kiro/steering/tech.md` / `.kiro/steering/structure.md` — 意味論対称・物理配送最適化・注入境界の一次方針。
- `.kiro/specs/core-runtime/design.md` / `.kiro/specs/actor-model/design.md` — 上流の注入境界・ActorDirectory・署名鍵供給契約。
