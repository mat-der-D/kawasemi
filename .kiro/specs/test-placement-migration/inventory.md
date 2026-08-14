# 移設対象テスト関数のインベントリ — test-placement-migration

作成: 2026-08-14 / 対象リビジョン: `5ad15a5` / 作業ツリー: `src/` `tests/` は無変更

タスク 1.1 の成果物。要件 1.4（対象 203 / 20 の内訳を移設前に確定）、要件 1.5（保存則の基礎）、
要件 2.6（移設前後のテスト関数総数の比較基準）、要件 6.4（計数単位の明示）に対応する。
本文の数値はすべて §7「計数の再現コマンド」に貼った出力から辿れる。

## 1. 計数の単位（要件 6.4）

本 spec は 2 つの異なる単位を使う。**混同すると保存則が合わない。**

| 単位 | 定義 | 求め方 |
|---|---|---|
| **呼び出し箇所** | `spawn_test_app(` というトークンの出現数 | `git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' \| wc -l` |
| **テスト関数の本数** | `#[test]` / `#[tokio::test]` が付いた fn の数 | 属性の出現数、または各呼び出し箇所を囲む fn への帰属 |

- **`placement-audit.md`・`requirements.md`・`design.md`・`HANDOFF.md` が挙げる 208 / 203 / 190 / 13 / 5 はすべて「呼び出し箇所」である。**
  既存文書との比較は必ずこの単位で行う。
- **要件 2.6 が求める「テスト関数の総数」は別単位である。** §5 にベースラインを置く。
- 両者は一致しない。本インベントリの実測では、対象 20 ファイルに限れば
  「移設対象テスト関数 203 本」と「呼び出し箇所 203」がたまたま 1:1 で一致するが、
  これは移設対象の全関数が `spawn_test_app` をちょうど 1 回だけ呼ぶという実測結果によるものであって、
  定義上の同値ではない。実際、対象外の `src/test_harness/tests.rs` には
  1 関数で 2 回呼ぶもの（`spawn_test_app_isolates_database_state_between_instances`）がある。

### 分類の規則

分類は **「そのテスト関数が `spawn_test_app` を呼ぶか」のみ**で機械的に行う（design.md「System Flows / 1 ファイル分の移設手順」の第 1・2 ステップ）。
呼ばない関数はパーサ・フォーマッタ等の純粋な単体テストであり、**元位置が正しい配置**なので対象外として残す
（design.md「Boundary Commitments / Out of Boundary」最終項）。可視性・Tier 判定はこの分類の後段であり、分類自体には影響しない。

帰属は各 `spawn_test_app(` の出現位置を、それを囲む `#[test]` / `#[tokio::test]` 付き fn の
本体（対応する波括弧の範囲）に割り当てて求めた。文字列・文字リテラル・行コメント・ブロックコメント・
ドキュメントコメントはマスクしてから走査している（§7 の再現手順を参照）。
未帰属の呼び出し箇所（テスト関数の外＝ヘルパー関数内の呼び出し）は **21 ファイルすべてで 0 件**だった。

## 2. ファイル別の内訳

`呼び出し箇所（grep）` は `git grep` が数えた出現数、`うちコード上` はコメント・文字列をマスクした後の出現数。
両者が食い違うのは `src/test_harness/tests.rs` の 1 件のみ（§6.1）。

| ファイル | 呼び出し箇所（grep） | うちコード上 | テスト関数 | 移設対象 | 元位置に残す | 移設先 |
|---|---|---|---|---|---|---|
| `src/notifications/endpoints/tests.rs` | 24 | 24 | 24 | 24 | 0 | `tests/notifications_endpoints_it.rs` |
| `src/social_graph/endpoints/tests.rs` | 22 | 22 | 26 | 22 | 4 | `tests/social_graph_endpoints_it.rs` |
| `src/statuses/endpoints/tests.rs` | 21 | 21 | 28 | 21 | 7 | `tests/statuses_endpoints_it.rs` |
| `src/notifications/service/tests.rs` | 17 | 17 | 20 | 17 | 3 | `tests/notifications_service_it.rs` |
| `src/accounts/account_service/tests.rs` | 16 | 16 | 16 | 16 | 0 | `tests/accounts_account_service_it.rs` |
| `src/statuses/render_assembler/tests.rs` | 13 | 13 | 13 | 13 | 0 | —（移設タスクなし・Tier 2 例外） |
| `src/search/hydrator/tests.rs` | 11 | 11 | 15 | 11 | 4 | `tests/search_hydrator_it.rs` |
| `src/search/service/tests.rs` | 11 | 11 | 11 | 11 | 0 | `tests/search_service_it.rs` |
| `src/search/endpoint/tests.rs` | 10 | 10 | 25 | 10 | 15 | `tests/search_endpoint_it.rs` |
| `src/federation/signatures/signer/tests.rs` | 8 | 8 | 10 | 8 | 2 | `tests/federation_signatures_signer_it.rs` |
| `src/social_graph/follow_request_service/tests.rs` | 8 | 8 | 8 | 8 | 0 | `tests/social_graph_follow_request_service_it.rs` |
| `src/oauth/middleware/tests.rs` | 7 | 7 | 9 | 7 | 2 | `tests/oauth_middleware_it.rs` |
| `src/timelines/endpoints/tests.rs` | 7 | 7 | 7 | 7 | 0 | `tests/timelines_endpoints_it.rs` **（既存ファイルと衝突。§6.2 参照）** |
| `src/federation/signatures/negotiation/tests.rs` | 6 | 6 | 8 | 6 | 2 | `tests/federation_signatures_negotiation_it.rs` |
| `src/social_graph/tests.rs` | 6 | 6 | 6 | 6 | 0 | `tests/social_graph_module_it.rs` |
| `src/federation/endpoints/webfinger/tests.rs` | 5 | 5 | 10 | 5 | 5 | `tests/federation_webfinger_endpoint_it.rs` |
| `src/test_harness/tests.rs` ※ | 5 | 4 | 3 | 3 ※ | 0 | —（対象外） |
| `src/federation/outbound/worker/tests.rs` | 4 | 4 | 4 | 4 | 0 | `tests/federation_outbound_worker_it.rs` |
| `src/statuses/account_provider/tests.rs` | 3 | 3 | 6 | 3 | 3 | `tests/statuses_account_provider_it.rs` |
| `src/notifications/tests.rs` | 2 | 2 | 2 | 2 | 0 | `tests/notifications_module_it.rs` |
| `src/search/tests.rs` | 2 | 2 | 2 | 2 | 0 | `tests/search_module_it.rs` |

※ `src/test_harness/tests.rs` の「移設対象 3」は**列の意味（`spawn_test_app` を呼ぶテスト関数の本数）に従った機械的な値**であって、
移設予定を意味しない。この 3 本は本 spec の**対象外**であり元位置に留まる（要件の Out of scope、および §1・§4・§6.1）。
§3 以降のすべての総計はこの行を除外して計算している。

### 行単位計数（`-c`）との照合

