# Implementation Plan

- [ ] 1. 基盤: ドメイン型と種別条件
- [x] 1.1 タイムラインのドメイン型を定義する
  - `TimelineKind`（Home/Public/Local/Tag）・`TimelineParams`（local/remote/only_media/tag/PageParams）・`TagFilter`（primary/any/all/none）・`TimelineQuerySpec`・`FilterContext`（viewer + blocked/blocked_by/muted/following/reblogs_hidden 集合 + now）を core-runtime の Id/時刻型・api-foundation `PageParams` の上に定義する
  - 観測可能な完了: 各型がコンパイルでき、`TimelineKind` が 4 種、`TagFilter` が any/all/none を表現し、`FilterContext` が関係集合を保持する（型の単体テストがグリーン）
  - _Requirements: 1.1, 2.1, 3.1, 4.1, 7.1, 8.1_
  - _Boundary: model_
- [x] 1.2 タイムライン種別条件を定義する
  - 種別ごとの集約条件を単一点で定義する：home=投稿者がフォロー集合∪自分・`direct`除外・ブースト含む、public/local/tag=`public`限定かつブースト除外、local=ローカル投稿者限定、tag=正規化タグ名で照合（any/all/none）
  - 観測可能な完了: 各 `TimelineKind` に対し条件が一意に決まり、public/local/tag が public 限定かつ非ブースト、home のみ direct 以外とブーストを含むことが単体テストで確認できる
  - _Requirements: 1.1, 1.3, 2.1, 2.2, 3.1, 3.2, 4.1, 4.2, 4.3_
  - _Boundary: TimelineKindRules_

- [ ] 2. データ層（候補取得）
- [x] 2.1 候補リポジトリを read-only で実装する
  - 種別条件をカーソル範囲（投稿 id・新しい順）の SQL に変換し、statuses-core の `statuses` / `status_media` / ハッシュタグ関連を read-only 照会して候補を取得する（home=following∪self、public=public非ブースト、local=加えてローカル、tag=正規化タグ照合 any/all/none）。`local`/`remote`/`only_media` を WHERE で絞り、フィルタ後充填のため `limit` より多めのバッチ取得に対応する
  - 観測可能な完了: 各種別で種別条件を満たす候補が id 降順で取得でき、`only_media`/`local`/`remote`/タグ条件が反映され、上流テーブルを一切書き換えない（リポジトリ統合テストがグリーン）
  - _Requirements: 1.1, 2.1, 2.3, 2.4, 2.5, 3.1, 3.4, 4.1, 4.4, 4.5, 7.1, 7.4_
  - _Boundary: CandidateRepository_

- [ ] 3. フィルタと単一生成点
- [x] 3.1 タイムラインフィルタを実装する
  - 候補に statuses-core `VisibilityPolicy::is_visible` を適用して不可視を除外（未認証 viewer は public のみ）し、social-graph `FilterQuery` 由来の blocked/blocked_by/muted（期限考慮）/ following / reblogs_hidden を `FilterContext` で束ねて投稿者・ブースト実行者・被ブースト元投稿者の関係除外とブースト表示可否を適用する。可視性・関係判定は再実装せず上流へ委譲する
  - 観測可能な完了: ブロック/被ブロック/ミュート対象が除外され、`show_reblogs` 無効フォローのブーストと被ブースト元が関係対象のブーストが除外され、未認証文脈で public のみ通過する（フィルタ単体テストがグリーン）
  - _Requirements: 1.2, 1.4, 1.5, 2.6, 3.3, 4.6, 5.1, 5.2, 5.3, 5.4, 6.1, 6.2, 6.3, 6.4_
  - _Boundary: TimelineFilter_
- [x] 3.2 単一生成点（TimelineMatcher）を実装する
  - 種別条件（`TimelineKindRules`）をカーソル付きクエリ仕様へ変換する `candidate_spec` と、単一投稿の所属を判定する `matches`（種別条件 + 可視性 + 関係フィルタ）を、REST 取得とロジックを二重定義せず同一の `TimelineKindRules`/`TimelineFilter` の上に実装する。配信そのものは含めない
  - 観測可能な完了: `matches` の判定が候補クエリ + フィルタ適用と同一結果を返し、下流が再利用可能なシームとして公開される（Matcher 単体テストがグリーン）
  - _Requirements: 8.1, 8.2, 8.4_
  - _Boundary: TimelineMatcher_
  - _Depends: 1.2, 3.1_

- [ ] 4. 具体化とサービス
- [ ] 4.1 (P) ステータス具体化を実装する
  - フィルタ後の候補を statuses-core `StatusSerializer::status_to_json` で Status JSON へ具体化し、viewer 操作状態（favourited/reblogged/bookmarked 等）反映のため閲覧者文脈を渡し、ブーストは `reblog` ネスト表現とする。Account・メディア等の埋め込みは上流委譲とし独自表現を持たない
  - 観測可能な完了: タイムライン要素が投稿 API と同一の Status JSON 形で具体化され、認証文脈で操作状態が反映され、ブーストが `reblog` にネストされる（具体化単体テストがグリーン）
  - _Requirements: 10.1, 10.2, 10.3, 10.4_
  - _Boundary: StatusHydrator_
