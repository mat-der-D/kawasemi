//! `outbound` submodule (design.md File Structure Plan). This file is not
//! itself tied to a single task: it only declares each child module as the
//! task that implements it lands, and re-exports that task's public items.
//!
//! - Task 3.3 (`Boundary: DeliveryQueue`): persists outbound delivery jobs
//!   (`delivery_jobs`, `migrations/0004_federation.sql`), lets a caller
//!   exclusively claim due jobs, and offers the done/reschedule/
//!   permanently-failed state transitions the (later) delivery worker
//!   drives, plus the exponential-backoff delay calculation the worker will
//!   use to compute `reschedule`'s `next_attempt_at` (Requirements 11.1,
//!   11.2, 11.3, 11.5) — see [`queue`].
//!
//! Later tasks in this spec's `outbound/` file plan (`delivery.rs` — task
//! 4.2, `target.rs` — task 3.4, `sink.rs` — task 4.2, `worker.rs` — task
//! 4.3) are out of this task's boundary and deliberately not declared here
//! yet; each is added by the task that actually implements it.

pub mod queue;

pub use queue::{
    DEFAULT_DELIVERY_BASE_DELAY, DEFAULT_DELIVERY_MAX_DELAY, DEFAULT_MAX_DELIVERY_ATTEMPTS,
    DbDeliveryQueue, DeliveryJob, DeliveryJobStatus, DeliveryQueue, NewDeliveryJob, backoff_delay,
};
