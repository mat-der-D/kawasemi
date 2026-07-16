//! `InboundActivityDispatcher` (design.md `#### InboundActivityDispatcher
//! （委譲境界）` -> Service Interface; Requirements 7.3, 7.5, 7.6; task 3.2,
//! `Boundary: InboundActivityDispatcher`): the outer-Activity-`type` ->
//! `InboundActivityHandler` multimap registry this spec delegates all
//! Activity-specific business semantics to (Requirement 7.5: "各 Activity
//! 種別固有の意味論を本 spec 内に実装せず、登録された業務処理へ委譲する").
//!
//! ## Multimap, not one-handler-per-type
//! ActivityPub wrapping types such as `Undo` (and `Create`/`Delete`) can
//! carry inner objects owned by entirely different downstream specs — e.g.
//! design.md's own example: `Undo` wrapping `Announce`/`Like` belongs to
//! statuses-core, while `Undo` wrapping `Follow`/`Block` belongs to
//! social-graph. A single-handler-per-type registry would let whichever
//! handler registers second silently replace the first, dropping the
//! earlier owner's Activities. [`InboundActivityDispatcher::register`] is
//! therefore an additive multimap insert (Requirement 7.6): registering a
//! second handler for an outer type that already has one keeps both.
//!
//! ## Fan-out and exactly-one-or-zero semantics
//! [`InboundActivityDispatcher::dispatch`] invokes every handler registered
//! for the Activity's outer `activity_type`. Each handler inspects the
//! Activity's inner `object` itself and returns [`HandleOutcome::Ignored`]
//! when it does not own that inner type, or [`HandleOutcome::Handled`] when
//! it acted. In an honest federation configuration at most one fanned-out
//! handler should ever report `Handled` for a given Activity (0 is a safe
//! "received only, no owner" outcome; 2+ is a misconfiguration — two specs
//! both claiming the same inner object). This dispatcher does not treat 2+
//! `Handled` as fatal (a single non-fatal `tracing::warn!` is emitted
//! instead, since federation must keep working even under host
//! misconfiguration); it still returns `Ok(())`.
//!
//! ## Unregistered outer types are a safe no-op
//! An outer `activity_type` with no registered handler at all (no downstream
//! spec cares about it) is not an error — `dispatch` simply returns
//! `Ok(())` without invoking anything, matching design.md's "未登録外側種別
//! は安全に無視（受領のみ）".
//!
//! ## Idempotency is the caller's responsibility, not this module's
//! design.md notes dispatch must happen "重複排除（`ReceivedActivityStore`）
//!通過後に一度だけ". This module trusts its caller (task 4.1's
//! `InboxService`) to have already deduplicated via `ReceivedActivityStore`
//! before calling [`InboundActivityDispatcher::dispatch`]; it does not
//! itself re-check activity ids, which is out of this task's boundary
//! (`_Boundary: InboundActivityDispatcher, BlockPolicy_`, not `InboxService`).

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::AppError;
use crate::federation::jsonld::ParsedActivity;
use crate::federation::signatures::VerifiedSigner;

/// Everything a registered [`InboundActivityHandler`] needs about the
/// request beyond the parsed Activity itself (design.md's exact
/// `InboundContext` interface): the already-verified signer identity that
/// signed the inbound HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundContext {
    pub signer: VerifiedSigner,
}

/// Whether a handler actually acted on the given Activity (design.md's exact
/// `HandleOutcome` interface). See this module's doc comment ("Fan-out and
/// exactly-one-or-zero semantics") for the full contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleOutcome {
    Handled,
    Ignored,
}

