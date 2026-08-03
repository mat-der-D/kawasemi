# Implementation Plan

- [ ] 1. 基盤: マイグレーション・ドメイン型・委譲境界・モジュール配線
- [x] 1.1 マイグレーション 0005 で本 spec 所有テーブルを定義する
  - `migrations/0005_accounts.sql` に `account_profiles` / `remote_accounts` / `custom_emojis` / `instance_settings` を作成（0001 core-runtime / 0002 actor-model / 0003 federation+oauth 衝突 / 0004 media と非衝突の番号として 0005 を採用）
  - `account_profiles` に `display_name` / `note`（Account/CredentialAccount の同名フィールド供給元）を含める
  - `remote_accounts.actor_uri` UNIQUE、`custom_emojis` 複合主キー（shortcode, domain）、`instance_settings` 単一行（id=1 CHECK、`thumbnail`/`languages` 列を含む）を含める
  - 観測可能な完了条件: `spawn_test_app` 起動時に 0005 が自動適用され、4 テーブルが存在しクエリ可能
  - _Requirements: 6.5, 7.2, 8.2, 9.1_
- [x] 1.2 アカウント関連のドメイン型を定義する
  - `AccountView` / `ProfileField` / `CredentialSource` / `AccountProfile`（`display_name`/`note` を含む） / `ProfilePatch`（項目別部分更新入力、`None` は変更なし） / `RemoteAccount` / `CustomEmojiView` / `RelationshipView` / `AccountCounts` / `InstanceSettings` を定義
  - `AccountRef` / `Visibility` は本 spec では定義せず、core-runtime の `domain` モジュールが所有する正準共有型を import して使用する
  - 観測可能な完了条件: 各型が `cargo build` でコンパイルでき、Account 必須フィールドとリモート/ローカルの acct 規律差を型で表現できる
  - _Requirements: 1.1, 1.2, 1.3, 2.2, 5.2, 6.1, 7.2, 8.1, 9.2_
  - _Boundary: model_
  - _Depends: 1.1_
- [x] 1.3 下流所有情報の委譲境界（port + 既定実装 + レジストリ）を定義する
  - `AccountStatusesProvider`（既定: 空ページ）/ `RelationshipStateProvider`（既定: 関係なし）/ `AccountCountsProvider`（既定: 0）を定義し既定実装を提供
  - 委譲レジストリを用意し、下流 spec が後から実装を差し替えられる形にする
  - 観測可能な完了条件: 既定実装が DB/ネットワークに触れず空・全 false/0・件数 0 を返す単体テストが green
  - _Requirements: 4.3, 5.4, 1.1_
  - _Boundary: ports_
  - _Depends: 1.2_
- [x] 1.4 AccountsModule の配線骨格を core-runtime Composition Root に追加する
  - `AccountsModule` を組み立て、委譲 port を既定実装で初期化して `AppState` に格納、accounts/instance/custom_emojis のルータ装着点を用意（ハンドラは後続で実装）
  - 観測可能な完了条件: `spawn_test_app` 起動後、`AppState` から `AccountsModule` ハンドルが取得でき、ルータが空ハンドラ/プレースホルダで mount される
  - _Requirements: 10.1, 10.5_
  - _Boundary: AccountsModule_
  - _Depends: 1.3_

- [ ] 2. データ層: リポジトリ
- [x] 2.1 (P) ローカルプロフィール拡張リポジトリを実装する
  - actor_id での取得と、`update_credentials` 用の部分 upsert（指定項目のみ更新、時刻は `RuntimeContext`）を提供。未作成アクターには安全な既定を返す
  - 観測可能な完了条件: upsert が patch 外の項目を変更しないことを検証する統合テストが green
  - _Requirements: 1.4, 2.2, 6.1, 6.5_
  - _Boundary: AccountProfileRepository_
  - _Depends: 1.1_
- [x] 2.2 (P) リモートアカウント正規化リポジトリを実装する
  - actor_uri / 内部 id での取得、正規化結果の upsert、`fetched_at` による陳腐化判定を提供
  - 観測可能な完了条件: 同一 actor_uri の再 upsert が重複行を作らず最新値で更新される統合テストが green
  - _Requirements: 3.1, 3.2, 7.2, 7.3_
  - _Boundary: RemoteAccountRepository_
  - _Depends: 1.1_
