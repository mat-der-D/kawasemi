//! Federation domain module (federation-core spec).
//!
//! Scope so far:
//! - Task 1.2 (`Boundary: JsonLdCodec`): ActivityPub JSON-LD serialization
//!   (`@context` stamping), safe parsing (unknown-property-tolerant,
//!   required-property validation), and ActivityPub media-type judgment for
//!   content negotiation — see [`jsonld`].
//! - Task 1.3 (`Boundary: ActorUrls`): builds the ActivityPub actor / inbox
//!   / outbox / shared-inbox / object / collection URLs and the keyId URL
//!   from this instance's configured server domain — see [`urls`].
//! - Task 1.4 (`Boundary: FederationHttpClient, Digest`): the outbound
//!   network boundary (mockable send/fetch port, production `reqwest`
//!   implementation, deterministic mock implementation) and SHA-256 body
//!   digest computation/mismatch detection — see [`signatures`].
//! - Task 2.1 (`Boundary: PublicKeyResolver`): resolves a `keyId` to
//!   cached/fetched public-key material (`remote_public_keys`), with
//!   cache-first/force/TTL-staleness semantics — see [`signatures`].
//! - Task 2.2 (`Boundary: RequestSigner`): attaches an HTTP Signature
//!   (draft-cavage or RFC 9421) to an outbound request using a local
//!   actor's currently valid signing key — see [`signatures`].
//! - Task 2.3 (`Boundary: SignatureVerifier`): verifies a received HTTP
//!   Signature end-to-end (format detection, signing-input reconstruction,
//!   public-key resolution with invalidate-and-retry, RSA-SHA256 check),
//!   returning the verified signer's identity — see [`signatures`].
//! - Task 2.4 (`Boundary: SignatureNegotiator`): double-knocks a signed
//!   outbound request against a host of unknown signature-format support,
//!   remembering the successful format per host in
//!   `instance_signature_capabilities` — see [`signatures`].
//! - Task 3.1 (`Boundary: ReceivedActivityStore`): records each inbound
//!   Activity's own `id` in `received_activities` and reports new-vs-known
//!   so business-logic dispatch never runs twice for the same Activity
//!   (Requirement 7.4), with periodic pruning of rows past the configured
//!   retention window — see [`inbound`].
//! - Task 3.2 (`Boundary: InboundActivityDispatcher, BlockPolicy`): the
//!   outer-Activity-type -> `InboundActivityHandler` multimap dispatch
//!   registry, fanning out to every handler registered for a type and
//!   safely no-oping unregistered types (Requirements 7.3, 7.5, 7.6), plus
//!   the destination-aware `BlockPolicy` delegation boundary whose default
//!   (`NoopBlockPolicy`) always answers non-blocked for both per-actor and
//!   shared-inbox destination contexts (Requirements 12.1, 12.2, 12.3) —
//!   see [`inbound`].
//! - Task 3.3 (`Boundary: DeliveryQueue`): persists outbound delivery jobs
//!   (`delivery_jobs`), lets a caller exclusively claim due jobs, and
//!   offers the done/reschedule/permanently-failed state transitions the
//!   (later) delivery worker drives, plus the exponential-backoff delay
//!   calculation the worker will use to compute `reschedule`'s
//!   `next_attempt_at` (Requirements 11.1, 11.2, 11.3, 11.5) — see
//!   [`outbound`].
//! - Task 3.4 (`Boundary: RecipientTargetResolver`): classifies each
//!   business-supplied `Recipient` (a local actor by `Handle`, or a remote
//!   actor by already-known inbox/shared-inbox URL) into a physical
//!   `DeliveryTarget`, deduplicating remote recipients that share an
//!   effective shared-inbox address into one delivery target (Requirements
//!   10.3, 10.4, 11.4) — see [`outbound`].
//! - Task 3.5 (`Boundary: ObjectDocumentProvider, OutboxSource`): the
//!   downstream-supply delegation boundary for local objects/collections
//!   (`ObjectDocumentRegistry`, ordered first-match-wins over
//!   `can_resolve`) and outbox contents (`OutboxSourceRegistry`, fan-out
//!   collection across all registered sources), each safely defaulting
//!   (`None` / empty page) while nothing downstream is registered yet
//!   (Requirements 6.2, 6.6, 8.1, 8.2, 8.3) — see [`endpoints`].
//! - Task 4.1 (`Boundary: InboxService`): the receive-pipeline orchestrator
//!   (signature verification -> required-property validation -> block
//!   judgment -> deduplication -> dispatch), with a `process_local` entry
//!   point providing the same semantic path minus signature verification so
//!   in-process and HTTP-received Activities converge on identical
//!   business-processing state (Requirements 6.4, 7.1-7.4, 9.3, 12.1, 12.2)
//!   — see [`inbound`].
//! - Task 4.2 (`Boundary: DeliveryService, DeliverySink`): the delivery
//!   common part (canonical Activity generation/validation, recipient
//!   resolution) run exactly once per `deliver()` call, branching only on
//!   physical delivery mechanism to an in-process `InboxService::process_local`
//!   hand-off (no queue) or a `DeliveryQueue::enqueue` call, so a single
//!   call's local and remote targets provably share one canonical Activity
//!   (Requirements 10.1-10.5, 11.1) — see [`outbound`].
//! - Task 5.1 (`Boundary: webfinger, nodeinfo`): the WebFinger `acct:`
//!   resolution handler — self-domain matching, owner-non-exposing
//!   multi-actor resolution via `ActorDirectory::resolve_actor_by_handle`,
//!   JRD `self`-link response (Requirements 4.1-4.5) — and the NodeInfo
//!   discovery + minimal-public-stats document handlers (software
//!   name/version/ActivityPub protocol only, no internal information,
//!   Requirements 5.1-5.3) — see [`endpoints`]. Not yet mounted on the live
//!   router (task 5.4's job); see `endpoints::webfinger`/`endpoints::nodeinfo`'s
//!   own doc comments.
//! - Task 5.2 (`Boundary: ap_get, outbox`): the ActivityPub GET handlers —
//!   local actor documents built via `ActivityPubDocumentBuilder`, every
//!   other local object/collection URL delegated to `ObjectDocumentRegistry`
//!   (`None` -> not-found), content-negotiated (Requirement 9.4/6.3),
//!   secure-mode authorized fetch reusing `SignatureVerifier` directly
//!   (Requirement 6.4) — and the outbox GET handler, a paged
//!   `OrderedCollectionPage` sourced from `OutboxSourceRegistry` with no
//!   authorized-fetch gate (design.md's own API Contract table scopes
//!   `401(secure)` to the actor/object rows only, not outbox) (Requirements
//!   6.1-6.4, 6.6, 8.1, 8.2, 9.4) — see [`endpoints`]. Not yet mounted on
//!   the live router (task 5.4's job); see `endpoints::ap_get`/
//!   `endpoints::outbox`'s own doc comments.
//! - Task 5.3 (`Boundary: inbox`): the per-actor inbox and shared-inbox POST
//!   handlers — connects a signed Activity to `InboxService::process_inbound`
//!   with the endpoint-derived `LocalRecipientContext` (per-actor `Actor`
//!   built from the matched `{handle}` segment, or domain-wide
//!   `SharedInbox`), mapping both non-rejecting `InboxOutcome` variants to
//!   `202 Accepted` and letting every rejection (signature failure,
//!   malformed body, blocked signer) surface as `AppError`'s own status
//!   (Requirements 7.1, 7.2) — see [`endpoints`]. Not yet mounted on the
//!   live router (task 5.4's job); see `endpoints::inbox`'s own doc
//!   comment.
//!
//! Later tasks in this spec (`config` — see design.md's File Structure
//! Plan) are out of this task's boundary and deliberately not declared here
//! yet; each is added by the task that actually implements it.