`git grep -c` は**行**を数えるため、1 行に 2 回呼ぶ箇所があると過少に出る。
21 ファイルすべてで `-c` の値と `-o` の値が一致した（§7 の出力 B と C を比較）。
したがって **`-c` と `-o` の乖離はこのリポジトリの現状では存在しない**。
`src/test_harness/tests.rs` の 2 回呼ぶ関数も L142 と L143 に分かれているため行単位でも 2 と数えられる。

## 3. 総計と保存則の照合（要件 1.4 / 1.5）

すべて**呼び出し箇所**単位。

| 集合 | ファイル数 | 呼び出し箇所 |
|---|---|---|
| `src/**/tests.rs` 全体 | 21 | 208 |
| 対象（本 spec が扱う） | 20 | 203 |
| 対象外（`src/test_harness/tests.rs`） | 1 | 5 |
| 対象のうち移設タスクを持つ | 19 | 190 |
| 対象のうち Tier 2 例外で移設タスクを持たない（`src/statuses/render_assembler/tests.rs`） | 1 | 13 |

照合 1: 208 = 203 + 5 ✓ / 21 ファイル = 20 + 1 ✓
照合 2: 203 = 190 + 13 ✓ / 20 ファイル = 19 + 1 ✓
照合 3: §2 の表の「呼び出し箇所（grep）」列の合計 = 208 ✓（対象 20 ファイルの合計 = 203 ✓）

**spec が述べる 208 / 203 / 190 / 13 / 5 はすべて実測と一致した。** ただし 5 の内訳には注意がある（§6.1）。

### テスト関数単位での対応する内訳

| 集合 | ファイル数 | テスト関数 | うち移設対象 | うち元位置に残す |
|---|---|---|---|---|
| 対象 20 ファイル | 20 | 250 | 203 | 47 |
| うち移設タスクを持つ 19 ファイル | 19 | 237 | 190 | 47 |
| うち Tier 2 例外 1 ファイル | 1 | 13 | 13 | 0 |
| 対象外 `src/test_harness/tests.rs` | 1 | 3 | —（対象外 3 本） | — |

照合: 250 = 203 + 47 ✓ / 250 = 237 + 13 ✓ / 203 = 190 + 13 ✓

**移設タスクが完了したとき、`src/**/tests.rs` から消えるテスト関数は 190 本、増える `tests/*_it.rs` の
テスト関数も 190 本**であることが要件 2.6 の期待値になる（例外が追加で発生しない場合。
Tier 2 判定が追加で出た分だけ両側が減る）。

## 4. ファイル別のテスト関数名

移設の単位はファイルではなく**テスト関数**である（HANDOFF §2）。以下が移設の作業単位そのものになる。

### `src/notifications/endpoints/tests.rs`

呼び出し箇所 24（うちコード上の呼び出し 24） / テスト関数 24 本 = 移設対象 24 + 元位置に残す 0。移設先: `tests/notifications_endpoints_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 24 本）**

- `list_notifications_without_a_bearer_token_is_401` — L278 `#[tokio::test]`
- `list_notifications_with_insufficient_scope_is_403` — L289 `#[tokio::test]`
- `list_notifications_returns_recipient_notifications_newest_first` — L307 `#[tokio::test]`
- `list_notifications_filters_by_types` — L352 `#[tokio::test]`
- `list_notifications_with_unknown_type_is_422` — L392 `#[tokio::test]`
- `list_notifications_with_unresolved_account_id_returns_empty_array_not_404` — L415 `#[tokio::test]`
- `list_notifications_with_non_numeric_account_id_returns_empty_array_not_404` — L457 `#[tokio::test]`
- `list_notifications_with_known_local_account_id_filters_to_that_origin` — L480 `#[tokio::test]`
- `list_notifications_with_known_remote_account_id_filters_to_that_origin` — L523 `#[tokio::test]`
- `list_notifications_link_header_present_when_more_pages_remain` — L558 `#[tokio::test]`
- `show_notification_without_a_bearer_token_is_401` — L595 `#[tokio::test]`
- `show_notification_with_insufficient_scope_is_403` — L606 `#[tokio::test]`
- `show_notification_returns_200_for_own_notification` — L621 `#[tokio::test]`
- `show_notification_for_another_actors_notification_is_404` — L653 `#[tokio::test]`
- `show_notification_for_nonexistent_id_is_404` — L683 `#[tokio::test]`
- `show_notification_with_non_numeric_id_is_404` — L705 `#[tokio::test]`
- `clear_notifications_without_a_bearer_token_is_401` — L728 `#[tokio::test]`
- `clear_notifications_with_insufficient_scope_is_403` — L739 `#[tokio::test]`
- `clear_notifications_dismisses_everything_and_returns_empty_object` — L754 `#[tokio::test]`
- `dismiss_notification_without_a_bearer_token_is_401` — L788 `#[tokio::test]`
- `dismiss_notification_with_insufficient_scope_is_403` — L799 `#[tokio::test]`
- `dismiss_notification_dismisses_and_returns_empty_object` — L820 `#[tokio::test]`
- `dismiss_notification_for_another_actors_notification_is_404` — L862 `#[tokio::test]`
- `dismiss_notification_for_nonexistent_id_is_404` — L892 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/social_graph/endpoints/tests.rs`

呼び出し箇所 22（うちコード上の呼び出し 22） / テスト関数 26 本 = 移設対象 22 + 元位置に残す 4。移設先: `tests/social_graph_endpoints_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 22 本）**

- `follow_succeeds_with_follow_scope_and_returns_relationship` — L464 `#[tokio::test]`
- `follow_without_a_bearer_token_is_401` — L491 `#[tokio::test]`
- `follow_with_insufficient_scope_is_403` — L518 `#[tokio::test]`
- `follow_of_a_nonexistent_account_is_404` — L545 `#[tokio::test]`
- `follow_applies_a_json_body_options_override` — L569 `#[tokio::test]`
- `follow_succeeds_with_write_follows_scope_and_returns_relationship` — L598 `#[tokio::test]`
- `follow_self_is_422_unprocessable_entity` — L628 `#[tokio::test]`
- `unfollow_succeeds_and_returns_relationship` — L658 `#[tokio::test]`
- `unfollow_succeeds_with_write_follows_scope_and_returns_relationship` — L695 `#[tokio::test]`
- `list_follow_requests_requires_read_follows_scope` — L737 `#[tokio::test]`
- `list_follow_requests_succeeds_with_follow_scope` — L767 `#[tokio::test]`
- `list_follow_requests_returns_account_json_with_link_header` — L838 `#[tokio::test]`
- `authorize_follow_request_establishes_a_follow` — L904 `#[tokio::test]`
- `reject_follow_request_drops_the_pending_request` — L949 `#[tokio::test]`
- `authorize_follow_request_with_no_pending_request_is_404` — L995 `#[tokio::test]`
- `mute_succeeds_with_write_mutes_scope_and_returns_relationship` — L1022 `#[tokio::test]`
- `mute_succeeds_with_write_follows_scope_and_returns_relationship` — L1052 `#[tokio::test]`
- `unmute_succeeds_and_returns_relationship` — L1082 `#[tokio::test]`
- `block_succeeds_with_write_blocks_scope_and_returns_relationship` — L1121 `#[tokio::test]`
- `block_succeeds_with_write_follows_scope_and_returns_relationship` — L1150 `#[tokio::test]`
- `unblock_succeeds_and_returns_relationship` — L1180 `#[tokio::test]`
- `block_of_a_nonexistent_account_is_404` — L1217 `#[tokio::test]`

