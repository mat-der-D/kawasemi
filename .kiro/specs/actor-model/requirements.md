# Requirements Document

## Introduction

actor-model は kawasemi の「一人鯖だが ActivityPub 上では複数アクターが独立して振る舞う」という中核方針を、データモデルと署名鍵基盤として実体化する spec である。本 spec が無いと、連合（federation-core）も Mastodon 互換 API（api-foundation 以降）も乗る土台が無く、アクター毎の署名鍵を安全に扱えないため何も配送・認証できない。

本 spec は、(1) 複数のローカルアクターを表現するコアデータモデル（識別子・ハンドル・プロフィール基礎・状態・種別）、(2) それらを束ねる管理層の「オーナー」概念とオーナー↔アクターの関連、(3) アクター毎の署名鍵ペアの生成・保管・ローテーション経路、(4) core-runtime が定義する署名鍵供給境界の本番実装、(5) 下流（api-foundation のアクター選択・federation-core のハンドル解決／公開鍵公開）が必要とする参照経路、を実現する。最重要の構造制約は「同一オーナー」を管理層のみの概念に閉じ込め、プロトコル層・外部公開には露出させないことである。

## Boundary Context

- **In scope**: ローカルアクターのデータモデル（内部識別子・ハンドル・種別・プロフィール基礎・状態・時刻）、管理層オーナー概念とオーナー↔アクターの関連、アクター毎署名鍵の生成・保管・ローテーション、core-runtime 署名鍵供給境界の本番実装、アクターの基本ライフサイクル（作成・無効化）、下流向けのアクター参照経路（オーナー別一覧・ハンドル解決・公開鍵供給）。
- **Out of scope**: WebFinger／inbox／outbox 等の連合エンドポイントと実際のアクター URL ルーティング・JSON-LD 表現（federation-core）、Mastodon Account エンティティの JSON シリアライズと update_credentials（accounts-and-instance）、OAuth トークン発行とトークンへのアクター選択結びつけ（api-foundation）、リモートアクターのフェッチ／正規化（federation-core／accounts-and-instance）、アバター／ヘッダ等のメディア（media-pipeline）、起動・設定・DI 境界定義・統一エラー型・マイグレーション基盤・テストハーネス土台（core-runtime）。
- **Adjacent expectations**: 本 spec は core-runtime が提供する ID 生成境界・乱数境界・署名鍵供給境界（trait）・DB 接続プール・統一エラー型・設定（起動シークレット）・テストハーネスに依存する。署名鍵供給境界の本番実装は core-runtime が「actor-model が差し込む拡張点」として残した箇所を埋める。api-foundation はオーナーの保有アクター一覧をアクター選択の基礎として消費する。federation-core はハンドル解決と公開鍵素材を消費し、アクター URL の構築・公開は federation-core が所有する。

## Requirements

### Requirement 1: ローカルアクターのデータモデルとハンドル

**Objective:** 一人鯖の運用者として、ActivityPub 上で独立して振る舞う複数のローカルアクターを表現したい。これにより、人格や BOT を個別のアクターとして運用できる。

#### Acceptance Criteria

1. When 新しいローカルアクターが作成されるとき, the Actor Model shall 一意の内部識別子・ハンドル・アクター種別・プロフィール基礎（表示名および要約）・状態を保持するアクターを永続化する。
2. The Actor Model shall 単一ドメイン構成を前提とし、ハンドル（ローカルユーザー名）をインスタンス内で一意とする。
3. If 既存アクターと重複するハンドルでアクター作成が要求されたとき, then the Actor Model shall 作成を拒否し、重複を示すエラーを返す。
4. The Actor Model shall アクター種別として少なくとも人格アクターと自動化アクター（BOT）を区別して保持する。
5. When アクターの内部識別子が生成されるとき, the Actor Model shall core-runtime の ID 生成境界を用いて識別子を採番する。
6. If ハンドルが許可されない形式（空文字・規定外の文字を含む等）で指定されたとき, then the Actor Model shall 作成を拒否し、形式不正を示すエラーを返す。

### Requirement 2: 管理層オーナーとアクターの関連

**Objective:** 一人鯖の運用者として、複数アクターを「同一オーナーが保有する」関係として管理層で束ねたい。これにより、全アクターを一元的に把握・操作できる。

#### Acceptance Criteria

1. The Actor Model shall 管理層の概念として「オーナー」を保持し、1つのオーナーが複数のローカルアクターを保有できる関連を表現する。
2. When ローカルアクターが作成されるとき, the Actor Model shall そのアクターを既存のオーナーに関連付ける。
3. If 存在しないオーナーに対してアクター作成が要求されたとき, then the Actor Model shall 作成を拒否し、対象オーナー不在を示すエラーを返す。
4. The Actor Model shall オーナーを作成する操作を提供し、作成されたオーナーに内部識別子を採番する。
5. While アクターがいずれかのオーナーに関連付いている間, the Actor Model shall その関連を管理層向けの問い合わせから取得可能にする。

### Requirement 3: 「同一オーナー」のプロトコル層非露出

**Objective:** 一人鯖の運用者として、「複数アクターが同一オーナーに属する」という管理層の事実をプロトコル層・外部公開に出したくない。これにより、レイヤー分離を構造で担保し実装を綺麗に保てる。

