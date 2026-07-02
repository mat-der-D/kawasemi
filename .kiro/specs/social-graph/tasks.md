# Implementation Plan

- [ ] 1. 基盤: スキーマ・ドメイン型・リポジトリ
- [ ] 1.1 関係状態スキーマのマイグレーション（0006）
  - `migrations/0006_social_graph.sql` に `follows` / `follow_requests` / `mutes` / `blocks` を定義し、各関係の一意制約と逆引きインデックス（followee / target+direction / blocked）を付与する
  - 先行 spec（0001-0005 / 0007）と番号衝突がなく、起動時の自動マイグレーションが成功する状態
  - _Requirements: 8.1_
  - _Boundary: RelationshipRepository_

- [ ] 1.2 ソーシャルグラフのドメイン型
  - フォロー・フォローリクエスト（送信中/受信保留の方向）・ミュート（通知/期限）・ブロックの型と、フォロー/ミュートの操作オプションを定義する。アカウント識別（ローカル/リモート）の `AccountRef` は再定義せず core-runtime の domain-primitives（正準所有）から import する
  - フォロー/ブロックが Undo 用の送信 Activity id を保持し、ミュートが任意の有効期限を保持する型としてコンパイルが通る状態
  - _Requirements: 1.5, 2.1, 4.2, 4.3, 5.1, 8.1_
  - _Boundary: model_
  - _Depends: 1.1_

- [ ] 1.3 関係状態リポジトリ
  - follows / follow_requests / mutes / blocks の upsert・削除・存在確認、受信保留一覧のページネーション取得、閲覧者+対象群のバッチ逆引き合成、ブロック/被ブロック/ミュート（期限考慮）/フォロー集合の問い合わせを実装する
  - 同一関係の二重 upsert が一意制約で冪等になり、期限切れミュートが集合・導出から除外されることをリポジトリ単体で確認できる状態
  - _Requirements: 1.6, 2.2, 4.3, 8.1, 8.4, 9.1, 9.2, 9.3_
  - _Boundary: RelationshipRepository_
  - _Depends: 1.2_

- [ ] 2. コア: ポリシー・遷移・Activity 生成・写像
- [ ] 2.1 (P) フォロー承認要否ポリシー（同一サーバースキップ特権の単一定義）
  - 宛先ロック状態と「送信元・宛先がともに同一サーバーのローカルか」から承認要否を返す単一判定を実装し、同一サーバー特権の分岐をこの一箇所のみに置く
  - 両ローカル同一サーバーはロック済みでも確立、片側リモートのロック済みは承認必要、と単体テストで確認できる状態
  - _Requirements: 3.1, 3.2, 3.3, 3.4_
  - _Boundary: FollowApprovalPolicy_
  - _Depends: 1.2_

- [ ] 2.2 (P) 関係状態遷移の共通関数
  - フォロー確立/解消・保留記録/昇格(Accept)/破棄(Reject)・ブロック適用（双方向フォローと両方向保留を単一トランザクションで解消してから確定）/解除・被ブロック設定/解除を、API 経路と受信経路が共有する冪等な遷移関数として実装する
  - ブロック適用で双方向フォロー・両方向保留が消え、同一遷移の二重適用が状態を壊さないことを単体テストで確認できる状態
  - _Requirements: 1.1, 1.4, 2.5, 2.6, 3.2, 5.1, 5.2, 7.7_
  - _Boundary: Transitions, RelationshipRepository_
  - _Depends: 1.3_

- [ ] 2.3 (P) 連合 Activity ビルダ
  - Follow / Accept / Reject / Block / Undo の正規 ActivityPub 表現を生成し、Undo には関係行が保持する元 Activity id を、Accept/Reject には受信 Follow の id を埋め込む
  - ローカル宛・リモート宛で同一の論理 Activity が生成され、Undo が元 Activity を参照することを単体テストで確認できる状態
  - _Requirements: 1.2, 1.3, 1.4, 2.3, 2.4, 5.3, 5.4_
  - _Boundary: ActivityBuilder_
  - _Depends: 1.2_

- [ ] 2.4 (P) 関係状態から Relationship 契約への写像
  - 本 spec の関係状態を accounts-and-instance の RelationshipView へ写像し、全関係フラグを決定論的に導出する。期限切れミュートは muting を偽、domain_blocking は常に偽として扱う
  - 関係なしで全フラグ既定、フォロー/ミュート/ブロックの組合せが正しいフラグになり、Relationship 契約を再定義していないことを単体テストで確認できる状態
  - _Requirements: 4.3, 8.3, 8.4, 8.5_
  - _Boundary: RelationshipMapper_
  - _Depends: 1.2_

