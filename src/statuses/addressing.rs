//! `Addressing` (design.md "Visibility / 可視性層" -> `#### VisibilityPolicy
//! / Addressing`, design.md lines ~451-481; Requirements 4.1, 4.2; task 3.2,
//! `Boundary: Addressing`): the single derivation from a post's visibility
//! to its ActivityPub `to`/`cc` addressing and, from there, to a concrete
//! delivery [`Recipient`] set — the same derivation local origination and
//! remote delivery both funnel through (Requirement 4.2's "単一の addressing
//! ロジック").
//!
//! ## Scope: `Addressing`/`derive_addressing`/`derive_recipients` only
//! design.md bundles `VisibilityPolicy` and `Addressing` under one heading,
//! but task 3.1 (`visibility.rs`) already split off `is_visible`/
//! `ViewerRelation`/`RelationshipQuery`/`NoRelationshipQuery`. This module
//! owns exactly the remainder: [`Addressing`], [`derive_addressing`], and
//! [`derive_recipients`]. It imports [`RelationshipQuery`]/[`ViewerRelation`]
//! from `crate::statuses::visibility` only insofar as its doc comments refer
//! to them (the `followers`/`mentions` slices this module's functions accept
//! are the *already-resolved* output of `RelationshipQuery::followers_of` /
//! a caller's mention lookup — this module never calls
//! [`RelationshipQuery`] itself, matching design.md's "本層は意味論のみ...
//! recipient を確定して...渡す").
//!
//! ## `ActorRef`: not literally defined in design.md, defined here
//! design.md's own `derive_addressing`/`derive_recipients` signatures name a
//! `mentions: &[ActorRef]` parameter but never spell out `ActorRef`'s
//! fields anywhere in the document — the same situation
//! `crate::federation::outbound::target`'s own module doc documents for
//! `Recipient` ("not literally defined in design.md... This module defines
//! it here"). This module is the natural owner (the sole consumer of
//! mentions within this task's boundary), so it defines [`ActorRef`] here
//! rather than inventing a second, parallel `Recipient`-shaped type: each
//! [`ActorRef`] pairs the mentioned actor's ActivityPub URI (placed
//! verbatim into `to`/`cc`, per Requirement 4.2's individual "メンション宛
//! 先") with the already-known [`Recipient`] it resolves to for delivery
//! (reusing `crate::federation::Recipient` — the same canonical delivery-
//! destination type task 3.1 imports for `RelationshipQuery::followers_of`,
//! per that module's own "reuses the existing `Recipient` type" doc
//! comment — rather than redefining a second one here). Building an
//! `ActorRef` (resolving a mention to its URI + `Recipient`) is a caller
//! responsibility (a later task, e.g. `StatusService::create_status`'s
//! mention extraction, per Requirement 3.6) outside this task's boundary.
//!
//! ## `PUBLIC_COLLECTION_URI`: the literal ActivityStreams public collection
//! No canonical helper for the ActivityPub "public" collection IRI
//! (`https://www.w3.org/ns/activitystreams#Public`) exists yet anywhere in
//! `src/federation/` (`JsonLdCodec`/`ActorUrls` do not build or export one —
//! federation-core has never needed it, since building `Create`/`Announce`
//! addressing is *this* spec's job, not federation-core's). This module
//! therefore defines the literal here as [`PUBLIC_COLLECTION_URI`], the
//! standard, well-known ActivityPub/Mastodon IRI (not a project-specific
//! invention) that a later task (`StatusActivityBuilder`, task 4.1) can
//! import from here rather than each duplicating the literal string.
//!
//! ## `to`/`cc` placement (Requirements 4.1, 4.2; ActivityPub/Mastodon
//! convention)
//! Mirrors Mastodon's own `ActivityPub::TagManager#to`/`#cc` convention
//! (which this task's own instructions point at as the real-world
//! convention to follow):
//! - `public`: `to = [public collection]`, `cc = [followers collection,
//!   ...mentions]`.
//! - `unlisted`: `to = [followers collection, ...mentions]`, `cc = [public
//!   collection]` — the `public`/`unlisted` "public collection の配置差"
//!   design.md's prose describes.
//! - `private`: `to = [followers collection, ...mentions]`, `cc = []` — the
//!   followers collection, never the public collection, appears anywhere.
//! - `direct`: `to = [...mentions]` only, `cc = []` — no collection
//!   reference (public or followers) appears anywhere, only individual
//!   mentioned-actor URIs.
//!
//! In every non-`direct` case mentions ride alongside the followers
//! collection in whichever of `to`/`cc` the followers collection itself
//! occupies (mirroring Mastodon's own placement) rather than always landing
//! in a fixed slot — `direct` is the sole case where mentions occupy `to`
//! without any collection.
//!
//! ## Why [`Addressing`] carries a private `addresses_followers` flag
//! design.md's own [`Addressing`] struct sketch is exactly `{ to: Vec<String>,
//! cc: Vec<String> }`, and [`derive_recipients`]'s own sketched signature
//! takes only `&Addressing` (not `&Status`/`Visibility`) alongside
//! `mentions`/`followers` — so, taken completely literally,
//! [`derive_recipients`] would have no way to tell whether a given
//! [`Addressing`] value came from a visibility that addresses the followers
//! collection at all (`public`/`unlisted`/`private`) or one that never does
//! (`direct`), short of re-parsing `to`/`cc` string contents against a
//! `followers_uri` it is never even passed. Rather than reintroduce
//! `Visibility` (or `followers_uri`) as an extra [`derive_recipients`]
//! parameter — a literal signature change design.md does not sketch — this
//! module keeps design.md's two `pub` fields exactly as specified and adds
//! one additional private field on [`Addressing`] itself, populated
//! deterministically by [`derive_addressing`] and consumed only by
//! [`derive_recipients`] in the same module: the minimal extra bit of state
//! actually needed to satisfy the completion criterion "direct はメンショ
//! ン限定になり...private/unlisted のフォロワー配送先が空集合になる" without
//! fragile string matching against collection URIs.
//!
//! ## Determinism (Invariant, design.md ~line 481)
//! Every collection here is a `Vec`, built by appending in a fixed order
//! (collection reference(s) first, then `mentions` in the caller's given
//! order) — never a `HashSet`/`HashMap` iteration — so the same
//! `(status.visibility, mentions, followers_uri)` / `(addressing, mentions,
//! followers)` input deterministically reproduces the same `Addressing` /
//! recipient `Vec` on every call, local origination or remote delivery
//! alike (Requirement 4.2, design.md's "同一入力...に対しローカル発生・リ
//! モート配送で同一 Addressing/recipient を返す").

