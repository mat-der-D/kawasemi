# Research & Design Decisions

## Summary

- **Feature**: `timelines`
- **Discovery Scope**: Extension（statuses-core / social-graph / api-foundation の確立済み境界の上に乗るタイムライン集約レイヤー）
- **Key Findings**:
  - 可視性・関係フィルタ・ページネーション・Status 契約はすべて上流に存在し、本 spec は「再実装せず消費する」ことが最重要方針。statuses-core `VisibilityPolicy`、social-graph `FilterQuery`、api-foundation Pagination/Scope/Error をそのまま使う。
  - サイブリング spec の search が確立した「上流投稿データを read-only で導出/消費し、上流→下流の書き込みフックを作らない」パターンを踏襲する。これにより依存方向（timelines → statuses-core / social-graph）を一方向に保てる。
  - 「Streaming が再利用する単一生成点」は共有テーブル（fan-out フィード）ではなく、**単一の membership 判定コンポーネント（`TimelineMatcher`）** として実現するのが最も依存クリーンで二重発火を防げる。MVP は query-on-read で、新規マイグレーション/テーブルを所有しない。

## Research Log

### 上流契約の棚卸し（消費する境界）

- **Context**: 本 spec は何を所有し、何を消費するのかを最初に確定する必要がある（再実装の禁止が方針）。
- **Sources Consulted**: `.kiro/specs/statuses-core/design.md`（`VisibilityPolicy` / `StatusSerializer` / Status モデル / 物理モデル）、`.kiro/specs/social-graph/design.md`（`FilterQuery` / `blocked_set` / `following_set` / follows の `show_reblogs`）、`.kiro/specs/api-foundation/design.md`（`PageParams` / `Cursor` / `Page<T>` / `build_link_header` / `Scope` / `MastodonError` / Bearer）、roadmap.md、brief.md。
- **Findings**:
  - statuses-core: `VisibilityPolicy::is_visible(status, viewer, rel)` が単一の可視性判定。`StatusSerializer::status_to_json(status, ctx)` が viewer 操作状態・reblog ネスト・null 規律を所有。Status モデルは `actor_id` / `visibility` / `reblog_of_id` / `in_reply_to_id` / `local` 等を持つ。物理モデルに `statuses` / `status_media` はあるが、**第一級のハッシュタグ問い合わせ境界は公開されていない**（search の research でも同様に指摘済み）。
  - social-graph: `FilterQuery` が `blocked_set`（blocked / blocked_by / muted / muted_notifications を含む `RelationshipSets`）と `following_set`（フォロー集合）を期限考慮込みで返す。follows 行は `show_reblogs` を保持（ブースト表示可否）し、social-graph はこれを基に **`reblogs_hidden`（`show_reblogs=false` のフォロー先集合）を明示的なクエリとして `FilterQuery` に公開する**。本 spec は当該集合を自前で導出せず、この明示メソッドをそのまま消費する。フィルタ「適用」自体は social-graph では実装しない（9.4）= 本 spec が適用する。
  - api-foundation: ページネーション（`max_id`/`since_id`/`min_id`/`limit` + `Link`）・`read:statuses` を含む Scope 内包判定・Bearer（任意認証モードあり `authenticate` が `Option<RequestActorContext>` を返す）・`MastodonError`・契約ハーネスを「乗るだけ」で提供。
- **Implications**: 本 spec のコンポーネントは (1) 上流 read-only クエリ + (2) 上流フィルタ/可視性の適用 + (3) 上流シリアライザでの具体化、に集約される。新しいエンティティ契約・可視性ロジック・関係状態は一切持たない。

### タイムライン生成方式：query-on-read vs fan-out-on-write

- **Context**: ホームタイムラインの古典的実装（Mastodon の Redis フィード fan-out）を採るか、query-on-read を採るか。これは「単一生成点」「Streaming 再利用」の解釈に直結する。
- **Sources Consulted**: brief.md（「Streaming が同じ結果を二重発火せず再利用できるよう、タイムライン生成のクエリ/生成点を単一に保つ」）、roadmap.md（timelines が「単一生成点」を確立、streaming がそれを再利用）、tech.md（一人鯖・低スペック VPS・DB 完結・外部ブローカー非依存）、statuses-core design（投稿作成は `StatusService.create_status`、受信は `InboundHandlers`。どちらも下流への通知フックを公開していない）。
- **Findings**:
  - fan-out-on-write はホームフィードを書き込み時に各ローカルアクターへ配るが、投稿作成/受信は upstream（statuses-core）が所有し、**下流へ通知するイベントフック（observer 登録境界）は upstream の公開契約に存在しない**。timelines がそれを使うと上流→下流依存になり依存方向に反する。
  - 一人鯖・低スペック VPS では、ローカルアクター数が少なく、`statuses(actor_id)` 等の既存インデックスで query-on-read（フォロー集合に対する IN クエリ）が十分実用的。
  - 「Streaming 再利用」の本質は共有フィードテーブルではなく、**membership 判定ロジックの単一化**（ある投稿がどのタイムラインに属するかの一意な定義）。これがあれば REST はクエリ側、Streaming はルーティング側で同じ定義を共有でき、二重発火（二重定義）が構造的に起きない。
