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
- [x] 4.1 (P) ステータス具体化を実装する
  - フィルタ後の候補を statuses-core `StatusSerializer::status_to_json` で Status JSON へ具体化し、viewer 操作状態（favourited/reblogged/bookmarked 等）反映のため閲覧者文脈を渡し、ブーストは `reblog` ネスト表現とする。Account・メディア等の埋め込みは上流委譲とし独自表現を持たない
  - 観測可能な完了: タイムライン要素が投稿 API と同一の Status JSON 形で具体化され、認証文脈で操作状態が反映され、ブーストが `reblog` にネストされる（具体化単体テストがグリーン）
  - _Requirements: 10.1, 10.2, 10.3, 10.4_
  - _Boundary: StatusHydrator_
- [x] 4.2 タイムラインサービスを実装する
  - 閲覧者の関係集合を `FilterQuery` から 1 回ロードして `FilterContext` を作り、`TimelineMatcher.candidate_spec`→候補取得→`TimelineFilter.keep`→`limit` 件へのフィルタ後充填（不足時は次カーソルバッチ取得・欠落/重複/無限ループ無し）→具体化→前後カーソル付き `Page` 組み立てを実装する。ページネーションは api-foundation 規約（max_id/since_id/min_id/limit・Link）に乗せる
  - フィルタ後充填ループに小さな固定の反復回数上限を設け、ブロック/ミュートが多い閲覧者やタグが疎な照会でもほぼ全件走査に陥らないようにする。上限に達した場合はエラーやブロッキングにせず、その時点までの蓄積結果（`limit` 未満になり得る部分ページ）と続きから再開可能な有効なカーソルを返し、上限到達を診断用の構造化ログイベントとして記録する
  - 観測可能な完了: 各種別で閲覧者にとって membership を満たす可視投稿が id 降順・`limit` 件以内・安定カーソルで返り、フィルタ後充填で件数が満たされる。反復回数上限に達した場合も無限ループ・エラー化せず、部分ページと有効な継続カーソルが返る（サービス統合テストがグリーン）
  - _Requirements: 1.1, 2.1, 3.1, 4.1, 5.1, 6.1, 7.1, 7.2, 7.3, 7.4, 10.1_
  - _Boundary: TimelineService_
  - _Depends: 2.1, 3.1, 3.2, 4.1_