#### Acceptance Criteria

1. The Actor Model shall プロトコル層／外部公開向けにアクターを参照する経路（ハンドル解決・公開鍵供給）でオーナー識別子およびオーナー関連を一切返さない。
2. When 下流がハンドルまたはアクター識別子でアクターを解決するとき, the Actor Model shall オーナーに依存しないアクター単体の情報のみを返す。
3. The Actor Model shall オーナーと複数アクターの対応を取得する操作を、管理層向けの明示的な操作に限定する。

### Requirement 4: アクター毎署名鍵の生成と保管

**Objective:** 連合の実装者として、アクター毎に独立した署名鍵ペアを安全に生成・保管したい。これにより、各アクターが独立して署名付き Activity を扱える基盤を持てる。

#### Acceptance Criteria

1. When 新しいローカルアクターが作成されるとき, the Actor Model shall そのアクター専用の署名鍵ペアを1つ生成し、有効な鍵として保管する。
2. When 署名鍵ペアを生成するとき, the Actor Model shall core-runtime の乱数境界を用いて鍵素材を生成する。
3. Where テストまたは連合検証構成で実行される場合, the Actor Model shall 決定的な乱数境界を用いて再現可能な署名鍵を生成できる。
4. The Actor Model shall 秘密鍵素材を平文のままログ・診断出力・プロトコル向け参照経路へ出力しない。
5. While 署名鍵を永続化している間, the Actor Model shall 秘密鍵素材を保管時に保護された形で格納する。
6. The Actor Model shall 各署名鍵に対応する公開鍵素材を、下流が連合で公開できる形式で取得可能にする。

### Requirement 5: 署名鍵のローテーション

**Objective:** 一人鯖の運用者として、鍵漏洩や定期更新に備えてアクターの署名鍵を入れ替えたい。これにより、鍵の危殆化リスクに対処できる。

#### Acceptance Criteria

1. When アクターの署名鍵ローテーションが要求されたとき, the Actor Model shall 新しい署名鍵ペアを生成し、当該アクターの有効鍵とする。
2. When 新しい署名鍵が有効化されるとき, the Actor Model shall それまで有効だった鍵を失効（非有効）状態へ遷移させる。
3. The Actor Model shall 各アクターについて同時に有効な署名鍵を最大1つに保つ。
4. Where 失効した鍵が保持される場合, the Actor Model shall 失効鍵を有効鍵と区別して識別できる状態で保持する。
5. If 存在しないアクターに対してローテーションが要求されたとき, then the Actor Model shall 操作を拒否し、対象アクター不在を示すエラーを返す。

### Requirement 6: 署名鍵の供給境界（core-runtime 連携）

**Objective:** 連合の実装者として、core-runtime の署名鍵供給境界を通じてアクターの署名鍵を取得したい。これにより、配送・署名のコードが鍵の保管詳細に依存せずに済む。

#### Acceptance Criteria

1. The Actor Model shall core-runtime が定義する署名鍵供給境界の本番実装を提供する。
2. When 署名鍵供給境界が鍵参照で署名鍵を要求されたとき, the Actor Model shall 対応するアクターの有効な署名鍵を返す。
3. If 要求された鍵参照に対応する有効な署名鍵が存在しないとき, then the Actor Model shall 鍵が見つからない旨のエラーを返す。
4. While アプリケーションが稼働している間, the Actor Model shall 署名鍵の生成・ローテーションの結果を以降の供給境界の応答へ反映する。

### Requirement 7: アクターのライフサイクル

**Objective:** 一人鯖の運用者として、アクターを作成し、必要に応じて無効化したい。これにより、運用中のアクターの可用性を管理できる。

#### Acceptance Criteria

1. The Actor Model shall ローカルアクターの状態として少なくとも有効状態と無効化状態を表現する。
2. When アクターが作成されたとき, the Actor Model shall そのアクターを有効状態で初期化する。
3. When アクターの無効化が要求されたとき, the Actor Model shall そのアクターを無効化状態へ遷移させる。
4. While アクターが無効化状態である間, the Actor Model shall 下流向けのアクター解決でその状態を判別可能にする。
5. The Actor Model shall アクターの作成時刻および更新時刻を記録し、時刻取得に core-runtime の時刻境界を用いる。

### Requirement 8: 下流向けアクター参照（選択・解決・公開鍵供給）

**Objective:** 下流 spec（api-foundation・federation-core）の実装者として、アクター選択・ハンドル解決・公開鍵公開に必要な参照経路を本 spec から受け取りたい。これにより、下流が独自にアクター管理を再発明せずに済む。

#### Acceptance Criteria

1. When 管理層が特定オーナーの保有アクター一覧を要求したとき, the Actor Model shall そのオーナーに関連付くアクターの一覧を返す。
2. When 下流がハンドルでアクターを要求したとき, the Actor Model shall 該当するローカルアクターを返し、存在しなければ未検出を示す。
3. When 下流がアクター識別子で公開鍵素材を要求したとき, the Actor Model shall 当該アクターの有効鍵に対応する公開鍵素材と鍵識別情報を返す。
4. The Actor Model shall プロトコル層向けの参照操作（ハンドル解決・公開鍵供給）でオーナー情報を返さない。
