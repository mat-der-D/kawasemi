# Product Overview

kawasemi は **一人鯖（single-user server）運用に特化した Fediverse サーバー**のフルスクラッチ実装（fork ではない）。ActivityPub で連合し、Mastodon 互換 API を通じて既存クライアントから利用できる。

## Core Capabilities

- **複数アクター / 一人運用**：人間ユーザーは一人だが、ActivityPub 上で独立して振る舞う複数アクターを保持する。「同一オーナー」は管理層のみの概念で、プロトコル層には露出させない。
- **ActivityPub 連合**：HTTP Signatures・WebFinger・NodeInfo・inbox/outbox（shared inbox 含む）を備えた連合基盤を前提として持つ。
- **Mastodon 互換 API**：既存クライアント（Ivory・Elk・Phanpy 等）で独自機能以外が完結することを目標とする。本プロジェクト最大の工数ブロック。
- **独自機能**：絵文字リアクション・引用投稿・MFM 等のリッチテキスト・投票・カスタム絵文字。フル体験は独自フロント経由。
- **全アクター統合の管理画面**：全アクターを一つのダッシュボードから俯瞰・操作する独自フロント。BOT 管理のしやすさが目玉。

## Target Use Cases

- 個人が自分専用の Fediverse サーバーを低スペック VPS 上で運用する。
- 一人のオーナーが複数アクター（人格・BOT）を一元管理する。
- BOT をアクターごとの API トークンで外部から操作する。

## Value Proposition

- **インストールが簡単**：事前ビルド済み配布・自動マイグレーション・内蔵 ACME により、非エンジニアでも立てられる。ユーザーにコンパイルをさせない。
- **設定レベルのカスタマイズ**：コーディング知識なしで管理画面から運用設定を変更できる。
- **Mastodon ライク / Misskey ライク**のインストール時テンプレート（機能セット + 連動 UI プリセット、独自フロント前提）。

## Scope Boundaries

- **送信は実装しないが受信は対応**：アカウント移転（受信側 `Move`）、通報（受信側 `Flag`）。
- **実装しない**：ディスカバリー（プロフィールディレクトリ）、アカウント移転の送信、通報の送信。
- **初期スコープ外（将来検討）**：Misskey API 互換。初期は Mastodon API のみに集中する。

---
_詳細な設計方針は `docs/fediverse-design.md` / `docs/mastodon-api-compat.md` / `docs/mastodon-api-estimate.md` を一次情報とする。_
