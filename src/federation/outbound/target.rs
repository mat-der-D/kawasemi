//! `RecipientTargetResolver` (design.md `#### RecipientTargetResolver` ->
//! Service Interface; Requirements 10.3, 10.4, 11.4; task 3.4, `Boundary:
//! RecipientTargetResolver`): classifies each business-supplied
//! [`Recipient`] into a physical [`DeliveryTarget`] (local in-process vs.
//! remote HTTP) and collapses remote recipients that share the same shared
//! inbox into a single delivery target (Requirement 11.4: "同一 Activity が
//! 同一の共有 inbox を持つ複数のリモート宛先へ配送されるとき...その共有
//! inbox への送信を重複させない").
//!
//! ## `Recipient`: not literally defined in design.md
//! design.md's Service Interface only shows `resolve`'s parameter as
//! `&[Recipient]` and `DeliveryRequest.recipients: Vec<Recipient>`; it never
//! spells out `Recipient`'s own fields anywhere in the document. This
//! module defines it here (the sole component whose boundary is "recipient
//! -> physical target", so it is the natural owner of the type recipients
//! are expressed in). The shape is driven directly by two established
//! constraints elsewhere in this spec/codebase:
//! - [`crate::actor::Handle`] validates only the local-actor charset
//!   (ASCII alphanumeric/underscore) and cannot represent a remote actor's
//!   IRI at all, so a local recipient must be carried as a `Handle`, not a
//!   generic URL/string.
//! - This spec explicitly does not own remote-actor profile storage or
//!   inbox discovery (requirements.md's Boundary Context: "リモートアクター
//!   の完全なプロフィール永続化...は accounts-and-instance"; this spec only
//!   caches public-key material for signature verification). A remote
//!   recipient's individual inbox — and shared inbox, if the sender's
//!   remote actor advertises one — must therefore already be known to
//!   whatever caller builds a `Recipient` (a future spec, e.g.
//!   statuses-core/social-graph, resolving addressing/visibility): this
//!   resolver never fetches or discovers either URL itself.
//!
//! This yields exactly two variants: [`Recipient::Local`] (a `Handle`) and
//! [`Recipient::Remote`] (an already-known `inbox`, plus an already-known
//! `shared_inbox` when the caller has one).
//!
//! ## Dedup rule
//! [`RecipientTargetResolver::resolve`] computes, for every
//! [`Recipient::Remote`], an *effective* address: `shared_inbox` when
//! present, otherwise the recipient's own `inbox`. Two remote recipients
//! that land on the same effective address collapse into exactly one
//! [`DeliveryTarget::Remote`] in the returned `Vec` — this covers both the
//! literal Requirement 11.4 case (same `shared_inbox`) and the general case
//! design.md's own wording implies (two `Remote` recipients with no shared
//! inbox but an identical individual `inbox` must not be delivered to
//! twice either): the final `Vec<DeliveryTarget>` never contains two
//! `Remote` entries with the same `inbox` string. Order is otherwise
//! preserved (first occurrence wins the position in the output).
//!
//! Note this module deliberately does not attempt to dedup
//! [`Recipient::Local`] entries that happen to name the same `Handle`
//! twice: addressing/visibility logic (deciding *which* recipients belong
//! in the list at all) is explicitly out of this spec's boundary
//! (design.md's `DeliveryService` Responsibilities: "共通部に意味論判定
//! （可視性・addressing）は持たず、呼び出し側が確定した recipient を受け
//! 取る") — a caller that hands this resolver the same local `Handle`
//! twice is a caller bug outside what this component can or should paper
//! over.
//!
//! ## Local handle that no longer resolves: whole call fails
//! A [`Recipient::Local`] wrapping a `Handle` that does not currently
//! resolve to an existing local actor via [`LocalActorLookup::resolve_actor_by_handle`]
//! (e.g. the actor was deleted between the caller deciding to address it
//! and this resolver running) fails the *entire* [`RecipientTargetResolver::resolve`]
//! call with a `404`-shaped [`AppError`], rather than silently skipping just
//! that one recipient. Silently dropping a recipient here would make a
//! caller's request look like it fully succeeded when a portion of the
//! addressed audience was quietly never delivered to at all — for a single-
//! owner server where every local delivery is addressed to a specific,
//! caller-chosen actor, that is a worse failure mode than surfacing the
//! inconsistency loudly so the caller (and its own tests) can see it and
//! decide how to handle a stale handle. This is bucketed as a `Client`
//! (4xx) [`AppError`], matching this spec's general convention of rejecting
//! structurally-impossible-to-satisfy requests rather than pretending they
//! partially succeeded.
//!
//! ## `ActorUrls`: named as a dependency in design.md's components table,
//! not actually needed here
//! design.md's Components table lists `ActorUrls (P0)` alongside
//! `ActorDirectory` as this component's dependencies. In an actual
//! implementation there turned out to be no genuine use for it:
//! [`DeliveryTarget::Local`] only needs the already-validated `Handle`
//! itself (in-process delivery never builds a URL for a local recipient),
//! and a [`Recipient::Remote`]'s inbox/shared-inbox URLs are already
//! supplied by the caller, never derived from this instance's own domain.
//! This spec has existing precedent for a design.md dependency-table entry
//! not exactly matching a component's literal implementation (task 2.2's
//! Implementation Note documents the reverse mismatch, a missing entry);
//! this is the same kind of harmless table/implementation drift, just in
//! the opposite direction, and is called out here rather than manufacturing
//! an artificial `ActorUrls` call with no real purpose.
//!
//! ## `LocalActorLookup`: a narrow mockable port over `ActorDirectory`
//! [`crate::actor::ActorDirectory`] is a concrete struct backed directly by
//! a `PgPool` (no trait of its own) — but this component must be a pure
//! in-memory/logic component with no database access at all (this task's
//! own testing strategy: "no Postgres/DB needed"). [`LocalActorLookup`] is
//! therefore defined here as the narrow single-method port this resolver
//! actually needs (mirroring this crate's established pattern of a
//! `#[allow(async_fn_in_trait)] pub trait ... : Send + Sync` boundary
//! around exactly the operations a component depends on, e.g.
//! `FederationHttpClient`, `BlockPolicy`, `DeliveryQueue`), implemented for
//! real [`crate::actor::ActorDirectory`] values directly so production
//! callers pass one in unchanged, while tests substitute a plain in-memory
//! [`MockLocalActorLookup`] (this module's own test module) with no
//! database involved.

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use axum::http::StatusCode;

