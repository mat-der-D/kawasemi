# Mastodon 互換 API 機能洗い出し・工数見積もり

`mastodon-api-compat.md` の方針に基づく、Mastodon クライアント API の機能項目の網羅と相対工数見積もり。

最終更新: 2026-06-27

---

## 0. 前提と読み方

- 対象は **Mastodon クライアント API（REST + Streaming）**。連合基盤（HTTP Signatures・WebFinger・NodeInfo 等）は `fediverse-design.md` 3.1 の範囲で、本書には含めない（ただし依存先として頻出する）。
- 判断軸は一貫して **一人鯖・複数アクター・連合前提**（`fediverse-design.md` 1.1〜1.2）。
- **要否の区分**:
  - **MVP 必須**：標準クライアント（Ivory・Elk・Phanpy）が実用的に動く最小ラインに含まれるもの。
  - **後回し**：クライアントは動くが体験向上のため後続フェーズで実装。
  - **不要**：本プロジェクトの前提で価値が無い、または独自フロント側で担う（標準 API では提供しない）。
- **見積もりの前提（重要）**：実装は **Opus による自律 TDD**（契約固定 → 実装 → グリーン → レビューの垂直スライスを自走）で行う。**コーディングのスループットはボトルネックではない**。したがって「人間が手で書く人日」では測らず、支配的コストを次の2つに置く:
  - **判断負荷（決め）**：契約・スキーマ・割り切りなど、AI に委ねられない設計判断。
  - **レビュー負荷（受け入れ）**：自律ループ出力の検収量と、リスクの高い箇所の作り込み。
- **規模ラベル**は「**自律スライス数**（≒ TDD 1 サイクルの本数）」と「**人間関与（判断＋レビュー）**」で読み替える。時間は **Opus 自律 TDD のウォールクロック**（人間のレビュー込み）。目安として、**人手なら半日〜1 日かかる塊が AI では概ね 10 分**で片づく圧縮率を基準にする:
  - **S** = スライス 1〜2／人間関与 低／〜20 分
  - **M** = スライス 3〜5／人間関与 中／20〜60 分
  - **L** = スライス 5〜10／人間関与 中〜高／1〜3 時間
  - **XL** = スライス 10+／**人間関与 高（設計判断が支配的）**／3〜8 時間
- 工数には**当該カテゴリ固有の実装＋テスト**を含むが、横断サブシステム（OAuth・ページネーション・Streaming 基盤）は別建て（→ 第 2 章）。二重計上を避けるため、横断側で見たものはカテゴリ側では「基盤に乗る差分」のみ計上する。

---

## 1. カテゴリ別 機能洗い出し・見積もり

### 1.1 apps / oauth（アプリ登録・認可）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| アプリ登録 | `POST /api/v1/apps`, `GET /api/v1/apps/verify_credentials` | クライアント資格情報の発行 |
| 認可 | `GET /oauth/authorize`, `POST /oauth/token`, `POST /oauth/revoke` | 認可コードフロー・トークン発行/失効 |

- **要否**：**MVP 必須**（すべての認証付き API の入口）。
- **工数**：横断サブシステム側で計上（→ 2.1）。エンドポイント自体は OAuth サーバー本体の薄い表層。
- **依存**：なし（最上流）。連合基盤とは独立。
- **リスク**：
  - 一人鯖でも**サインアップ UI は不要**だが、`oauth/authorize` の**承認 UI（最小限の HTML 画面）**は標準クライアントのログインに必須。ここを忘れると「ログインできない」で詰む。
  - **複数アクターと 1 トークン = 1 アカウント**の衝突（`mastodon-api-compat.md` 3）。`authorize` で**どのアクターとしてログインするか**を選ばせる画面が要る。Mastodon には無い独自の分岐点。
  - スコープ（`read` / `write` / `follow` / `push` と細分スコープ）の検証を最初から正しく。後付けは破壊的。

### 1.2 accounts（アカウント）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 自己情報 | `GET /api/v1/accounts/verify_credentials`, `PATCH …/update_credentials` | ログイン中アカウント取得・プロフィール更新 |
| 他者取得 | `GET /api/v1/accounts/:id`, `GET …/lookup`, `POST …/relationships` | アカウント表示・関係取得 |
| 投稿一覧 | `GET /api/v1/accounts/:id/statuses` | プロフィールのトゥート一覧 |
| フォロー操作 | `POST …/follow`, `…/unfollow`, `…/block`, `…/mute` | 関係操作（→ follows/blocks に再掲） |
| フォロワー/フォロー中 | `GET …/followers`, `…/following` | リスト取得 |
| 認証情報 | `GET /api/v1/familiar_followers`, `…/featured_tags` | 補助情報 |