pub mod endpoints;
pub mod inbound;
pub mod jsonld;
pub mod outbound;
pub mod signatures;
pub mod urls;

pub use endpoints::{
    ActivityPubDocumentBuilder, ApGetState, InboxState, NodeInfoState, ObjectDocumentProvider,
    ObjectDocumentRegistry, OutboxItemsPage, OutboxQuery, OutboxSource, OutboxSourceRegistry,
    OutboxState, PageCursor, WebfingerQuery, WebfingerState, actor_get, actor_inbox,
    nodeinfo_discovery, nodeinfo_document, object_get, outbox_get, shared_inbox, webfinger,
};
pub use inbound::{
    BlockPolicy, DEFAULT_RECEIVED_ACTIVITY_RETENTION, DbReceivedActivityStore, HandleOutcome,
    InboundActivityDispatcher, InboundActivityHandler, InboundContext, InboxOutcome, InboxService,
    LocalRecipientContext, NoopBlockPolicy, ReceivedActivityStore,
};
pub use jsonld::{ParsedActivity, accepts_activitypub, parse_activity, serialize};
pub use outbound::{
    CanonicalActivity, DEFAULT_DELIVERY_BASE_DELAY, DEFAULT_DELIVERY_MAX_DELAY,
    DEFAULT_MAX_DELIVERY_ATTEMPTS, DbDeliveryQueue, DeliveryJob, DeliveryJobStatus, DeliveryQueue,
    DeliveryRequest, DeliveryService, DeliverySink, DeliveryTarget, HttpDeliverySink,
    LocalActorLookup, LocalDeliverySink, NewDeliveryJob, Recipient, RecipientTargetResolver,
    backoff_delay,
};
pub use signatures::{
    DEFAULT_PUBLIC_KEY_CACHE_TTL, DEFAULT_SIGNATURE_MAX_AGE, DbFederationPublicKeyResolver, Digest,
    FederationHttpClient, HttpResponse, HttpSignatureVerifier, IncomingRequest,
    MockFederationHttpClient, OutboundRequest, PublicKeyResolver, RemotePublicKey, RequestSigner,
    ReqwestFederationHttpClient, SignatureNegotiator, SignatureVerifier, VerifiedSigner,
};
pub use urls::{ActorUrls, ObjectKind};
