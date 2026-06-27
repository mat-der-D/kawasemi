# Brief: actor-model

## Problem
一人鯖だが ActivityPub 上では複数アクターが独立して振る舞う。「同一オーナー」を管理層だけの概念に閉じ込めつつ、アクター毎の署名鍵を安全に管理する基盤が無いと、連合も API も構築できない。

## Current State
core-runtime の土台のみ。アクターやユーザーのデータモデルは未定義。

## Desired Outcome
複数のローカルアクターを表現するデータモデルと、アクター毎の署名鍵ペアの生成・保管・ローテーション経路が存在し、「同一オーナー」はプロトコル層に露出しない（管理層のみ）構造になっている。

## Approach
ローカルアクターのコアモデル（識別子・ハンドル・プロフィール基礎・状態）と、それを束ねる管理層の「オーナー」概念を分離して定義。アクター毎に署名鍵ペアを生成・保管し、ローテーション経路を確保。鍵プロバイダは core-runtime の DI 境界を使う。単一ドメイン構成（相関秘匿は非目標）。

## Scope
- **In**: ローカルアクターのデータモデル、管理層オーナー↔アクターの関連、アクター毎署名鍵の生成・保管・ローテーション、アクターの基本ライフサイクル（作成等）。
- **Out**: WebFinger/inbox 等の連合エンドポイント（→ federation-core）、accounts API シリアライズ（→ accounts-and-instance）、OAuth トークンのアクター選択（→ api-foundation）、リモートアクターのフェッチ/正規化（→ federation-core / accounts-and-instance）。

## Boundary Candidates
- ローカルアクターのデータモデル
- 管理層オーナー概念（プロトコル層に漏らさない）
- アクター毎署名鍵の管理・ローテーション

## Out of Boundary
- プロトコル層の連合表現（federation-core）
- Mastodon Account エンティティの JSON 契約（accounts-and-instance）

## Upstream / Downstream
- **Upstream**: core-runtime。
- **Downstream**: api-foundation(アクター選択)、federation-core(署名鍵・アクター URL)、accounts-and-instance、social-graph。

## Existing Spec Touchpoints
- **Extends**: なし。
- **Adjacent**: federation-core（署名鍵の消費側）、api-foundation（OAuth アクター選択）。

## Constraints
- 「同一オーナー」は管理層のみ。プロトコル層・外部公開には出さない（実装の綺麗さが目的、秘匿は非目標）。
- 単一ドメイン構成でよい。
- 署名鍵プロバイダは差し替え可能境界（テスト・連合検証）。
