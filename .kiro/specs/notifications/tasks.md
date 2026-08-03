# Implementation Plan

- [ ] 1. 基盤: スキーマ・ドメイン型・リポジトリ
- [x] 1.1 通知スキーマのマイグレーション（0009）
  - `migrations/0009_notifications.sql` に `notifications`（受信者・種別・通知元 kind/id・対象投稿・消去フラグ・作成時刻）を定義し、(recipient, kind, origin_kind, origin_id, COALESCE(status_id,0)) に対する未消去限定（`WHERE NOT dismissed`）の部分一意インデックスによる重複排除と受信者カーソルインデックスを付与する
  - 先行 spec（0001-0007 / social-graph 0006）と番号衝突がなく、起動時の自動マイグレーションが成功し、重複排除の部分一意制約（未消去限定）が作成される状態
  - _Requirements: 8.1_
  - _Boundary: NotificationRepository_

- [x] 1.2 通知ドメイン型
  - 通知 v1 種別の列挙（mention/follow/follow_request/favourite/reblog/poll/status/update）、通知本体（受信者・通知元 AccountRef・対象投稿任意・消去・作成時刻）、通知生成イベント（受信者・通知元・種別・対象投稿・発生時刻）を定義する
  - 範囲外の種別が型として表現できず、status を伴わない種別が対象投稿なしで表現できる状態でコンパイルが通る状態
  - _Requirements: 1.1, 1.5, 5.1, 6.1, 8.1_
  - _Boundary: model_
  - _Depends: 1.1_

- [x] 1.3 通知リポジトリ
  - 重複排除付き挿入（未消去限定の部分一意インデックスを衝突対象とした `ON CONFLICT ... WHERE NOT dismissed DO NOTHING` で新規/既存無視を返し、消去済みの同一キー通知は新規挿入を妨げない）、受信者スコープ + 消去済み除外 + 通知 ID カーソル + 種別/account（解決済み `AccountRef` を受け取り、本層では ID 解決を行わない）フィルタの一覧取得、(id, recipient) 単一取得、dismiss、clear を実装する
  - 同一重複排除キーの二重挿入が冪等になり、消去済みにした通知と同一キーの新規挿入は新規作成として扱われ（取り消し→再実行の再通知）、消去済みが一覧から除外され、他者宛が単一取得で None になることをリポジトリ単体で確認できる状態
  - _Requirements: 2.1, 2.2, 2.3, 2.4, 3.1, 4.1, 4.2, 4.4, 8.1, 8.2_
  - _Boundary: NotificationRepository_
  - _Depends: 1.2_

- [ ] 2. コア: シリアライズ・委譲シーム・フィルタ・生成
- [x] 2.1 (P) 通知シリアライザ
  - Notification の JSON 外殻（id/type/created_at）を生成し、投稿関連種別では関連投稿を受信者視点で statuses-core のシリアライザへ委譲して status に埋め込み、通知元を accounts-and-instance のシリアライザで account に構成し、follow/follow_request の null 規律を維持して契約ハーネスへゴールデン登録する
  - type が v1 種別のみで、follow 系で status が null、投稿関連種別で status 埋め込み点が受信者 viewer となり、外殻ゴールデンが決定的に再現される状態
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6_
  - _Boundary: NotificationSerializer_
  - _Depends: 1.2_

- [x] 2.2 (P) 通知委譲シームの定義（イベントシンク・配信シーク）
  - 上流イベント受領シーム（NotificationEventSink trait + 既定 NoopSink）と後段配信シーム（NotificationDeliverySink trait + 既定 no-op）を定義し、AppState レジストリ用のハンドルを用意する
  - 既定 no-op が上流イベントを受けても何もせず、配信シークが永続化済み通知を受け取る形でコンパイルが通り、上流が未登録でも成功する状態
  - _Requirements: 5.4, 5.5_
  - _Boundary: ports_
  - _Depends: 1.2_

