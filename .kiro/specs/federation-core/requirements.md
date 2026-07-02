# Requirements Document

## Introduction

federation-core は kawasemi の ActivityPub 連合の土台を提供する spec である。本 spec が無いと、外部インスタンスから WebFinger でアクターを解決できず、署名付き Activity の受信・検証も、outbox・オブジェクトの `application/activity+json` 配信もできず、何も連合できない。さらに本プロジェクトの中核制約である「意味論は対称・物理配送のみ最適化」を構造で担保しないと、ローカル配送では顕在化せずリモート連合でだけ壊れるバグを生む。

本 spec は、(1) HTTP Signatures の送受信（draft-cavage と RFC 9421 双方 + double-knocking フォールバック交渉、公開鍵取得とキャッシュ、ブロック先署名の拒否）、(2) WebFinger による `acct:` からローカルアクターへの解決、(3) NodeInfo による最小限の公開統計、(4) inbox / outbox / shared inbox の受信と配信、(5) アクター・オブジェクト・コレクションの `application/activity+json` GET、(6) JSON-LD `@context` 付与と未知プロパティの安全な展開、(7) 「論理的に同一の Activity を生成・検証してから配送関数のみ分岐（in-process / HTTP）」する配送抽象、(8) DB を用いた非同期配送キュー、(9) 署名検証・公開鍵取得・ネットワークのモック可能境界、(10) 2 インスタンス往復を検証する連合テスト基盤、を実現する。

具体 Activity 種別（Create / Follow / Block 等）の業務処理・可視性ルールの中身・独自連合方言の正規化・受信側 Move/Flag・Mastodon REST API は本 spec の範囲外であり、本 spec は受信 Activity を業務処理へ受け渡す境界と、業務側が構築した Activity を配送する境界のみを所有する。

## Boundary Context

- **In scope**: HTTP Signatures の生成と検証（draft-cavage / RFC 9421 / double-knocking）、署名検証・公開鍵取得・送信ネットワークのモック可能境界、WebFinger、NodeInfo、inbox / outbox / shared inbox エンドポイント、アクター・オブジェクト・コレクションの `application/activity+json` GET（authorized fetch を要求できる構造を含む）、JSON-LD `@context` 付与と未知プロパティの安全な展開、アクター URL・コレクション URL の構築と公開、配送抽象（意味論共通・配送関数のみ分岐）、DB 配送キューと非同期配送ワーカー、受信 Activity を業務処理へ受け渡すディスパッチ境界、ブロック判定を委譲する境界、連合テスト基盤（2 インスタンス往復・ローカル/HTTP 同一結果検証）。
- **Out of scope**: 具体 Activity 種別の業務処理（Create / Follow / Block / Announce 等の意味論は statuses-core・social-graph 等が所有）、可視性・addressing ルールの中身（共通パスの呼び出し点のみ本 spec が所有し、判定ロジックは利用側が定義）、独自連合方言（絵文字リアクション・引用・MFM）の正規化（custom-federation）、受信側 Move / Flag（inbound-move-flag）、リモートアクターの完全なプロフィール永続化と Mastodon Account 化（accounts-and-instance。本 spec は署名検証に必要な公開鍵素材の取得・キャッシュのみ）、Mastodon REST API（api-foundation 以降）、ブロックリストそのものの保持・管理（social-graph）。
- **Adjacent expectations**: 本 spec は core-runtime が提供する非決定性境界（時刻・ID・乱数・署名鍵供給）・DB 接続プール・統一エラー型・構造化ログ・テストハーネスに依存する。actor-model が提供するハンドル解決（オーナー非露出のアクター解決）と公開鍵供給に依存し、それらの戻り値にオーナー情報が含まれないことを前提とする。下流 spec（statuses-core・social-graph 等）は本 spec が公開する受信ディスパッチ境界に各 Activity 種別の処理を登録し、配送境界に Activity と宛先を渡して配送を依頼する。social-graph は本 spec のブロック判定委譲境界に実際のブロック判定を供給する。

## Requirements

### Requirement 1: HTTP Signatures の生成（送信側署名）

**Objective:** 連合の実装者として、送信する Activity と取得要求にローカルアクターの鍵で署名を付与したい。これにより、外部インスタンスが kawasemi 由来の要求の真正性を検証できる。