- [x] 2.3 (P) カスタム絵文字読み取りリポジトリを実装する
  - `visible_in_picker` を含む一覧取得とショートコード→絵文字解決を read 専用で提供（書き込み API を持たない）
  - 観測可能な完了条件: 投入済み絵文字に対し一覧取得とショートコード解決が期待値を返す統合テストが green
  - _Requirements: 1.4, 9.1, 9.3_
  - _Boundary: CustomEmojiRepository_
  - _Depends: 1.1_
- [x] 2.4 (P) インスタンス運用設定リポジトリを実装する
  - 運用設定行を取得し、未設定項目を安全な既定にマージして常に全項目埋まった値を返す（`thumbnail` は既定 `null`、`languages` は既定 `[]` を含む）
  - 観測可能な完了条件: 設定未投入でも全項目（`thumbnail`/`languages` を含む）が既定で埋まった値が返る統合テストが green
  - _Requirements: 8.1, 8.2, 8.3_
  - _Boundary: InstanceSettingsRepository_
  - _Depends: 1.1_

- [ ] 3. シリアライザとエンティティ契約ゴールデン
- [x] 3.1 (P) Account シリアライザ（ローカル/リモート/Credential 統一）を実装する
  - ローカル（`ResolvedActor` + `AccountProfile`）とリモート（`RemoteAccount`）を共通 Account JSON へ写像。acct/url/uri 規律分け、avatar/header 既定 URL（非 null）、emojis 解決、CredentialAccount の source/role 付与、counts は `AccountCountsProvider`
  - `display_name`/`note` はローカルは `AccountProfile`、リモートは `RemoteAccount` の同名フィールドから供給する
  - 観測可能な完了条件: 同一入力で決定的 JSON を生成し、avatar/header が常に非 null になる単体テストが green
  - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 2.2_
  - _Boundary: AccountSerializer_
  - _Depends: 1.2, 2.1, 2.3_
- [x] 3.2 (P) Relationship シリアライザを実装する
  - `RelationshipView`（既定: 関係なし）から Req 5.2 の全フラグを持つ JSON を生成
  - 観測可能な完了条件: 既定値で全フラグ false・件数 0・note 空の JSON を生成する単体テストが green
  - _Requirements: 5.1, 5.2, 5.4_
  - _Boundary: RelationshipSerializer_
  - _Depends: 1.2_
- [x] 3.3 (P) Instance(v2) シリアライザを実装する
  - `title`/`description`/`contact`/`rules`/`registrations`/`thumbnail`/`languages` は運用設定（`InstanceSettings`）から供給し、`version`/`source_url` はビルド時定数（`env!("CARGO_PKG_VERSION")` 等）から、`usage.users.active_month` は MVP 固定値プレースホルダから供給して Instance(v2) JSON を合成。`configuration` は media-pipeline の上限等と整合させる
  - 観測可能な完了条件: 運用設定値が反映され、未設定項目が既定で埋まり、`version`/`source_url`/`usage.users.active_month` が決定的に再現され、`configuration` が実制約と整合する単体テストが green
  - _Requirements: 8.1, 8.2, 8.3, 8.4_
  - _Boundary: InstanceSerializer_
  - _Depends: 1.2, 2.4_
- [x] 3.4 (P) CustomEmoji シリアライザを実装する
  - `CustomEmojiView` から CustomEmoji JSON を生成し、Account の `emojis` と同一表現を共有
  - 観測可能な完了条件: shortcode/url/static_url/visible_in_picker/category を持つ JSON を生成する単体テストが green
  - _Requirements: 9.2, 9.4_
  - _Boundary: CustomEmojiSerializer_
  - _Depends: 1.2_
- [x] 3.5 全エンティティ契約を api-foundation 契約ハーネスにゴールデン登録する
  - Account / CredentialAccount / Relationship / Instance(v2) / CustomEmoji を決定的 `RuntimeContext` 上でゴールデン固定し、フィールド有無・型・null 規律（avatar/header 非 null、emojis/fields 形）を比較
  - 観測可能な完了条件: 各エンティティのゴールデン比較テストが green で、差分が箇所特定で報告される
  - _Requirements: 1.6, 2.4, 5.6, 8.5, 9.5_
  - _Depends: 3.1, 3.2, 3.3, 3.4_