- **要否**：**MVP 必須**（`verify_credentials`・`:id`・`statuses`・`relationships` はクライアント起動直後に叩かれる）。`familiar_followers` は**後回し**。
- **工数**：**L**（2〜3 時間）。実装より **`Account` 契約の固定が判断の山**（`source`/`fields`/`emojis`/`roles`/`moved` の形を先に決め切る）。`update_credentials`（プロフィールメタデータ＝キー・バリュー、アバター/ヘッダのメディア処理）はスライス数が増える。
- **依存**：OAuth（2.1）／メディア（1.6・アバター/ヘッダ）／連合基盤（リモートアカウントの取得・正規化）。
- **リスク**：
  - **`Account` の JSON 形は表面積が広い**（`source`・`fields`・`emojis`・`roles`・`moved` 等）。契約テストを先に固定（`mastodon-api-compat.md` 4.2）。
  - **プロフィールメタデータ（fields）** は独自機能ではないが MFM／リッチテキスト（`fediverse-design.md` 2.1）と絡む。
  - **受信側 `Move`**（`moved` フィールド・`fediverse-design.md` 2.2）を `Account` に出す必要。送信は不要。
  - `update_credentials` は `multipart/form-data` でネスト構造（`fields_attributes`・`source[privacy]` 等）を取る。パースが地味に厄介。

### 1.3 statuses（投稿）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 投稿 CRUD | `POST /api/v1/statuses`, `GET …/:id`, `PUT …/:id`, `DELETE …/:id` | 作成・取得・編集・削除 |
| コンテキスト | `GET …/:id/context` | スレッド（祖先・子孫） |
| 編集履歴 | `GET …/:id/history`, `GET …/:id/source` | 編集差分・原文 |
| ブースト | `POST …/reblog`, `…/unreblog`, `GET …/reblogged_by` | 再投稿 |
| お気に入り | `POST …/favourite`, `…/unfavourite`, `GET …/favourited_by` | ふぁぼ |
| ピン留め | `POST …/pin`, `…/unpin` | プロフィール固定 |
| ミュート会話 | `POST …/mute`, `…/unmute` | スレッドミュート |

- **要否**：**MVP 必須**（プロジェクトの中核）。`history`/`source` は編集機能（`fediverse-design.md` 2.1 投稿編集）に伴い**MVP 必須**。
- **工数**：**XL**（4〜8 時間）。最大の塊で**人間の判断が支配的**：`Status` エンティティの契約、可視性（public/unlisted/private/direct）の意味論、CW、メンション/タグ/絵文字の抽出、編集（`PUT` + 履歴）、引用投稿（独自・`fediverse-design.md` 3.4）。実装は自走するが、可視性・addressing の決めをスライスごとにレビューする量が効く。
- **依存**：OAuth／メディア（添付）／Poll（1.7）／連合基盤（配送・可視性は**ローカル/リモート共通コードパス**＝`fediverse-design.md` 1.2）／カスタム絵文字（1.13）。
- **リスク**：
  - **可視性判定・addressing をローカル最適化パスとリモートで一致させる**（`fediverse-design.md` 1.2 のショートカットの罠）。最重要リスク。
  - **投稿編集履歴**：`StatusEdit` の保持・差分表現、編集による連合 `Update` 送信、`favourited`/`reblogged` のリセット挙動。
  - **冪等性**：`Idempotency-Key` ヘッダの尊重（二重投稿防止）。標準クライアントが送ってくる。
  - **引用投稿・絵文字リアクション**は方言の出し分け（`fediverse-design.md` 3.4）。標準 Mastodon クライアントは引用 UI を持たないものが多く、**API では出すが体験は独自フロント**の割り切り。
  - **絵文字リアクション**は Mastodon API に存在しない。標準クライアントには出せないので、ここでは**カウントを壊さず受信・無害化**するに留め、操作 API は独自側。

### 1.4 timelines（タイムライン）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| ホーム | `GET /api/v1/timelines/home` | フォロー中のフィード |
| 公開/ローカル | `GET …/public`（`local`/`remote`） | 連合・ローカル TL |
| ハッシュタグ | `GET …/tag/:hashtag` | タグ TL |
| リスト | `GET …/list/:id` | リスト TL |
| 直近会話 | `GET /api/v1/conversations` | DM スレッド（→ 1.16） |