**元位置に残す純粋単体テスト（4 本）**

- `parse_follow_options_defaults_reblogs_true_on_empty_body` — L1243 `#[test]`
- `parse_follow_options_rejects_malformed_json_as_422` — L1251 `#[test]`
- `parse_mute_options_defaults_notifications_true_on_empty_body` — L1257 `#[test]`
- `parse_optional_limit_rejects_non_numeric_value_as_422` — L1264 `#[test]`

### `src/statuses/endpoints/tests.rs`

呼び出し箇所 21（うちコード上の呼び出し 21） / テスト関数 28 本 = 移設対象 21 + 元位置に残す 7。移設先: `tests/statuses_endpoints_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 21 本）**

- `create_status_without_bearer_is_401` — L701 `#[tokio::test]`
- `create_status_with_wrong_scope_is_403` — L722 `#[tokio::test]`
- `create_status_with_empty_body_is_422` — L747 `#[tokio::test]`
- `create_status_returns_200_with_rendered_account_and_repeats_on_the_same_idempotency_key` — L779 `#[tokio::test]`
- `show_status_is_404_for_an_unknown_id` — L839 `#[tokio::test]`
- `show_status_returns_200_for_a_public_post_unauthenticated` — L854 `#[tokio::test]`
- `delete_status_by_a_non_owner_is_404` — L898 `#[tokio::test]`
- `delete_status_by_the_owner_succeeds` — L958 `#[tokio::test]`
- `favourite_then_unfavourite_toggles_state` — L1009 `#[tokio::test]`
- `favourite_without_the_write_favourites_scope_is_403` — L1068 `#[tokio::test]`
- `pin_rejects_a_direct_visibility_status_with_422` — L1112 `#[tokio::test]`
- `reblog_then_unreblog_returns_200` — L1156 `#[tokio::test]`
- `bookmarks_list_requires_read_bookmarks_scope` — L1215 `#[tokio::test]`
- `bookmarking_a_status_makes_it_appear_in_the_bookmark_list_with_a_link_header` — L1241 `#[tokio::test]`
- `poll_vote_updates_the_tally_and_get_reflects_it` — L1311 `#[tokio::test]`
- `create_status_resolves_a_registered_shortcode_into_the_emojis_field` — L1440 `#[tokio::test]`
- `create_status_with_no_registered_shortcode_has_empty_emojis` — L1491 `#[tokio::test]`
- `poll_json_resolves_a_registered_shortcode_from_an_option_title` — L1525 `#[tokio::test]`
- `status_context_renders_every_ancestor_and_descendant_material_in_thread_order` — L1770 `#[tokio::test]`
- `bookmark_list_renders_every_material_newest_bookmark_first` — L2040 `#[tokio::test]`
- `the_bookmark_page_batches_every_material_except_its_polls` — L2345 `#[tokio::test]`

**元位置に残す純粋単体テスト（7 本）**

- `parse_visibility_accepts_every_canonical_variant` — L57 `#[test]`
- `parse_visibility_rejects_unknown_value` — L65 `#[test]`
- `parse_media_ids_parses_decimal_strings` — L71 `#[test]`
- `parse_media_ids_rejects_a_non_numeric_value` — L77 `#[test]`
- `parse_optional_limit_is_none_when_absent` — L83 `#[test]`
- `parse_optional_limit_rejects_a_non_numeric_value` — L88 `#[test]`
- `parse_id_treats_an_unparseable_segment_as_404` — L94 `#[test]`

### `src/notifications/service/tests.rs`

呼び出し箇所 17（うちコード上の呼び出し 17） / テスト関数 20 本 = 移設対象 17 + 元位置に残す 3。移設先: `tests/notifications_service_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 17 本）**

- `list_returns_only_the_requesting_recipients_notifications` — L175 `#[tokio::test]`
- `list_excludes_dismissed_notifications` — L222 `#[tokio::test]`
- `list_applies_types_filter` — L260 `#[tokio::test]`
- `list_applies_account_id_filter` — L303 `#[tokio::test]`
- `list_embeds_related_status_for_post_related_kinds` — L350 `#[tokio::test]`
- `list_null_status_for_follow_kinds` — L389 `#[tokio::test]`
- `show_returns_the_notification_for_its_own_recipient` — L427 `#[tokio::test]`
- `show_404_for_another_recipients_notification` — L455 `#[tokio::test]`
- `show_404_for_a_nonexistent_notification` — L480 `#[tokio::test]`
- `dismiss_excludes_from_subsequent_show_and_list` — L497 `#[tokio::test]`
- `dismiss_404_for_another_recipients_notification` — L536 `#[tokio::test]`
- `dismiss_404_for_a_nonexistent_notification` — L561 `#[tokio::test]`
- `clear_dismisses_every_notification_for_the_recipient` — L576 `#[tokio::test]`
- `clear_succeeds_when_the_recipient_has_no_notifications` — L618 `#[tokio::test]`
- `list_renders_a_mixed_page_of_every_status_shape_in_order` — L756 `#[tokio::test]`
- `list_still_resolves_a_status_the_envelope_will_discard` — L1070 `#[tokio::test]`
- `show_and_list_render_the_same_notification_identically` — L1116 `#[tokio::test]`

**元位置に残す純粋単体テスト（3 本）**

- `resolve_many_raises_this_modules_not_found_for_a_dangling_poll_id` — L1221 `#[tokio::test]`
- `resolve_many_returns_every_existing_poll_in_the_requested_order` — L1248 `#[tokio::test]`
- `resolve_many_reports_the_viewers_own_votes` — L1276 `#[tokio::test]`

### `src/accounts/account_service/tests.rs`