- [x] 4. リモートアカウントフェッチャを実装する
  - 有効キャッシュ時は取得せず、ミス/陳腐化時のみ federation-core の `FederationHttpClient` で取得し、JSON-LD 安全展開で正規化（未知プロパティで失敗させず、必須欠落のみ失敗）、結果をキャッシュ upsert
  - 観測可能な完了条件: `FederationHttpClient` モックで取得→正規化→キャッシュ保存が成立し、未知プロパティ付き文書でも正規化が成功する統合テストが green
  - _Requirements: 7.1, 7.2, 7.3, 7.4, 7.5_
  - _Boundary: RemoteAccountFetcher_
  - _Depends: 2.2, 3.1_

- [ ] 5. サービス層
- [x] 5.1 verify_credentials と accounts/:id を実装する
  - verify_credentials はトークンの単一アクターを CredentialAccount で返す。accounts/:id はローカル（`ActorDirectory`）/既知リモート/必要時フェッチで解決し、未存在は 404
  - 観測可能な完了条件: ローカル/リモートいずれも Account を返し、未存在で 404 を返すサービス単位テストが green
  - _Requirements: 2.1, 3.1, 3.2, 3.3_
  - _Boundary: AccountService_
  - _Depends: 3.1, 4_
- [x] 5.2 accounts/:id/statuses（委譲）を実装する
  - アカウント解決 + ページネーション解釈 + 絞り込み/可視性コンテキストを `AccountStatusesProvider` へ受け渡し、未登録時は空ページを返す
  - 観測可能な完了条件: provider 未登録で空ページ（`Link` 付き）を返し、絞り込み条件が provider へ伝達されるテストが green
  - _Requirements: 4.1, 4.2, 4.4, 4.5_
  - _Boundary: AccountService_
  - _Depends: 5.1_
- [x] 5.3 relationships（委譲）を実装する
  - 対象 id 群を `RelationshipStateProvider` へ問い合わせ `RelationshipSerializer` で配列化。未登録時は全既定
  - 観測可能な完了条件: 複数 id で Relationship 配列を返し、provider 未登録で全既定になるテストが green
  - _Requirements: 5.1, 5.3, 5.4_
  - _Boundary: AccountService_
  - _Depends: 3.2, 5.1_
- [x] 5.4 update_credentials を実装する
  - 検証（フィールド上限・privacy 許容値・focus 範囲）→ avatar/header の media-pipeline 取込 → プロフィール部分 upsert → 更新後 CredentialAccount を返す。検証違反は 422
  - 観測可能な完了条件: 部分更新が verify_credentials/accounts/:id に反映され、検証違反で 422 を返すテストが green
  - _Requirements: 6.1, 6.2, 6.3, 6.5_
  - _Boundary: AccountService, AccountProfileRepository_
  - _Depends: 2.1, 3.1_
- [x] 5.5 (P) InstanceService を実装する
  - 運用設定読取 + 実制約合成で instance v2 を返す
  - 観測可能な完了条件: 運用設定が反映された Instance(v2) を返すサービステストが green
  - _Requirements: 8.1, 8.2_
  - _Boundary: InstanceService_
  - _Depends: 2.4, 3.3_
- [x] 5.6 (P) CustomEmojiService を実装する
  - visible なカスタム絵文字一覧を返す
  - 観測可能な完了条件: visible 絵文字一覧を CustomEmoji 配列で返すサービステストが green
  - _Requirements: 9.1_
  - _Boundary: CustomEmojiService_
  - _Depends: 2.3, 3.4_

- [x] 6. 全エンドポイントを横断レイヤーに乗せて mount する
  - verify_credentials(`read:accounts`)/relationships(`read:follows`)/update_credentials(`write:accounts`) に Bearer+Scope を適用、accounts/:id・accounts/:id/statuses・instance・custom_emojis は任意/公開、エラーは Mastodon 互換本文、リスト系は `Link`+プロキシ尊重 URL、`X-RateLimit-*` 装着点に乗せる
  - 観測可能な完了条件: 各エンドポイントが期待スコープを要求し、未認証で公開応答する箇所が応答し、全エラーが互換 JSON で返る統合テストが green
  - _Requirements: 2.3, 3.4, 5.5, 6.4, 10.1, 10.2, 10.3, 10.4, 10.5_
  - _Boundary: AccountsEndpoints, AccountsModule_
  - _Depends: 5.1, 5.2, 5.3, 5.4, 5.5, 5.6_

