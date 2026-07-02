# Research & Design Decisions

## Summary
- **Feature**: `actor-model`
- **Discovery Scope**: New Feature（グリーンフィールドだが core-runtime 土台上の拡張）
- **Key Findings**:
  - core-runtime は `SigningKeyProvider` trait を所有し、本番実装を「actor-model が差し込む拡張点」として明示的に空けている。actor-model はこの trait を**再定義せず**、本番実装のみを供給する。
  - core-runtime の `SigningKeyProvider::signing_key` は**同期**シグネチャ。DB I/O を同期境界で行えないため、鍵は起動時にメモリへ温め、作成／ローテーション時に更新する**インメモリ鍵キャッシュ**方式を採る。
  - 「同一オーナー」のプロトコル層非露出は、`owner_id` を管理層クエリ専用に閉じ、プロトコル向け参照（ハンドル解決・公開鍵供給）が返す型にオーナーを一切含めないことで構造的に担保する。

## Research Log

### core-runtime との接合（署名鍵供給境界）
- **Context**: 本 spec は core-runtime の DI 境界に対し、署名鍵の生成／保管／ローテーションを委譲されている。
- **Sources Consulted**: `.kiro/specs/core-runtime/design.md`（RuntimeContext・SigningKeyProvider・bootstrap 順序）、`requirements.md`（5.4, 5.5, 5.6）。
- **Findings**:
  - core-runtime: `pub trait SigningKeyProvider: Send + Sync { fn signing_key(&self, key_ref: KeyRef) -> Result<SigningKey, KeyError>; }`。`RuntimeContext::production()` が本番実装で構築、`deterministic(seed)` がテスト実装。
  - core-runtime の bootstrap 順序は `config → telemetry → pool → migrate → runtime context → AppState → serve`。本番 `SigningKeyProvider` はプール確立後に構築する必要がある（DB 由来の鍵を読むため）。
- **Implications**:
  - actor-model は本番 `SigningKeyProvider` 実装（`DbSigningKeyProvider`）を提供し、bootstrap が `RuntimeContext` 構築時にこれを注入する。これは core-runtime が想定した拡張点の充足であり、trait シグネチャ変更を伴わない。
  - 同期 trait に DB 非同期 I/O を載せないため、鍵キャッシュ（`Arc` + 内部可変性）を導入し、起動時にロード・作成／ローテーション時に更新する。
  - `KeyRef` は core-runtime 所有型。actor-model はその意味を「どのアクターの有効鍵か」を指す参照として解釈する。`KeyRef` の形が actor 参照を表現できない場合は core-runtime との再検証トリガとなる。

### 署名鍵アルゴリズムの選定
- **Context**: 連合（Mastodon 互換）で広く検証可能な鍵種別を選ぶ必要がある。
- **Findings**:
  - Mastodon の HTTP Signatures は RSA 公開鍵（PEM, SPKI）を前提に相互運用してきた歴史があり、RSA-2048 が最も広く検証される最大公約数。Ed25519 は対応実装が限られる。
  - 鍵生成は core-runtime の注入乱数境界（`Rng`）を用いる必要があり、決定的乱数で再現可能であること（テスト要件 4.3）。
- **Implications**:
  - デフォルトは **RSA-2048**、公開鍵は SPKI/PEM、秘密鍵は PKCS#8/PEM で保持。アルゴリズム種別はカラムとして保持し将来拡張可能にする（インターフェースのみ一般化、実装は RSA に限定）。
  - 鍵生成は `Rng` を受け取る関数として実装し、決定的シードで同一鍵列を再現する。

### 秘密鍵の保管（at-rest 保護）
- **Context**: 「アクター毎の署名鍵を安全に管理」（要件 4.5）。秘匿非目標はあくまで「同一オーナーの相関」であって鍵素材ではない。
- **Findings**:
  - 秘密鍵を DB に平文格納するのは不適切。起動シークレット（core-runtime の二層設定のうち起動設定側、`Secret<T>`）として鍵暗号鍵（KEK）を受け取り、AEAD（例: ChaCha20-Poly1305 / AES-256-GCM）で封緘する方式が単純で堅牢。
  - nonce は注入乱数境界から取得し、暗号文・nonce・アルゴリズムタグを保持する。