- [x] 2.3 (P) 通知フィルタ
  - 受信者と通知元について social-graph の関係状態問い合わせからブロック/被ブロック/通知ミュート（期限考慮）集合を取得し、いずれかに該当すれば抑制を返す判定を実装する（関係状態・期限判定は再実装しない）
  - ブロック/被ブロック/通知ミュートで抑制が真、期限切れミュートでは抑制が偽になることを単体で確認できる状態
  - _Requirements: 7.1, 7.2, 7.3, 7.4_
  - _Boundary: NotificationFilter_
  - _Depends: 1.2_

- [x] 2.4 通知ジェネレータ（単一生成点）
  - 受信者ローカル限定 → フィルタ抑制判定 → 種別ごとのイベント→通知写像 → 重複排除付き永続化 → 新規時のみ配信シーク引き渡しを、唯一の生成点として集約する。重複判定は「同一の未消去通知が既に存在するか」に限られ、dismiss/clear 済み通知と同一キーのイベントは新規通知として生成されることを前提とする
  - 非ローカル受信者・抑制・重複では永続化も配信引き渡しも起こらず、新規生成時のみ配信シークが呼ばれ、fav/reblog/mention/follow/follow_request/poll の各種別で受信者宛通知が作られる状態
  - _Requirements: 5.1, 5.2, 5.3, 5.5, 6.1, 6.2, 6.3, 6.4, 6.5, 6.6, 8.1, 8.2_
  - _Boundary: NotificationGenerator_
  - _Depends: 1.3, 2.2, 2.3_

- [ ] 3. コア: イベントシンク実装・取得サービス
- [x] 3.1 通知イベントシンク本実装
  - NotificationEventSink の本実装を提供し、上流（statuses-core / social-graph）がローカル発生・受信のいずれの経路から emit したイベントも単一のジェネレータへ流す
  - 既定 no-op を差し替えた本実装が、emit されたイベントをジェネレータへ渡し通知生成を起動する状態
  - _Requirements: 5.1, 5.2, 6.1, 6.2, 6.3, 6.4, 6.5, 6.6_
  - _Boundary: NotificationEventSink_
  - _Depends: 2.4_

- [x] 3.2 通知取得サービス
  - 一覧（ページネーション・種別/account フィルタ・消去済み除外・シリアライズ）、単一取得（他者宛/未存在 404 相当）、dismiss、clear の業務を集約する
  - 一覧が受信者宛のみをフィルタ適用して返し、単一取得が他者宛で未検出、dismiss/clear 後に取得から除外される状態
  - _Requirements: 2.1, 2.2, 2.3, 2.4, 3.1, 3.2, 4.1, 4.2, 4.3, 4.4_
  - _Boundary: NotificationService_
  - _Depends: 1.3, 2.1_

- [ ] 4. 統合: エンドポイントと配線
- [x] 4.1 エンドポイント表層
  - 一覧/単一/clear/dismiss の各エンドポイントを、取得系 read:notifications・消去系 write:notifications のスコープ要求、未存在 404、互換エラー本文、一覧の Link 付与、レート制限レイヤー装着で実装する
  - 一覧の `account_id` クエリパラメータを accounts-and-instance のアカウント解決（ローカルは `ActorDirectory`、既知リモートは `RemoteAccountRepository`）で `AccountRef` へ解決してリポジトリへ渡し、未知の ID に解決できない場合はエラーにせず空配列 + 通常のページネーションヘッダを返す
  - 各エンドポイントが正しいスコープで保護され、一覧が Link 付きで返り、他者宛/未存在で 404、未認証/権限不足で互換エラーを返し、account_id が未解決のときは 404 ではなく 200 + 空配列を返す状態
  - _Requirements: 2.1, 2.3, 2.5, 3.1, 3.2, 4.1, 4.2, 4.3, 9.1, 9.2, 9.3, 9.4_
  - _Boundary: NotificationEndpoints_
  - _Depends: 3.2_

