//! `inbound` submodule (design.md File Structure Plan). This file is not
//! itself tied to a single task: it only declares each child module as the
//! task that implements it lands, and re-exports that task's public items.
//!
//! - Task 3.1 (`Boundary: ReceivedActivityStore`): the inbound Activity-id
//!   idempotency ledger (dedup + retention pruning), Requirement 7.4 — see
//!   [`dedup`].
//!
//! Later tasks in this spec's `inbound/` file plan (`dispatcher.rs`,
//! `block_policy.rs` — task 3.2; `service.rs` — task 4.1) are out of this
//! task's boundary and deliberately not declared here yet; each is added by
//! the task that actually implements it.

pub mod dedup;

pub use dedup::{
    DEFAULT_RECEIVED_ACTIVITY_RETENTION, DbReceivedActivityStore, ReceivedActivityStore,
};
