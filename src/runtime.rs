//! Runtime injection boundaries (`RuntimeContext` and its constituent
//! non-determinism boundaries), Requirements 5.1-5.6.
//!
//! This module aggregates the four injection boundaries (clock / id / rng /
//! signing key) behind [`RuntimeContext`] (design.md's "RuntimeContext と
//! 注入境界"): `clock` (Requirement 5.1), `ids` (Requirement 5.2), `rng`
//! (Requirement 5.3), and `signing_key` (Requirement 5.4) each define one
//! boundary's trait plus production/deterministic implementations, and
//! [`RuntimeContext::production`]/[`RuntimeContext::deterministic`] (this
//! file) construct a `RuntimeContext` from all four at once (Requirements
//! 5.5, 5.6).

use std::sync::Arc;

use time::{Duration, OffsetDateTime};

pub mod clock;
pub mod ids;
pub mod rng;
pub mod signing_key;

pub use clock::{Clock, FixedClock, SystemClock};
pub use ids::{IdGenerator, SeqIdGenerator, SnowflakeIdGenerator};
pub use rng::{Rng, SeededRng, SystemRng};
pub use signing_key::{FixedSigningKeyProvider, KeyError, KeyRef, SigningKey, SigningKeyProvider};

/// Seed for [`RuntimeContext::deterministic`] (Requirement 5.5): a single
/// numeric value from which all four boundary implementations derive their
/// fixed/seeded state, so that two `RuntimeContext`s built from the same
/// seed reproduce the same time/id/rng/key sequence end to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeterministicSeed(u64);

impl DeterministicSeed {
    /// Constructs a seed from a raw numeric value. Any `u64` is a valid
    /// seed; only equality of the seed value governs reproducibility.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
}

/// Fixed PEM byte content used as the `keys` boundary for
/// [`RuntimeContext::production`]. See that constructor's doc comment for
/// why a fixed key stands in here.
const PRODUCTION_PLACEHOLDER_KEY_PEM: &[u8] =
    b"-----BEGIN PRIVATE KEY-----\ncore-runtime-production-placeholder-key\n-----END PRIVATE KEY-----\n";

/// Builds deterministic, seed-derived PEM byte content for
/// [`RuntimeContext::deterministic`]'s `keys` boundary: embedding the seed
/// value in the PEM body means the same seed always reproduces the same key
/// material, and (incidentally, though not required by Requirement 5.5)
/// different seeds produce different key material too.
fn deterministic_key_pem(seed: u64) -> Vec<u8> {
    format!("-----BEGIN PRIVATE KEY-----\ndeterministic-seed-{seed}\n-----END PRIVATE KEY-----\n")
        .into_bytes()
}

/// Aggregates the four non-determinism injection boundaries (clock / id /
/// rng / signing key) behind a single handle that downstream code shares
/// (design.md's "RuntimeContext と注入境界", Requirements 5.1-5.6). Each
/// field is a trait object behind `Arc` so `RuntimeContext` can be cloned
/// cheaply and shared across concurrent request handlers while still
/// allowing production and deterministic implementations to be swapped in
/// interchangeably.
#[derive(Clone)]
pub struct RuntimeContext {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    pub rng: Arc<dyn Rng>,
    pub keys: Arc<dyn SigningKeyProvider>,
}

impl RuntimeContext {
    /// Constructs a `RuntimeContext` backed by production implementations
    /// for all four boundaries (Requirement 5.6): [`SystemClock`],
    /// [`SnowflakeIdGenerator`], [`SystemRng`].
    ///
    /// `keys` is a deliberate, spec-sanctioned exception rather than an
    /// oversight: per design.md's "RuntimeContext と注入境界" component
    /// notes, core-runtime owns only the signing-key *supply* boundary and
    /// its test implementation — production key generation/storage/
    /// rotation is Out of Boundary here and belongs to actor-model, which
    /// is expected to supply its own [`SigningKeyProvider`] against this
    /// same trait. Until that implementation exists, [`FixedSigningKeyProvider`]
    /// stands in as a placeholder so `production()` remains fully
    /// constructible today.
    pub fn production() -> Self {
        Self {
            clock: Arc::new(SystemClock::new()),
            ids: Arc::new(SnowflakeIdGenerator::new()),
            rng: Arc::new(SystemRng::new()),
            keys: Arc::new(FixedSigningKeyProvider::new(SigningKey::from_pem_bytes(
                PRODUCTION_PLACEHOLDER_KEY_PEM.to_vec(),
            ))),
        }
    }

    /// Constructs a `RuntimeContext` backed by deterministic implementations
    /// for all four boundaries, each derived from `seed` (Requirement 5.5):
    /// two contexts built from the same seed reproduce the same
    /// `clock.now()` value, `ids.next_id()` sequence, `rng.fill_bytes()`
    /// byte stream, and `keys.signing_key()` material.
    pub fn deterministic(seed: DeterministicSeed) -> Self {
        // The acceptance criterion only requires "same seed -> same fixed
        // time", not that the seed encode a real calendar date, so any
        // fixed base time offset by the seed (in seconds) is sufficient;
        // this also keeps the fixed time within `OffsetDateTime`'s valid
        // range for arbitrary `u64` seeds.
        let fixed_time = OffsetDateTime::UNIX_EPOCH + Duration::seconds((seed.0 % 1_000_000_000) as i64);
        Self {
            clock: Arc::new(FixedClock::new(fixed_time)),
            ids: Arc::new(SeqIdGenerator::new(seed.0 as i64)),
            rng: Arc::new(SeededRng::new(seed.0)),
            keys: Arc::new(FixedSigningKeyProvider::new(SigningKey::from_pem_bytes(
                deterministic_key_pem(seed.0),
            ))),
        }
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    use crate::domain::Id;

    #[test]
    fn deterministic_context_reproduces_the_same_clock_ids_rng_and_key_for_the_same_seed() {
        let a = RuntimeContext::deterministic(DeterministicSeed::new(42));
        let b = RuntimeContext::deterministic(DeterministicSeed::new(42));

        assert_eq!(a.clock.now(), b.clock.now());

        for _ in 0..5 {
            assert_eq!(a.ids.next_id(), b.ids.next_id());
        }

        let mut buf_a = [0u8; 32];
        let mut buf_b = [0u8; 32];
        a.rng.fill_bytes(&mut buf_a);
        b.rng.fill_bytes(&mut buf_b);
        assert_eq!(buf_a, buf_b);

        let key_ref = KeyRef(Id::from_i64(7));
        let key_a = a.keys.signing_key(key_ref).expect("deterministic provider never fails");
        let key_b = b.keys.signing_key(key_ref).expect("deterministic provider never fails");
        assert_eq!(key_a.expose_pem_bytes(), key_b.expose_pem_bytes());
    }

    #[test]
    fn production_context_constructs_with_production_backed_implementations() {
        let context = RuntimeContext::production();

        let now = context.clock.now();
        let real_now = time::OffsetDateTime::now_utc();
        assert!(
            (real_now - now).abs() < time::Duration::seconds(5),
            "production clock should track real wall-clock time, got {now:?} vs {real_now:?}"
        );

        let first = context.ids.next_id();
        let second = context.ids.next_id();
        assert!(first.as_i64() < second.as_i64());

        let mut first_buf = [0u8; 16];
        let mut second_buf = [0u8; 16];
        context.rng.fill_bytes(&mut first_buf);
        context.rng.fill_bytes(&mut second_buf);
        assert_ne!(first_buf, second_buf);

        let key_ref = KeyRef(Id::from_i64(1));
        assert!(context.keys.signing_key(key_ref).is_ok());
    }
}