- [ ] 4.2 タイムラインサービスを実装する
  - 閲覧者の関係集合を `FilterQuery` から 1 回ロードして `FilterContext` を作り、`TimelineMatcher.candidate_spec`→候補取得→`TimelineFilter.keep`→`limit` 件へのフィルタ後充填（不足時は次カーソルバッチ取得・欠落/重複/無限ループ無し）→具体化→前後カーソル付き `Page` 組み立てを実装する。ページネーションは api-foundation 規約（max_id/since_id/min_id/limit・Link）に乗せる
  - フィルタ後充填ループに小さな固定の反復回数上限を設け、ブロック/ミュートが多い閲覧者やタグが疎な照会でもほぼ全件走査に陥らないようにする。上限に達した場合はエラーやブロッキングにせず、その時点までの蓄積結果（`limit` 未満になり得る部分ページ）と続きから再開可能な有効なカーソルを返し、上限到達を診断用の構造化ログイベントとして記録する
  - 観測可能な完了: 各種別で閲覧者にとって membership を満たす可視投稿が id 降順・`limit` 件以内・安定カーソルで返り、フィルタ後充填で件数が満たされる。反復回数上限に達した場合も無限ループ・エラー化せず、部分ページと有効な継続カーソルが返る（サービス統合テストがグリーン）
  - _Requirements: 1.1, 2.1, 3.1, 4.1, 5.1, 6.1, 7.1, 7.2, 7.3, 7.4, 10.1_
  - _Boundary: TimelineService_
  - _Depends: 2.1, 3.1, 3.2, 4.1_

- [ ] 5. エンドポイントと配線
- [ ] 5.1 タイムラインエンドポイントを実装する
  - `GET /api/v1/timelines/home`（Bearer + `read:statuses`、未認証 401）・`GET /api/v1/timelines/public`（任意認証、local/remote/only_media、未認証は public のみ）・`GET /api/v1/timelines/tag/:hashtag`（任意認証、any[]/all[]/none[]/local/only_media）の HTTP ハンドラを実装し、スコープ検証・Mastodon 互換エラー・`Link` ヘッダ付与を適用する（local TL は public?local=true 経路）
  - 観測可能な完了: 各エンドポイントが正しい応答コード（200/401/403）とスコープ検証で動作し、未認証の public/local/tag が公開投稿のみを返し、応答に Link ヘッダが付く（エンドポイント統合テストがグリーン）
  - _Requirements: 1.1, 1.6, 2.1, 2.3, 3.1, 4.1, 7.2, 9.1, 9.2, 9.3, 9.4_
  - _Boundary: TimelineEndpoints_
  - _Depends: 4.2_
- [ ] 5.2 モジュール配線と Matcher シーム公開を行う
  - `TimelinesModule` を構築して各コンポーネントを束ね、`VisibilityPolicy`（statuses-core）/ `FilterQuery`（social-graph）/ `StatusSerializer`（statuses-core）/ Pagination（api-foundation）を結線し、タイムラインルータを土台へ装着、`TimelineMatcher` を下流 streaming が再利用できる公開シームとして `AppState` に格納する
  - 観測可能な完了: アプリ起動時に home/public/tag のルートが有効になり、`TimelineMatcher` が `AppState` から参照可能になる（起動・配線テストで E2E にタイムライン取得が一気通貫で動く）
  - _Requirements: 8.1, 8.3_
  - _Boundary: TimelinesModule, server, bootstrap, state_
  - _Depends: 5.1_

- [ ] 6. 検証
- [ ] 6.1 統合テスト（home/public/local/tag・フィルタ・ページネーション・認証）を整備する
  - フォロー/ブロック/ミュート/可視性の各状態を作り、home（フォロー+自分・direct除外・show_reblogs無効ブースト除外・未認証401）・public/local（public限定・ブースト除外・local/remote/only_media・未認証は公開のみ）・tag（正規化照合・any/all/none・local/only_media）・可視性（private のフォロー反映・ローカル/リモート同一）・ページネーション（max_id/since_id/min_id/limit・Link・フィルタ後充填で欠落/重複/無限ループ無し）を `spawn_test_app` 上で検証する。加えて、充填ループが反復回数上限に到達するケース（ブロック/ミュートが多い・タグが疎など）で、エラー化せず部分ページと有効な継続カーソルが返ることを検証する
  - 観測可能な完了: 上記シナリオおよび充填上限到達時の部分ページ・継続カーソル返却の統合テストが全てグリーンになる
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 2.1, 2.2, 2.3, 2.4, 2.5, 2.6, 3.1, 3.2, 3.3, 3.4, 4.1, 4.2, 4.3, 4.4, 4.5, 4.6, 5.1, 5.2, 5.3, 5.4, 6.1, 6.2, 6.3, 6.4, 7.1, 7.2, 7.3, 7.4, 9.1, 9.2, 9.3, 9.4_
  - _Boundary: TimelineService, TimelineEndpoints, TimelineFilter_
  - _Depends: 5.2_