- **要否**：ホームは**MVP 必須**。public/local も**MVP 必須**（クライアントの基本タブ）。tag・list は**MVP 必須〜後回し**（list は機能依存）。
- **工数**：**L**（1〜3 時間）。フィード生成（fan-out か pull か）の**設計判断が中心**。一人鯖なのでホームは**読み取り時集約（pull）で十分**になりやすく、決めが付けば実装は軽い。
- **依存**：statuses（1.3）／follows（1.10）／ページネーション（2.4）／Streaming（同じクエリをストリームにも流用）。
- **リスク**：
  - **ページネーション契約**（`max_id`/`since_id`/`min_id` + `Link` ヘッダ）を**全 TL で一貫**させる（2.4 と二重テスト）。
  - **複数アクター**：ホーム TL は「どのアクターの視点か」を OAuth トークンで決める。統合 TL は標準 API では出さず**独自フロント**（`fediverse-design.md` 1.3）。
  - filters（1.9）の TL 適用とサーバ/クライアント側フィルタの線引き。

### 1.5 notifications（通知）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 一覧/取得 | `GET /api/v1/notifications`, `GET …/:id` | 通知リスト |
| 既読・消去 | `POST …/clear`, `POST …/:id/dismiss` | クリア |
| グループ化(v2) | `GET /api/v2/notifications` | グルーピング通知 |
| 通知ポリシー | `GET/PATCH …/policy`, requests 系 | フィルタ済み通知 |

- **要否**：v1 一覧・dismiss・clear は**MVP 必須**。v2 グループ化・policy・requests は**後回し**（新しめのクライアントが使うが無くても動く）。
- **工数**：**M**（30〜60 分）。`Notification` 種別（mention/reblog/favourite/follow/follow_request/poll/update/status）の生成と契約。種別ごとにスライスを並べれば自走しやすい。
- **依存**：statuses・follows・poll（通知トリガ）／Web Push（同じ通知を push に転送 → 2.3）／Streaming（`notification` イベント）。
- **リスク**：
  - **通知の生成点を連合共通コードパスに置く**（ローカル発でもリモート発でも同じ）。
  - v2 グループ化の集約キー設計を後から足すとスキーマ変更になりがち。MVP では v1 のみと割り切る。
  - 絵文字リアクション通知は Mastodon 種別に無い → 標準クライアントには出さない（独自フロント）。

### 1.6 media（メディア）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| アップロード | `POST /api/v2/media`（`v1` 互換） | 添付の作成 |
| 取得・更新 | `GET /api/v1/media/:id`, `PUT …/:id` | 処理状況・説明/フォーカルポイント更新 |

- **要否**：**MVP 必須**（画像投稿は実用の前提）。動画は**後回し可**。
- **工数**：**L**（2〜4 時間）。非同期処理基盤・BlurHash・サムネイル・フォーカルポイント（`fediverse-design.md` 2.1）。**ネイティブ依存（libvips/ffmpeg）の方針決め**（`fediverse-design.md` 6.1）が判断の山で、ここが重いと上振れ。
- **依存**：DB キュー（非同期タスク・`fediverse-design.md` 5）／ストレージ抽象（ローカル/外部）／配布形態（libvips・ffmpeg 同梱判断・`fediverse-design.md` 6.1）。
- **リスク**：
  - **非同期メディア処理の `202` 契約**：`v2/media` は処理中に `202` を返し、クライアントは `GET /media/:id` をポーリング。同期で返すと大きいファイルでタイムアウト。
  - **投稿前にメディア ID を確保**するフロー（media 作成 → status に `media_ids`）。`description`/`focus` の後追い更新。
  - ネイティブ依存（libvips/ffmpeg）で**単一バイナリ配布が崩れる**（`fediverse-design.md` 6.1 と直結）。pure-Rust でどこまで賄うかの判断点。
  - BlurHash・サムネイル生成失敗時のフォールバック。

### 1.7 polls（投票）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 取得・投票 | `GET /api/v1/polls/:id`, `POST …/:id/votes` | 投票結果取得・投票 |

- **要否**：**MVP 必須相当〜後回し**（`fediverse-design.md` 2.1 で投票は実装対象。ただし投稿コア後でよい）。
- **工数**：**M**（20〜45 分）。`Poll` エンティティ・期限・複数選択・連合（`Question`/`Vote`）。
- **依存**：statuses（投稿に内包）／連合（`Question` オブジェクト・`Vote` 受信）。
- **リスク**：締切・再集計、リモート投票の集計の信頼（自前で数えるか相手の数を信じるか）、`voted`/`own_votes` の視点依存。

### 1.8 search（検索）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 統合検索 | `GET /api/v2/search` | アカウント・投稿・ハッシュタグ横断 |