- [ ] 7. 統合と検証
- [x] 7.1 アカウント系エンドポイントの統合テストを通す
  - verify_credentials / accounts/:id / accounts/:id/statuses / relationships / update_credentials の往復を `spawn_test_app` 上で検証（401/403/404/422・ページネーション・委譲未登録時の空/既定・更新反映）
  - 観測可能な完了条件: 上記シナリオの統合テストがすべて green
  - _Requirements: 2.1, 2.3, 3.1, 3.3, 3.4, 4.1, 4.2, 5.1, 5.4, 6.1, 6.3, 6.4_
  - _Depends: 6_
- [x] 7.2 instance v2 / custom_emojis とリモート取得の統合テストを通す
  - instance v2 の運用設定反映と既定、custom_emojis の visible 一覧、`FederationHttpClient` モックでのリモート取得→正規化→Account→キャッシュ再利用→取得失敗の互換応答を検証
  - 観測可能な完了条件: instance/custom_emojis/リモート取得の統合テストがすべて green
  - _Requirements: 7.1, 7.2, 7.3, 7.4, 8.1, 8.2, 9.1_
  - _Depends: 6_
- [x] 7.3 エンティティ契約ゴールデンの最終検証を行う
  - Account / CredentialAccount / Relationship / Instance(v2) / CustomEmoji のゴールデンが決定的に再現され、null 規律・配列形が固定されていることを最終確認
  - 観測可能な完了条件: 契約ゴールデンテストが決定的に再現し green
  - _Requirements: 1.6, 2.4, 5.6, 8.5, 9.5_
  - _Depends: 3.5_

## Implementation Notes

- タスク 3.4: `CustomEmojiSerializer::to_custom_emoji_json` は task 3.1 の `AccountSerializer`（`serializer.rs`）が定義した `pub` な `CustomEmojiJson` 型をそのまま再利用し Req 9.4 の表現共有を型レベルで満たしているが、`serializer.rs::emoji_to_json`（マッピング関数自体）は非公開のままのため、フィールドマッピングのロジックは２箇所に独立実装されている（現状は完全に同一挙動で、両者を突き合わせる回帰テストで担保）。将来のクリーンアップタスクで `emoji_to_json` を `pub(crate)` にして `custom_emoji_serializer.rs` から直接呼び出す形に統合する余地がある（レビューでの提案、本 run では対応不要と判断）。

- タスク 2.1: `AccountProfileRepository::find_profile` は design.md の Service Interface の文字どおり `Option<AccountProfile>` を返す（プロフィール未作成時は `None`）。task 文の「未作成アクターには安全な既定を返す」は `AccountProfile::default_for(actor_id)`（`profile_repository.rs` 内、呼び出し側が `None` 時に使う既定値コンストラクタ）で満たす。後続タスク（5.1 AccountService 等）で `find_profile` を使う際は `Option` を明示的に `default_for` へフォールバックさせること。`CredentialSource::follow_requests_count` は本リポジトリでは常に `0`（social-graph 委譲、`account_profiles` に対応列なし）。
- タスク 2.2: `RemoteAccountRepository` も同型パターン。design.md の Service Interface に無い「`fetched_at` による陳腐化判定」は `is_stale(fetched_at, now, ttl)`（`remote_repository.rs` 内の純粋関数、TTL は呼び出し側指定）として追加。TTL の実値は spec のどこにも規定が無いため、実際のキャッシュポリシー決定は `RemoteAccountFetcher`（task 4）の責務とする。`upsert_remote` は `ON CONFLICT (actor_uri) DO UPDATE` で `id` 列を SET から除外し、同一 `actor_uri` の再 upsert では既存行の `id` を保持する（`remote_accounts.id` はアプリ採番・DB 非採番のため、呼び出し側が入力に異なる `id` を渡しても無視され、戻り値の実際の行が正）。後続タスクで `upsert_remote` を呼ぶ側は戻り値の `id` を正として扱うこと（入力の `id` をそのまま信用しない）。
- タスク 2.3: `CustomEmojiRepository::resolve_emojis`/`list_visible_emojis` は **いずれも domain フィルタを持たない**（ローカル/リモート問わず全ドメインの custom_emojis 行が対象）。初回実装は `resolve_emojis` を `domain = ''`（ローカルのみ）に絞る判断をしたがレビューで REJECTED — design.md の「accounts/:id 取得」フロー図がローカル/リモート両方で同一の emoji 解決ステップを通ること、`AccountSerializer::build_account_remote` が `build_account_local` と同じ `emojis: &[CustomEmojiView]` 引数を取ること、Requirement 9.4（emojis 構築は常に同一読み取りモデル）、`migrations/0006_accounts.sql` のコメント（リモートドメイン別の行は想定内）と矛盾するため。後続タスク（3.1 AccountSerializer 等）で `resolve_emojis` を呼ぶ際は、ローカル/リモートどちらのアカウントに対しても同じ呼び出しで良い（呼び出し側でのドメイン絞り込みは不要）。
- タスク 2.4: `InstanceSettingsRepository::load_instance_settings` は `instance_settings` テーブルに行が一切無い状態（初期状態、書き込みは admin-frontend の責務のため本 spec は絶対に INSERT/UPDATE/UPSERT しない）でも、アプリ側で構築した既定値（`migrations/0006_accounts.sql` の各列 `DEFAULT` と一致させた値）を返す。`id=1` 行が存在すればそれをそのまま読む。後続タスク（5.5 InstanceService 等）はこの関数を呼ぶだけでよく、`instance_settings` の未初期化を心配する必要はない。
- タスク 4: `RemoteAccountFetcher` のキャッシュ TTL は design.md/requirements.md のどこにも規定が無かったため（task 2.2 のノートが本タスクへ委譲）、`key_resolver.rs::DEFAULT_PUBLIC_KEY_CACHE_TTL`（24h）と同値を `DEFAULT_REMOTE_ACCOUNT_CACHE_TTL` として踏襲した（同種のリモートアクター文書由来キャッシュであるため）。`ttl` はコンストラクタ引数であり config 未配線（`key_resolver.rs` と同じ前例）。フェッチ失敗（非成功ステータス/トランスポート失敗）は内部 `AppError` として `404` にマッピングしている（`key_resolver.rs` の `502` とは異なる選択）が、`RemoteAccountFetcher` は現時点でどのエンドポイントにも配線されていないため、実際の HTTP ステータスは task 5.1（`AccountService::show_account`、Requirement 3.3「未存在は404」）が最終的に決定してよい内部既定値に過ぎない。