- [ ] 3. コア: 関係操作サービス
- [ ] 3.1 フォロー / アンフォローサービス
  - 自己フォロー拒否・既存関係の冪等返却・承認要否判定・遷移実行・Follow/Undo Activity の共通配送依頼・Relationship 応答生成を集約する
  - ローカル/リモート対象いずれもフォローで関係（または保留）が作られ Relationship を返し、重複フォローが冪等、アンフォローで Undo が配送される状態
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7_
  - _Boundary: FollowService_
  - _Depends: 2.1, 2.2, 2.3, 2.4_

- [ ] 3.2 フォローリクエストサービス
  - 受信保留フォローリクエスト一覧（ページネーション）・承認（昇格 + Accept 配送）・拒否（破棄 + Reject 配送）を集約する
  - ロック済みアクター宛フォローが保留として一覧に現れ、承認でフォロー確立 + Accept 配送、拒否で削除 + Reject 配送が起こる状態
  - _Requirements: 2.2, 2.3, 2.4_
  - _Boundary: FollowRequestService_
  - _Depends: 2.2, 2.3, 2.4_

- [ ] 3.5 (P) 通知イベント（NotificationEvent）を emit する
  - notifications が所有する `NotificationEventSink`（既定 no-op）へ、フォロー / フォローリクエストの状態遷移コミット後に `follow` / `follow_request` の `NotificationEvent` を冪等に emit する。イベント型/シンク契約は notifications 所有で再定義しない
  - フォロー確立・フォローリクエスト受信のコミット後にシンクへ 1 度だけイベントが渡り、既定 no-op のため notifications 未配線でも成功する状態
  - _Requirements: 1.1, 2.2_
  - _Boundary: FollowService, FollowRequestService_
  - _Depends: 3.1, 3.2_

- [ ] 3.3 (P) ミュート / アンミュートサービス
  - ミュート/アンミュートを連合 Activity を伴わない DB 状態更新として実装し、通知ミュートと有効期限を反映する
  - ミュートで muting が真、通知ミュート指定で muting_notifications が真、期限指定が記録され、連合配送が発生しない状態
  - _Requirements: 4.1, 4.2, 4.3, 4.4, 4.5_
  - _Boundary: MuteService_
  - _Depends: 2.2, 2.4_

- [ ] 3.4 (P) ブロック / アンブロックサービス
  - ブロックで関係解消込みの遷移を実行し Block を共通配送依頼、アンブロックで解除 + Undo(Block) 配送、いずれも Relationship を返す
  - ブロックで双方向フォロー・保留が解消され blocking が真、Block が配送され、アンブロックで Undo(Block) が配送される状態
  - _Requirements: 5.1, 5.2, 5.3, 5.4, 5.5_
  - _Boundary: BlockService_
  - _Depends: 2.2, 2.3, 2.4_

- [ ] 4. コア: 受信処理と委譲実装
- [ ] 4.1 受信 Activity ハンドラ
  - Follow/Accept/Reject/Block/Undo の受信を、API 経路と同じ承認ポリシー・遷移関数へ合流させる。受信 Follow は承認要否で確立 + Accept 配送 / 保留記録に分岐し、Accept/Reject/Block/Undo はそれぞれの状態遷移を冪等に行う
  - 受信 Follow がロック有無で確立/保留に分かれ、受信 Accept で送信中リクエストが確立、受信 Block で被ブロック + 関係解消、受信 Undo で逆操作が起こり、再受信で状態が二重変更されない状態
  - _Requirements: 7.1, 7.2, 7.3, 7.4, 7.5, 7.6, 7.7, 2.5, 2.6, 3.2_
  - _Boundary: InboundHandler, Transitions, ActivityBuilder_
  - _Depends: 2.1, 2.2, 2.3_

- [ ] 4.2 (P) ブロック判定の委譲実装（署名拒否）
  - federation-core のブロック判定委譲境界に、本 spec のブロック関係に基づく判定を供給する。署名者 URI をアカウントへ解決し、宛先ローカルアクターがブロック中なら真を返し、解除後は偽を返す
  - ブロック中の署名者についてブロック判定が真を返し、解除後に偽へ戻ることを単体/統合で確認できる状態
  - _Requirements: 6.1, 6.2, 6.3, 6.4_
  - _Boundary: BlockPolicyImpl_
  - _Depends: 1.3_

