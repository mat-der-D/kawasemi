//! `endpoints` submodule (design.md File Structure Plan). This file is not
//! itself tied to a single task: it only declares each child module as the
//! task that implements it lands, and re-exports that task's public items.
//!
//! - Task 3.5 (`Boundary: ObjectDocumentProvider, OutboxSource`): the
//!   downstream-supply delegation boundary for local objects/collections
//!   (`ObjectDocumentRegistry`, first-registered-match-wins over
//!   `can_resolve`) and outbox contents (`OutboxSourceRegistry`, fan-out
//!   collection across all registered sources), each with a safe default
//!   response while nothing downstream is registered yet (Requirements 6.2,
//!   6.6, 8.1, 8.2, 8.3) — see [`document`].
//!
//! - Task 3.6 (`Boundary: ActivityPubDocumentBuilder`): builds the actor
//!   representation (id/inbox/outbox/public key, owner-free) and the outbox
//!   `OrderedCollectionPage` container, consuming task 3.5's registries above
//!   without extending them (Requirements 6.1, 6.2, 6.5, 8.1, 8.2, 8.3) —
//!   see [`document`].
//!
//! - Task 5.1 (`Boundary: webfinger, nodeinfo`): the WebFinger `acct:`
//!   resolution handler (owner-non-exposing, multi-actor, domain-matching,
//!   Requirements 4.1-4.5 — see [`webfinger`]) and the NodeInfo discovery +
//!   document handlers (minimal public stats, no internal information,
//!   Requirements 5.1-5.3 — see [`nodeinfo`]).
//!
//! - Task 5.2 (`Boundary: ap_get, outbox`): the ActivityPub GET handlers for
//!   local actors and non-actor local objects/collections (content
//!   negotiation, secure-mode authorized fetch, not-found — Requirements
//!   6.1-6.4, 6.6, 9.4 — see [`ap_get`]), and the outbox GET handler (paged
//!   `OrderedCollectionPage`, no authorized-fetch gate per design.md's own
//!   API Contract table — Requirements 8.1, 8.2, 9.4 — see [`outbox`]).
//!
//! Later sibling modules in this spec's `endpoints/` file plan (`inbox.rs`
//! — task 5.3) are out of this task's boundary and deliberately not
//! declared here yet; each is added by the task that actually implements
//! it.

pub mod ap_get;
pub mod document;
pub mod nodeinfo;
pub mod outbox;
pub mod webfinger;

pub use ap_get::{ApGetState, actor_get, object_get};
pub use document::{
    ActivityPubDocumentBuilder, ObjectDocumentProvider, ObjectDocumentRegistry, OutboxItemsPage,
    OutboxSource, OutboxSourceRegistry, PageCursor,
};
pub use nodeinfo::{NodeInfoState, nodeinfo_discovery, nodeinfo_document};
pub use outbox::{OutboxQuery, OutboxState, outbox_get};
pub use webfinger::{WebfingerQuery, WebfingerState, webfinger};