- **要否**：**MVP 必須**（クライアントの検索バー）。ただし**最小実装**で可。
- **工数**：**M**（30〜60 分）。`fediverse-design.md` 5.1 の抽象レイヤー＋最小 PostgreSQL 実装。
- **依存**：accounts・statuses・tags／検索抽象（5.1）／WebFinger（`acct:` 解決でリモート取得 → 連合基盤）。
- **リスク**：
  - **`q` が URL/`acct:` のとき＝リモート解決**（その場フェッチして取り込む挙動）をクライアントが期待する。純粋な全文検索ではない。
  - 日本語分かち書き不可の既知制約（`fediverse-design.md` 5.1）。MVP は割り切る。
  - `type`/`resolve`/`following` パラメータの挙動差。

### 1.9 filters（フィルタ）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| v2 フィルタ | `GET/POST/PUT/DELETE /api/v2/filters`, `…/keywords`, `…/statuses` | キーワード/投稿フィルタ |
| v1 互換 | `/api/v1/filters` | 旧フィルタ |

- **要否**：**後回し**（無くてもクライアントは動く。あると体験向上）。v1 は**不要寄り**（v2 のみでよいが、古いクライアント対応で薄く出す可能性）。
- **工数**：**M**（20〜45 分）。`Filter`/`FilterKeyword`/`FilterStatus` と TL への適用。
- **依存**：timelines（適用先）／statuses（`filtered` フィールドの付与）。
- **リスク**：サーバ側適用 vs クライアント側適用の境界、`filtered` を各 Status に動的付与するコスト、`expires_at` の扱い。

### 1.10 follows / follow_requests（フォロー）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| フォロー操作 | `POST /api/v1/accounts/:id/follow`, `…/unfollow` | フォロー/解除 |
| 承認制 | `GET /api/v1/follow_requests`, `…/:id/authorize`, `…/reject` | リクエスト承認 |
| 関係 | `GET /api/v1/accounts/relationships` | 関係状態 |

- **要否**：**MVP 必須**（フォロー無しでは Fediverse が成立しない）。follow_requests も**MVP 必須**（`fediverse-design.md` 2.1 フォロー承認制：連合先のみ）。
- **工数**：**L**（2〜4 時間）。`Follow`/`Accept`/`Reject`/`Undo` の連合往復、`Relationship` 契約。連合往復は**2 インスタンス起動の統合テスト**（`mastodon-api-compat.md` 4.5）でスライスが増える。
- **依存**：連合基盤（Activity 往復）／accounts／notifications（follow 通知）。
- **リスク**：
  - **同一サーバー内のフォロー承認スキップ**は明示的特権（`fediverse-design.md` 2.4）。連合先は承認制。この分岐を**意味論共通・配送のみ最適化**で実装。
  - 承認制のステート遷移（requested → following）と `Relationship` フィールド（`requested`/`following`/`followed_by`）の整合。
  - リモートの非同期承認（`Accept` 受信までラグ）。

### 1.11 mutes / blocks（ミュート・ブロック）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| ブロック | `POST …/:id/block`, `…/unblock`, `GET /api/v1/blocks` | ブロック（送信 = `fediverse-design.md` 2.1） |
| ミュート | `POST …/:id/mute`, `…/unmute`, `GET /api/v1/mutes` | ミュート |
| ドメインブロック | `GET/POST/DELETE /api/v1/domain_blocks` | インスタンス単位 |

- **要否**：block/mute は**MVP 必須**（`fediverse-design.md` 2.1 Block 送信）。domain_blocks は**MVP 必須〜後回し**（連合運用上は欲しいが、`fediverse-design.md` 4.1 の連合ブロックリストと重複検討）。
- **工数**：**M**（30〜60 分）。
- **依存**：連合基盤（`Block`/`Undo` 送信、署名拒否は `fediverse-design.md` 3.1）／TL・通知のフィルタリング。
- **リスク**：
  - **ブロックの効力（配送停止・取得拒否・署名拒否）を全経路に効かせる**。`Block` 先からの署名は拒否（`fediverse-design.md` 3.1）。
  - mute の `notifications` オプション（通知だけミュート）、`duration`（時限ミュート）。
  - 大規模モデレーション（リモートユーザー一括処理）は**不要**（一人鯖前提）。必要十分な単体操作に絞る。

### 1.12 instance（インスタンス情報）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| インスタンス | `GET /api/v2/instance`（`v1` 互換） | サーバ情報・制限値・機能 |
| 補助 | `…/peers`, `…/rules`, `…/extended_description`, `…/translation_languages` | 付随情報 |
| 告知 | `GET /api/v1/announcements`, `…/:id/dismiss`, reactions | アナウンス |