#### Acceptance Criteria

1. When ローカルアクターとして外部宛のリクエストを送信するとき, the Federation Core shall そのアクターの有効な署名鍵を core-runtime の署名鍵供給境界から取得し、リクエストに署名を付与する。
2. The Federation Core shall 署名の鍵識別子（keyId）を当該アクターの公開鍵を取得可能な URL として設定する。
3. When リクエストに本文が含まれるとき, the Federation Core shall 本文のダイジェストを算出して署名対象に含める。
4. The Federation Core shall 署名形式として draft-cavage 形式と RFC 9421 形式の双方を生成できる。
5. If 署名対象アクターの有効な署名鍵が取得できないとき, then the Federation Core shall 署名付き送信を中止し、原因を示すエラーを返す。

### Requirement 2: HTTP Signatures の検証（受信側）

**Objective:** 連合の実装者として、受信した署名付きリクエストの真正性を検証したい。これにより、なりすましや改ざんされた Activity を排除できる。

#### Acceptance Criteria

1. When 署名付きリクエストを受信したとき, the Federation Core shall 署名者の公開鍵を用いて署名を検証する。
2. The Federation Core shall draft-cavage 形式と RFC 9421 形式の双方の署名を検証できる。
3. When 署名検証のために署名者の公開鍵が必要なとき, the Federation Core shall 鍵識別子から公開鍵素材を取得し、取得した公開鍵をキャッシュする。
4. While 同一の鍵識別子の公開鍵がキャッシュに有効に存在する間, the Federation Core shall ネットワーク取得を行わずキャッシュされた公開鍵を用いる。
5. When 本文を伴うリクエストを受信したとき, the Federation Core shall 受信本文のダイジェストが署名対象のダイジェストと一致することを検証する。
6. If 署名が欠落・不正・期限切れ、または公開鍵が取得できないとき, then the Federation Core shall リクエストを拒否し、認証失敗を示す応答を返す。
7. The Federation Core shall 署名検証・公開鍵取得・ネットワーク取得を差し替え可能な境界の背後に置き、テストでモック実装へ差し替えられるようにする。

### Requirement 3: 署名形式の double-knocking 交渉

**Objective:** 連合の実装者として、相手インスタンスが対応する署名形式が事前に分からなくても送達できるようにしたい。これにより、draft-cavage のみ・RFC 9421 のみのいずれの実装とも相互運用できる。

#### Acceptance Criteria

1. When 送信先の対応署名形式が未知のとき, the Federation Core shall 一方の署名形式で送信し、署名関連の拒否を受けた場合にもう一方の署名形式で再送する。
2. When ある送信先に対していずれかの署名形式で送達に成功したとき, the Federation Core shall その送信先について成功した署名形式を記録する。
3. While 送信先について成功した署名形式が記録されている間, the Federation Core shall 以降の送信でまず記録済みの署名形式を用いる。

### Requirement 4: WebFinger によるアクター解決

**Objective:** 外部インスタンスの利用者として、`acct:` 形式の識別子から kawasemi 上のアクターを発見したい。これにより、ハンドルを指定してフォローや言及ができる。

#### Acceptance Criteria

1. When WebFinger エンドポイントが自インスタンスのアクターを指す `acct:` リソースで照会されたとき, the Federation Core shall 当該アクターの ActivityPub アクター URL を `application/activity+json` を指すリンクとして含む JRD 応答を返す。
2. The Federation Core shall 同一インスタンスが保持する複数のローカルアクターをそれぞれ WebFinger で解決できる。
3. If 照会されたリソースのドメインが自インスタンスのドメインと一致しないとき, then the Federation Core shall その照会を自インスタンスのアクターとして解決しない。
4. If 照会されたアクターが存在しないとき, then the Federation Core shall 未検出を示す応答を返す。
5. When WebFinger 照会でアクターを解決するとき, the Federation Core shall オーナー情報を含まないアクター参照のみを用いる。

### Requirement 5: NodeInfo による公開統計

**Objective:** 外部インスタンスおよびツールの運用者として、kawasemi インスタンスのソフトウェア種別と対応プロトコルを機械可読に取得したい。これにより、連合先としての互換性を判断できる。

#### Acceptance Criteria