- タスク群 2 の run スコープ最終レビューでの注意喚起（task 3.1 AccountSerializer 実装時に要確認）: `CustomEmojiRepository::resolve_emojis(pool, shortcodes)` は design.md の文字どおりのシグネチャ（domain 引数なし）に従うため、同一 shortcode がローカルと特定リモートドメインの両方に存在する場合、両方の行を返してしまい呼び出し側では区別できない（`CustomEmojiView`/`custom_emojis` テーブルの複合 PK は `(shortcode, domain)` だが `CustomEmojiView` 自体は `domain` フィールドを持たない — task 1.2 の model.rs 由来、本 run では変更していない）。task 3.1 で「shortcode だけで zip する」ような素朴な実装をすると、衝突時に誤った絵文字 URL を選んでしまう可能性がある。対応方針は task 3.1 実装時に設計判断すること（例: 対象アカウントの domain も渡す/`CustomEmojiView` に domain を追加する等、design.md/model.rs への revalidation が必要になる可能性がある）。

- タスク 5.1: `AccountService::verify_credentials`/`show_account` を `src/accounts/account_service.rs` に実装。`emoji_candidates` は `serializer.rs` のショートコード抽出ヘルパーが非公開のため `resolve_emojis`（対象ショートコードのみ）ではなく `list_visible_emojis`（`visible_in_picker=TRUE` の全件）を使っている — 本文に含まれない絵文字も候補に入り得るが、`AccountSerializer` 側の突き合わせで最終的な `emojis` 配列には無関係なものは混入しない（レビューで non-blocking 確認済み）。後続タスクで真にショートコード限定の解決が必要になった場合は、`serializer.rs` 側のヘルパーを `pub(crate)` 化して共有する対応を検討すること。`show_account` の id 解決規律: 数値文字列はローカル actor id → 既知リモート id の順、非数値文字列は `actor_uri` とみなし `RemoteAccountFetcher::fetch_and_normalize` に渡す（design.md のフロー図に明示の「フェッチ」分岐は無いが task 文の「必要時フェッチ」と `_Depends: 3.1, 4_` から導出、レビュー承認済み）。`AccountsModule`/`build_accounts_module` のコンストラクタ引数が増えたため `bootstrap.rs`/`test_harness.rs`/`federation/test_harness.rs`/`server/tests.rs`/`state/tests.rs` を配線更新。`ActorDirectory::actor_created_at`（`src/actor/directory.rs`）を新規追加 — `resolve_actor_by_id`/`sole_owner` と同型の「下流タスクによる narrow な上流追加」パターン。