- **要否**：`v2/instance` は**MVP 必須**（クライアントが起動時に機能判定に使う）。`peers`/`rules`/announcements は**後回し**。一部（discovery 系）は**不要**。
- **工数**：**M**（30〜60 分）。`Instance` v2 の広い JSON（`configuration`・`limits`・`urls.streaming`）を正しく返すのが要点。announcements は別 S。
- **依存**：運用設定（`fediverse-design.md` 6.2 DB 保存設定）／Streaming URL（2.2）。
- **リスク**：
  - **`configuration` の制限値（文字数・メディア・投票）がクライアント挙動を左右**する。実値と一致させる。
  - `v1`/`v2` 両形の差。新クライアントは `v2`、古いものは `v1`。
  - ディレクトリ/ディスカバリーは**提供しない**（`fediverse-design.md` 2.3）→ `instance` の該当フィールドは無効化を明示。

### 1.13 custom_emojis（カスタム絵文字）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 一覧 | `GET /api/v1/custom_emojis` | サーバの絵文字一覧 |

- **要否**：**MVP 必須**（`fediverse-design.md` 2.1 カスタム絵文字。投稿表示に必要）。
- **工数**：**S**（〜20 分、読み取り API のみ）。管理（登録・削除）は**独自フロント**（標準 API では admin 扱い）。
- **依存**：メディア（絵文字画像）／statuses・accounts（`emojis` 配列の埋め込み）。
- **リスク**：リモート絵文字のキャッシュ取り込み、`shortcode` の名前衝突、カテゴリ。

### 1.14 trends（トレンド）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| トレンド | `GET /api/v1/trends/tags`, `…/statuses`, `…/links` | 流行タグ・投稿・リンク |

- **要否**：**不要〜後回し**。一人鯖では母数が無くトレンドが成立しない（`fediverse-design.md` 2.3 ディスカバリー不要の延長）。空配列を返して**クライアントを壊さない**だけで十分。
- **工数**：**S**（〜10 分、スタブで空返し）。
- **依存**：なし。
- **リスク**：完全に欠落させると一部クライアントがエラー表示する → **空 200 を返すスタブは用意**する。

### 1.15 bookmarks / favourites（ブックマーク・お気に入り一覧）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| ブックマーク | `POST …/:id/bookmark`, `…/unbookmark`, `GET /api/v1/bookmarks` | あとで読む |
| ふぁぼ一覧 | `GET /api/v1/favourites` | 自分のふぁぼ一覧 |

- **要否**：**MVP 必須〜後回し**。bookmark/favourite の**操作**は statuses(1.3) 側で MVP 必須。**一覧**は後回し可。
- **工数**：**S**（〜20 分、操作はローカル状態のみ＝連合送信なし）。
- **依存**：statuses／ページネーション。
- **リスク**：bookmark は完全ローカル（連合しない）。favourite は連合（`Like`）。両者の混同に注意。一覧のページネーションは**独自カーソル**（`max_id` が status id でない実装が本家にある）。

### 1.16 conversations（会話 / DM）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 会話 | `GET /api/v1/conversations`, `DELETE …/:id`, `POST …/:id/read` | DM スレッド一覧 |

- **要否**：**後回し**（direct 可視性投稿で代替可能。専用 UI を使うクライアントのみ恩恵）。
- **工数**：**M**（30〜60 分）。`Conversation` 集約と未読管理。
- **依存**：statuses（direct 可視性）／notifications。
- **リスク**：会話のグルーピングキー、未読カウント、direct のグループ DM 的振る舞い。

### 1.17 lists（リスト）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| リスト CRUD | `GET/POST/PUT/DELETE /api/v1/lists`, `…/:id/accounts` | ユーザーリスト管理 |

- **要否**：**後回し**（便利機能。無くても実用可）。
- **工数**：**M**（30〜60 分、TL 連携・Streaming `list` チャンネル含む）。
- **依存**：timelines（list TL）／follows／Streaming。
- **リスク**：`replies_policy`、リストメンバーは「フォロー中のみ」制約、list 用 Streaming チャンネル。

### 1.18 markers（既読位置マーカー）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| マーカー | `GET/POST /api/v1/markers` | home/notifications の既読位置同期 |

- **要否**：**後回し**（複数端末同期の補助。単体でも動く）。
- **工数**：**S**（〜20 分）。
- **依存**：timelines・notifications。
- **リスク**：`home`/`notifications` の 2 種、`version`（楽観ロック）。小さいが契約は守る。

### 1.19 preferences（環境設定）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 設定取得 | `GET /api/v1/preferences` | デフォルト公開範囲・言語等 |

- **要否**：**後回し**（`source` 系の派生。クライアントは無くても動く）。
- **工数**：**S**（〜10 分、`update_credentials` の `source` 派生）。
- **依存**：accounts（`source`）。
- **リスク**：`update_credentials` の `source` と二重管理にしない。

### 1.20 scheduled_statuses（予約投稿）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 予約 | `GET /api/v1/scheduled_statuses`, `GET/PUT/DELETE …/:id` | 予約投稿管理 |

