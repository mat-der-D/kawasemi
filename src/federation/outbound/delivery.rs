//! `DeliveryService` (design.md `#### DeliveryService（共通部）` -> Service
//! Interface; Requirements 10.1, 10.2, 10.3, 10.4, 10.5; task 4.2, `Boundary:
//! DeliveryService`): the delivery entry point downstream business logic
//! calls into. Performs the common part — canonical Activity generation and
//! validation, and recipient-to-target resolution — exactly once per
//! [`DeliveryService::deliver`] call, strictly before branching to whichever
//! physical [`super::sink::DeliverySink`] each resolved target requires.
//!
//! ## The "one canonical Activity, one resolution" invariant (Requirements
//! 10.1, 10.2)
//! [`DeliveryService::deliver`] contains exactly one call to
//! [`crate::federation::jsonld::serialize`] +
//! [`crate::federation::jsonld::parse_activity`] (producing the single
//! [`CanonicalActivity`] this call's every target observes) and exactly one
//! call to [`super::target::RecipientTargetResolver::resolve`] — both above
//! and outside the subsequent per-target loop, matching design.md's own
//! sequence diagram ("配送（意味論対称・物理配送のみ分岐）") line for line:
//! `Delivery->>Codec: serialize and validate canonical activity` then
//! `Delivery->>Target: resolve recipients to targets` then `loop per
//! target`. This is a structural property of the code (a single call site,
//! not inside a loop), not something that merely happens to hold today: any
//! future change moving either call inside the loop would have to
//! deliberately restructure this method, and this module's own tests assert
//! observable consequences of the invariant (a malformed `req.activity`
//! fails before any sink is ever invoked; a `LocalActorLookup` call count
//! that scales with distinct local recipients, not with per-target
//! dispatch).
//!
//! ## Per-target failure handling: fail-fast, not partial-success
//! The per-target loop propagates the first [`super::sink::DeliverySink::dispatch`]
//! failure immediately via `?`, rather than collecting per-target results
//! and continuing. design.md does not specify partial-failure semantics for
//! `deliver()` (its Service Interface returns a single `Result<(),
//! AppError>`, not a per-target report), and this spec's established
//! precedent for a structurally-impossible-to-fully-satisfy request is to
//! fail loudly rather than silently succeed for only part of the addressed
//! audience (mirrors `RecipientTargetResolver::resolve`'s own "whole call
//! fails" contract for an unresolvable local recipient, `target.rs`). A
//! caller that needs finer-grained partial-delivery reporting is expected to
//! build it on top of this contract (e.g. one `deliver()` call per
//! recipient), not something this common-part orchestrator owns.
//!
//! ## Generic composition, not `Arc<dyn ..>`
//! [`DeliveryService`] is generic over `D: LocalActorLookup` (threaded into
//! its owned [`super::target::RecipientTargetResolver`]) and `L`/`H:
//! DeliverySink` (its local/HTTP sinks), mirroring this crate's established
//! pattern for composing non-`dyn`-safe, literal-`async fn` traits (e.g.
//! `InboxService<V, B, D>`, `inbound/service.rs`) via generics rather than
//! trait objects.

#[cfg(test)]
mod tests;

use crate::actor::Handle;
use crate::error::AppError;
use crate::federation::jsonld::{parse_activity, serialize};

use super::sink::{CanonicalActivity, DeliverySink};
use super::target::{DeliveryTarget, LocalActorLookup, Recipient, RecipientTargetResolver};

/// A caller's request to deliver `activity` to `recipients`, sent as
/// `sender` (design.md's exact `DeliveryRequest`). `activity` is the raw,
/// not-yet-canonicalized JSON document a downstream business-logic caller
/// has built (this common part stamps `@context` and validates required
/// properties exactly once — see this module's doc comment).
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryRequest {
    pub activity: serde_json::Value,
    pub sender: Handle,
    pub recipients: Vec<Recipient>,
}

/// The delivery common part plus physical-delivery branch point (design.md's
/// exact `DeliveryService` component; Requirements 10.1-10.5). See this
/// module's doc comment for the "one canonical Activity, one resolution"
/// structural invariant and the fail-fast per-target error contract.
pub struct DeliveryService<D, L, H>
where
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
{
    target_resolver: RecipientTargetResolver<D>,
    local_sink: L,
    http_sink: H,
}

impl<D, L, H> DeliveryService<D, L, H>
where
    D: LocalActorLookup,
    L: DeliverySink,
    H: DeliverySink,
{
    /// Builds a service composing `target_resolver` (recipient -> physical
    /// target classification and shared-inbox dedup, task 3.4),
    /// `local_sink` (in-process delivery, Requirement 10.3), and `http_sink`
    /// (queue-backed delivery, Requirement 10.4).
    pub fn new(target_resolver: RecipientTargetResolver<D>, local_sink: L, http_sink: H) -> Self {
        Self {
            target_resolver,
            local_sink,
            http_sink,
        }
    }

    /// Delivers `req.activity` to every recipient in `req.recipients`,
    /// generating and validating the canonical Activity and resolving
    /// targets exactly once (Requirements 10.1, 10.2), then branching only
    /// on the resulting physical [`DeliveryTarget`] (Requirements 10.3,
    /// 10.4). Every target observes the identical [`CanonicalActivity`]
    /// value by shared reference (Requirement 10.5) — see this module's and
    /// `sink.rs`'s doc comments for why this is structural, not incidental.
    pub async fn deliver(&self, req: DeliveryRequest) -> Result<(), AppError> {
        // Common part, executed exactly once regardless of recipient count
        // or local/remote mix (Requirements 10.1, 10.2): stamp `@context`
        // and validate required properties, producing the one
        // `CanonicalActivity` every target below shares by reference.
        let serialized = serialize(&req.activity)?;
        let parsed = parse_activity(&serialized)?;
        let canonical = CanonicalActivity::from_parsed(parsed);

        let targets = self.target_resolver.resolve(&req.recipients).await?;

        // Branch only on the physical delivery mechanism (Requirements
        // 10.3, 10.4) -- both arms receive `&canonical`, never a
        // re-derived copy.
        for target in targets {
            match target {
                DeliveryTarget::Local { .. } => {
                    self.local_sink
                        .dispatch(target, &canonical, &req.sender)
                        .await?;
                }
                DeliveryTarget::Remote { .. } => {
                    self.http_sink
                        .dispatch(target, &canonical, &req.sender)
                        .await?;
                }
            }
        }

        Ok(())
    }
}