use crate::actor::{ActorDirectory, Handle, ResolvedActor};
use crate::error::AppError;

/// A business-supplied delivery recipient, classified into either a local
/// actor (by its already-validated [`Handle`]) or a remote actor (by an
/// already-known individual inbox URL, and an already-known shared inbox
/// URL if the caller has one). See this module's doc comment ("`Recipient`:
/// not literally defined in design.md") for why this exact shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    /// A local actor recipient, addressed by handle.
    Local(Handle),
    /// A remote actor recipient, addressed by an already-known inbox URL,
    /// plus an already-known shared inbox URL when the caller has one.
    Remote {
        inbox: String,
        shared_inbox: Option<String>,
    },
}

/// The physical delivery target a [`Recipient`] resolves to (design.md's
/// exact `DeliveryTarget` interface): either in-process local delivery (by
/// `Handle`) or a remote HTTP delivery to a single `inbox` URL (which may
/// already be a shared inbox URL, once [`RecipientTargetResolver::resolve`]
/// has deduplicated it — see this module's doc comment, "Dedup rule").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryTarget {
    Local { handle: Handle },
    Remote { inbox: String },
}

/// The narrow local-actor-existence port [`RecipientTargetResolver`]
/// depends on. See this module's doc comment ("`LocalActorLookup`: a
/// narrow mockable port over `ActorDirectory`") for why this exists rather
/// than depending on [`ActorDirectory`] directly.
#[allow(async_fn_in_trait)]
pub trait LocalActorLookup: Send + Sync {
    /// Returns `Ok(Some(_))` when `handle` currently resolves to an
    /// existing local actor, `Ok(None)` when it does not (mirroring
    /// [`ActorDirectory::resolve_actor_by_handle`]'s own "no error for
    /// absence" contract).
    async fn resolve_actor_by_handle(
        &self,
        handle: &Handle,
    ) -> Result<Option<ResolvedActor>, AppError>;
}

impl LocalActorLookup for ActorDirectory {
    async fn resolve_actor_by_handle(
        &self,
        handle: &Handle,
    ) -> Result<Option<ResolvedActor>, AppError> {
        ActorDirectory::resolve_actor_by_handle(self, handle).await
    }
}

/// Classifies [`Recipient`]s into physical [`DeliveryTarget`]s and
/// deduplicates remote shared-inbox destinations (Requirements 10.3, 10.4,
/// 11.4). See this module's doc comment for the exact dedup rule and the
/// local-handle-not-found failure contract.
pub struct RecipientTargetResolver<D: LocalActorLookup> {
    directory: D,
}

impl<D: LocalActorLookup> RecipientTargetResolver<D> {
    /// Builds a resolver backed by `directory` (a real
    /// [`crate::actor::ActorDirectory`] in production, a
    /// [`LocalActorLookup`] test double in tests).
    pub fn new(directory: D) -> Self {
        Self { directory }
    }

    /// Resolves every entry in `recipients` to a physical [`DeliveryTarget`],
    /// classifying local vs. remote (Requirements 10.3, 10.4) and
    /// collapsing remote recipients that share an effective inbox address
    /// into a single target (Requirement 11.4). Returns a `404`-shaped
    /// [`AppError`] if any [`Recipient::Local`] handle no longer resolves to
    /// an existing local actor — see this module's doc comment ("Local
    /// handle that no longer resolves: whole call fails").
    pub async fn resolve(&self, recipients: &[Recipient]) -> Result<Vec<DeliveryTarget>, AppError> {
        let mut targets = Vec::with_capacity(recipients.len());
        let mut seen_remote_addresses: HashSet<String> = HashSet::new();

        for recipient in recipients {
            match recipient {
                Recipient::Local(handle) => {
                    let resolved = self.directory.resolve_actor_by_handle(handle).await?;
                    if resolved.is_none() {
                        return Err(AppError::client(
                            StatusCode::NOT_FOUND,
                            format!(
                                "recipient handle {:?} does not resolve to an existing local actor",
                                handle.as_str()
                            ),
                        ));
                    }
                    targets.push(DeliveryTarget::Local {
                        handle: handle.clone(),
                    });
                }
                Recipient::Remote {
                    inbox,
                    shared_inbox,
                } => {
                    let effective_address = shared_inbox.as_deref().unwrap_or(inbox.as_str());
                    if seen_remote_addresses.insert(effective_address.to_string()) {
                        targets.push(DeliveryTarget::Remote {
                            inbox: effective_address.to_string(),
                        });
                    }
                }
            }
        }

        Ok(targets)
    }
}