呼び出し箇所 16（うちコード上の呼び出し 16） / テスト関数 16 本 = 移設対象 16 + 元位置に残す 0。移設先: `tests/accounts_account_service_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 16 本）**

- `show_account_returns_an_account_for_a_local_actor` — L344 `#[tokio::test]`
- `show_account_returns_an_account_for_a_known_cached_remote_account` — L370 `#[tokio::test]`
- `show_account_returns_404_for_an_unknown_numeric_id` — L397 `#[tokio::test]`
- `show_account_attempts_a_fetch_for_a_non_numeric_id_and_maps_failure_to_404` — L417 `#[tokio::test]`
- `verify_credentials_returns_a_credential_account_with_source_and_role` — L438 `#[tokio::test]`
- `verify_credentials_fails_for_an_actor_that_no_longer_exists` — L466 `#[tokio::test]`
- `list_statuses_returns_an_empty_page_when_no_provider_is_registered` — L489 `#[tokio::test]`
- `list_statuses_returns_404_for_an_unknown_account` — L516 `#[tokio::test]`
- `list_statuses_threads_filters_and_pagination_to_the_provider` — L535 `#[tokio::test]`
- `relationships_returns_all_default_when_no_provider_is_registered` — L597 `#[tokio::test]`
- `relationships_omits_an_unresolvable_id_instead_of_failing_the_batch` — L647 `#[tokio::test]`
- `relationships_returns_a_relationship_array_from_a_registered_provider` — L751 `#[tokio::test]`
- `update_credentials_partial_update_is_reflected_in_verify_credentials_and_leaves_other_fields_untouched` — L812 `#[tokio::test]`
- `update_credentials_rejects_too_many_profile_fields_with_422_and_does_not_write` — L874 `#[tokio::test]`
- `update_credentials_rejects_an_out_of_range_avatar_focus_before_any_write` — L934 `#[tokio::test]`
- `update_credentials_ingests_an_avatar_upload_via_media_service` — L980 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/statuses/render_assembler/tests.rs`

呼び出し箇所 13（うちコード上の呼び出し 13） / テスト関数 13 本 = 移設対象 13 + 元位置に残す 0。移設先: —（移設タスクなし・Tier 2 例外）

**移設対象（`spawn_test_app` を呼ぶ 13 本）**

- `muted_is_false_when_the_caller_supplies_no_mute_context` — L165 `#[tokio::test]`
- `muted_reflects_the_supplied_mute_context` — L194 `#[tokio::test]`
- `muted_is_judged_per_status_by_its_own_author` — L224 `#[tokio::test]`
- `a_missing_poll_renders_as_none_under_the_tolerant_resolver` — L270 `#[tokio::test]`
- `a_missing_poll_is_an_error_under_the_strict_resolver` — L300 `#[tokio::test]`
- `a_present_poll_renders_its_options` — L328 `#[tokio::test]`
- `assemble_many_preserves_input_order` — L397 `#[tokio::test]`
- `assemble_one_matches_a_single_element_batch` — L437 `#[tokio::test]`
- `resolves_registered_shortcodes_in_the_status_content` — L494 `#[tokio::test]`
- `resolves_registered_shortcodes_in_poll_option_titles` — L525 `#[tokio::test]`
- `a_rich_batch_keeps_every_resolved_material_and_its_order` — L702 `#[tokio::test]`
- `an_unauthenticated_batch_reports_no_interactions_but_still_mutes` — L886 `#[tokio::test]`
- `author_resolution_tracks_distinct_authors_and_not_list_length` — L974 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/search/hydrator/tests.rs`

呼び出し箇所 11（うちコード上の呼び出し 11） / テスト関数 15 本 = 移設対象 11 + 元位置に残す 4。移設先: `tests/search_hydrator_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 11 本）**

- `hydrate_accounts_dedups_duplicate_refs_and_renders_every_distinct_account` — L160 `#[tokio::test]`
- `hydrate_accounts_following_only_narrows_to_followed_accounts` — L243 `#[tokio::test]`
- `hydrate_statuses_excludes_a_post_invisible_to_the_viewer` — L282 `#[tokio::test]`
- `hydrate_statuses_includes_a_private_post_visible_to_its_own_author` — L326 `#[tokio::test]`
- `hydrate_statuses_truncates_to_the_requested_limit_after_visibility_filtering` — L360 `#[tokio::test]`
- `hydrate_statuses_skips_an_unknown_id_without_erroring` — L393 `#[tokio::test]`
- `hydrate_hashtags_renders_a_matched_tag_to_tag_json` — L427 `#[tokio::test]`
- `hydrate_hashtags_skips_a_tag_that_no_longer_resolves` — L465 `#[tokio::test]`
- `hydrate_hashtags_renders_multiple_tags_in_order` — L485 `#[tokio::test]`
- `hydrate_statuses_keeps_every_rendered_material_order_and_truncation` — L789 `#[tokio::test]`
- `hydrate_statuses_renders_nothing_for_a_zero_limit` — L1055 `#[tokio::test]`

**元位置に残す純粋単体テスト（4 本）**

- `account_ref_id_recovers_the_id_regardless_of_local_remote` — L51 `#[test]`
- `resolve_many_drops_a_dangling_poll_id_instead_of_failing` — L583 `#[tokio::test]`
- `resolve_many_returns_polls_in_the_requested_order` — L614 `#[tokio::test]`
- `resolve_many_reports_the_viewers_own_votes` — L642 `#[tokio::test]`

### `src/search/service/tests.rs`

呼び出し箇所 11（うちコード上の呼び出し 11） / テスト関数 11 本 = 移設対象 11 + 元位置に残す 0。移設先: `tests/search_service_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 11 本）**

- `search_rejects_empty_query_with_422` — L258 `#[tokio::test]`
- `search_type_accounts_returns_empty_arrays_for_other_types` — L282 `#[tokio::test]`
- `search_unscoped_returns_all_three_types` — L328 `#[tokio::test]`
- `search_resolve_true_adds_remote_account_to_results` — L363 `#[tokio::test]`
- `search_resolve_false_never_fetches_remotely` — L390 `#[tokio::test]`
- `search_resolved_account_is_not_leaked_when_type_excludes_accounts` — L414 `#[tokio::test]`
- `search_backend_failure_propagates_as_err` — L470 `#[tokio::test]`
- `search_remote_resolution_failure_does_not_fail_the_whole_search` — L494 `#[tokio::test]`
- `search_exclude_unreviewed_is_accepted_without_changing_results` — L517 `#[tokio::test]`
- `search_threads_account_id_and_limit_offset_to_the_backend` — L550 `#[tokio::test]`
- `search_end_to_end_with_the_default_pg_backend` — L586 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/search/endpoint/tests.rs`

呼び出し箇所 10（うちコード上の呼び出し 10） / テスト関数 25 本 = 移設対象 10 + 元位置に残す 15。移設先: `tests/search_endpoint_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 10 本）**

- `search_without_a_bearer_token_is_401` — L386 `#[tokio::test]`
- `search_with_insufficient_scope_is_403` — L400 `#[tokio::test]`
- `search_with_empty_query_is_422` — L422 `#[tokio::test]`
- `search_returns_search_results_shape_for_an_authenticated_request` — L451 `#[tokio::test]`
- `search_returns_a_matching_account_end_to_end` — L481 `#[tokio::test]`
- `search_type_hashtags_excludes_a_matching_account` — L520 `#[tokio::test]`
- `search_with_unrecognized_type_is_422` — L549 `#[tokio::test]`
- `search_limit_and_offset_are_extracted_and_threaded_through` — L576 `#[tokio::test]`
- `search_with_malformed_account_id_is_422` — L618 `#[tokio::test]`
- `search_with_malformed_resolve_boolean_is_422` — L641 `#[tokio::test]`

**元位置に残す純粋単体テスト（15 本）**