- [ ] 5. エンドポイントと配線
- [x] 5.1 タイムラインエンドポイントを実装する
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
- タスク 3.2（`TimelineMatcher`）実装時、design.md の `candidate_spec(&self, kind: TimelineKind, params: &TimelineParams, ctx: &FilterContext) -> TimelineQuerySpec` と `matches(&self, status: &Status, kind: TimelineKind, params: &TimelineParams, ctx: &FilterContext) -> bool` のスケッチに対し、2 点の乖離を記録した（詳細は `src/timelines/matcher.rs` のモジュールドキュメントコメント「Signature deviation #1」「Signature deviation #2」に理由を記載）。(1) `candidate_spec` はシグネチャの四引数形状（`&self` 含む、design.md のスケッチどおり）を維持しつつ `ctx: &FilterContext` を `_ctx` として受け取るのみで読まない — `TimelineQuerySpec`（タスク 1.1 の `model.rs`、本タスクの境界外）には関係集合を渡すフィールドが無く、戻り値へ経路が存在しないため（home の following 集合はタスク 2.1 の前例に倣い、`candidate_spec`/`TimelineQuerySpec` を経由せず `TimelineService`（タスク 4.2）が `fetch_candidates` へ直接渡す責務のまま）。(2) `matches` は `ctx` の手前に `tags: &HashSet<String>`（`Status` にはタグフィールドが無く、`TimelineKind::Tag` 判定に必要 — Tag 以外の種別では空集合を渡せばよい）と `reblogged_author: Option<Id>`（タスク 3.1 の `TimelineFilter::keep` と同一の理由でブースト元投稿者を `keep` へそのまま転送するため）の 2 引数を追加した。`TimelineService`（タスク 4.2）はこの拡張済みシグネチャ（`candidate_spec`/`matches` とも）を呼び出し元として踏襲すること。
- タスク 4.1（`StatusHydrator`）実装時、design.md の `hydrate(&self, statuses: &[Status], viewer: Option<Id>, now: OffsetDateTime) -> Vec<serde_json::Value>` スケッチは、実際の statuses-core `status_to_json(input: &StatusRenderInput) -> Value`（design.md のスケッチにある `status_to_json(status, ctx)` とは既に乖離済み）が要求する `StatusRenderInput` 組み立てに必要な `origin: &ForwardedOrigin`（Account/media/tags の URL 組み立てに必須）を渡す経路が無かった。タスク 2.1/3.1/3.2 の前例（`following_and_self`/`reblogged_author`/`tags` 追加）に倣い、`hydrate` に `ctx: &FilterContext`（`viewer`/`now`/`muted` 集合をまとめて受け取る。`StatusInteractionState::muted` は statuses-core 自身の境界外で「呼び出し元が供給する」設計のため、`ctx.muted` をそのまま使う）と `origin: &ForwardedOrigin` を追加し、`async fn ... -> Result<Vec<Value>, AppError>` とした（DB アクセスを伴うため）。**レビュー第1ラウンドで重大な可視性欠落を検出・修正**: ブースト（`reblog_of_id.is_some()`）の被ブースト元投稿を取得する際、当初は `status_repository::find_by_id` の生の結果をそのまま `reblog` へネストしており、被ブースト元投稿者に対する可視性再チェックが欠落していた。`TimelineFilter::keep`（タスク 3.1）はブースト行自身の可視性（ブースト実行者との関係）のみを検証し、被ブースト元投稿の実際の投稿者との関係は検証しないため、閲覧者がフォローしているアカウントが、閲覧者がフォローしていない別アカウントの `Private`（フォロワー限定）投稿をブーストした場合、その非公開投稿の全文が漏洩する脆弱性だった（`src/statuses/endpoints.rs::render_status_json` は `StatusService::show` 経由でこの再チェックを行っており、本タスクはそれを見落としていた）。修正: `TimelineFilter::keep` が使う同じ純関数（`crate::statuses::visibility::is_visible` + `ViewerRelation`）を被ブースト元投稿者自身に対して再適用し、不可視の場合は欠落行と同じ扱い（`reblog: None`）にフォールバックする（`src/timelines/hydrator.rs::reblog_target_visible`）。この漏洩シナリオと、被参照 `reblog_of_id`/`poll_id` が存在しない場合の graceful degradation は、いずれも統合テストで検証済み（`tests/timeline_hydrator_it.rs`）。`TimelineService`（タスク 4.2）はこの拡張済みシグネチャ（`ctx`/`origin` 引数、`Result` 戻り値）を呼び出し元として踏襲すること。
- タスク 4.2（`TimelineService`）実装時、design.md の `timeline(&self, kind: TimelineKind, viewer: Option<&RequestActorContext>, params: TimelineParams) -> Result<Page<serde_json::Value>, AppError>` スケッチに対し、2 点の乖離を記録した（詳細は `src/timelines/service.rs` のモジュールドキュメントコメント「Deliberate deviations」に理由を記載）。(1) `viewer: Option<&RequestActorContext>` を `viewer_id: Option<Id>` に変更 — `social-graph::FollowService` の同種の前例（`RequestActorContext` は oauth スコープ等 `timelines` が使わない情報まで運ぶため、実際に必要な `actor_id` のみを受け取る）に倣った。スコープ検証と `RequestActorContext -> Id` 抽出自体は `TimelineEndpoints`（タスク 5.1）の責務のまま。(2) `origin: &ForwardedOrigin` 引数を追加 — タスク 4.1 の `StatusHydrator::hydrate` が既に必要としていた引数で、design.md のスケッチはその乖離より前のもの。フィルタ後充填ループは設計定数 `MAX_FILL_ITERATIONS`（目安 5 反復）で打ち切り、各バッチの上限カーソルは直前バッチの最小取得 id（除外）から縮めていくため取りこぼし・重複が起きない。**レビュー第1ラウンドで検出**: 上限到達かつ生存候補 0 件の場合、`paginate` 単体では `next_cursor: None` を返し「継続可能」要件（7.4）に反するため、その場合に限り最後に走査した候補 id を `next_cursor` として上書きするフォールバック（"Cap-hit cursor override"）を追加し、実際に再開可能であることを統合テストで検証した。また同ラウンドで、フィルタ後に生存者が 0 件になる「2 バッチ目取得」シナリオのテストのフィクスチャ順序（ブースト対象より可視投稿の id が新しい）が誤っており、ループが実際には 1 バッチで終わってしまう vacuous なテストだったため、フィクスチャ順序を修正（可視投稿を古い id、フィルタ対象を新しい id に）し、意図的にループを 1 バッチに固定した壊れた実装に対してテストが失敗することを確認した上で復元した。`reblogged_author` の解決は `StatusHydrator::hydrate_one`（タスク 4.1）と同一の `status_repository::find_by_id` ルックアップを再利用し、別ロジックを持たない。`TimelineEndpoints`（タスク 5.1）はこの拡張済みシグネチャ（`viewer_id`/`origin` 引数）を呼び出し元として踏襲すること。
- タスク 5.1（`TimelineEndpoints`）実装時、`home_timeline`/`public_timeline`/`tag_timeline` の 3 ハンドラを `src/timelines/endpoints.rs` に実装したが、`social_graph::endpoints` の前例に倣い **`pub fn router(...)` はこのファイルに追加しなかった**（既存の全 sibling モジュールのルータ組み立て関数は `src/server.rs` 側に存在し、モジュール自身は持たない）。タスク 5.2（`TimelinesModule, server, bootstrap, state`）はこの前例を踏襲し、`timelines_router()` 相当を `src/server.rs` 側に新設して `home_timeline`/`public_timeline`/`tag_timeline` を装着すること。また `public_timeline` は `local=true` 指定時に `TimelineKind::Local` を選ぶ一方、クライアント指定の `local`/`remote`/`only_media` を `TimelineParams` へそのまま転送するため、`local=true` 時は `CandidateRepository` の SQL に無害な冗長 `AND s.local = TRUE` が重複することがある（`src/timelines/endpoints.rs` のモジュールドキュメントコメント「Kind selection for public/local」に記載済み、実害なし）。この 3 ハンドラは本タスク時点ではまだ `AppState`/ルータへ配線されておらず（タスク 5.2 の境界）、`src/timelines/endpoints/tests.rs` はテスト専用の小さな `Router` を直接組み立てて検証している。
