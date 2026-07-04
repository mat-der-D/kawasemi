//! `SigningKeyProvider` injection boundary (Requirement 5.4): signing-key
//! supply placed behind an abstract boundary, decoupling callers from any
//! concrete key generation/storage/rotation strategy.
//!
//! Scope: this module owns only the *supply* boundary itself —
//! [`KeyRef`] (the canonical reference to "the actor whose currently
//! valid signing key is being requested"), the [`SigningKeyProvider`]
//! trait, and a fixed-key test implementation
//! ([`FixedSigningKeyProvider`]) that reproducibly returns the same key
//! material for the same `KeyRef`. Per design.md's "RuntimeContext と注入
//! 境界" component notes, it deliberately does NOT own production signing
//! key generation, storage, or rotation — that belongs to actor-model (Out
//! of Boundary for core-runtime), which is expected to supply its own
//! `SigningKeyProvider` implementation against this same trait.

use std::fmt;

use crate::config::Secret;
use crate::domain::Id;

/// Reference to the signing key currently in effect for a given actor.
///
/// Single-key-per-actor model (design.md): at any time an actor has
/// exactly one currently valid signing key, so `KeyRef` wraps the actor's
/// [`Id`] directly rather than carrying an independent key ID that would
/// distinguish key versions/generations. The wrapped field is `pub`
/// (unlike `Id`'s own private field) to match design.md's exact
/// `pub struct KeyRef(pub Id);` interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyRef(pub Id);

/// Signing key material handed back by a [`SigningKeyProvider`].
///
/// Holds opaque PEM-encoded PKCS#8 private key bytes — the representation
/// expected by HTTP Signatures per `docs/fediverse-design.md`'s "HTTP
/// Signatures" section (draft-cavage / RFC 9421). core-runtime's job at
/// this boundary is only to supply key material, not to perform
/// cryptographic operations with it, so no further parsing/structure is
/// imposed here: a downstream consumer (e.g. federation-core) is
/// responsible for parsing this into a concrete key type it can sign
/// with.
///
/// The bytes are wrapped in [`Secret`] so that formatting a `SigningKey`
/// (`Debug`) can never leak private key material, mirroring how
/// `DatabaseConfig::url` protects its connection string (Requirement
/// 2.5's masking convention, reused here for the same class of hazard).
#[derive(Clone, PartialEq, Eq)]
pub struct SigningKey(Secret<Vec<u8>>);

impl SigningKey {
    /// Wraps raw PEM-encoded PKCS#8 private key bytes as a `SigningKey`.
    pub fn from_pem_bytes(pem: Vec<u8>) -> Self {
        Self(Secret::new(pem))
    }

    /// Returns the wrapped PEM-encoded PKCS#8 private key bytes.
    pub fn expose_pem_bytes(&self) -> &[u8] {
        self.0.expose_secret()
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SigningKey").field(&self.0).finish()
    }
}

/// Failure looking up a signing key for a given [`KeyRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// No signing key is available for the referenced actor.
    NotFound(KeyRef),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::NotFound(key_ref) => {
                write!(f, "no signing key found for {key_ref:?}")
            }
        }
    }
}

impl std::error::Error for KeyError {}

/// Supplies the currently valid signing key for a given actor, decoupling
/// callers from any concrete key generation/storage/rotation strategy
/// (Requirement 5.4). Implementations must be safe to share across threads
/// (`Send + Sync`) since `RuntimeContext` hands out a single shared
/// instance to concurrent request handlers.
///
/// core-runtime provides only [`FixedSigningKeyProvider`] below (a fixed
/// test implementation) and this trait as the extension point; a
/// production implementation backed by real key generation/storage/
/// rotation is out of boundary here and is supplied by actor-model.
pub trait SigningKeyProvider: Send + Sync {
    fn signing_key(&self, key_ref: KeyRef) -> Result<SigningKey, KeyError>;
}