- [ ] 6.2 (P) 単一生成点の一致と契約テストを整備する
  - REST タイムライン取得結果と `TimelineMatcher.matches` の単一投稿判定が同一 membership になることを検証し、タイムライン応答が statuses-core Status ゴールデンと整合（viewer 操作状態・reblog ネスト・null 規律を上流から継承）することを api-foundation 契約ハーネスで検証、実クライアントキャプチャをフィクスチャ登録する
  - 観測可能な完了: REST と membership 判定の一致テストがグリーンになり、タイムライン応答の Status 契約テストが決定的に再現する
  - _Requirements: 8.1, 8.2, 10.1, 10.2, 10.3, 10.4_
  - _Boundary: TimelineMatcher, StatusHydrator_
  - _Depends: 5.2_

## Implementation Notes

- タスク 1.2（`TimelineKindRules`）実装時、`src/timelines/kind_rules.rs` に候補投稿を表す `pub struct TimelineCandidate { author, local, visibility, is_boost, tags }` を導入した（`model.rs` はタスク 1.1 の境界上、候補形状の型をあえて定義していない）。この型は `TimelineKindRules::matches`/`matches_home`/`matches_public`/`matches_local`/`matches_tag` の実引数型であり、`CandidateRepository`（タスク 2.1）が読み取る `statuses` 行や `TimelineMatcher`（タスク 3.2）が呼び出す形状と一致させる必要がある（「条件の二重定義禁止」）。タスク 2.1 の実装者は、実際のリポジトリ行形状から `TimelineCandidate` を構築するか、両者の候補形状を意図的にすり合わせること — 暗黙に発散させないこと。
- タスク 2.1（`CandidateRepository`）実装時、`fetch_candidates` の SQL は `local`/`remote`/`only_media` 絞り込みを 4 種別すべて（home 含む）に一様適用した。要件 2.3-2.5/3.4/4.5 は public/local/tag のみを明示するが、`TimelineParams`（タスク 1.1 で確定済み）に種別ごとの絞り込み可否を表すフィールドが無いため、home を除外する分岐は本タスクの境界（`model.rs` 非改変）内では表現できない。現状は home 向けリクエストでこれらのフィールドを立てて呼ぶ呼び出し元が存在しないため実害はないが、`TimelineService`（タスク 4.2）・`TimelineEndpoints`（タスク 5.1）はこの前提（= home リクエストでは `local`/`remote`/`only_media` を立てて渡さない）を暗黙に守る必要がある。design.md 側でも home にこれらの絞り込みを明示的に許可/禁止する記述は無く、対称的な現状維持（=一様適用のまま）が妥当と判断した。
- タスク 2.1 実装時、design.md の `fetch_candidates(pool, spec, batch_limit)` インターフェースには home 種別が必要とする「following ∪ self」ID 配列を渡す引数が無かった（design.md 自身が `TimelineQuerySpec` にこの一覧を含めていない）。最も保守的な解決として、`fetch_candidates` に `following_and_self: &[Id]`（home 以外では無視される）引数を追加する形で単一の呼び出し窓口を維持した（`src/timelines/candidate_repository.rs` の該当関数のドキュメントコメントに理由を記載）。`TimelineMatcher`（タスク 3.2）・`TimelineService`（タスク 4.2）はこの拡張済みシグネチャを呼び出し元として踏襲すること。
- タスク 3.1（`TimelineFilter`）実装時、design.md の `keep(&self, status: &Status, ctx: &FilterContext) -> bool` インターフェースには要件 6.3（ブースト実行者または被ブースト元投稿の投稿者がブロック/被ブロック/ミュート対象の場合にブーストを除外）が必要とする「被ブースト元投稿の投稿者」を渡す経路が無かった（ブースト自体は `statuses` の別行として `actor_id`=ブースト実行者で永続化され、被ブースト元の投稿者は単一の `Status` から到達不能。`src/statuses/interaction_service.rs::reblog` で確認済み）。タスク 2.1 の前例（`following_and_self` 追加）に倣い、`keep` に `reblogged_author: Option<Id>`（非ブースト候補やブースト対象外の種別では無視される）引数を追加する形で単一の呼び出し窓口を維持した（`src/timelines/filter.rs` のモジュールドキュメントコメントに理由を記載）。`reblogged_author` が `None` の場合は 6.3 のうち被ブースト元投稿者側の除外のみをスキップし（ブースト実行者側の関係除外・`reblogs_hidden` 除外は無条件に適用される）、fail-open（見せすぎ）ではなく実害の少ない側に倒す設計判断とした。`TimelineMatcher`（タスク 3.2）・`TimelineService`（タスク 4.2）はこの拡張済みシグネチャを呼び出し元として踏襲し、可能な限り被ブースト元投稿者を解決して渡すこと。