- [ ] 4.3 (P) 関係状態プロバイダ実装とフィルタ問い合わせ
  - accounts-and-instance の関係状態委譲境界へ、閲覧者+対象群の RelationshipView を返す本実装を供給し、タイムライン/通知向けのブロック/被ブロック/ミュート（期限考慮）/フォロー集合、およびブースト表示無効（`show_reblogs=false`）集合 `reblogs_hidden` の問い合わせを `FilterQuery` に公開する（フィルタ適用本体は実装しない）
  - プロバイダが対象群の実フラグを返し、フィルタ集合問い合わせが期限切れミュートを除外して関係集合を返し、`reblogs_hidden` が `show_reblogs=false` のフォロー先集合を返す状態
  - _Requirements: 8.2, 8.3, 8.4, 9.1, 9.2, 9.3, 9.4_
  - _Boundary: RelProviderImpl, FilterQuery_
  - _Depends: 1.3, 2.4_
- [ ] 4.4 (P) アカウント数プロバイダ（フォロワー数/フォロー中数）の実装供給
  - accounts-and-instance `AccountCountsProvider` の `followers_count` / `following_count` 部分を follows グラフから算出して供給する（投稿数 / `last_status_at` は本 spec 範囲外で既定 0 / None）。契約は再定義せず bootstrap で既定実装（0）を差し替える
  - 配線後に Account のフォロワー数/フォロー中数が実値になり、未配線時は accounts の既定実装で安全に 0 になる状態
  - _Requirements: 8.2_
  - _Boundary: AccountCountsContribution_
  - _Depends: 1.3_

- [ ] 5. 統合: エンドポイントと配線
- [ ] 5.1 エンドポイント表層
  - follow/unfollow/follow_requests(一覧/authorize/reject)/mute/unmute/block/unblock の各エンドポイントを、適切なスコープ要求・Relationship 応答（accounts シリアライザ消費）・未存在 404・互換エラー本文・リスト系の Link 付与で実装する
  - 各エンドポイントが正しいスコープで保護され、成功時に Relationship（または Account 一覧 + Link）を返し、未認証/権限不足/未存在で互換エラーを返す状態
  - _Requirements: 1.8, 2.7, 4.6, 5.6, 10.1, 10.2, 10.4, 10.5_
  - _Boundary: SocialGraphEndpoints_
  - _Depends: 3.1, 3.2, 3.3, 3.4_

- [ ] 5.2 モジュール配線（受信ハンドラ・委譲実装・ルータ登録）
  - SocialGraphModule を組み立て、受信ハンドラを連合ディスパッチャへ登録、関係状態プロバイダを accounts レジストリへ、ブロック判定を federation-core へ既定実装と差し替え登録し、ルータを土台へ装着して AppState へ格納する
  - 起動後に受信 Activity がハンドラへ届き、accounts の relationships が実値を返し、ブロック判定が連合受信に効く状態
  - _Requirements: 6.1, 7.1, 8.2, 10.1_
  - _Boundary: SocialGraphModule_
  - _Depends: 4.1, 4.2, 4.3, 5.1_

- [ ] 6. 検証: 統合・連合テスト
- [ ] 6.1 (P) 関係操作の統合テスト
  - follow/unfollow（ローカル/リモート・冪等・自己拒否・オプション反映）、follow_requests（保留・一覧ページネーション・authorize/reject 配送）、同一サーバースキップ、mute/block（通知/期限・関係解消・配送）を統合検証する
  - 上記シナリオがエンドポイント経由で期待どおりの Relationship 応答と状態遷移・配送依頼を起こすことをテストで確認できる状態
  - _Requirements: 1.1, 1.5, 1.6, 1.7, 2.1, 2.2, 2.3, 2.4, 3.1, 3.4, 4.1, 4.2, 4.3, 5.1, 5.2, 5.4_
  - _Boundary: SocialGraphEndpoints, FollowService, BlockService, MuteService_
  - _Depends: 5.2_

- [ ] 6.2 (P) 受信処理・署名拒否・プロバイダ統合テスト
  - 受信 Follow/Accept/Reject/Block/Undo の状態遷移と冪等、ブロック後の連合受信拒否と解除後の復帰、関係状態プロバイダ供給後に accounts の relationships が実値を返すことを統合検証する
  - 受信往復・署名拒否・プロバイダ供給が期待どおり動作することをテストで確認できる状態
  - _Requirements: 6.1, 6.2, 6.3, 6.4, 7.2, 7.3, 7.4, 7.5, 7.6, 7.7, 8.2, 8.3_
  - _Boundary: InboundHandler, BlockPolicyImpl, RelProviderImpl_
  - _Depends: 5.2_

- [ ] 6.3 連合対称性テスト（2 インスタンス往復）
  - 2 インスタンスで Follow/Block の往復（確立・承認・ブロック受信拒否）を検証し、同一 Activity をローカル in-process と HTTP 配送で実行して関係状態遷移結果が同値になることを検証する
  - ローカル配送と HTTP 配送で関係状態が一致し、往復シナリオが成立することをテストで確認できる状態
  - _Requirements: 1.2, 1.3, 10.3_
  - _Depends: 6.1, 6.2_