- `resolve_search_limit_defaults_when_absent` — L60 `#[test]`
- `resolve_search_limit_clamps_to_max_when_over_limit` — L65 `#[test]`
- `resolve_search_limit_passes_through_a_value_within_bounds` — L70 `#[test]`
- `resolve_search_limit_rejects_a_malformed_value_with_422` — L75 `#[test]`
- `resolve_search_offset_defaults_to_zero_when_absent` — L81 `#[test]`
- `resolve_search_offset_passes_through_a_present_value_unclamped` — L86 `#[test]`
- `resolve_search_offset_rejects_a_malformed_value_with_422` — L91 `#[test]`
- `parse_search_type_accepts_all_three_known_values` — L97 `#[test]`
- `parse_search_type_rejects_an_unknown_value_with_422` — L104 `#[test]`
- `parse_optional_bool_query_defaults_to_false_when_absent` — L110 `#[test]`
- `parse_optional_bool_query_accepts_true_and_false_spellings` — L115 `#[test]`
- `parse_optional_bool_query_rejects_an_unrecognized_value_with_422` — L123 `#[test]`
- `parse_optional_account_id_defaults_to_none_when_absent` — L129 `#[test]`
- `parse_optional_account_id_parses_a_present_numeric_value` — L134 `#[test]`
- `parse_optional_account_id_rejects_a_non_numeric_value_with_422` — L142 `#[test]`

### `src/federation/signatures/signer/tests.rs`

呼び出し箇所 8（うちコード上の呼び出し 8） / テスト関数 10 本 = 移設対象 8 + 元位置に残す 2。移設先: `tests/federation_signatures_signer_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 8 本）**

- `sign_request_draft_cavage_sets_signature_header_with_matching_key_id` — L126 `#[tokio::test]`
- `sign_request_rfc9421_sets_signature_input_and_signature_headers` — L174 `#[tokio::test]`
- `sign_request_with_body_sets_a_digest_header_matching_the_body` — L221 `#[tokio::test]`
- `sign_request_without_body_sets_no_digest_header` — L259 `#[tokio::test]`
- `sign_request_for_an_unknown_actor_fails_and_does_not_mutate_req` — L281 `#[tokio::test]`
- `sign_request_for_an_actor_with_no_valid_key_fails_and_does_not_mutate_req` — L306 `#[tokio::test]`
- `sign_request_produces_a_signature_verifiable_with_the_actors_own_public_key` — L331 `#[tokio::test]`
- `sign_request_rfc9421_produces_a_signature_verifiable_with_the_actors_own_public_key` — L403 `#[tokio::test]`

**元位置に残す純粋単体テスト（2 本）**

- `http_date_matches_rfc_9110s_own_worked_example_shape` — L453 `#[test]`
- `host_from_url_extracts_the_authority_without_scheme_or_path` — L462 `#[test]`

### `src/social_graph/follow_request_service/tests.rs`

呼び出し箇所 8（うちコード上の呼び出し 8） / テスト関数 8 本 = 移設対象 8 + 元位置に残す 0。移設先: `tests/social_graph_follow_request_service_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 8 本）**

- `list_requests_is_empty_when_no_pending_requests` — L239 `#[tokio::test]`
- `list_requests_returns_the_pending_requesters_account_json` — L254 `#[tokio::test]`
- `list_requests_paginates_with_the_given_limit` — L287 `#[tokio::test]`
- `authorize_request_establishes_follow_and_delivers_accept_to_a_remote_requester` — L338 `#[tokio::test]`
- `authorize_request_delivers_locally_when_the_requester_is_local` — L384 `#[tokio::test]`
- `authorize_request_returns_not_found_when_no_pending_request_exists` — L436 `#[tokio::test]`
- `reject_request_drops_the_pending_request_and_delivers_reject` — L456 `#[tokio::test]`
- `reject_request_returns_not_found_when_no_pending_request_exists` — L492 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/oauth/middleware/tests.rs`

呼び出し箇所 7（うちコード上の呼び出し 7） / テスト関数 9 本 = 移設対象 7 + 元位置に残す 2。移設先: `tests/oauth_middleware_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 7 本）**

- `optional_route_with_no_bearer_header_continues_unauthenticated` — L183 `#[tokio::test]`
- `optional_route_with_a_real_valid_token_resolves_the_single_bound_actor` — L205 `#[tokio::test]`
- `required_route_with_no_bearer_header_is_401` — L237 `#[tokio::test]`
- `required_route_with_a_garbage_never_issued_token_is_401` — L254 `#[tokio::test]`
- `required_route_with_a_genuinely_revoked_token_is_401_not_a_crash_or_success` — L271 `#[tokio::test]`
- `scoped_route_with_a_valid_unrevoked_token_missing_the_required_scope_is_403` — L305 `#[tokio::test]`
- `scoped_route_with_a_top_level_scope_subsuming_the_required_granular_scope_succeeds` — L330 `#[tokio::test]`

**元位置に残す純粋単体テスト（2 本）**

- `require_scope_allows_when_the_required_scope_is_satisfied_by_the_granted_scope` — L360 `#[test]`
- `require_scope_rejects_with_403_when_the_required_scope_is_missing` — L370 `#[test]`

### `src/timelines/endpoints/tests.rs`

呼び出し箇所 7（うちコード上の呼び出し 7） / テスト関数 7 本 = 移設対象 7 + 元位置に残す 0。移設先: `tests/timelines_endpoints_it.rs` **（既存ファイルと衝突。§6.2 参照）**

**移設対象（`spawn_test_app` を呼ぶ 7 本）**

- `home_timeline_without_a_bearer_token_is_401` — L257 `#[tokio::test]`
- `home_timeline_with_insufficient_scope_is_403` — L268 `#[tokio::test]`
- `home_timeline_returns_followed_and_self_posts_with_link_header` — L284 `#[tokio::test]`
- `public_timeline_unauthenticated_returns_only_public_posts` — L318 `#[tokio::test]`
- `public_timeline_local_true_excludes_remote_posts` — L343 `#[tokio::test]`
- `tag_timeline_matches_the_path_hashtag_case_insensitively` — L373 `#[tokio::test]`
- `tag_timeline_any_filter_requires_at_least_one_additional_tag` — L402 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/federation/signatures/negotiation/tests.rs`

呼び出し箇所 6（うちコード上の呼び出し 6） / テスト関数 8 本 = 移設対象 6 + 元位置に残す 2。移設先: `tests/federation_signatures_negotiation_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 6 本）**

- `unknown_host_retries_with_other_format_after_401_and_records_success_format` — L134 `#[tokio::test]`
- `host_with_recorded_format_uses_it_first_and_sends_only_once_on_success` — L198 `#[tokio::test]`
- `blocked_403_response_does_not_retry_or_record_capability` — L241 `#[tokio::test]`
- `general_failure_500_response_does_not_retry` — L276 `#[tokio::test]`
- `transport_level_failure_is_propagated_without_retry` — L310 `#[tokio::test]`
- `successful_retry_overwrites_a_previously_recorded_different_format` — L344 `#[tokio::test]`

**元位置に残す純粋単体テスト（2 本）**

- `format_to_db_and_back_round_trips_both_variants` — L397 `#[test]`
- `other_format_is_the_opposite_variant` — L409 `#[test]`