- **Implications**:
  - 秘密鍵の暗号化を `KeyCipher` 境界（trait）に切り出し、本番は KEK ベースの AEAD、テストは決定的・検証容易な実装に差し替え可能とする（「差し替え可能境界」制約に合致）。
  - KEK は core-runtime の起動設定に項目を1つ追加して供給する（`Secret<T>`）。これは core-runtime config の小規模拡張（Modified Files）。

### オーナー概念のプロトコル非露出の担保方法
- **Context**: 構造制約（要件 3）。
- **Findings / Implications**:
  - `owner_id` は `local_actors` の FK として保持するが、プロトコル向け参照型（`ResolvedActor` / `ActorPublicKey`）には含めない。
  - オーナー↔アクターの対応を返す操作は管理層 API（`ActorDirectory::list_actors_for_owner`）に限定し、ハンドル解決・公開鍵供給はオーナーを引数にも戻り値にも取らない。

## Architecture Pattern Evaluation

| Option | Description | Strengths | Risks / Limitations | Notes |
|--------|-------------|-----------|---------------------|-------|
| Repository + Service（採用） | 永続化（Repository）と業務（Service）を分離、core-runtime の DI 境界に乗る | core-runtime のレイヤー分離・依存方向に整合、テスト容易 | サービス層の薄さに注意 | steering「レイヤー分離最優先」に合致 |
| Active Record | モデルが永続化を内包 | 記述量が少ない | 境界が曖昧化、owner 非露出の担保が弱い | 却下 |
| イベントソーシング | 鍵ローテーションを履歴で表現 | 監査性 | MVP には過剰、DB完結方針に対し複雑 | 却下（過剰） |

## Design Decisions

### Decision: 署名鍵供給は同期 trait + インメモリ鍵キャッシュ
- **Context**: core-runtime の `SigningKeyProvider::signing_key` は同期。鍵は DB に永続化される。
- **Alternatives Considered**:
  1. trait を async 化 — core-runtime の契約変更（再検証トリガ大）。却下。
  2. 呼び出し毎にブロッキング DB アクセス — 同期境界でのブロッキング I/O はランタイムを阻害。却下。
  3. インメモリ鍵キャッシュ（採用）— 起動時ロード、作成／ローテーションで更新。
- **Selected Approach**: `DbSigningKeyProvider` が共有鍵キャッシュ（`Arc<KeyCache>`）を読む。`SigningKeyService` が DB 書き込みと同時にキャッシュを更新する。
- **Rationale**: 一人鯖はアクター数が小さく全鍵をメモリに保持して問題ない。core-runtime の同期契約を変えずに整合。
- **Trade-offs**: キャッシュと DB の整合維持が必要（更新は単一サービス経路に集約して担保）。
- **Follow-up**: ローテーション直後にキャッシュへ反映されることを統合テストで検証。

### Decision: 秘密鍵 at-rest 暗号化を `KeyCipher` 境界に切り出す
- **Context**: 要件 4.5 の安全保管と「差し替え可能境界」制約。
- **Selected Approach**: AEAD による封緘。KEK は起動設定（`Secret<T>`）。本番／決定的テスト実装を差し替え可能。
- **Rationale**: 鍵素材の漏洩面を縮小しつつテスト容易性を確保。
- **Trade-offs**: 起動設定に KEK 項目が増える（core-runtime config の小拡張）。

## Risks & Mitigations
- 鍵キャッシュと DB の不整合 — 更新を `SigningKeyService` 単一経路に集約し、テストで反映を検証。
- `KeyRef` の形がアクター参照に不足 — core-runtime との再検証トリガとして明示。alignment が崩れたら早期に検出。
- オーナー情報のプロトコル経路への漏洩 — プロトコル向け戻り値型に owner を構造的に含めない + 境界テストで検証。
- 決定的鍵生成の取り違え（本番で決定的実装を使う事故）— `RuntimeContext::production/deterministic` の切替を bootstrap に閉じ、テストでのみ deterministic を使う。

## References
- `.kiro/specs/core-runtime/design.md` — RuntimeContext / SigningKeyProvider / bootstrap 順序（一次参照）。
- `.kiro/specs/federation-core/brief.md` — 公開鍵・アクター URL・ハンドル解決の消費側。
- `.kiro/specs/api-foundation/brief.md` — 複数アクター×1トークンのアクター選択（保有アクター一覧の消費側）。
- `.kiro/steering/structure.md` — プロトコル層と管理層の分離原則。