- タスク 5.2: `AccountService::list_statuses` を追加（`AccountsModule`/コンストラクタの配線変更は不要 — `AccountPortsRegistry` は task 5.1 の時点で既に `AccountService` のフィールドだった）。`StatusesQueryInput`（design.md に型定義は無く、Service Interface のシグネチャ名のみ存在 — フィールド構成 `{ page: PageParams, pinned, only_media, exclude_replies, exclude_reblogs }` は本タスクの判断、レビューで承認済み）を新設し `src/accounts.rs` から re-export（task 6 の `AccountsEndpoints` が使う）。`id` 解決は task 5.1 の `show_account`/`resolve_local` と同じ判別規律を再利用する private helper `resolve_account_ref` を新設。`StatusesQuery.page` は `ports.rs` 定義どおり未パース（生の `PageParams`）のまま `AccountStatusesProvider` へ渡す — カーソル形式の決定は該当 provider（将来の statuses-core 登録）側の責務。フルスイート実行中に `federation::outbound::worker::tests::run_once_delivers_a_due_job_and_marks_it_done`／`run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves` が単発で flaky に失敗することがある（本 spec の変更と無関係、DB プール競合起因と推測、単体実行では毎回 green）。後続タスクでこの flaky が再発した場合は本 spec の変更を疑う前にまず単体再実行で切り分けること。

- タスク 5.3: `AccountService::relationships` を追加（`RelationshipSerializer` は無フィールドの unit struct のため呼び出しごとに `RelationshipSerializer::new()` を生成 — `AccountsModule`/コンストラクタの配線変更は不要）。**未解決 id の扱いに関する未確定事項**: Requirement 5.1 は「各識別子に対応する Relationship エンティティの配列を返す」と読めるが、対象 id が既知アカウントに解決できない場合の挙動は requirements.md/design.md のどちらにも明記が無い。本タスクでは該当 id を結果配列から黙って除外する実装を選択した（`RelationshipView.id` は内部 `Id` 型のため解決不能な id に対する合成値を持てないこと、実際の呼び出し元は通常既に解決済みの Account から id を渡すため稀なエッジケースであること、Mastodon 実装も同様に未解決アカウントを黙って除外することを根拠とし、レビューで defensible な暫定解釈として承認済み）。後続タスクでこの挙動が問題になった場合（例: クライアントが入力 id 数と出力配列長の対応を期待する）は、requirements.md 5.1 の明確化（除外を明文化する、または不明アカウント用の既定 Relationship エントリを返す規約を追加する等）を検討すること。フルスイート実行中に `media_upload_it::upload_exceeding_the_configured_size_limit_is_422_with_a_compatible_error_body` が broken pipe で単発 flaky することも観測（本 spec と無関係、単体実行では green）— task 5.2 の note にある `federation::outbound::worker` の flaky と合わせ、本 spec とは無関係な並行実行環境要因による flaky が複数存在することを後続タスクの実装者は認識しておくこと。

- タスク 5.4: `AccountService::update_credentials` を追加。`AccountService` に `media: Arc<MediaService<S>>` と `runtime: RuntimeContext` フィールドを新設（`AccountService::new` は 8 引数になったため `#[allow(clippy::too_many_arguments)]` を付与 — `AppState::new` と同様の既存precedent）。`AccountsModule`/`build_accounts_module` は既存の `MediaModule::service()` をそのまま受け取り、2 つ目の `MediaService`/`MediaConfig` を新規構築しない（`bootstrap.rs`/`test_harness.rs`/`federation/test_harness.rs`/`server/tests.rs`/`state/tests.rs` を配線更新）。検証は「フィールド数上限・各フィールド長・focus 範囲」を write 前にすべて fail-fast で行い、いずれか違反時は 422 で一切の副作用（media ingest も profile upsert も）を起こさない。`UpdateCredentialsInput`（design.md に型定義なし、本タスクの判断）の数値上限定数（`MAX_PROFILE_FIELDS=4`/フィールド名 255/値 255/`display_name` 30/`note` 500）は requirements.md/design.md のどちらにも具体的な数値の規定が無いため Mastodon 実際の制限値を参考に選定（レビューで défensible な暫定解釈として承認済み、正式な数値要件が必要な場合は requirements.md 6.3 の明確化を検討すること）。`avatar`/`header` は「アップロードして設定」のみサポートし、`ProfilePatch::avatar_media`/`header_media` が本来持つ `Option<Option<Id>>` の「明示的にクリア」は本タスクでは未実装（Requirement 6.1 の文言はクリア操作を明示要求していないためレビューで承認済み、将来必要になれば拡張すること）。avatar/header を両方アップロードする際、片方の ingest 成功後にもう片方が失敗した場合のロールバック/孤立メディア削除は無い（design.md のシーケンス図自体がこの補償トランザクションを想定していないため本タスク由来の欠陥ではない、将来のメディア孤立クリーンアップ検討時に留意）。初回レビューで REJECTED（"部分更新が verify_credentials/accounts/:id に反映され" という完了条件のうち `accounts/:id`（`show_account`）経由の反映が未検証だった）→ テストに `show_account` 呼び出しでの反映確認を追加し 2 回目レビューで APPROVED。