### `src/social_graph/tests.rs`

呼び出し箇所 6（うちコード上の呼び出し 6） / テスト関数 6 本 = 移設対象 6 + 元位置に残す 0。移設先: `tests/social_graph_module_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 6 本）**

- `follow_via_the_live_router_wires_endpoints_and_providers` — L234 `#[tokio::test]`
- `blocking_via_the_service_makes_the_live_block_policy_report_blocked` — L293 `#[tokio::test]`
- `register_downstream_handlers_wires_a_dispatch_reachable_handler` — L383 `#[tokio::test]`
- `register_downstream_handlers_emits_a_follow_notification_for_an_unlocked_target` — L524 `#[tokio::test]`
- `register_downstream_handlers_emits_a_follow_request_notification_for_a_locked_target` — L610 `#[tokio::test]`
- `combined_account_counts_provider_composes_statuses_and_social_graph_sub_counts` — L700 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/federation/endpoints/webfinger/tests.rs`

呼び出し箇所 5（うちコード上の呼び出し 5） / テスト関数 10 本 = 移設対象 5 + 元位置に残す 5。移設先: `tests/federation_webfinger_endpoint_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 5 本）**

- `webfinger_resolves_a_local_actor_to_a_jrd_self_link` — L111 `#[tokio::test]`
- `webfinger_resolves_multiple_distinct_local_actors_independently` — L149 `#[tokio::test]`
- `webfinger_does_not_resolve_a_non_matching_domain` — L192 `#[tokio::test]`
- `webfinger_reports_an_unknown_actor_as_not_found` — L214 `#[tokio::test]`
- `webfinger_rejects_a_malformed_resource_with_bad_request` — L235 `#[tokio::test]`

**元位置に残す純粋単体テスト（5 本）**

- `parse_acct_resource_accepts_a_well_formed_acct_uri` — L31 `#[test]`
- `parse_acct_resource_rejects_a_missing_acct_prefix` — L39 `#[test]`
- `parse_acct_resource_rejects_a_missing_at_separator` — L44 `#[test]`
- `parse_acct_resource_rejects_an_empty_user_segment` — L49 `#[test]`
- `parse_acct_resource_rejects_an_empty_domain_segment` — L54 `#[test]`

### `src/federation/outbound/worker/tests.rs`

呼び出し箇所 4（うちコード上の呼び出し 4） / テスト関数 4 本 = 移設対象 4 + 元位置に残す 0。移設先: `tests/federation_outbound_worker_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 4 本）**

- `run_once_delivers_a_due_job_and_marks_it_done` — L132 `#[tokio::test]`
- `run_once_reschedules_a_job_on_transient_failure_with_backoff_applied` — L174 `#[tokio::test]`
- `run_once_marks_a_job_permanently_failed_once_attempts_are_exhausted` — L220 `#[tokio::test]`
- `run_once_marks_a_job_failed_immediately_when_sender_no_longer_resolves` — L276 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/statuses/account_provider/tests.rs`

呼び出し箇所 3（うちコード上の呼び出し 3） / テスト関数 6 本 = 移設対象 3 + 元位置に残す 3。移設先: `tests/statuses_account_provider_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 3 本）**

- `list_statuses_keeps_every_rendered_material_and_order` — L374 `#[tokio::test]`
- `a_page_costs_the_same_ancillary_queries_at_one_status_and_at_twenty` — L745 `#[tokio::test]`
- `the_only_media_and_pinned_filters_still_cost_one_query_per_candidate` — L830 `#[tokio::test]`

**元位置に残す純粋単体テスト（3 本）**

- `resolve_many_raises_this_modules_not_found_for_a_dangling_poll_id` — L123 `#[tokio::test]`
- `resolve_many_returns_every_existing_poll_in_the_requested_order` — L150 `#[tokio::test]`
- `resolve_many_reports_the_viewers_own_votes` — L178 `#[tokio::test]`

### `src/notifications/tests.rs`

呼び出し箇所 2（うちコード上の呼び出し 2） / テスト関数 2 本 = 移設対象 2 + 元位置に残す 0。移設先: `tests/notifications_module_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 2 本）**

- `notification_endpoints_require_bearer_auth` — L198 `#[tokio::test]`
- `favourite_flows_through_the_wired_sink_to_the_generator_and_is_visible_via_real_account_resolution` — L215 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/search/tests.rs`

呼び出し箇所 2（うちコード上の呼び出し 2） / テスト関数 2 本 = 移設対象 2 + 元位置に残す 0。移設先: `tests/search_module_it.rs`

**移設対象（`spawn_test_app` を呼ぶ 2 本）**

- `search_endpoint_requires_bearer_auth_and_carries_rate_limit_headers` — L177 `#[tokio::test]`
- `search_end_to_end_through_the_real_router_with_the_default_pg_backend` — L205 `#[tokio::test]`

**元位置に残す純粋単体テスト**: なし（このファイルは全関数が移設対象）

### `src/test_harness/tests.rs`（対象外）

呼び出し箇所 5（`git grep` 基準） / うちコード上の呼び出し 4 / テスト関数 3 本。**残り 1 箇所は L2 のモジュールドキュメントコメント内の記述であり、呼び出しではない**（§6.1 参照）。

**対象外として元位置に残す 3 本**

- `spawn_test_app_boots_with_applied_migrations_and_deterministic_runtime` — L82 `#[tokio::test]`
- `spawn_test_app_isolates_database_state_between_instances` — L136 `#[tokio::test]`（呼び出し 2 箇所）
- `cleanup_releases_pool_listener_and_isolated_schema` — L188 `#[tokio::test]`

## 5. 要件 2.6 のベースライン（移設前のテスト関数総数）

移設の前後で比較するための基準値。**測定は 2 通りの方法で行い、両方を記録する。**

| 対象 | `git grep -o` による属性出現数 | コメント・文字列をマスクした後の数 | 差 |
|---|---|---|---|
| `src/**`（対象 320 ファイル） | 1807 | 1779 | 28 |
| `tests/**`（対象 87 ファイル） | 524 | 507 | 17 |

差はすべて**ドキュメントコメント内の `#[test]` / `#[tokio::test]` の記述**である
（該当ファイルは §7 の出力 H に列挙した）。**要件 2.6 の比較にはマスク後の値 1779 / 507 を使うこと。**
grep 値 1807 / 524 はドキュメント文言の編集で動くため、移設の影響と切り分けられない。

属性の綴りは `#[test]` と `#[tokio::test]` の 2 種類のみで、
`#[tokio::test(flavor = ...)]` のような引数つきの変種はリポジトリ内に存在しない（§7 の出力 I）。
したがって上記 2 パターンの走査で網羅している。

移設後に期待される値（例外が追加で発生しない場合）:

- `src/**`: 1779 − 190 = **1589**
- `tests/**`: 507 + 190 = **697**
- 合計 2286 は移設前後で不変