1. When NodeInfo ディスカバリ用エンドポイントが照会されたとき, the Federation Core shall 利用可能な NodeInfo ドキュメントの場所を示すリンク集合を返す。
2. When NodeInfo ドキュメントが照会されたとき, the Federation Core shall ソフトウェア名・バージョン・対応プロトコル（ActivityPub）を含む最小限の公開統計を返す。
3. The Federation Core shall NodeInfo に外部公開を意図しない内部情報を含めない。

### Requirement 6: アクター・オブジェクト・コレクションの ActivityPub GET

**Objective:** 外部インスタンスの実装者として、kawasemi のアクター・オブジェクト・コレクションを ActivityPub 表現で取得したい。これにより、フォローや配送に必要なアクター情報や対象オブジェクトを参照できる。

#### Acceptance Criteria

1. When ローカルアクターの URL が ActivityPub メディアタイプで取得されたとき, the Federation Core shall そのアクターの ActivityPub 表現（識別子・inbox・outbox・公開鍵を含む）を `application/activity+json` で返す。
2. When ローカルオブジェクトまたはコレクションの URL が ActivityPub メディアタイプで取得されたとき, the Federation Core shall その ActivityPub 表現を `application/activity+json` で返す。
3. When 取得要求の受理可能メディアタイプが ActivityPub 表現を含まないとき, the Federation Core shall ActivityPub 表現を返さず、非 ActivityPub 表現へ振り分けられる拡張点に委ねる。
4. Where セキュアモードが有効な場合, the Federation Core shall ActivityPub 表現の取得に対して署名付き取得要求（authorized fetch）を要求し、署名検証に失敗した要求には ActivityPub 表現を返さない。
5. When アクター ActivityPub 表現を構築するとき, the Federation Core shall オーナー情報を表現に含めない。
6. If 取得対象のアクター・オブジェクト・コレクションが存在しないとき, then the Federation Core shall 未検出を示す応答を返す。

### Requirement 7: inbox / shared inbox の受信と業務処理への受け渡し

**Objective:** 連合の実装者として、外部インスタンスから送信された Activity を inbox および shared inbox で安全に受信し、業務処理へ受け渡したい。これにより、各 Activity 種別の処理を本 spec が抱えずに連合受信を成立させられる。

#### Acceptance Criteria

1. When 署名付き Activity が inbox または shared inbox に投函されたとき, the Federation Core shall 署名を検証してから受理する。
2. If 署名検証に失敗した Activity が投函されたとき, then the Federation Core shall その Activity を受理せず認証失敗を示す応答を返す。
3. When 検証済みの Activity を受理したとき, the Federation Core shall その Activity を、種別ごとの業務処理を登録できるディスパッチ境界へ受け渡す。
4. If 既に受理済みの識別子を持つ Activity が再度投函されたとき, then the Federation Core shall その Activity を重複として扱い、業務処理を二重に実行しない。
5. While Activity の業務処理を実行している間, the Federation Core shall 各 Activity 種別固有の意味論を本 spec 内に実装せず、登録された業務処理へ委譲する。

### Requirement 8: outbox の公開

**Objective:** 外部インスタンスの実装者として、ローカルアクターの outbox を ActivityPub コレクションとして取得したい。これにより、当該アクターの公開済み Activity を参照できる。

#### Acceptance Criteria

1. When ローカルアクターの outbox URL が ActivityPub メディアタイプで取得されたとき, the Federation Core shall その outbox を ActivityPub の順序付きコレクションとして返す。
2. The Federation Core shall outbox コレクションをページ単位で取得できる形で公開する。
3. The Federation Core shall outbox に公開対象範囲外の Activity を含めない。

### Requirement 9: JSON-LD のシリアライズと安全な展開

**Objective:** 連合の実装者として、送受信する ActivityPub ドキュメントを相互運用可能な JSON-LD として扱いたい。これにより、他実装が付与する未知の拡張プロパティで処理が壊れないようにできる。

#### Acceptance Criteria

