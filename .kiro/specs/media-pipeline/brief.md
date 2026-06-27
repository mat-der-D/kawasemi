# Brief: media-pipeline

## Problem
投稿・アバター・カスタム絵文字はメディア添付に依存する。非同期処理・ストレージ抽象・BlurHash・フォーカルポイントが無いと statuses も accounts も完成しない。さらにメディア処理のネイティブ依存は配布形態（単一バイナリ）を左右する判断ゲートになる。

## Current State
core-runtime（DB キュー基盤）と api-foundation（非同期 202→poll の HTTP 規約）が利用可能。メディア処理は未着手。

## Desired Outcome
クライアントがメディアを非同期アップロード（`202` 受理 → 処理完了をポーリング）でき、BlurHash とフォーカルポイントが付与され、ストレージが抽象境界の背後に置かれ、ネイティブ依存（libvips/ffmpeg 等）を許容する範囲が決定されている状態。

## Approach
DB ジョブキューでメディア処理を非同期化（`202` → ステータスポーリング）。ストレージは抽象インターフェース（ローカル FS / 後で差し替え可能）の背後。BlurHash・サムネイル・フォーカルポイントを生成。画像を MVP とし動画は後回し。pure-Rust で賄える範囲と、libvips/ffmpeg 等のネイティブ依存を許容する範囲を明示的に決め、配布方針（distribution）に橋渡しする。

## Scope
- **In**: メディアアップロード API（非同期 202→poll）、DB キューによる処理、ストレージ抽象、BlurHash、フォーカルポイント、サムネイル生成、ネイティブ依存範囲の決定。
- **Out**: 動画処理の本格対応（後回し）、配布形態そのものの実装（→ distribution、ただし依存判断結果を渡す）、statuses への添付ロジック（→ statuses-core）。

## Boundary Candidates
- メディアアップロードの非同期処理（DB キュー）
- ストレージ抽象境界
- 画像処理（BlurHash/フォーカル/サムネイル）とネイティブ依存判断

## Out of Boundary
- 投稿本体への紐付け（statuses-core）
- 配布パッケージング（distribution）

## Upstream / Downstream
- **Upstream**: core-runtime, api-foundation。
- **Downstream**: statuses-core(添付)、accounts-and-instance(アバター/ヘッダ)、custom_emojis。

## Existing Spec Touchpoints
- **Extends**: なし。
- **Adjacent**: distribution（ネイティブ依存判断の結果を消費）、statuses-core（添付の消費側）。

## Constraints
- 外部ジョブブローカー不使用（DB キュー）。
- ネイティブ依存（libvips/ffmpeg）を許容するかは配布の容易さと両立して決める（単一バイナリは手段であり目的ではない）。
- MVP は画像。動画は後回し。
