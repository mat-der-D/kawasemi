# Implementation Plan

- [ ] 1. 基盤: スキーマ・ドメイン型・リポジトリ
- [ ] 1.1 通知スキーマのマイグレーション（0009）
  - `migrations/0009_notifications.sql` に `notifications`（受信者・種別・通知元 kind/id・対象投稿・消去フラグ・作成時刻）を定義し、(recipient, kind, origin_kind, origin_id, COALESCE(status_id,0)) の重複排除一意インデックスと受信者カーソルインデックスを付与する
  - 先行 spec（0001-0007 / social-graph 0006）と番号衝突がなく、起動時の自動マイグレーションが成功し、重複排除一意制約が作成される状態
  - _Requirements: 8.1_
  - _Boundary: NotificationRepository_

- [ ] 1.2 通知ドメイン型
  - 通知 v1 種別の列挙（mention/follow/follow_request/favourite/reblog/poll/status/update）、通知本体（受信者・通知元 AccountRef・対象投稿任意・消去・作成時刻）、通知生成イベント（受信者・通知元・種別・対象投稿・発生時刻）を定義する
  - 範囲外の種別が型として表現できず、status を伴わない種別が対象投稿なしで表現できる状態でコンパイルが通る状態
  - _Requirements: 1.1, 1.5, 5.1, 6.1, 8.1_
  - _Boundary: model_
  - _Depends: 1.1_

- [ ] 1.3 通知リポジトリ
  - 重複排除付き挿入（ON CONFLICT DO NOTHING で新規/既存無視を返す）、受信者スコープ + 消去済み除外 + 通知 ID カーソル + 種別/account フィルタの一覧取得、(id, recipient) 単一取得、dismiss、clear を実装する
  - 同一重複排除キーの二重挿入が冪等になり、消去済みが一覧から除外され、他者宛が単一取得で None になることをリポジトリ単体で確認できる状態
  - _Requirements: 2.1, 2.2, 2.3, 2.4, 3.1, 4.1, 4.2, 4.4, 8.1, 8.2_
  - _Boundary: NotificationRepository_
  - _Depends: 1.2_

- [ ] 2. コア: シリアライズ・委譲シーム・フィルタ・生成
- [ ] 2.1 (P) 通知シリアライザ
  - Notification の JSON 外殻（id/type/created_at）を生成し、投稿関連種別では関連投稿を受信者視点で statuses-core のシリアライザへ委譲して status に埋め込み、通知元を accounts-and-instance のシリアライザで account に構成し、follow/follow_request の null 規律を維持して契約ハーネスへゴールデン登録する
  - type が v1 種別のみで、follow 系で status が null、投稿関連種別で status 埋め込み点が受信者 viewer となり、外殻ゴールデンが決定的に再現される状態
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6_
  - _Boundary: NotificationSerializer_
  - _Depends: 1.2_

- [ ] 2.2 (P) 通知委譲シームの定義（イベントシンク・配信シーク）
  - 上流イベント受領シーム（NotificationEventSink trait + 既定 NoopSink）と後段配信シーム（NotificationDeliverySink trait + 既定 no-op）を定義し、AppState レジストリ用のハンドルを用意する
  - 既定 no-op が上流イベントを受けても何もせず、配信シークが永続化済み通知を受け取る形でコンパイルが通り、上流が未登録でも成功する状態
  - _Requirements: 5.4, 5.5_
  - _Boundary: ports_
  - _Depends: 1.2_

- [ ] 2.3 (P) 通知フィルタ
  - 受信者と通知元について social-graph の関係状態問い合わせからブロック/被ブロック/通知ミュート（期限考慮）集合を取得し、いずれかに該当すれば抑制を返す判定を実装する（関係状態・期限判定は再実装しない）
  - ブロック/被ブロック/通知ミュートで抑制が真、期限切れミュートでは抑制が偽になることを単体で確認できる状態
  - _Requirements: 7.1, 7.2, 7.3, 7.4_
  - _Boundary: NotificationFilter_
  - _Depends: 1.2_

- [ ] 2.4 通知ジェネレータ（単一生成点）
  - 受信者ローカル限定 → フィルタ抑制判定 → 種別ごとのイベント→通知写像 → 重複排除付き永続化 → 新規時のみ配信シーク引き渡しを、唯一の生成点として集約する
  - 非ローカル受信者・抑制・重複では永続化も配信引き渡しも起こらず、新規生成時のみ配信シークが呼ばれ、fav/reblog/mention/follow/follow_request/poll の各種別で受信者宛通知が作られる状態
  - _Requirements: 5.1, 5.2, 5.3, 5.5, 6.1, 6.2, 6.3, 6.4, 6.5, 6.6, 8.1, 8.2_
  - _Boundary: NotificationGenerator_
  - _Depends: 1.3, 2.2, 2.3_