- **Implications**: **query-on-read を採用**。`TimelineMatcher` を「タイムライン membership の唯一の定義点」とし、REST タイムライン取得（候補クエリ + フィルタ適用）と将来の Streaming ルーティング（単一投稿の所属判定）が同一の述語/条件を共有する。これは Requirement 8 を満たす。

### ハッシュタグ照合データの取得

- **Context**: タグタイムラインは投稿をハッシュタグで絞り込む必要があるが、statuses-core は第一級のタグ問い合わせ境界を公開していない（search も同問題に直面し、自前の `search_tags`/`search_status_tags` を 0010 で所有した）。
- **Sources Consulted**: statuses-core requirements 3.6（投稿作成時に本文から mentions/tags/emojis を抽出し Status に反映）、statuses-core design 物理モデル、search research（「statuses-core は tags を抽出・保持するが、検索に最適化された問い合わせ可能インデックスを公開境界として提供していない」「上流投稿の保持タグから read-only 導出」）、roadmap（timelines が tag タイムラインを所有、search はサイブリングで timelines の上流ではない）。
- **Findings**:
  - statuses-core は投稿作成/受信時にハッシュタグを抽出し永続化している（`tags` をシリアライズできる以上、status↔tag の関連が DB に存在する）。
  - search のタグインデックス（0010）はサイブリング所有であり、timelines は依存できない（roadmap の timelines 依存は statuses-core / social-graph のみ）。
  - timelines は query-on-read 方針のため、新規の write 派生インデックスを所有するより、**statuses-core が永続化した status↔tag 関連を read-only で照会する**のが依存クリーンで方針一貫。
- **Implications**: タグタイムラインは statuses-core が永続化したハッシュタグ関連を read-only で消費する（正規化済みタグ名で照合）。statuses-core が status↔tag 関連を問い合わせ可能な形で保持していることを統合前提とし、もし上流がタグ関連を問い合わせ不能な形でしか持たない場合は**コーディネーション/再検証トリガ**とする（下記 Risks 参照）。本 spec は新規テーブルを所有しない。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| Query-on-read + 単一 Matcher（採用） | 候補を統計テーブル無しで上流 `statuses` から読み、上流フィルタ/可視性を適用。membership は単一コンポーネントで定義 | 依存方向クリーン（上流書き込みフック不要）、新規テーブル/マイグレーション不要、Streaming が同一述語を再利用可能、一人鯖規模に十分 | 大規模ではホーム集約のコスト増（一人鯖前提では非問題） | brief「単一生成点」を判定ロジック単一化として実現 |
| Fan-out-on-write（ホームフィード materialization） | 投稿作成時に各ローカルアクターのフィード行を書き込み | 大規模ホーム取得が高速 | upstream→downstream 通知フックが必要（statuses-core 非公開）→依存方向違反。外部ブローカー無し方針とも緊張 | 却下 |
| search のタグインデックスを共有 | タグ照合を search 0010 に委譲 | 重複インデックス回避 | search は timelines の上流ではない（依存方向違反） | 却下 |

## Design Decisions

### Decision: 単一生成点を `TimelineMatcher`（membership 判定）として実現する

- **Context**: brief/roadmap が要求する「Streaming が再利用する単一生成点」を、依存クリーンかつ二重発火しない形で実現する必要がある。
- **Alternatives Considered**:
  1. 共有フィードテーブル（fan-out）— upstream フックが必要で依存方向違反。
  2. membership 判定コンポーネントの単一化（query-on-read）— REST はクエリ側、Streaming はルーティング側で同一述語を共有。