- **要否**：**後回し**（BOT 運用では有用＝`fediverse-design.md` 1.3 BOT 重視 と整合。ただし MVP 外）。
- **工数**：**M**（30〜60 分）。`POST /statuses` の `scheduled_at` 分岐＋DB キューでの発火。
- **依存**：statuses／DB キュー（`fediverse-design.md` 5）。
- **リスク**：予約投稿は `ScheduledStatus` を返し `Status` を返さない（クライアント分岐）。発火の正確性、メディア ID の有効期限。

### 1.21 featured_tags / pinned（注目タグ・固定）

| 項目 | エンドポイント例 | 目的 |
|------|------------------|------|
| 注目タグ | `GET/POST/DELETE /api/v1/featured_tags`, `…/suggestions` | プロフィールの注目タグ |
| フォロータグ | `GET /api/v1/followed_tags`, タグの follow/unfollow | ハッシュタグフォロー |
| 投稿固定 | `POST …/:id/pin`（statuses 側 1.3） | プロフィール固定投稿 |

- **要否**：投稿ピン留めは**MVP 必須**（`fediverse-design.md` 2.1）。featured_tags・followed_tags は**後回し**。
- **工数**：**S〜M**（20〜45 分）。
- **依存**：tags・accounts・timelines（followed_tags は home に合流）。
- **リスク**：followed_tags は home TL への合流ロジックが要る。featured_tags の連合表現。

### 1.22 不要・スタブで済ませるカテゴリ（明示）

| カテゴリ | エンドポイント例 | 扱い | 根拠 |
|----------|------------------|------|------|
| ディレクトリ | `GET /api/v1/directory` | **不要**（空配列） | ディスカバリー不要（`fediverse-design.md` 2.3） |
| サジェスト | `GET /api/v2/suggestions`, follow_suggestions | **不要**（空配列） | 一人鯖で推薦母数なし |
| trends | （1.14） | **スタブ** | 同上 |
| Admin API | `/api/v1/admin/*` | **不要**（標準 API では非提供） | 管理は独自フロント（`mastodon-api-compat.md` 3 / `fediverse-design.md` 3.2） |
| 通報送信 | `POST /api/v1/reports` | **不要**（送信しない） | 通報送信は非実装（`fediverse-design.md` 2.2、受信 `Flag` は連合側で対応） |
| アカウント移転（送信） | `POST /api/v1/accounts/:id/move`（本家には無いが移転設定 UI 相当） | **不要**（送信側 `Move` 非実装） | `fediverse-design.md` 2.2（受信のみ対応） |
| Endorsements | `GET /api/v1/endorsements`, account `pin`/`unpin` | **後回し〜不要** | 価値低 |
| Tags（follow） | `GET /api/v1/tags/:id` | **後回し** | featured/followed_tags 依存 |
| OEmbed / proofs | `/api/oembed`, identity proofs | **不要** | 廃止/低価値 |

> 注：**不要カテゴリも「404 で落とす」ではなく、クライアントが期待する形（空 200 等）で無害に応答する**スタブを置くこと。これが標準クライアント完結（`mastodon-api-compat.md` 1）の隠れた必須要件。

---

## 2. 横断サブシステム（独立見積もり）

`mastodon-api-compat.md` 2 の「見落としやすい土台」を独立した見積もり項目として切り出す。**カテゴリ別工数とは別建て**。

### 2.1 OAuth 2.0 サーバー

- **範囲**：`apps` 登録、認可コードフロー（`oauth/authorize` の承認画面含む）、トークン発行/失効、スコープ検証、Bearer 認証ミドルウェア。
- **要否**：**MVP 必須**（全認証 API の前提）。
- **工数**：**L**（2〜4 時間）。**複数アクター × 1 トークンの設計確定**（`authorize` のアクター選択）が判断の山で、ここを先に決め切れるかで上下する。
- **依存**：なし（最上流）。**全カテゴリがこれに依存**。
- **リスク**：
  - **複数アクター × 1 トークン**の橋渡し（`authorize` でアクター選択）は Mastodon に無い独自設計点。最初に決め切る。
  - スコープ設計（細分スコープ）を後から厳格化すると破壊的。
  - 認可画面の最小 HTML（クライアントログインの実通過点）。PKCE 対応の有無。

### 2.2 WebSocket Streaming