- タスク 5.5: `InstanceService::instance_v2` を新設（`src/accounts/instance_service.rs`、`AccountService` とは別系統、`account_service.rs` は無変更）。`ServerCapabilities` は `InstanceService` 構築時に `MediaConfig`（`AppConfig` 由来、起動時に一度だけ読み込まれ実行時再読込パスが存在しない）から一度だけ `ServerCapabilities::from_media_config` で構築し保持する（`instance_serializer.rs` 自身のドキュメントコメントが同じパターンを想定・推奨済み、レビューで staleness の懸念なしと確認済み）。`build_accounts_module` に `media_config: MediaConfig` パラメータを追加（8 引数目、`#[allow(clippy::too_many_arguments)]` は `AppState::new`/`AccountService::new` と同一precedent）。

- タスク 5.6: `CustomEmojiService::list_custom_emojis` を新設（`src/accounts/emoji_service.rs`、`AccountService`/`InstanceService` とは別系統、両ファイルとも無変更）。本タスク群で最小の配線差分（`src/accounts.rs` のみ、`bootstrap.rs`/`test_harness.rs` 等は無変更 — `pool` のみで構築可能なため）。これでタスク群 5（サービス層）の全サブタスク（5.1-5.6）が完了。フルスイート実行中に `federation::outbound::worker::tests::run_once_reschedules_a_job_on_transient_failure_with_backoff_applied` が単発 flaky することも新たに観測（task 5.2/5.3 note の `run_once_delivers_a_due_job_and_marks_it_done`／`run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves` と同一モジュールの並行実行タイミング起因、本 spec は `src/federation/**` を一切変更していないため無関係と確認済み、単体実行では green）。`federation::outbound::worker` 配下のテストは並行フルスイート実行下でこの種の flaky が複数観測されており、後続タスクの実装者はこのモジュール由来の flaky を本 spec の回帰と誤認しないよう注意すること。

- タスク 6: `src/accounts/endpoints.rs`（`AccountsEndpoints`）を新設し、7 エンドポイント（`verify_credentials`/`relationships`/`update_credentials`/`show_account`/`list_statuses`/`instance_v2`/`custom_emojis`）を `accounts_router()`（`src/server.rs`）の `501` プレースホルダから実ハンドラへ差し替え。`AccountsEndpointsState`（`MediaEndpointsState<LocalFsStore>` と同型の router-local state bundle）＋ `impl FromRef<AppState> for AccountsEndpointsState` を追加。スコープは `read:accounts`/`read:follows`/`write:accounts`、`accounts/:id`・`accounts/:id/statuses`・`instance`・`custom_emojis` は `OptionalActor`/無認証。**未確定事項（レビューで non-blocking 確認済み、spec 明確化の余地あり）**: Requirement 10.4 の文言は「リスト系応答（`accounts/:id/statuses` / `relationships`）」の両方に `Link`+プロキシ尊重 URL を要求するように読めるが、design.md の API Contract table は `+ Link` を `accounts/:id/statuses` の行にしか付けておらず、`AccountService::relationships`（task 5.3、既承認、本タスクの境界外）はカーソル概念を持たないフラットな配列を返す。本タスクは design.md のテーブルに従い `relationships` に `Link` ヘッダを付けていない。後続タスク（7.1 の統合テスト作成時、または spec メンテナのレビュー時）でこの requirements.md/design.md 間の文言齟齬を解消する場合、`relationships` に真のページネーションを持たせるなら `AccountService::relationships`（5.3 の境界）と `RelationshipView`/design.md 双方の revalidation が必要になる — 本タスク単独では対応不可な規模の変更であることに留意。`update_credentials` のマルチパートフィールド名（`fields_attributes[N][name]`/`[value]`、`source[privacy]`/`[sensitive]`/`[language]`）と `relationships` の `id` クエリパラメータ（`id=1&id=2` と Mastodon 実際の `id[]=1&id[]=2` の両方を受理）は design.md/requirements.md 未規定のため本タスクの判断（Mastodon 実際の wire 形式を参考に選定、レビューで承認済み）。