- **Selected Approach**: `TimelineMatcher` を唯一の membership 定義点とする。REST 取得は Matcher の「候補クエリ仕様 + フィルタ述語」を、将来の Streaming は同 Matcher の「単一投稿の所属判定」を呼ぶ。条件（タイムライン種別条件・可視性・ブロック/ミュート）は一箇所に集約。
- **Rationale**: 依存方向を一方向に保ち、二重ロジック化を構造的に排除。一人鯖規模に最適。
- **Trade-offs**: 大規模ホーム集約は不利だが一人鯖前提では非問題。Streaming は本 spec のシームに依存することになる（意図どおり）。
- **Follow-up**: Streaming spec 着手時に Matcher のシーム形が単一投稿ルーティングに十分か再確認する。

### Decision: 可視性・関係フィルタ・ページネーション・Status 契約を再実装しない

- **Context**: ローカルで動くがリモート/投稿側と食い違う最重要リスクを避けるため、判定の出典を一つに保つ。
- **Selected Approach**: 可視性 = statuses-core `VisibilityPolicy`、関係集合 = social-graph `FilterQuery`、ページネーション = api-foundation Pagination、Status JSON = statuses-core `StatusSerializer` をそのまま消費。
- **Rationale**: steering「可視性判定は共通コードパス」「契約の集約」に直接整合。
- **Trade-offs**: 上流契約変更が本 spec の再検証トリガになるが、整合性の利得が上回る。
- **Follow-up**: 解決済み。social-graph が `FilterQuery` に明示的な `reblogs_hidden`（`show_reblogs=false` のフォロー先集合）クエリを追加したため、本 spec はそれを直接消費する（下記 Risks 参照、再検証トリガは解消）。

## Migration Numbering Coordination

- **既存の使用番号**: 0001 core-runtime / 0002 actor-model / 0003 api-foundation（`0003_oauth.sql`）/ 0004 media-pipeline / 0005 accounts-and-instance / 0006 social-graph / 0007 statuses-core / 0008 federation-core（`0008_federation.sql`）/ 0010 search。
- **本 spec の選択**: timelines は **query-on-read 方針で新規の永続テーブルを所有せず、マイグレーション番号を一切所有/予約しない**。以前 timelines が確保していた予約番号 `0008` は federation-core が `0008_federation.sql` として取得したため、本 spec は当該予約を解放する。将来タイムライン高速化のための非正規化フィード/インデックスを導入する必要が生じた場合は、その時点で空き番号を新規に確保する（事前予約はしない）。
- **理由**: 上流に書き込みフックが無く依存方向を保つには read-only 消費が最適で、テーブル所有は不要。番号を予約しないことで federation-core=0008 / notifications=0009 / search=0010 の前提と衝突しない。

## Risks & Mitigations

- **上流タグ関連の問い合わせ可能性** — statuses-core が status↔tag 関連を問い合わせ可能な形で永続化していない場合、タグタイムラインが実装できない。Mitigation: 統合前提として明記し、不足時は再検証トリガとして statuses-core にタグ問い合わせ境界（または read-only ビュー）を求める。本 spec は新規インデックスを所有しない方針を維持。
- **`reblogs_hidden`（ブースト表示可否）の問い合わせ** — ホームのブースト除外（Req 1.4）に必要。**解決済み**: social-graph が `FilterQuery` に明示的な `reblogs_hidden`（`show_reblogs=false` のフォロー先集合）クエリを追加したため、本 spec はこの明示メソッドを消費し、集合を自前導出しない。必要シグネチャは充足済みで、当該再検証トリガは解消した。
- **query-on-read のフィルタ後ページ充填** — 可視性/ブロック/ミュート除外後にページ件数が不足しうる。Mitigation: 候補を多めに取得しフィルタ後に `limit` 件へ充填する安定カーソル戦略（欠落・重複・無限ループ無し、Req 7.4）。
- **ホーム集約のコスト** — フォロー集合 IN クエリ + フィルタが低スペック VPS で重くなりうる。Mitigation: 既存 `statuses(actor_id)` インデックス活用と件数上限。大規模化が必要になった時点で materialization 用の空きマイグレーション番号を新規確保する（事前予約はしない）。

## References

- `.kiro/specs/statuses-core/design.md` — `VisibilityPolicy` / `StatusSerializer` / Status モデル / 物理モデル（消費元）
- `.kiro/specs/social-graph/design.md` — `FilterQuery`（`blocked_set` / `following_set` / 明示的 `reblogs_hidden`（`show_reblogs=false` 集合））（消費元）
- `.kiro/specs/api-foundation/design.md` — Pagination / Scope / MastodonError / Bearer（消費元）
- `.kiro/specs/search/design.md` / `research.md` — 上流タグの read-only 消費パターン・マイグレーション番号調整の前例
- `.kiro/steering/tech.md` / `structure.md` / `product.md` — 一人鯖・DB 完結・共通コードパス・契約集約の方針
