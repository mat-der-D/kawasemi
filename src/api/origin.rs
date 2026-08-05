//! This instance's own canonical origin, for code with no request in hand.
//!
//! [`ForwardedOrigin`] normally answers "what scheme and host did *this
//! request* arrive on", honouring `X-Forwarded-Proto`/`X-Forwarded-Host` so
//! URLs built behind a reverse proxy point back at the proxy rather than at
//! the listener. But several call sites build URLs with no request to
//! consult at all: an Activity assembled for delivery, a notification or
//! hashtag serialized outside a handler. Those sites were each writing
//! `ForwardedOrigin::resolve("https", &self.domain, None, None)` — passing
//! two `None`s precisely to say "there is nothing to forward from".
//!
//! Spelling that intent out as its own function makes the two cases legible
//! at the call site, and gives the "no request context" rule a single place
//! to live should it ever need to account for a configured base URL.

#[cfg(test)]
mod tests;

use crate::api::pagination::ForwardedOrigin;

/// Builds this instance's canonical public origin from its configured
/// domain, for call sites that have no incoming request to derive one from.
///
/// Always `https`: the domain is this server's public identity, and every
/// URL minted from it (Activity ids, actor URIs, tag URLs) is one remote
/// instances will fetch over the public internet.
pub fn self_origin(domain: &str) -> ForwardedOrigin {
    ForwardedOrigin::resolve("https", domain, None, None)
}