/// Fixed-key test [`SigningKeyProvider`] implementation (Requirement 5.4's
/// test-side extension point): always returns the same, constructed
/// [`SigningKey`] regardless of which actor's [`KeyRef`] is requested, so
/// tests can assert against known, reproducible key material instead of
/// depending on real, non-reproducible key generation. Mirrors
/// [`FixedClock`](super::clock::FixedClock)'s "always returns the fixed
/// value it was constructed with" shape.
#[derive(Debug, Clone)]
pub struct FixedSigningKeyProvider {
    fixed: SigningKey,
}

impl FixedSigningKeyProvider {
    /// Constructs a provider that always returns `fixed` for any
    /// [`KeyRef`] passed to [`signing_key`](SigningKeyProvider::signing_key).
    pub fn new(fixed: SigningKey) -> Self {
        Self { fixed }
    }
}

impl SigningKeyProvider for FixedSigningKeyProvider {
    fn signing_key(&self, _key_ref: KeyRef) -> Result<SigningKey, KeyError> {
        Ok(self.fixed.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED_PEM: &[u8] = b"-----BEGIN PRIVATE KEY-----\nfixed-test-key\n-----END PRIVATE KEY-----\n";

    #[test]
    fn fixed_signing_key_provider_reproduces_the_same_key_material_across_repeated_calls() {
        let provider = FixedSigningKeyProvider::new(SigningKey::from_pem_bytes(FIXED_PEM.to_vec()));
        let key_ref = KeyRef(Id::from_i64(42));

        let first = provider.signing_key(key_ref).expect("fixed provider never fails");
        let second = provider.signing_key(key_ref).expect("fixed provider never fails");

        assert_eq!(first.expose_pem_bytes(), FIXED_PEM);
        assert_eq!(first.expose_pem_bytes(), second.expose_pem_bytes());
    }

    #[test]
    fn fixed_signing_key_provider_reproduces_the_same_key_material_across_separately_constructed_instances() {
        let a = FixedSigningKeyProvider::new(SigningKey::from_pem_bytes(FIXED_PEM.to_vec()));
        let b = FixedSigningKeyProvider::new(SigningKey::from_pem_bytes(FIXED_PEM.to_vec()));
        let key_ref = KeyRef(Id::from_i64(7));

        let from_a = a.signing_key(key_ref).expect("fixed provider never fails");
        let from_b = b.signing_key(key_ref).expect("fixed provider never fails");

        assert_eq!(from_a.expose_pem_bytes(), from_b.expose_pem_bytes());
    }

    #[test]
    fn fixed_signing_key_provider_returns_the_same_fixed_key_for_different_actors() {
        let provider = FixedSigningKeyProvider::new(SigningKey::from_pem_bytes(FIXED_PEM.to_vec()));

        let for_actor_one = provider
            .signing_key(KeyRef(Id::from_i64(1)))
            .expect("fixed provider never fails");
        let for_actor_two = provider
            .signing_key(KeyRef(Id::from_i64(2)))
            .expect("fixed provider never fails");

        // Not randomly generated per call/actor: the same fixed key comes
        // back regardless of which actor's KeyRef is requested.
        assert_eq!(for_actor_one.expose_pem_bytes(), for_actor_two.expose_pem_bytes());
        assert_eq!(for_actor_one.expose_pem_bytes(), FIXED_PEM);
    }

    #[test]
    fn signing_key_debug_does_not_leak_key_material() {
        let key = SigningKey::from_pem_bytes(FIXED_PEM.to_vec());
        let formatted = format!("{key:?}");
        assert!(
            !formatted.contains("fixed-test-key"),
            "SigningKey's Debug output must never leak key material: {formatted}"
        );
    }

    #[test]
    fn key_error_display_identifies_the_missing_key_ref() {
        let key_ref = KeyRef(Id::from_i64(99));
        let error = KeyError::NotFound(key_ref);

        let formatted = error.to_string();
        assert!(formatted.contains("no signing key found"));
    }
}
