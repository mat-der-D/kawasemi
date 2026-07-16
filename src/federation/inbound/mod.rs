//! `inbound` submodule (design.md File Structure Plan). This file is not
//! itself tied to a single task: it only declares each child module as the
//! task that implements it lands, and re-exports that task's public items.
//!
//! - Task 3.1 (`Boundary: ReceivedActivityStore`): the inbound Activity-id
//!   idempotency ledger (dedup + retention pruning), Requirement 7.4 — see
//!   [`dedup`].
//! - Task 3.2 (`Boundary: InboundActivityDispatcher, BlockPolicy`): the
//!   outer-Activity-type -> handler multimap dispatch registry (fan-out,
//!   Requirements 7.3, 7.5, 7.6 — see [`dispatcher`]) and the
//!   destination-aware block-judgment delegation boundary with its
//!   always-non-blocked default (Requirements 12.1, 12.2, 12.3 — see
//!   [`block_policy`]).
//!
//! Later tasks in this spec's `inbound/` file plan (`service.rs` — task 4.1)
//! are out of this task's boundary and deliberately not declared here yet;
//! each is added by the task that actually implements it.

pub mod block_policy;
pub mod dedup;
pub mod dispatcher;

pub use block_policy::{BlockPolicy, LocalRecipientContext, NoopBlockPolicy};
pub use dedup::{
    DEFAULT_RECEIVED_ACTIVITY_RETENTION, DbReceivedActivityStore, ReceivedActivityStore,
};
pub use dispatcher::{
    HandleOutcome, InboundActivityDispatcher, InboundActivityHandler, InboundContext,
};
