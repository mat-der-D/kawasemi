//! Unit tests for `FollowApprovalPolicy` (Requirements 3.1, 3.2, 3.3, 3.4),
//! per task 2.1's observable completion condition: "両ローカル同一サーバー
//! はロック済みでも確立、片側リモートのロック済みは承認必要となり、同一
//! サーバー判定が本判定の内部でのみ行われる（呼び出し側は判定ロジックを
//! 持たない）ことを単体テストで確認できる状態".
//!
//! Pure in-memory logic — no DB, no HTTP, no async; plain `#[test]` unit
//! tests. Every call below passes only the raw `source`/`target`
//! `AccountRef` and the raw `target_locked` bool — exactly what a caller
//! (`FollowService`/`InboundHandler`) is allowed to pass per this module's
//! doc comment; no test precomputes a same-server boolean and hands it in,
//! because the Service Interface has no such parameter to accept one.

use super::*;
use crate::domain::Id;

// --- 3.1: both-local (same-server) always establishes, locked or not ---

#[test]
fn both_local_and_target_locked_still_establishes() {
    let policy = FollowApprovalPolicy;
    let source = AccountRef::Local(Id::from_i64(1));
    let target = AccountRef::Local(Id::from_i64(2));

    let decision = policy.requires_approval(&source, &target, true);

    assert_eq!(
        decision,
        FollowDecision::Establish,
        "same-server (both-local) follows must establish immediately even \
         when the destination is locked (Requirement 3.1) — this is the \
         sole expression of the admin privilege"
    );
}

#[test]
fn both_local_and_target_unlocked_establishes() {
    let policy = FollowApprovalPolicy;
    let source = AccountRef::Local(Id::from_i64(3));
    let target = AccountRef::Local(Id::from_i64(4));

    let decision = policy.requires_approval(&source, &target, false);

    assert_eq!(decision, FollowDecision::Establish);
}

// --- 3.4: either side remote -> normal policy applies ---

#[test]
fn source_remote_target_local_and_locked_requires_approval() {
    let policy = FollowApprovalPolicy;
    let source = AccountRef::Remote(Id::from_i64(10));
    let target = AccountRef::Local(Id::from_i64(11));

    let decision = policy.requires_approval(&source, &target, true);

    assert_eq!(
        decision,
        FollowDecision::RequireApproval,
        "a remote source following a locked local target must not receive \
         the same-server privilege (Requirement 3.4) and must require \
         approval like the normal flow"
    );
}

#[test]
fn source_local_target_remote_and_locked_requires_approval() {
    // The design.md excerpt frames the privilege as "both local", so a
    // remote *target* being reported as locked (e.g. by a federated
    // provenance signal) must also fall through to normal policy — either
    // side remote disqualifies the privilege (Requirement 3.4).
    let policy = FollowApprovalPolicy;
    let source = AccountRef::Local(Id::from_i64(12));
    let target = AccountRef::Remote(Id::from_i64(13));

    let decision = policy.requires_approval(&source, &target, true);

    assert_eq!(decision, FollowDecision::RequireApproval);
}

#[test]
fn both_remote_and_locked_requires_approval() {
    let policy = FollowApprovalPolicy;
    let source = AccountRef::Remote(Id::from_i64(14));
    let target = AccountRef::Remote(Id::from_i64(15));

    let decision = policy.requires_approval(&source, &target, true);

    assert_eq!(decision, FollowDecision::RequireApproval);
}

#[test]
fn either_side_remote_and_unlocked_establishes() {
    let policy = FollowApprovalPolicy;
    let source = AccountRef::Remote(Id::from_i64(16));
    let target = AccountRef::Local(Id::from_i64(17));

    let decision = policy.requires_approval(&source, &target, false);

    assert_eq!(
        decision,
        FollowDecision::Establish,
        "normal policy: an unlocked destination establishes regardless of \
         locality (Requirement 3.4)"
    );
}

// --- 3.3: same-server judgment happens only inside requires_approval;
// this is exercised structurally above (every call site here passes only
// raw AccountRef + target_locked, never a precomputed same-server bool —
// there is no such parameter in the Service Interface to pass one to). The
// following test pins the decision-table symmetry regardless of *which*
// side (source vs target) is local, since the internal check inspects both.

#[test]
fn same_server_decision_is_symmetric_in_which_party_is_examined() {
    let policy = FollowApprovalPolicy;
    let a = AccountRef::Local(Id::from_i64(20));
    let b = AccountRef::Local(Id::from_i64(21));

    // Both directions of a purely-local pair establish under lock.
    assert_eq!(
        policy.requires_approval(&a, &b, true),
        FollowDecision::Establish
    );
    assert_eq!(
        policy.requires_approval(&b, &a, true),
        FollowDecision::Establish
    );
}