- [x] 4.2 モジュール配線（イベントシンク・配信シーク・ルータ登録）
  - NotificationModule を組み立て、イベントシンク本実装をレジストリへ登録（上流既定 no-op を差し替え）、配信シークを既定 no-op で初期化（下流が後で差し替え可能）、accounts-and-instance のアカウント解決ハンドル（ActorDirectory / RemoteAccountRepository）を NotificationEndpoints の account_id 解決に注入し、ルータを土台へ装着して AppState へ格納する
  - 起動後に上流 emit がジェネレータへ届いて通知が生成され、配信シークが既定 no-op として配線され、通知エンドポイントが横断レイヤー適用点で応答し、一覧の account_id 解決が実際のアカウント解決経路を使って動作する状態
  - _Requirements: 5.4, 5.5, 9.1_
  - _Boundary: NotificationModule_
  - _Depends: 3.1, 4.1_

- [ ] 5. 検証: 契約・統合テスト
- [x] 5.1 (P) Notification 契約ゴールデンテスト
  - 各種別（mention/follow/favourite/poll 等）の Notification 外殻 JSON を決定的境界でゴールデン化し、type 種別・account/status 埋め込み点・null 規律を固定する（埋め込み内側は上流ゴールデンへ委譲）
  - 決定的 RuntimeContext 下で外殻ゴールデンが再現され、follow 系の status null と投稿関連種別の status 埋め込みが固定されることをテストで確認できる状態
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6_
  - _Depends: 4.2_

- [x] 5.2 (P) 取得・消去の統合テスト
  - 一覧（ページネーション・types/exclude_types/account_id フィルタ・消去済み除外・スコープ、account_id が未知の ID に解決できない場合の空配列応答を含む）、単一取得（他者宛 404）、dismiss/clear（消去後除外・スコープ）をエンドポイント経由で統合検証する
  - 上記シナリオが期待どおりの Notification 応答・Link・エラーを返し、account_id 未解決時に 404 ではなく空配列が返ることをテストで確認できる状態
  - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5, 3.1, 3.2, 4.1, 4.2, 4.3, 4.4, 9.1, 9.2, 9.3, 9.4_
  - _Depends: 4.2_

- [x] 5.3 (P) 生成・フィルタの統合テスト
  - 各種別のイベント消費による生成、受信者ローカル限定、重複排除の冪等、新規時の配信シーク引き渡し、ブロック/被ブロック/通知ミュート（期限考慮）での生成抑制を統合検証する
  - 同一キーの通知を dismiss 済みにした後に同一イベント（unfollow→re-follow / unfavourite→re-favourite 等）を再度消費すると新規通知が生成され、未消去のまま同一イベントが再送された場合は重複として抑制されることを含めて検証する
  - イベント消費で受信者宛通知が一度だけ作られ、非ローカル/抑制/重複で生成されず、新規時のみ配信シークが呼ばれ、消去後の取り消し→再実行では新規通知が生成されることをテストで確認できる状態
  - _Requirements: 5.1, 5.2, 5.3, 5.5, 6.1, 6.2, 6.3, 6.4, 6.5, 6.6, 7.1, 7.2, 7.3, 7.4, 8.1, 8.2_
  - _Depends: 4.2_

## Implementation Notes