#[cfg(test)]
mod tests;

use crate::domain::Visibility;
use crate::federation::Recipient;
use crate::statuses::model::Status;

/// The standard ActivityStreams/ActivityPub "public" collection IRI. See
/// this module's doc comment ("`PUBLIC_COLLECTION_URI`") for why it is
/// defined here rather than imported from `crate::federation`.
pub const PUBLIC_COLLECTION_URI: &str = "https://www.w3.org/ns/activitystreams#Public";

/// A mentioned actor to address (design.md's `mentions: &[ActorRef]`
/// parameter — see this module's doc comment, "`ActorRef`: not literally
/// defined in design.md", for why this module defines it). `uri` is placed
/// verbatim into `to`/`cc` by [`derive_addressing`]; `recipient` is the
/// same actor already resolved to a concrete delivery destination, reused
/// by [`derive_recipients`] rather than re-resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorRef {
    pub uri: String,
    pub recipient: Recipient,
}

/// A post's derived ActivityPub `to`/`cc` addressing (design.md's exact
/// `Addressing` struct — `to`/`cc` are its only `pub` fields, matching
/// design.md verbatim). See this module's doc comment ("Why `Addressing`
/// carries a private `addresses_followers` flag") for the one additional,
/// non-`pub` field this module adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addressing {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    addresses_followers: bool,
}

/// Derives `status`'s `to`/`cc` addressing from its visibility alone
/// (design.md's exact `derive_addressing` interface; Requirements 4.1, 4.2).
/// `mentions` are the post's already-resolved mentioned actors (Requirement
/// 3.6's extraction, threaded in by a later task's caller); `followers_uri`
/// is the post author's followers-collection URI (e.g. built by a later
/// task via `ActorUrls`, out of this task's boundary).
///
/// See this module's doc comment ("`to`/`cc` placement") for the exact
/// per-visibility shape this function produces.
pub fn derive_addressing(
    status: &Status,
    mentions: &[ActorRef],
    followers_uri: &str,
) -> Addressing {
    let mention_uris = || mentions.iter().map(|m| m.uri.clone());

    match status.visibility {
        Visibility::Public => {
            let mut cc = vec![followers_uri.to_string()];
            cc.extend(mention_uris());
            Addressing {
                to: vec![PUBLIC_COLLECTION_URI.to_string()],
                cc,
                addresses_followers: true,
            }
        }
        Visibility::Unlisted => {
            let mut to = vec![followers_uri.to_string()];
            to.extend(mention_uris());
            Addressing {
                to,
                cc: vec![PUBLIC_COLLECTION_URI.to_string()],
                addresses_followers: true,
            }
        }
        Visibility::Private => {
            let mut to = vec![followers_uri.to_string()];
            to.extend(mention_uris());
            Addressing {
                to,
                cc: Vec::new(),
                addresses_followers: true,
            }
        }
        Visibility::Direct => Addressing {
            to: mention_uris().collect(),
            cc: Vec::new(),
            addresses_followers: false,
        },
    }
}

/// Resolves `addressing` (produced by [`derive_addressing`] for the same
/// post) into a concrete delivery [`Recipient`] set for
/// `DeliveryService`/`StatusActivityBuilder` (design.md's exact
/// `derive_recipients` interface; Requirements 4.1, 4.2). `mentions`'
/// already-known [`Recipient`]s are always included; `followers` (the
/// already-resolved output of `RelationshipQuery::followers_of`) is
/// included only when `addressing` came from a visibility that addresses
/// the followers collection at all (`public`/`unlisted`/`private` — never
/// `direct`, see this module's doc comment, "Why `Addressing` carries a
/// private `addresses_followers` flag").
///
/// With the default [`crate::statuses::visibility::NoRelationshipQuery`]
/// (`followers_of` always empty), `followers` is `&[]` here too, so
/// `private`/`unlisted` posts contribute zero followers-derived recipients
/// — the completion criterion "既定 RelationshipQuery（空フォロワー）では
/// private/unlisted のフォロワー配送先が空集合になる" — while still
/// including any mentions.
pub fn derive_recipients(
    addressing: &Addressing,
    mentions: &[ActorRef],
    followers: &[Recipient],
) -> Vec<Recipient> {
    let mut recipients = Vec::with_capacity(
        mentions.len()
            + if addressing.addresses_followers {
                followers.len()
            } else {
                0
            },
    );
    if addressing.addresses_followers {
        recipients.extend(followers.iter().cloned());
    }
    recipients.extend(mentions.iter().map(|m| m.recipient.clone()));
    recipients
}
