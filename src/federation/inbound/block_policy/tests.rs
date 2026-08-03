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

// --- BlockPolicyRegistry (task 5.2, `_Boundary: SocialGraphModule_`) ------

#[tokio::test]
async fn registry_defaults_to_noop_when_nothing_registered() {
    let registry = BlockPolicyRegistry::new();

    let is_blocked = registry
        .is_blocked(
            SIGNER_ACTOR_URI,
            LocalRecipientContext::Actor {
                actor_uri: "https://kawasemi.example/actors/local-owner".to_string(),
            },
        )
        .await
        .expect("a freshly built registry must never fail");

    assert!(
        !is_blocked,
        "a freshly built registry must default to NoopBlockPolicy"
    );
}

struct FixedPolicy(bool);

impl BlockPolicy for FixedPolicy {
    async fn is_blocked(
        &self,
        _actor_uri: &str,
        _local_recipient: LocalRecipientContext,
    ) -> Result<bool, AppError> {
        Ok(self.0)
    }
}

#[tokio::test]
async fn registry_uses_a_registered_policy_instead_of_the_default() {
    let registry = BlockPolicyRegistry::new();
    registry.set_policy(FixedPolicy(true));

    let is_blocked = registry
        .is_blocked(
            SIGNER_ACTOR_URI,
            LocalRecipientContext::Actor {
                actor_uri: "https://kawasemi.example/actors/local-owner".to_string(),
            },
        )
        .await
        .expect("a registered policy must not fail here");

    assert!(
        is_blocked,
        "the registered policy's own verdict must win over the default"
    );
}

#[tokio::test]
async fn registry_clone_shares_the_same_slot() {
    let registry = BlockPolicyRegistry::new();
    let clone = registry.clone();
    registry.set_policy(FixedPolicy(true));

    // Registering on `registry` must be visible through `clone` too --
    // proves the two share one underlying slot, not independent copies.
    let is_blocked = clone
        .is_blocked(
            SIGNER_ACTOR_URI,
            LocalRecipientContext::Actor {
                actor_uri: "https://kawasemi.example/actors/local-owner".to_string(),
            },
        )
        .await
        .expect("a registered policy must not fail here");

    assert!(
        is_blocked,
        "clone must observe the same registration as the original registry"
    );
}