- **範囲**：`wss://…/api/v1/streaming`、チャンネル（`user`/`public`/`public:local`/`hashtag`/`list`/`direct`）、サブスクライブ/アンサブスクライブ、イベント（`update`/`delete`/`notification`/`status.update`/`filters_changed`）。
- **要否**：**MVP 後半で必須**（多くのクライアントが事実上前提＝`mastodon-api-compat.md` 2）。read/write が固まってから。
- **工数**：**L（重め）**（3〜5 時間）。fan-out 配信基盤・認証（トークンを query/サブプロトコルで受ける）・購読管理。**生成点の一本化（REST とストリームで二重生成しない）**が設計の山。
- **依存**：OAuth（接続認証）／全 TL・通知（同一クエリ/生成点を再利用）／内部イベントバス（投稿・通知発生をストリームへ）。
- **リスク**：
  - **ストリーミングの再接続契約**（クライアントは切断→`since_id` で歯抜けを埋める）。サーバ側のバックフィル前提。
  - **同一 Activity を REST とストリームで二重生成しない**（生成点を一本化＝`fediverse-design.md` 1.2）。
  - 接続スケール（tokio タスク/メモリ）、ハートビート、`instance.urls.streaming` の整合（1.12）。
  - 認証方式の差（`access_token` query vs `Sec-WebSocket-Protocol`）。

### 2.3 Web Push（VAPID）

- **範囲**：`POST/GET/PUT/DELETE /api/v1/push/subscription`、VAPID 鍵管理、暗号化ペイロード（`aes128gcm`）、push サービスへの送信、`alerts` 種別。
- **要否**：**後回し（最終フェーズ）**。モバイル背景通知に必要だが、起動時必須ではない。
- **工数**：**L**（2〜4 時間）。Web Push 暗号化（ECDH/HKDF・RFC 8291）の**正しさ検証**が中心で、自律ループでも契約（既知ベクタ）を先に置けるかが効く。購読管理は軽い。
- **依存**：notifications（push 元）／OAuth（`push` スコープ）／VAPID 鍵生成・保管。
- **リスク**：
  - **VAPID 鍵のライフサイクル**（生成・永続化・`instance` 公開）。
  - **ペイロード暗号化（RFC 8291）** の正しい実装。pure-Rust crate の選定。
  - push サービス側の失効（410）処理・購読の自動削除。
  - 送信は DB キュー経由（`fediverse-design.md` 5）でリトライ。

### 2.4 ページネーション規約（横断契約）

- **範囲**：`Link` ヘッダ（`rel="next"`/`rel="prev"`）+ `max_id`/`since_id`/`min_id`/`limit`。全リスト系で一貫。
- **要否**：**MVP 必須**（最初に共通実装）。
- **工数**：**M**（30〜60 分、共通レイヤー＋テストハーネス）。以後の各カテゴリは「乗るだけ」。
- **依存**：なし（全リスト系の土台）。
- **リスク**：
  - **`since_id`（新しい方を埋める・先頭固定）と `min_id`（古い方から進む）の挙動差**を取り違えると無限ループ/歯抜け。
  - 一部 API はカーソルが status id でない（bookmarks/favourites/notifications）。**カテゴリごとのカーソル種別**を契約に明記。
  - `Link` ヘッダの URL 生成（背後プロキシ時の host・scheme＝`fediverse-design.md` 6.4 `X-Forwarded-*`）。

### 2.5 レート制限・エラー互換

- **範囲**：`X-RateLimit-Limit/Remaining/Reset` ヘッダ、エラー JSON（`{"error": "...", "error_description": "..."}`）、HTTP ステータス互換。
- **要否**：**MVP 必須（薄く）**。一人鯖なので**厳格な制限は不要**だが、ヘッダ形とエラー形は出す。
- **工数**：**S〜M**（〜45 分）。
- **依存**：全 API（ミドルウェア）。
- **リスク**：
  - 一人鯖でレート制限の実値は緩くてよいが、**ヘッダ欠落でクライアントが誤動作**するケースがある → 形は守る。
  - エラー形の不一致でクライアントが「不明なエラー」表示。`422`/`401`/`404` の出し分けを契約テスト（`mastodon-api-compat.md` 4.2）。

---

## 3. MVP 境界の推奨

**「標準クライアントでログインして、読んで、書いて、フォローして、画像を貼れる」**を MVP 完了ラインとする。具体的には:

**MVP に含む（read + 基本 write）**:
- OAuth サーバー（2.1）＋アクター選択ログイン
- ページネーション/エラー/レート制限の横断契約（2.4・2.5）
- accounts（verify_credentials・:id・statuses・relationships・update_credentials）（1.2）
- statuses（投稿・取得・削除・編集・context・reblog/favourite/bookmark・pin）（1.3）
- timelines（home・public/local・tag）（1.4）
- notifications（v1 一覧・dismiss・clear）（1.5）
- media（画像・BlurHash・focus）（1.6）
- follows / follow_requests / relationships（1.10）
- mutes / blocks（1.11）
- search v2（最小）（1.8）
- instance v2（1.12）・custom_emojis（1.13）
- polls（1.7）
- trends/directory/suggestions の**無害スタブ**（1.14・1.22）