統合テストバイナリ数のベースライン: `git ls-files 'tests/*_it.rs' | wc -l` = **87**（§7 の出力 E）。
移設先 19 ファイルのうち 1 つは既存ファイルと名前が衝突するため（§6.2）、増分は 18 または 19 になる。

## 6. spec の記述と実測の食い違い・要注意事項

### 6.1 対象外 5 箇所のうち 1 箇所は呼び出しではない

`src/test_harness/tests.rs` の 5 箇所のうち、**L2 はモジュールドキュメントコメント内の記述**であり、
コード上の呼び出しは 4 箇所である。

```
src/test_harness/tests.rs:2://! (task 8.1, Requirements 8.1-8.5): proving `spawn_test_app()` boots a real,
src/test_harness/tests.rs:89:    let app = spawn_test_app().await;
src/test_harness/tests.rs:142:    let app_a = spawn_test_app().await;
src/test_harness/tests.rs:143:    let app_b = spawn_test_app().await;
src/test_harness/tests.rs:193:    let app = spawn_test_app().await;
```

**この食い違いは「対象 203」には影響しない。** 208 − 5 = 203 も、コード上の 207 − 4 = 203 も同じ値になる。
影響するのは要件 4.5 が求める「対象外 5 箇所の記載」だけであり、
`exceptions.md`（タスク 8.1）にはこの内訳を書き添える必要がある。
また要件 1.1 の完了判定（`git grep -o` の結果が「例外の件数 + 対象外 5」に一致すること）は、
**`git grep` 基準で 5 と数える**のが正しい。ドキュメントコメントの文言を編集するとこの値が動く点に注意する。

### 6.2 移設先ファイル名 `tests/timelines_endpoints_it.rs` は既に存在する

design.md「File Structure Plan」が `src/timelines/endpoints/tests.rs` の移設先として挙げる
`tests/timelines_endpoints_it.rs` は、**すでに 1341 行の既存統合テストとして存在する**（§7 の出力 F）。
HANDOFF §7 が「名前の重複は実装時に再確認すること」と指示していた事項に実際に該当する。
提案された 19 の移設先名のうち、既存 87 本と衝突するのはこの 1 件のみである。

さらに既存ファイルのモジュールドキュメントは、`src/timelines/endpoints/tests.rs` が
**手組みのテスト専用ルータ**に対して検証しているのに対し、自身は
`kawasemi::server::build_router` を通した実ルータで検証していると明記している。
これは要件 2.4（既存統合テストと重複させない）の判定が必要な箇所であり、
タスク 7.2 の実装者は移設先ファイル名の変更だけでなく、**検証内容の重複の有無**を先に判定すること。

### 6.3 `#[tokio::test]` だが `spawn_test_app` を呼ばないテストがある

`resolve_many_*` 系（`src/notifications/service/tests.rs` 3 本 / `src/search/hydrator/tests.rs` 3 本 /
`src/statuses/account_provider/tests.rs` 3 本）は `#[tokio::test]` でありながら `spawn_test_app` を呼ばない。
**分類規則どおり、これらは移設対象外として元位置に残す。** 非同期であることや DB を触ることは
分類の基準ではない（要件 1.2 が言う「実起動インスタンス」＝ `spawn_test_app` が返す `TestApp`）。

## 7. 計数の再現コマンド（要件 6.2 / 6.4）

すべて `/home/smoothpudding/Documents/dev/github/kawasemi` をカレントディレクトリとして
リビジョン `5ad15a5` で実行した。出力はそのまま貼っている。

### 出力 A — 呼び出し箇所の総数

```
$ git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' | wc -l
208
```

### 出力 B — ファイル別（行単位 `-c`）

```
$ git grep -c 'spawn_test_app(' -- 'src/**/tests.rs' | sort -t: -k2 -rn
src/notifications/endpoints/tests.rs:24
src/social_graph/endpoints/tests.rs:22
src/statuses/endpoints/tests.rs:21
src/notifications/service/tests.rs:17
src/accounts/account_service/tests.rs:16
src/statuses/render_assembler/tests.rs:13
src/search/service/tests.rs:11
src/search/hydrator/tests.rs:11
src/search/endpoint/tests.rs:10
src/social_graph/follow_request_service/tests.rs:8
src/federation/signatures/signer/tests.rs:8
src/timelines/endpoints/tests.rs:7
src/oauth/middleware/tests.rs:7
src/social_graph/tests.rs:6
src/federation/signatures/negotiation/tests.rs:6
src/test_harness/tests.rs:5
src/federation/endpoints/webfinger/tests.rs:5
src/federation/outbound/worker/tests.rs:4
src/statuses/account_provider/tests.rs:3
src/search/tests.rs:2
src/notifications/tests.rs:2
```

### 出力 C — ファイル別（出現単位 `-o`）

`-c` の値と全ファイルで一致することの確認。

```
$ git grep -o 'spawn_test_app(' -- 'src/**/tests.rs' | cut -d: -f1 | sort | uniq -c | sort -rn
     24 src/notifications/endpoints/tests.rs
     22 src/social_graph/endpoints/tests.rs
     21 src/statuses/endpoints/tests.rs
     17 src/notifications/service/tests.rs
     16 src/accounts/account_service/tests.rs
     13 src/statuses/render_assembler/tests.rs
     11 src/search/service/tests.rs
     11 src/search/hydrator/tests.rs
     10 src/search/endpoint/tests.rs
      8 src/social_graph/follow_request_service/tests.rs
      8 src/federation/signatures/signer/tests.rs
      7 src/timelines/endpoints/tests.rs
      7 src/oauth/middleware/tests.rs
      6 src/social_graph/tests.rs
      6 src/federation/signatures/negotiation/tests.rs
      5 src/test_harness/tests.rs
      5 src/federation/endpoints/webfinger/tests.rs
      4 src/federation/outbound/worker/tests.rs
      3 src/statuses/account_provider/tests.rs
      2 src/search/tests.rs
      2 src/notifications/tests.rs
```

### 出力 D — 対象外ファイルの呼び出し位置

```
$ git grep -n 'spawn_test_app(' -- 'src/test_harness/tests.rs'
src/test_harness/tests.rs:2://! (task 8.1, Requirements 8.1-8.5): proving `spawn_test_app()` boots a real,
src/test_harness/tests.rs:89:    let app = spawn_test_app().await;
src/test_harness/tests.rs:142:    let app_a = spawn_test_app().await;
src/test_harness/tests.rs:143:    let app_b = spawn_test_app().await;
src/test_harness/tests.rs:193:    let app = spawn_test_app().await;
```

### 出力 E — 統合テストバイナリ数

```
$ git ls-files 'tests/*_it.rs' | wc -l
87
```

### 出力 F — 移設先ファイル名の衝突確認

```
$ for n in accounts_account_service federation_webfinger_endpoint federation_outbound_worker \
      federation_signatures_negotiation federation_signatures_signer notifications_endpoints \
      notifications_service notifications_module oauth_middleware search_endpoint search_hydrator \
      search_service search_module social_graph_endpoints social_graph_follow_request_service \
      social_graph_module statuses_account_provider statuses_endpoints timelines_endpoints; do
    [ -e "tests/${n}_it.rs" ] && echo "COLLISION tests/${n}_it.rs"
  done
COLLISION tests/timelines_endpoints_it.rs

$ wc -l tests/timelines_endpoints_it.rs
1341 tests/timelines_endpoints_it.rs
```