/// A downstream business-logic handler for one or more outer Activity types
/// (design.md's exact `InboundActivityHandler` interface). This spec never
/// implements Activity-specific semantics itself (Requirement 7.5) — it only
/// defines this trait and fans out to whatever implementations downstream
/// specs register.
///
/// ## Why `handle` returns a boxed future instead of a literal `async fn`
/// design.md's Service Interface prints `handle` as a plain `async fn` in
/// the trait. Unlike this crate's other design.md-pinned `async fn` traits
/// (`ReceivedActivityStore`, `PublicKeyResolver`, `SignatureVerifier`), which
/// are all consumed via a generic type parameter (never `dyn`) so
/// `#[allow(async_fn_in_trait)]` alone suffices, [`InboundActivityDispatcher`]
/// must hold a *heterogeneous* collection of handler implementations
/// together in one `HashMap<String, Vec<_>>` (design.md's own `register`
/// signature takes `Arc<dyn InboundActivityHandler>` for exactly this
/// reason) — that requires actual dynamic dispatch, and a trait with a
/// literal `async fn` method is not dyn-compatible in Rust (native
/// return-position-`impl Trait`-in-traits is not object-safe either). This
/// method's signature is therefore written in the equivalent
/// `Pin<Box<dyn Future<...> + Send + '_>>` desugared form — the same
/// transformation the `async-trait` crate's macro performs automatically —
/// so `Arc<dyn InboundActivityHandler>` is a valid, dyn-dispatchable type,
/// without adding that (or any) new dependency to this crate. Callers of
/// `handle` still simply `.await` it; only implementors need to write
/// `Box::pin(async move { .. })` in the body (see this module's test stub
/// handler for the pattern).
pub trait InboundActivityHandler: Send + Sync {
    /// The outer Activity type(s) (e.g. `["Undo"]`) this handler wants to
    /// receive. Multiple handlers may name the same type (Requirement 7.6).
    fn activity_types(&self) -> &[&str];

    /// Inspects `activity`'s inner object and acts on it if (and only if)
    /// this handler owns that inner object's type, returning
    /// [`HandleOutcome::Handled`]; otherwise returns [`HandleOutcome::Ignored`]
    /// without side effects. See this trait's doc comment for why this is
    /// written as a boxed-future-returning method rather than `async fn`.
    fn handle<'a>(
        &'a self,
        activity: &'a ParsedActivity,
        ctx: &'a InboundContext,
    ) -> Pin<Box<dyn Future<Output = Result<HandleOutcome, AppError>> + Send + 'a>>;
}

/// The outer-Activity-`type` -> `InboundActivityHandler` multimap registry
/// (design.md's exact `InboundActivityDispatcher` Service Interface). See
/// this module's doc comment for the full multimap/fan-out/no-op contract.
#[derive(Default)]
pub struct InboundActivityDispatcher {
    handlers: HashMap<String, Vec<Arc<dyn InboundActivityHandler>>>,
}

impl InboundActivityDispatcher {
    /// Builds an empty dispatcher (no outer types registered yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `handler` for every outer type it names via
    /// `handler.activity_types()`. Additive multimap insert: registering a
    /// second handler for an outer type that already has one does NOT
    /// overwrite the first — both are kept and both are fanned out to on
    /// `dispatch` (Requirement 7.6, see this module's doc comment).
    pub fn register(&mut self, handler: Arc<dyn InboundActivityHandler>) {
        for activity_type in handler.activity_types() {
            self.handlers
                .entry((*activity_type).to_string())
                .or_default()
                .push(Arc::clone(&handler));
        }
    }

    /// Fans out `activity` to every handler registered for
    /// `activity.activity_type` (Requirement 7.3: 検証済み Activity を
    /// ディスパッチ境界へ受け渡す). An outer type with no registered
    /// handlers is a safe no-op (received only, never an error — see this
    /// module's doc comment, "Unregistered outer types are a safe no-op").
    /// If two or more handlers report [`HandleOutcome::Handled`] for the
    /// same Activity, logs a `tracing::warn!` (a constitutional
    /// double-ownership violation this dispatcher still tolerates rather
    /// than treats as fatal) but still returns `Ok(())`.
    pub async fn dispatch(
        &self,
        activity: &ParsedActivity,
        ctx: &InboundContext,
    ) -> Result<(), AppError> {
        let Some(handlers) = self.handlers.get(&activity.activity_type) else {
            return Ok(());
        };

        let mut handled_count = 0usize;
        for handler in handlers {
            if let HandleOutcome::Handled = handler.handle(activity, ctx).await? {
                handled_count += 1;
            }
        }

        if handled_count > 1 {
            tracing::warn!(
                activity_type = %activity.activity_type,
                activity_id = %activity.id,
                handled_count,
                "multiple InboundActivityHandlers reported Handled for the same Activity \
                 (constitutional double-ownership violation — not fatal)"
            );
        }

        Ok(())
    }
}
