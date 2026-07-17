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
//! - Task 4.1 (`Boundary: InboxService`): the receive-pipeline orchestrator
//!   composing the three modules above plus `JsonLdCodec` and
//!   `SignatureVerifier` — signature verification -> required-property
//!   validation -> block judgment -> deduplication -> dispatch, with a
//!   `process_local` entry point providing the same semantic path minus
//!   signature verification (Requirements 6.4, 7.1-7.4, 9.3, 12.1, 12.2 —
//!   see [`service`]).

pub mod block_policy;
pub mod dedup;
pub mod dispatcher;
pub mod service;

pub use block_policy::{BlockPolicy, LocalRecipientContext, NoopBlockPolicy};
pub use dedup::{
    DEFAULT_RECEIVED_ACTIVITY_RETENTION, DbReceivedActivityStore, ReceivedActivityStore,
};
pub use dispatcher::{
    HandleOutcome, InboundActivityDispatcher, InboundActivityHandler, InboundContext,
};
pub use service::{InboxOutcome, InboxService};
