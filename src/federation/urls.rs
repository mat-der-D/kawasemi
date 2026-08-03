//! `ActorUrls` (design.md "Serialization / 直列化層" -> "ActorUrls";
//! Requirements 6.1, 8.1; task 1.3, `Boundary: ActorUrls`): builds the
//! ActivityPub actor / inbox / outbox / shared-inbox / object / collection
//! URLs and the keyId URL, all from this instance's configured server
//! domain (`AppConfig.server.domain`, `crate::config::ServerConfig::domain`).
//!
//! Per design.md's ActorUrls Responsibilities: "アクター URL の構築・公開を
//! 本 spec が所有（actor-model は構築しない）" and "keyId は公開鍵取得可能な
//! URL（アクター URL + フラグメント等）として一貫構築（1.2 に供給）" —
//! federation-core, not actor-model, owns every URL an actor is addressed
//! by, and `key_id` is built as a deterministic function of `actor_url`
//! (a URL fragment), so `RequestSigner` (task 2.3) and `SignatureVerifier`
//! (task 2.4) always agree with `ActivityPubDocumentBuilder` (task 3.6) on
//! what an actor's keyId is.
//!
//! ## URL shape convention (this task fixes it; downstream tasks depend on it)
//! design.md's Endpoints table (`## Endpoints`) only names the placeholders
//! `{actor_url}` / `{inbox_url}` / `{outbox_url}` / `{shared_inbox_url}`; it
//! does not pin concrete path segments. This module fixes a conventional
//! ActivityPub/Mastodon-style shape:
//! - actor:        `https://{domain}/users/{handle}`
//! - inbox:         `{actor_url}/inbox`
//! - outbox:        `{actor_url}/outbox`
//! - shared inbox:  `https://{domain}/inbox` (one instance-wide endpoint,
//!   deliberately *not* handle-scoped — Requirement 7's shared-inbox
//!   dedup semantics only make sense if every local actor resolves to the
//!   same shared inbox URL)
//! - keyId:         `{actor_url}#main-key` (the conventional ActivityPub/
//!   `http-signature` fragment identifier; any consumer that dereferences
//!   this URL for the public key strips the fragment and gets the actor
//!   document back, matching "公開鍵取得可能な URL" above)
//! - object/collection: `https://{domain}/{kind}/{id}`, where `kind` is the
//!   caller-supplied path segment ([`ObjectKind`])
//!
//! Later tasks (`ActivityPubDocumentBuilder` at 3.6, `ObjectDocumentProvider`
//! at 3.5, the `ap_get`/`inbox`/`outbox` endpoint handlers) must register
//! their routes to match these exact paths.
//!
//! ## `ObjectKind`: deliberately minimal, not an exhaustive enum
//! design.md's Service Interface names an `ObjectKind` parameter for
//! `object_url` but does not enumerate concrete kinds anywhere in this
//! spec yet -- concrete object/collection kinds only become necessary at
//! `ObjectDocumentProvider` (task 3.5) and `ActivityPubDocumentBuilder`
//! (task 3.6), neither of which is in this task's boundary. Rather than
//! guessing at a fixed variant set this task has no requirement driving,
//! `ObjectKind` is a thin newtype wrapping the URL path segment a caller
//! supplies (e.g. `"statuses"`, `"collections/followers"`) -- extensible by
//! construction at any later call site, never by editing this type again.

#[cfg(test)]
mod tests;

use crate::actor::Handle;
use crate::domain::Id;

/// The URL path segment identifying an object or collection kind, for
/// [`ActorUrls::object_url`]. See this module's doc comment ("`ObjectKind`:
/// deliberately minimal") for why this is a newtype rather than an
/// enumerated set of variants at this stage of the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectKind(&'static str);

impl ObjectKind {
    /// Builds an `ObjectKind` naming `path_segment` (e.g. `"statuses"`), used
    /// verbatim as the URL path segment between the domain and the
    /// object/collection id in [`ActorUrls::object_url`].
    pub const fn new(path_segment: &'static str) -> Self {
        ObjectKind(path_segment)
    }
}

/// Builds every ActivityPub-addressable URL for this instance's local
/// actors -- actor, inbox, outbox, shared inbox, keyId, and
/// object/collection URLs -- from a single configured server domain
/// (Requirements 6.1, 8.1). See this module's doc comment for the exact URL
/// shape convention.
///
/// Constructed directly from the domain string, not the whole `AppConfig`
/// (mirrors `crate::actor::keys::cipher::ChaCha20Poly1305KeyCipher::new`'s
/// established precedent of a service taking only the one config value it
/// actually needs): callers pass `app_config.server.domain.clone()` (or any
/// owned `String`/`&str`), not `&AppConfig` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorUrls {
    domain: String,
}

impl ActorUrls {
    /// Builds an `ActorUrls` for `domain` (a bare domain, e.g.
    /// `"example.com"`, no scheme -- matching
    /// `crate::config::ServerConfig::domain`'s already-validated shape;
    /// this constructor does not re-validate it).
    pub fn new(domain: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
        }
    }

    /// The ActivityPub actor URL identifying `handle`'s local actor
    /// (Requirement 6.1): `https://{domain}/users/{handle}`.
    pub fn actor_url(&self, handle: &Handle) -> String {
        format!("https://{}/users/{}", self.domain, handle.as_str())
    }

    /// `handle`'s per-actor inbox URL (Requirement 7): `{actor_url}/inbox`.
    pub fn inbox_url(&self, handle: &Handle) -> String {
        format!("{}/inbox", self.actor_url(handle))
    }

    /// This instance's single shared inbox URL (Requirement 7's shared-inbox
    /// delivery-dedup semantics): `https://{domain}/inbox`. Deliberately not
    /// a function of any [`Handle`] -- see this module's doc comment for
    /// why it must be domain-level, not per-actor.
    pub fn shared_inbox_url(&self) -> String {
        format!("https://{}/inbox", self.domain)
    }

    /// `handle`'s outbox URL (Requirement 8.1): `{actor_url}/outbox`.
    pub fn outbox_url(&self, handle: &Handle) -> String {
        format!("{}/outbox", self.actor_url(handle))
    }

    /// `handle`'s HTTP Signatures `keyId` (Requirement 6.1; design.md:
    /// "公開鍵取得可能な URL（アクター URL + フラグメント等）として一貫構築"):
    /// `{actor_url}#main-key`. Deterministic in `actor_url`, so
    /// `RequestSigner` (task 2.3) and `SignatureVerifier` (task 2.4) always
    /// agree with the actor document `ActivityPubDocumentBuilder` (task 3.6)
    /// serves at `actor_url` on what this actor's keyId is.
    pub fn key_id(&self, handle: &Handle) -> String {
        format!("{}#main-key", self.actor_url(handle))
    }

    /// The URL for a local object or collection of `kind` identified by
    /// `id` (Requirements 6.1, 8.1's object/collection URLs):
    /// `https://{domain}/{kind}/{id}`.
    pub fn object_url(&self, kind: ObjectKind, id: Id) -> String {
        format!("https://{}/{}/{}", self.domain, kind.0, id.as_i64())
    }
}