1. When ActivityPub ドキュメントをシリアライズするとき, the Federation Core shall ActivityPub の `@context` を付与した JSON-LD として出力する。
2. When 受信した JSON-LD を解釈するとき, the Federation Core shall 自身が認識しない未知のプロパティによって解釈を失敗させない。
3. If 受信した JSON-LD に処理に必要な必須プロパティ（種別・識別子等）が欠落しているとき, then the Federation Core shall その文書を不正として扱い、業務処理へ受け渡さない。
4. The Federation Core shall `application/activity+json` と `application/ld+json` の双方の受理可能メディアタイプを ActivityPub 表現要求として受け付ける。

### Requirement 10: 配送抽象（意味論対称・物理配送のみ分岐）

**Objective:** 一人鯖の運用者および連合の実装者として、ローカル宛とリモート宛で同一の Activity 意味論が適用され、配送手段のみが最適化されることを構造で保証したい。これにより、ローカルでは顕在化せずリモート連合でだけ壊れるバグを防げる。

#### Acceptance Criteria

1. When 業務側が Activity の配送を依頼したとき, the Federation Core shall ローカル宛・リモート宛にかかわらず、論理的に同一の Activity を生成・検証してから配送する。
2. The Federation Core shall Activity の生成・検証・宛先解決を共通のコードパスで行い、分岐は最終的な配送手段（in-process 関数呼び出しか HTTP 送信か）に限定する。
3. When 宛先がローカルアクターのとき, the Federation Core shall その Activity を、リモート受信と意味論的に同一の受理・業務処理経路へ in-process で受け渡す。
4. When 宛先がリモートアクターのとき, the Federation Core shall その Activity を署名付き HTTP 送信として配送する。
5. The Federation Core shall ローカル配送経路と HTTP 連合配送経路が同一の Activity に対して同一の業務処理結果を生むことを検証可能にする。

### Requirement 11: DB 配送キューと非同期配送

**Objective:** 一人鯖の運用者として、配送先の応答遅延や一時的失敗が利用者操作をブロックしないようにしたい。これにより、低スペック環境でも操作の応答性を保てる。

#### Acceptance Criteria

1. When リモート宛の配送が依頼されたとき, the Federation Core shall 配送ジョブをデータベースに永続化し、依頼元の処理を配送完了まで待たせない。
2. While 配送ワーカーが稼働している間, the Federation Core shall 永続化された配送ジョブを取り出して署名付き HTTP 送信を実行する。
3. If 配送が一時的に失敗したとき, then the Federation Core shall その配送ジョブを後で再試行し、再試行間隔を段階的に広げる。
4. When 同一 Activity が同一の共有 inbox を持つ複数のリモート宛先へ配送されるとき, the Federation Core shall その共有 inbox への送信を重複させない。
5. If 配送が再試行上限に達して恒久的に失敗したとき, then the Federation Core shall そのジョブを恒久失敗として記録し、無限に再試行しない。

### Requirement 12: ブロック先への署名拒否境界

**Objective:** 一人鯖の運用者として、ブロックした相手からの署名付き要求を受理したくない。これにより、ブロックの意図を連合層でも貫ける。

#### Acceptance Criteria

1. When 署名付きリクエストを受信したとき, the Federation Core shall 署名者がブロック対象かどうかをブロック判定委譲境界へ問い合わせる。
2. If 署名者がブロック対象であると判定されたとき, then the Federation Core shall そのリクエストを受理せず拒否する。
3. The Federation Core shall ブロック対象の保持・管理そのものを所有せず、ブロック判定を差し替え可能な委譲境界の背後に置く。

### Requirement 13: 連合テスト基盤（2 インスタンス往復）

**Objective:** AI 自律 TDD を行う実装者として、2 つの自前インスタンスを起動して Activity 往復を検証したい。これにより、署名・JSON-LD・配送の連合経路を実環境に近い形で検証できる。

#### Acceptance Criteria

1. The Federation Core shall 連合検証のために、2 つの分離されたインスタンスを起動できるテスト基盤を提供する。
2. While 連合テストが実行される間, the Federation Core shall 非決定性境界（時刻・ID・乱数・署名鍵）を決定的実装へ差し替えた状態で各インスタンスを起動する。
3. When 連合テストが一方のインスタンスから他方へ署名付き Activity を送信するとき, the Federation Core shall 受信側で署名検証と業務処理受け渡しが成立することを検証可能にする。
4. The Federation Core shall ローカル配送経路と HTTP 連合配送経路の結果が同一であることを検証する手段を提供する。