### 出力 G — テスト関数総数（grep 基準）

```
$ git grep -o -E '#\[(tokio::)?test\]' -- 'src/**' | wc -l
1807

$ git grep -o -E '#\[(tokio::)?test\]' -- 'tests/**' | wc -l
524
```

### 出力 H — テスト関数総数（コメント・文字列をマスクした後）

`git grep` はドキュメントコメント内の `#[tokio::test]` という**記述**も数えてしまう。
下のスクリプトは行コメント・ブロックコメント・ドキュメントコメント・文字列リテラル・
raw 文字列・文字リテラルを空白に置換してから走査する。

```
$ python3 mask_count.py
src files 320 raw 1807 masked(code only) 1779
    ('src/federation/endpoints/document/tests.rs', 18, 17)
    ('src/federation/endpoints/nodeinfo/tests.rs', 4, 3)
    ('src/federation/endpoints/webfinger/tests.rs', 11, 10)
    ('src/federation/inbound/block_policy/tests.rs', 6, 5)
    ('src/federation/inbound/dispatcher/tests.rs', 6, 5)
    ('src/federation/inbound/service/tests.rs', 12, 11)
    ('src/media.rs', 1, 0)
    ('src/migrate/tests.rs', 5, 3)
    ('src/notifications/endpoints/tests.rs', 25, 24)
    ('src/notifications/generator/tests.rs', 6, 5)
    ('src/notifications/service/tests.rs', 21, 20)
    ('src/search/remote_resolver/tests.rs', 20, 18)
    ('src/social_graph/approval_policy/tests.rs', 8, 7)
    ('src/social_graph/relationship_mapper/tests.rs', 13, 12)
    ('src/state/tests.rs', 5, 3)
    ('src/statuses/visibility/tests.rs', 21, 19)
    ('src/test_harness.rs', 3, 0)
    ('src/test_harness/query_log.rs', 2, 0)
    ('src/test_harness/reaper.rs', 1, 0)
    ('src/test_harness/reaper/tests.rs', 4, 2)
tests files 87 raw 524 masked(code only) 507
    ('tests/actor_bootstrap_wiring_it.rs', 3, 2)
    ('tests/bootstrap_fail_fast_it.rs', 5, 1)
    ('tests/media_attachment_contract_it.rs', 4, 3)
    ('tests/notification_contract_it.rs', 12, 10)
    ('tests/notification_filter_it.rs', 6, 5)
    ('tests/notification_generation_it.rs', 8, 7)
    ('tests/notification_list_it.rs', 8, 7)
    ('tests/notification_show_dismiss_it.rs', 11, 10)
    ('tests/search_contract_it.rs', 6, 5)
    ('tests/status_contract_it.rs', 9, 8)
    ('tests/test_harness_lifecycle_it.rs', 5, 4)
    ('tests/test_isolation_and_shutdown_it.rs', 4, 2)
```

（各行は `(ファイル, grep 値, マスク後の値)`。差はすべてドキュメントコメント内の記述である。）

### 出力 I — テスト属性の綴りの網羅性確認

```
$ git grep -oh -E '#\[[a-z_:]*test[a-z_:]*[^]]*\]' -- 'src/**' | sort | uniq -c | sort -rn
    957 #[tokio::test]
    850 #[test]

$ git grep -oh -E '#\[[a-z_:]*test[a-z_:]*[^]]*\]' -- 'tests/**' | sort | uniq -c | sort -rn
    522 #[tokio::test]
      2 #[test]
```

957 + 850 = 1807 ✓（出力 G と一致） / 522 + 2 = 524 ✓

### 出力 J — 呼び出し箇所のテスト関数への帰属（§4 の表の生成に使用）

コメント・文字列を空白化したうえで、`#[test]` / `#[tokio::test]` の各出現から
直後の `fn NAME` と本体の対応波括弧範囲を求め、その範囲に含まれる `spawn_test_app(` の位置を
その関数に帰属させる。未帰属（`orphan`）の呼び出しは 0 件だった。属性の出現数と検出した関数数が
全 21 ファイルで一致することも同時に検証している（不一致は 0 ファイル）。

```
$ python3 attrib.py
src/notifications/endpoints/tests.rs: calls=24 fns=24 with=24 without=0 orphan=[]
src/social_graph/endpoints/tests.rs: calls=22 fns=26 with=22 without=4 orphan=[]
src/statuses/endpoints/tests.rs: calls=21 fns=28 with=21 without=7 orphan=[]
src/notifications/service/tests.rs: calls=17 fns=20 with=17 without=3 orphan=[]
src/accounts/account_service/tests.rs: calls=16 fns=16 with=16 without=0 orphan=[]
src/statuses/render_assembler/tests.rs: calls=13 fns=13 with=13 without=0 orphan=[]
src/search/hydrator/tests.rs: calls=11 fns=15 with=11 without=4 orphan=[]
src/search/service/tests.rs: calls=11 fns=11 with=11 without=0 orphan=[]
src/search/endpoint/tests.rs: calls=10 fns=25 with=10 without=15 orphan=[]
src/federation/signatures/signer/tests.rs: calls=8 fns=10 with=8 without=2 orphan=[]
src/social_graph/follow_request_service/tests.rs: calls=8 fns=8 with=8 without=0 orphan=[]
src/oauth/middleware/tests.rs: calls=7 fns=9 with=7 without=2 orphan=[]
src/timelines/endpoints/tests.rs: calls=7 fns=7 with=7 without=0 orphan=[]
src/federation/signatures/negotiation/tests.rs: calls=6 fns=8 with=6 without=2 orphan=[]
src/social_graph/tests.rs: calls=6 fns=6 with=6 without=0 orphan=[]
src/federation/endpoints/webfinger/tests.rs: calls=5 fns=10 with=5 without=5 orphan=[]
src/federation/outbound/worker/tests.rs: calls=4 fns=4 with=4 without=0 orphan=[]
src/test_harness/tests.rs: calls=4 fns=3 with=3 without=0 orphan=[]
src/statuses/account_provider/tests.rs: calls=3 fns=6 with=3 without=3 orphan=[]
src/notifications/tests.rs: calls=2 fns=2 with=2 without=0 orphan=[]
src/search/tests.rs: calls=2 fns=2 with=2 without=0 orphan=[]
TOTAL calls 207 | fns 253 | with 206 | calls_in_fns 207
```

この出力の `calls` はマスク後（コード上の呼び出し）である。合計 207 が `git grep` の 208 と
1 だけ違うのは `src/test_harness/tests.rs:2` のドキュメントコメントによる（§6.1）。
`fns 253` は 21 ファイル合計、`with 206` は `spawn_test_app` を呼ぶ関数の合計。
対象 20 ファイルに絞ると 253 − 3 = 250、206 − 3 = 203 になる。
