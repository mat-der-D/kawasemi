//! `FollowApprovalPolicy` (design.md "Social Graph Domain / ドメイン層" ->
//! `#### FollowApprovalPolicy`; Requirements 3.1, 3.2, 3.3, 3.4; task 2.1,
//! `Boundary: FollowApprovalPolicy`): the single judgment point for whether
//! a follow requires manual approval, and the *sole* place the "same-server
//! approval skip" admin privilege (Requirement 3's "同一サーバー承認スキッ
//! プ（管理者特権の単一定義）") is defined.
//!
//! ## Scope
//! This module owns exactly design.md's `FollowApprovalPolicy` Service
//! Interface: [`FollowDecision`] and [`FollowApprovalPolicy::requires_approval`].
//! No `Transitions` (state-transition execution), no `FollowService`/
//! `InboundHandler` (callers of this policy), and no persistence live here —
//! those consume this module's decision but are out of scope for task 2.1
//! (`Boundary: FollowApprovalPolicy`).
//!
//! ## The same-server judgment is made *only* inside this method
//! [`FollowApprovalPolicy::requires_approval`] takes the raw `source`/
//! `target` [`AccountRef`] (core-runtime's canonical `Local(Id)`/`Remote(Id)`
//! primitive, imported from `crate::domain`, never redefined here) and the
//! destination's raw lock state (`target_locked`). It derives "are both
//! parties local (same server)?" itself, by matching the `AccountRef`
//! variants directly — callers (the follow service's API path and the
//! inbound handler's federation-receive path) never precompute or pass in a
//! same-server boolean; they pass only the two raw account references and
//! the lock flag. This is the structural guarantee Requirement 3.3 asks for:
//! since there is exactly one place in the entire follow-processing path
//! that inspects `AccountRef` variants for this purpose, the API path and
//! the inbound path cannot drift apart on the same-server admin privilege
//! (e.g. one path accidentally granting it to a remote party).
//!
//! ## Decision table (Requirements 3.1, 3.4)
//! - `source` local AND `target` local (same server) -> always
//!   [`FollowDecision::Establish`], regardless of `target_locked` (3.1): this
//!   is the one and only expression of the admin privilege.
//! - Otherwise (either side [`AccountRef::Remote`]) -> normal policy applies:
//!   `target_locked == true` -> [`FollowDecision::RequireApproval`],
//!   `target_locked == false` -> [`FollowDecision::Establish`] (3.4).
//!
//! This method is a pure, synchronous, infallible judgment (no I/O, no
//! `Result`): it never fails, so it returns `FollowDecision` directly, not
//! `Result<FollowDecision, AppError>`.

#[cfg(test)]
mod tests;

use crate::domain::AccountRef;

/// The outcome of [`FollowApprovalPolicy::requires_approval`]: whether a
/// follow should be established immediately, or must wait as a pending
/// follow request until the destination approves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowDecision {
    /// Establish the follow relationship immediately (no pending follow
    /// request) — either because the destination is not locked, or because
    /// the same-server admin privilege applies (Requirement 3.1).
    Establish,
    /// Do not establish the follow yet; record it as a pending follow
    /// request awaiting the destination's authorize/reject (Requirement
    /// 2.1's normal locked-account flow).
    RequireApproval,
}

/// The single judgment point for follow-approval necessity (design.md's
/// exact `FollowApprovalPolicy`). See this module's doc comment for why the
/// same-server check must only ever be performed inside
/// [`Self::requires_approval`].
#[derive(Debug, Clone, Copy, Default)]
pub struct FollowApprovalPolicy;

impl FollowApprovalPolicy {
    /// Judges whether a follow from `source` to `target` requires manual
    /// approval, given `target`'s raw lock state (`target_locked`).
    ///
    /// Same-server (both-local) detection happens *only* here, by matching
    /// `source`/`target`'s [`AccountRef`] variants directly — see this
    /// module's doc comment. Callers must never precompute this and must
    /// never pass anything other than the raw `source`/`target` references
    /// and `target`'s raw lock flag.
    ///
    /// Postconditions (Requirements 3.1, 3.4):
    /// - `source` and `target` both [`AccountRef::Local`] -> always
    ///   [`FollowDecision::Establish`], regardless of `target_locked`.
    /// - Otherwise -> [`FollowDecision::RequireApproval`] iff `target_locked`
    ///   is `true`, else [`FollowDecision::Establish`].
    pub fn requires_approval(
        &self,
        source: &AccountRef,
        target: &AccountRef,
        target_locked: bool,
    ) -> FollowDecision {
        let same_server = matches!(
            (source, target),
            (AccountRef::Local(_), AccountRef::Local(_))
        );

        if same_server {
            // Requirement 3.1, 3.3: the sole expression of the same-server
            // admin privilege. Locked or not is irrelevant here.
            return FollowDecision::Establish;
        }

        // Requirement 3.4: at least one side is remote -> normal policy.
        if target_locked {
            FollowDecision::RequireApproval
        } else {
            FollowDecision::Establish
        }
    }
}