- [ ] 3. コア: イベントシンク実装・取得サービス
- [ ] 3.1 通知イベントシンク本実装
  - NotificationEventSink の本実装を提供し、上流（statuses-core / social-graph）がローカル発生・受信のいずれの経路から emit したイベントも単一のジェネレータへ流す
  - 既定 no-op を差し替えた本実装が、emit されたイベントをジェネレータへ渡し通知生成を起動する状態
  - _Requirements: 5.1, 5.2, 6.1, 6.2, 6.3, 6.4, 6.5, 6.6_
  - _Boundary: NotificationEventSink_
  - _Depends: 2.4_

- [ ] 3.2 通知取得サービス
  - 一覧（ページネーション・種別/account フィルタ・消去済み除外・シリアライズ）、単一取得（他者宛/未存在 404 相当）、dismiss、clear の業務を集約する
  - 一覧が受信者宛のみをフィルタ適用して返し、単一取得が他者宛で未検出、dismiss/clear 後に取得から除外される状態
  - _Requirements: 2.1, 2.2, 2.3, 2.4, 3.1, 3.2, 4.1, 4.2, 4.3, 4.4_
  - _Boundary: NotificationService_
  - _Depends: 1.3, 2.1_

- [ ] 4. 統合: エンドポイントと配線
- [ ] 4.1 エンドポイント表層
  - 一覧/単一/clear/dismiss の各エンドポイントを、取得系 read:notifications・消去系 write:notifications のスコープ要求、未存在 404、互換エラー本文、一覧の Link 付与、レート制限レイヤー装着で実装する
  - 各エンドポイントが正しいスコープで保護され、一覧が Link 付きで返り、他者宛/未存在で 404、未認証/権限不足で互換エラーを返す状態
  - _Requirements: 2.1, 2.5, 3.1, 3.2, 4.1, 4.2, 4.3, 9.1, 9.2, 9.3, 9.4_
  - _Boundary: NotificationEndpoints_
  - _Depends: 3.2_

- [ ] 4.2 モジュール配線（イベントシンク・配信シーク・ルータ登録）
  - NotificationModule を組み立て、イベントシンク本実装をレジストリへ登録（上流既定 no-op を差し替え）、配信シークを既定 no-op で初期化（下流が後で差し替え可能）、ルータを土台へ装着して AppState へ格納する
  - 起動後に上流 emit がジェネレータへ届いて通知が生成され、配信シークが既定 no-op として配線され、通知エンドポイントが横断レイヤー適用点で応答する状態
  - _Requirements: 5.4, 5.5, 9.1_
  - _Boundary: NotificationModule_
  - _Depends: 3.1, 4.1_

- [ ] 5. 検証: 契約・統合テスト
- [ ] 5.1 (P) Notification 契約ゴールデンテスト
  - 各種別（mention/follow/favourite/poll 等）の Notification 外殻 JSON を決定的境界でゴールデン化し、type 種別・account/status 埋め込み点・null 規律を固定する（埋め込み内側は上流ゴールデンへ委譲）
  - 決定的 RuntimeContext 下で外殻ゴールデンが再現され、follow 系の status null と投稿関連種別の status 埋め込みが固定されることをテストで確認できる状態
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6_
  - _Depends: 4.2_

- [ ] 5.2 (P) 取得・消去の統合テスト
  - 一覧（ページネーション・types/exclude_types/account_id フィルタ・消去済み除外・スコープ）、単一取得（他者宛 404）、dismiss/clear（消去後除外・スコープ）をエンドポイント経由で統合検証する
  - 上記シナリオが期待どおりの Notification 応答・Link・エラーを返すことをテストで確認できる状態
  - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5, 3.1, 3.2, 4.1, 4.2, 4.3, 4.4, 9.1, 9.2, 9.3, 9.4_
  - _Depends: 4.2_

- [ ] 5.3 (P) 生成・フィルタの統合テスト
  - 各種別のイベント消費による生成、受信者ローカル限定、重複排除の冪等、新規時の配信シーク引き渡し、ブロック/被ブロック/通知ミュート（期限考慮）での生成抑制を統合検証する
  - イベント消費で受信者宛通知が一度だけ作られ、非ローカル/抑制/重複で生成されず、新規時のみ配信シークが呼ばれることをテストで確認できる状態
  - _Requirements: 5.1, 5.2, 5.3, 5.5, 6.1, 6.2, 6.3, 6.4, 6.5, 6.6, 7.1, 7.2, 7.3, 7.4, 8.1, 8.2_
  - _Depends: 4.2_
