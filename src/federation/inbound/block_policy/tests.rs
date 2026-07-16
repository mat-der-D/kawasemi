//! Unit tests for `NoopBlockPolicy` (Requirements 12.1, 12.2, 12.3), per
//! task 3.2's observable completion condition: "既定ブロックポリシーがアク
//! ター宛・shared inbox 宛いずれの宛先コンテキストでも常に非ブロックを返す".
//!
//! Pure in-memory logic — no DB, no HTTP; plain `#[tokio::test]` unit tests.

use super::*;

const SIGNER_ACTOR_URI: &str = "https://remote.example/actors/mallory";

// --- 5: default is never-blocked for Actor context ---

#[tokio::test]
async fn noop_block_policy_never_blocks_for_actor_recipient_context() {
    let policy = NoopBlockPolicy;

    let is_blocked = policy
        .is_blocked(
            SIGNER_ACTOR_URI,
            LocalRecipientContext::Actor {
                actor_uri: "https://kawasemi.example/actors/local-owner".to_string(),
            },
        )
        .await
        .expect("the default BlockPolicy must never fail");

    assert!(
        !is_blocked,
        "the default BlockPolicy (12.3) must always report non-blocked, \
         even for a known destination-local-actor context"
    );
}

// --- 6: default is never-blocked for SharedInbox context ---

#[tokio::test]
async fn noop_block_policy_never_blocks_for_shared_inbox_context() {
    let policy = NoopBlockPolicy;

    let is_blocked = policy
        .is_blocked(SIGNER_ACTOR_URI, LocalRecipientContext::SharedInbox)
        .await
        .expect("the default BlockPolicy must never fail");

    assert!(
        !is_blocked,
        "the default BlockPolicy (12.3) must always report non-blocked for shared inbox too \
         (never bulk-reject before destination resolution)"
    );
}