- 5.3: `tests/notification_generation_it.rs`（7 テスト）・`tests/notification_filter_it.rs`（5 テスト）を新規追加。tasks 5.1/5.2 と同じ実プロダクションパイプライン規約（`spawn_test_app`・`kawasemi::server::build_router`）を踏襲し、この spec の最後の group を完了させた。種別ごとの生成（favourite/reblog/mention/follow は実 HTTP 経路、follow_request は `Transitions::record_pending` 直接駆動、poll は実 emitter 不在のため `ports().emit()` 直接投入——いずれも task 5.1 の既レビュー済み前例を踏襲）、非ローカル受信者でのスキップ、配信シーク（`RecordingDeliverySink` を実 `NotificationPortsRegistry::set_delivery_sink` 経由で登録し実生成点への到達を確認）を検証。重複排除（未消去の同一イベント再送は抑制）と dismiss→再送（新規通知生成）の対比は同一 dedup キー（受信者/種別/通知元/status）で back-to-back に検証し、`insert_dedup` の `WHERE NOT dismissed` 部分一意インデックス（migration 0009）の実セマンティクスと突き合わせ済み。フィルタ側はブロック/被ブロック/通知ミュート（`muted_notifications`、素の `muted` は対象外）での抑制と、期限切れミュート・素のミュート単体では抑制されないことの両方向を検証（後者は「通知が実際に作られたこと」を明示的にアサートし、過抑制がないことを証明）。dedup/dismiss/フィルタの各シナリオは `NotificationGenerator`/`NotificationFilter` が種別非依存（`kind` で分岐しない）であることをレビューで実装突き合わせ確認済みのため、代表として `Follow` 種別のイベントを直接構築して用いている（種別ごとの網羅性は本ファイルの他 5 テストが別途担保）。このサンドボックスには到達可能な Postgres が無く、12 件のテストは全て実行不能——`cargo check --tests`/`cargo test --no-run`/`cargo clippy --all-targets`/`cargo fmt --check` のクリーンな成功と、`cargo test --lib notifications::`（47 passed / 64 failed、失敗は全て既存タスクの DB 依存単体テストの `PoolTimedOut` で本タスクとは無関係）を検証根拠とした。レビューでの指摘なし。**これにより notifications feature の全タスクが完了。**
- 5.2: `tests/notification_list_it.rs`（8 テスト）・`tests/notification_show_dismiss_it.rs`（11 テスト）を新規追加。task 5.1 と同じ「実プロダクションルータ（`kawasemi::server::build_router`）を `spawn_test_app` 経由で駆動」する統合テスト規約（`tests/timelines_endpoints_it.rs` の前例）に倣い、task 4.1 のルータ内蔵テスト（`endpoints/tests.rs`、テスト専用ルータ）が当時未配線ゆえに延期していた HTTP 統合テストを実配線後（task 4.2 完了後）に埋める。一覧（ページネーション・`Link`・`types[]`/`exclude_types[]`・`account_id` フィルタ・消去済み除外・スコープ）と単一取得/dismiss/clear（他者宛 404・消去後除外・スコープ分離）を検証。最重要ケースである「`account_id` が未知の ID に解決できない場合は 404 ではなく 200 + 空配列（`Link` ヘッダなし）」を明示的にアサートし、`src/notifications/endpoints.rs` の `resolve_account_id_filter`/`list_notifications` の実分岐と突き合わせ確認済み。通知の発生は実アップストリーム経路（`POST .../follow`・`@handle` メンション付き投稿・`favourite`）または task 5.1 で確立済みの `Transitions::record_pending` 直接駆動（follow_request、実 emitter 不在のため）を用い、生 DB 直挿入は行わない。このサンドボックスには到達可能な Postgres が無く、両ファイルとも実行不能——`cargo check --tests`/`cargo test --no-run`（全 74 バイナリコンパイル成功）/`cargo clippy --all-targets`/`cargo fmt --check` のクリーンな成功を検証根拠とし、レビューで `repository.rs`（dismiss/clear の受信者スコープ `WHERE` 句）・`endpoints.rs`・`api/pagination.rs`（`build_link_header` の cursor なし時 `Link` 省略）との突き合わせにより正当性を確認済み。レビューでの指摘なし。
- 5.1: `tests/notification_contract_it.rs`（新規、10 `#[tokio::test]`）を追加。全 8 種の v1 通知（mention/follow/follow_request/favourite/reblog/poll/status/update）を実ルータ（`crate::server::build_router`）/ `NotificationService` / `NotificationGenerator` の実パイプライン経由で駆動し、外殻（`id`/`type`/`created_at`）・`account`/`status` 埋め込み・follow 系の null 規律（1.4）をアサートし、新規ゴールデン（`tests/golden/notifications/notification_contract_it_*.json`、task 2.1 の単体ゴールデンとは別系統）へ `assert_golden` 登録するコードを実装（`tests/status_contract_it.rs`（statuses-core task 8.2）の構造的前例を踏襲）。2 インスタンス独立起動での再現性証明・`register_fixture`/`load_fixture` による実クライアントキャプチャ登録の証明も同様に含む。poll/status/update の 3 種別は本番の emitter が未だ存在しない（`status_service.rs` 自身のドキュメントに明記された既知のギャップ）ため `app.state.notifications().ports().emit()` で単一生成点へ直接投入（`src/notifications/tests.rs` の既存前例と同一手法）、follow_request は 2 つのローカルアクター間では `FollowApprovalPolicy` により即時成立するため `social_graph::Transitions::record_pending` を直接駆動（真の保留フォローリクエストは署名検証込みの連合パスが必要で `tests/social_graph_inbound_it.rs` の責務）——いずれもレビューで実装ギャップに基づく正当な代替と確認済み。このサンドボックスには到達可能な Postgres が無く（`pg_isready` 応答なしを確認済み）、10 件のテストは全て `PoolTimedOut` で実行不能・ゴールデン JSON ファイルも `KAWASEMI_UPDATE_GOLDEN=1` での生成が不能（DB 到達可能な環境での一回限りの実行が別途必要、`status_contract_it.rs` の前例と同じ）——`cargo check --tests`/`cargo test --no-run`/`cargo clippy --all-targets`/`cargo fmt --check` のクリーンな成功と、`PoolTimedOut` まで到達する（＝コンパイル・ロジックが DB 境界まで正しく動作する）ことの確認を検証根拠とした。レビューでの指摘なし。
- 4.1: `src/notifications/endpoints.rs`（+ `endpoints/tests.rs`、24 テスト）を追加。`list`/`show`/`clear`/`dismiss` の 4 ハンドラは design.md の API Contract テーブルに厳密準拠し、`NotificationService`（task 3.2）へ委譲。`account_id` 解決は `AccountService::show_account` を直接呼ばず、その内部で使われている `ActorDirectory::resolve_actor_by_id` と `accounts::remote_repository::find_remote_by_id` の 2 分岐のみを独立に再利用（`show_account` 側にある非数値 ID → ライブ fetch の第三分岐は本タスクの design 記述に無いため意図的に除外——レビューで `account_service.rs` の実装と突き合わせ確認済み）。ルータ関数・per-route レート制限層はこのファイルに置かない（`src/server.rs::build_router` が全ルータ横断で単一のレート制限層を適用する既存パターンに合わせた——`social_graph::endpoints`/`timelines::endpoints` の前例と一致することをレビューで確認済み）。`tests/notification_list_it.rs`/`tests/notification_show_dismiss_it.rs`（File Structure Plan記載）は本タスクでは作成せず、task 5.2（`_Depends: 4.2_`、ルータ実装後）に委譲——未装着のエンドポイントモジュールに対する HTTP 統合テストは実行不能なため、`social_graph`/`timelines` の前例と同じく `#[cfg(test)]` 形式のルータ内テストに留めた。本モジュールは `src/notifications.rs`/`src/server.rs`/`src/state.rs`/`src/bootstrap.rs` のいずれにも未配線（task 4.2 の責務）。このサンドボックスには到達可能な Postgres が無く、24 件のテストは全て `PoolTimedOut` で実行不能——一時的に `pub mod endpoints;` を追加してコンパイル可能な状態を作り `cargo check`/`cargo test --lib` でコンパイル成功・想定どおりの接続エラーを確認した後、変更を byte-identical に復元（md5sum 確認）した（実装者・レビュアー双方が独立に実施・確認済み）。レビューでの唯一の指摘（非ブロッキング）：`tests.rs` はステータスコードのみ検証し、401/403/404/422 のエラー本文 JSON 形状までは直接アサートしていない（`AppError::IntoResponse` が一様に `mastodon_error_body` へ委譲するため挙動自体は正しいが、回帰検知はできない）。
- 4.2: `src/notifications.rs`（`NotificationModule` 組み立て）を拡張し、`src/state.rs`/`src/bootstrap.rs`/`src/server.rs` へ配線。イベントシンク本実装（task 3.1）は upstream の実消費点である `statuses::notification_sink::NotificationSinkRegistry`（`src/statuses.rs`/`src/social_graph.rs` 共有）へ登録——`NotificationPortsRegistry` 自身の `event_sink` スロットは今後も未読（ドキュメント上の対称性のみ）であることをレビューで確認済み。配信シークは新設の `DeliverySinkBridge`（design.md 未記載だが `ports.rs` のレジストリ差し替え可能設計に忠実な最小限の橋渡し）経由で `NotificationPortsRegistry` の差し替えを `NotificationGenerator` の固定 `Arc<dyn NotificationDeliverySink>` に反映できるようにした上で既定 no-op のまま初期化。`NotificationEndpointsState` へは実 `ActorDirectory`/`PgPool` を注入（スタブではない）。`AppState::new` に必須引数が増える影響で `src/test_harness.rs`/`src/federation/test_harness.rs`/`src/state/tests.rs`/`src/server/tests.rs` が機械的に追従（commit `4fefae2`（timelines task 5.2）・`ce33f3a`（social-graph task 5.2）と同型の前例、レビューで確認済み）。`src/notifications/tests.rs`（配線のみを狙った2テスト、design.md の File Structure Plan 上は task 5.x 帰属だが `social_graph::tests` の前例に倣い追加）はこのサンドボックスでは `PoolTimedOut` により実行不能——`cargo check/build/clippy --all-targets` のクリーンな成功と、DB非依存な `state::`/`server::tests`/`bootstrap::`/`notifications::` の単体テスト（合計47件）の実行成功を主な検証根拠とした。レビューでの指摘なし。