- タスク 7.1: `tests/account_show_it.rs`/`account_statuses_it.rs`/`relationships_it.rs`/`update_credentials_it.rs` を新設（既存の task 6 エンドポイントに対する統合テストのみ、`src/` の変更は無し）。task 5.4 の初回レビューで REJECTED になった「`update_credentials` の部分更新が `show_account` にも反映されるか」を本タスクのテストで実際に往復確認済み。avatar/header の focus 範囲バリデーション（Requirement 6.3）は `endpoints.rs` 自身のドキュメントコメントの通り HTTP 経由では到達不能な dead code のため本統合テストの対象外（account_service 単体テストで既にカバー済み）。リモートアカウント解決（`accounts/:id` がリモートアクターを指す場合）は task 7.1 の `_Requirements:_` に含まれないため意図的に対象外とし、task 7.2 の担当とする。

- タスク 7.2: `tests/instance_v2_it.rs`/`custom_emojis_it.rs`/`remote_account_fetch_it.rs` を新設（`src/` の変更は無し）。`instance_v2_it.rs`/`custom_emojis_it.rs` は task 7.1 と同じく `spawn_test_app()` の実ルータ経由（生 TCP）で検証。`remote_account_fetch_it.rs` のみ `AccountService<LocalFsStore, MockFederationHttpClient>` を直接構築する形で検証（実マウント済みエンドポイント経由ではない）— `AccountsModule`/`build_accounts_module`（`src/accounts.rs`）が具象型 `ReqwestFederationHttpClient` にモノモーフィズされておりモック差し替えの継ぎ目が無いこと、`FederationHttpClient`（`async fn` トレイトメソッドのためオブジェクト安全でない）が本コードベース全体で同じ理由により常にジェネリック（`Arc<H>`）として扱われ `Arc<dyn FederationHttpClient>` 化された前例が無いことをレビューで確認済み。`tests/signatures_it.rs`/`inbox_delivery_it.rs` に既に同型の前例（`spawn_test_app()` で実 DB プール/スキーマのみ使い、対象コンポーネントは `MockFederationHttpClient` で直接構築）があり、本タスクもそれに倣った。fetch→normalize→Account・キャッシュ再利用（2 回目呼び出しで fetch 回数が増えないことを `mock.fetched_urls()` で確認）・非成功ステータス/トランスポート失敗→Mastodon 互換エラー応答、をすべてカバー。

- タスク 7.3: 検証のみ、`src/`/`tests/` の変更は無し。task 3.5 が登録済みの 6 ゴールデン（Account ローカル/リモート/CredentialAccount/Relationship/Instance(v2)/CustomEmoji、いずれも `crate::contract::assert_golden` + `tests/golden/accounts/*.json`）を再確認し、決定性（フィクスチャ構築が `Id::from_i64`/`datetime!` 等のリテラルのみで clock/uuid/DB 由来値が無いこと）・null 規律（`last_status_at`/`verified_at` の null と非null の両方をゴールデン内に同居させて個別フィールド粒度で検証、avatar/header は常に非null）・配列形固定（`emojis`/`fields`/`languages`/`rules` が空でも `[]`、要素ありでも固定形）を実コードとゴールデン JSON を直接読んで確認済み。レビューで判明: design.md の File Structure Plan（196行目）は `tests/entity_contract_it.rs` という専用集約ファイルを想定していたが、task 3.5 は実際には各シリアライザの unit `tests.rs` にインラインでゴールデンを配置しており、この逸脱について task 3.5 自身のコミット/Implementation Notes に説明が無かった（本 spec の他の逸脱は全て記録済みなのに対し例外）。7.3 の観測可能完了条件（決定的に再現し green）には影響しないため対応不要と判断（レビューで non-blocking 確認済み）だが、将来 task 3.5 に遡って Implementation Notes を追記する余地がある。