**MVP に含めない（後続フェーズ）**:
- Streaming（2.2）→ フェーズ 3
- Web Push（2.3）→ フェーズ 4
- lists・filters・conversations・markers・scheduled・featured_tags・preferences・announcements・notifications v2/policy

> MVP の肝は**機能数ではなく契約の正確さ**（`mastodon-api-compat.md` 4.2）。エンティティ JSON（Account/Status/Notification/Poll/Relationship/Instance）のゴールデンテストを先に固定し、垂直スライス（`mastodon-api-compat.md` 4.3）で 1 本ずつ通す。

---

## 4. 総工数のおおまかなレンジ

横断サブシステムとカテゴリの合算（Opus 自律 TDD のウォールクロック・人間レビュー込み）。**連合基盤（`fediverse-design.md` 3.1）と独自フロントは含まない**。

| ブロック | 規模 | 時間レンジ |
|----------|------|------------|
| 横断サブシステム（OAuth・ページネーション・エラー/RL） | L+M+(S〜M) | 3〜6 時間 |
| MVP コア（accounts/statuses/timelines/notifications/media/follows/blocks/search/instance/emojis/polls） | XL 中心 | 13〜27 時間 |
| Streaming（2.2） | L（重め） | 3〜5 時間 |
| Web Push（2.3） | L | 2〜4 時間 |
| 後回しカテゴリ群（lists/filters/conversations/markers/scheduled/featured/preferences/announcements/v2 通知） | M×多数 | 4〜8 時間 |
| **合計** | — | **約 25〜50 時間** |

- **MVP 境界まで（横断＋MVP コア）**：**約 16〜33 時間**。
- **Streaming まで含む実用ライン**：**約 19〜38 時間**。
- ここでの時間は **Opus 自律 TDD のウォールクロック**（実装スループットではなく、設計判断とレビューが律速）。人手前提の旧見積もり（数十〜百数十人日規模）とはオーダーが違う点が要点。幅が大きいのは、**連合の意味論共通化（`fediverse-design.md` 1.2）の作り込み深さ**と、**メディアのネイティブ依存判断（`fediverse-design.md` 6.1）**が支配的リスクのため。これらが軽く済めば下限、作り込むと上限に寄る。

---

## 5. 優先順位付きフェーズ計画

`mastodon-api-compat.md` 4.3 の「**read 系 → write 系 → streaming → push**」を基本線に、垂直スライスで進める。

### フェーズ 0：土台（横断）
- OAuth サーバー＋アクター選択ログイン（2.1）
- ページネーション/エラー/レート制限の共通レイヤー（2.4・2.5）
- エンティティ契約テストの骨組み（Account/Status/Instance のゴールデン）
- → 出口：標準クライアントが**ログインだけは通る**。

### フェーズ 1：read 系
- instance v2・custom_emojis・accounts(verify/:id/statuses/relationships)
- timelines(home/public/local/tag)・statuses 取得・context
- notifications v1 取得・search v2(最小)
- trends/directory/suggestions スタブ
- → 出口：**ログインして読める**（タイムライン・プロフィール・通知の閲覧）。

### フェーズ 2：write 系
- statuses 投稿/編集/削除（履歴・引用・CW・可視性）＋冪等性
- media アップロード（非同期・BlurHash・focus）
- reblog/favourite/bookmark/pin、polls 投票
- follows/follow_requests/relationships、mutes/blocks
- update_credentials（プロフィール・fields）
- → 出口：**標準クライアントで実用運用が成立（MVP 完了）**。

### フェーズ 3：streaming
- WebSocket Streaming（user/public/hashtag/list/direct）（2.2）
- 再接続・バックフィル契約、生成点の一本化
- → 出口：**リアルタイム更新**。多くのクライアントが「本来の体験」になる。

### フェーズ 4：push
- Web Push（VAPID・暗号化・購読管理）（2.3）
- notifications を push へ転送
- → 出口：**モバイル背景通知**。

### フェーズ 5：体験拡充（後回しカテゴリ）
- lists・filters・conversations・markers・scheduled_statuses・featured_tags・preferences・announcements・notifications v2/policy
- → 出口：新しめのクライアントの全機能が埋まる。

---

## 6. 関連

- 方針・AI 自律 TDD 指針: `mastodon-api-compat.md`
- 全体設計・機能範囲: `fediverse-design.md`（2 機能範囲 / 1.2 連合の意味論共通化 / 5 技術スタック / 6 配布）
- 連合基盤の見積もりは本書の対象外（`fediverse-design.md` 3.1）。
