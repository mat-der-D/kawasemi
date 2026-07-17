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
//! - Task 3.4 (`Boundary: RecipientTargetResolver`): classifies each
//!   business-supplied `Recipient` into a physical `DeliveryTarget` (local
//!   in-process vs. remote HTTP) and collapses remote recipients sharing an
//!   effective shared-inbox address into a single delivery target
//!   (Requirements 10.3, 10.4, 11.4) — see [`target`].
//! - Task 4.2 (`Boundary: DeliveryService, DeliverySink`): runs the delivery
//!   common part (canonical Activity generation/validation, recipient
//!   resolution) exactly once per `deliver()` call, then branches only on
//!   the resulting physical target to either an in-process hand-off to
//!   `InboxService::process_local` (no queue) or a `DeliveryQueue::enqueue`
//!   call, so a single call's local and remote targets provably observe the
//!   identical canonical Activity (Requirements 10.1-10.5, 11.1) — see
//!   [`delivery`] (the common part) and [`sink`] (the branch point).
//!
//! Later tasks in this spec's `outbound/` file plan (`worker.rs` — task 4.3)
//! are out of this task's boundary and deliberately not declared here yet;
//! each is added by the task that actually implements it.

pub mod delivery;
pub mod queue;
pub mod sink;
pub mod target;

pub use delivery::{DeliveryRequest, DeliveryService};
pub use queue::{
    DEFAULT_DELIVERY_BASE_DELAY, DEFAULT_DELIVERY_MAX_DELAY, DEFAULT_MAX_DELIVERY_ATTEMPTS,
    DbDeliveryQueue, DeliveryJob, DeliveryJobStatus, DeliveryQueue, NewDeliveryJob, backoff_delay,
};
pub use sink::{CanonicalActivity, DeliverySink, HttpDeliverySink, LocalDeliverySink};
pub use target::{DeliveryTarget, LocalActorLookup, Recipient, RecipientTargetResolver};