- 3.2: `src/notifications/service.rs`（+ `service/tests.rs`）を追加。`NotificationService::{list, show, dismiss, clear}` は design.md の Service Interface を厳密に踏襲し（`&RequestActorContext` を受け、`ctx.actor_id` を受信者とする）、`repository::{list, find_for_recipient, dismiss, clear}`（task 1.3）へ委譲。`account`/`status` の埋め込みは `AccountService::show_account`（accounts-and-instance）と `statuses::serializer::status_to_json`（statuses-core）への実委譲——契約再定義なし——を独立レビューで確認済み。`find_for_recipient` の `None` / `dismiss` の `Ok(false)` はいずれも `AppError::client(StatusCode::NOT_FOUND, ...)` へ一様に写像（他者宛/未存在の区別をリポジトリ層で意図的に潰している設計に整合）。**既知の技術的負債（非ブロッキング、レビューで確認済み）**: `render_status`/`leaf_render_input` 等の埋め込み組み立てグルーが `src/statuses/account_provider.rs::render`/`leaf_render_input` とほぼ同一の約140行を重複させている（この crate に共有 `pub(crate)` の「Status→JSON」ヘルパーが未だ存在しないため）。3箇所目の重複が生じた時点で共通化を検討すること。また `render_status` はネストしたリブースト対象への `visibility::is_visible` 再チェックを行わない（本 spec の要求範囲外・`account_provider.rs` との意図的な差分、requirements.md の Out of Scope にも整合）。このサンドボックスには到達可能な Postgres が無く、14 件のテストは全て `spawn_test_app` 経由の実 DB 統合テストとして実装したが実行不能（`PoolTimedOut`、`pg_isready`/生 TCP プローブでも接続拒否を確認済み）——レビューでのコード直読（`repository.rs`/`serializer.rs` の既存契約との突き合わせ）により正当性を確認した。
- 3.1: `src/notifications/event_sink.rs`（+ `event_sink/tests.rs`）を追加。`GeneratorEventSink`（`crate::notifications::ports::NotificationEventSink` を実装し `NotificationGenerator::generate` へ転送）と `StatusesEventSinkAdapter`（`crate::statuses::notification_sink::NotificationEventSink` を実装——`src/bootstrap.rs` が構築し `src/statuses.rs`/`src/social_graph.rs` の両方が共有する既存の `NotificationSinkRegistry` スロットが実際に要求する trait はこちら——`.into()` 変換後に内側の正準シンクへ委譲）を実装。`From<statuses::notification_sink::{NotificationType,NotificationEvent}> for notifications::model::{NotificationType,NotificationEvent}` はフィールド単位の全射変換（両モジュールの型定義を直接比較し 8 種別・5 フィールドとも完全一致を確認済み）。`src/state.rs`/`src/bootstrap.rs` への実登録（レジストリへの `set_sink` 呼び出し）は tasks.md 4.2（`_Depends: 3.1_`）の境界として意図的に未着手のまま——design.md の Modified Files 節が bootstrap 配線を 4.2 の責務として明示しており、本タスクは「本実装を供給する」ところまで（レビューで独立に確認済み）。RED フェーズは Rust の性質上コンパイルエラーを正当な RED 証跡として採用（`event_sink.rs` を型/impl 抜きのスタブに一時置換 → `cargo test --lib notifications::event_sink::` で 72 件のコンパイルエラー → 復元、md5sum で復元後の byte-identical を確認）——task 2.4 の前例を踏襲。レビュー1周目は実装ではなく「親コントローラがレビュアーへ渡したレポート要約から RED_PHASE_OUTPUT フィールドを落とした」ことのみが理由で REJECTED（コード自体は無変更のまま、RED 証跡を再現・報告するのみの是正ラウンドで即 APPROVED）——今後、実装者のステータスレポートをレビュアーへ要約して渡す際は、`RED_PHASE_OUTPUT` を含む全フィールドを欠落させないこと。
- 2.4: `src/notifications/generator.rs` を追加。`NotificationGenerator::generate` は design.md のシーケンス図どおり「ローカル受信者判定（非ローカルは DB に触れず即 `SkippedNonLocal`）→ `NotificationFilter::should_suppress` → 種別非依存のイベント→通知写像（`id`/`created_at` は `RuntimeContext` から採番、`event.occurred_at` は使わない）→ `insert_dedup`（独自の存在事前チェックはしない）→ `Created` 時のみ配信シーク呼び出し」の順。配信シークのエラーは `generate` 自体の失敗にせず `tracing::warn!` でログして握りつぶす（フィルタ/リポジトリのエラーは `?` で伝播、配信のみ非対称に扱う）。`NotificationDeliverySink` は `NotificationPortsRegistry` 全体ではなく `Arc<dyn NotificationDeliverySink>` のみを保持（design.md の Components 表が `DeliverySink` 単体を依存として挙げているため）。このサンドボックスには到達可能な Postgres が無く、5 テスト中 DB 非依存の 1 件（`SkippedNonLocal` を `PgPool::connect_lazy` 上で検証、フィルタより先にローカル判定が走ることの証明にもなる）のみ実行確認、残り 4 件（抑制・6 種別生成・重複・dismiss 後の再生成=8.1/8.2 の取り消し→再実行）はレビューでの手動トレースにより正当性を確認した。
- 2.3: `src/notifications/filter.rs` を追加。`NotificationFilter::should_suppress` は `social_graph::FilterQuery::blocked_set(recipient)` を一度呼ぶだけで、`blocked`/`blocked_by`/`muted_notifications` のいずれかに `origin` が含まれれば抑制（素の `muted` は 7.2 の要求どおり判定に使わない）。関係状態・期限判定の再実装なし（7.4）。このサンドボックスには到達可能な Postgres が無く（`social_graph::providers` 自身の既存テストも同一の `PoolTimedOut` で失敗することをレビューで確認済み・タスク横断のサンドボックス制約）、6 件のテストは実行不能だがレビューでの手動トレース（各テストが実装のどのバグを検出できるか 1 行ずつ検証済み）で正当性を確認した。
- 2.1/2.2: `src/notifications/serializer.rs`・`src/notifications/ports.rs` を追加。`NotificationSerializer`/`NotificationEventSink`/`NotificationDeliverySink` は design.md の `&self` メソッド案ではなく `StatusSerializer`/`AccountSerializer`/`accounts::ports` と同じ「事前解決済み値・ハンドル」パターンで実装（既存踏襲パターンとしてレビューで実コード照合済み）。**根本原因を特定・恒久修正済み**: `tests/timeline_status_contract_it.rs` は HEAD 時点で rustfmt 非準拠だった。この repo では `cargo fmt -- <単一ファイル>` が cargo-fmt の仕様上 `--` 以降の引数を「自動検出したファイル一覧への追加」として扱うため、実際には**クレート全体**を再フォーマットする（単一ファイルにスコープされない）。`.claude/settings.json` の PostToolUse フック（`.rs` の Edit/Write 毎に `cargo fmt -- "$f"` を実行）がこれを踏むため、**どのタスクであっても `.rs` ファイルを編集するたびに**この差分が再発する。commit `b927681` で `tests/timeline_status_contract_it.rs` を rustfmt 準拠に一度だけ直し（純粋な空白差分、挙動変更なし、`cargo test --test timeline_status_contract_it` で DB 未接続以外の失敗が無いことを確認済み）、恒久的に解消した。以降のタスクでこの diff が再出現することは想定されないが、`.rs` を編集した後は常に `git status`/`git diff --name-only` で意図しないファイルが混入していないか確認すること（フック自体は今後も全体を re-fmt するため、真にリスクがあるのは「まだ rustfmt 非準拠な行が repo 内に残っている場合」のみ）。
- 1.3: `src/notifications/repository.rs` を追加。design.md の Service Interface（`insert_dedup`/`list`/`find_for_recipient`/`dismiss`/`clear`/`ListFilter`）に一致。`ON CONFLICT (recipient_id, kind, origin_kind, origin_id, COALESCE(status_id, 0)) WHERE NOT dismissed` は migration 0009 の `notifications_dedup_idx` 定義と厳密に一致させる必要がある（Postgres の ON CONFLICT 推論対象は既存インデックス定義と完全一致が必須）ことを確認済み。`find_for_recipient` は Requirement 4.4 の文言（「一覧取得・単一取得から...除外する」）どおり消去済み行も除外する。`list` は全件取得後インメモリページングで、`social_graph/repository.rs::list_inbound_requests` 等の既存踏襲パターン。
- 1.2: `src/notifications/model.rs` を追加。`NotificationType`/`Notification`/`NotificationEvent` は design.md の型定義に完全一致（フィールド名・型を1対1で確認済み）。`crate::domain::{Id, AccountRef}` を再定義せず再利用。`src/statuses/notification_sink.rs`（task 9.2 が用意した暫定プレースホルダ、同一形状の `NotificationType`/`NotificationEvent`）は本タスクの境界（`model` のみ）外のため意図的に未着手・共存のまま — 移行は task 2.2/3.1 の責務。ユニットテストは `model.rs` 内インライン（`src/statuses/model.rs` 等、既存の `model.rs` すべてに共通する先例に合わせた。`model/tests.rs` はリポジトリ内に一つも存在しない）。
- 1.1: `migrations/0009_notifications.sql` を追加。`research.md` の番号調整記録どおり `0009` は未使用で、既存の `0011`/`0012` と衝突しない（sqlx は埋め込み時に昇順で適用するのみで、履歴上の適用順は要求しないため安全）。`tests/notifications_migrations_it.rs` で実 Postgres 経由の RED→GREEN を確認し、`WHERE NOT dismissed` 部分一意インデックスの重複排除/再通知セマンティクスを実際の制約違反 INSERT + SQLSTATE 23505 で検証済み。
- ワークスペース全体の `cargo test`（約1500件、デフォルト並列度）は、このサンドボックスのリソース制約下で実行するたびに異なる無関係テスト集合が単発失敗する（本タスクとは無関係な既存不安定性）。タスクスコープの検証は、対象テスト単体実行と隣接モジュール（`statuses::`）の反復実行で行うこと。フル `cargo test` の単発失敗のみを根拠に REJECTED としない — 対象テストの単体反復実行結果を優先する。
