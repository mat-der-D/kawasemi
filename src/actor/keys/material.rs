//! `KeyMaterial` (design.md "Crypto / 暗号層" -> "KeyMaterial"; Requirements
//! 4.2, 4.3, 4.6; task 3.1): generates an RSA-2048 signing key pair from an
//! injected `core-runtime` random-byte boundary ([`crate::runtime::rng::Rng`])
//! and PEM-encodes it — the public key as SPKI/PEM (4.6, so a downstream
//! federation consumer can publish it), the private key as PKCS#8/PEM
//! wrapped in a [`SecretString`] that never exposes the plaintext via
//! `Debug`/`Display` (4.4; formally owned by later tasks 3.2/4.1, but this
//! type's own shape must not leak it either).
//!
//! Scope: this module owns exactly [`generate_keypair`] and its supporting
//! types ([`GeneratedKeyPair`], [`KeyAlgorithm`]) plus the private
//! [`RngAdapter`] bridge described below. It does not seal the private key
//! at rest (`KeyCipher`, task 3.2), persist anything (`ActorSigningKeyRepository`,
//! already done in task 2.3's `repository` sibling module), or decide when a
//! new key pair is generated (`SigningKeyService`, task 4.1) — it only turns
//! an injected `Rng` into PEM bytes.
//!
//! ## Bridging `core-runtime`'s `Rng` to the `rsa` crate's `CryptoRngCore`
//! The `rsa` crate's key-generation entry points (`RsaPrivateKey::new`)
//! require `rand_core::CryptoRngCore` (`RngCore + CryptoRng`, from the
//! `rand_core` 0.6.x lineage the `rsa` 0.9 crate itself depends on), while
//! `core-runtime`'s `Rng` trait (`src/runtime/rng.rs`) exposes only
//! `fill_bytes`. [`RngAdapter`] bridges the two, kept private to this module
//! per design.md ("両者を橋渡しする `RngAdapter`… を本コンポーネント内部に
//! 閉じて保持し、境界の外には出さない").
//!
//! This module deliberately imports `rand_core` via `rsa::rand_core` (the
//! `rsa` crate re-exports its own `rand_core` dependency, `pub use
//! rand_core;` in `rsa`'s `lib.rs`) rather than adding a second, separately
//! versioned `rand_core` direct dependency to `Cargo.toml`. `rand_core`'s 0.x
//! versions are not semver-compatible with each other (confirmed by
//! inspection: the transitively-resolved `rand_core` already in this
//! workspace's `Cargo.lock`, pulled in via `sqlx-postgres` -> `rand` 0.10, is
//! `rand_core` 0.10.1, a *different, source-incompatible* API from the
//! `rand_core` 0.6.4 that `rsa` 0.9.10 actually requires — `rand_core` 0.10
//! renamed/restructured `RngCore`/`CryptoRng` entirely). Importing the exact
//! `rand_core` version `rsa` itself depends on via its re-export, rather
//! than independently pinning a matching version number in `Cargo.toml`,
//! removes any chance of the two drifting out of sync later.
//!
//! ## Assumptions carried over from design.md
//! - `RngAdapter`'s `CryptoRng` marker impl assumes the injected `Rng` is
//!   CSPRNG-grade. `Rng` itself does not encode that in its type; this is a
//!   reasonable assumption for the production `SystemRng` (OS entropy) and
//!   an acceptable one for the test-only `SeededRng` used below (this test
//!   exercises deterministic *reproducibility*, not a security property of
//!   the test RNG itself).
//! - Deterministic reproduction (4.3) depends on the `rsa` crate consuming
//!   its injected RNG stream in a fixed, deterministic order for a given
//!   input stream. This is an internal implementation detail of the `rsa`
//!   crate that design.md flagged as unconfirmed at spec-writing time; the
//!   `same_seed_reproduces_the_same_keypair` test below is exactly the
//!   empirical check design.md calls for, and it passes against `rsa`
//!   0.9.10.

use axum::http::StatusCode;
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::rand_core::{CryptoRng, Error as RandCoreError, RngCore, impls};
use rsa::{RsaPrivateKey, RsaPublicKey};

use crate::config::Secret;
use crate::error::AppError;
use crate::runtime::rng::Rng;

/// RSA modulus size this module generates (design.md: "RSA-2048").
const RSA_KEY_BITS: usize = 2048;

/// A secret-masking `String` (mirrors `crate::config::Secret<String>`'s
/// existing masking convention for the same class of hazard — see
/// `src/runtime/signing_key.rs`'s `SigningKey`, which wraps signing-key
/// bytes the same way). Named per design.md's exact `GeneratedKeyPair`
/// interface (`private_key_pem: SecretString`); there is no existing
/// `SecretString` alias elsewhere in the codebase, so this is its first use
/// — a plain alias over the existing `Secret<T>` wrapper rather than a new,
/// second, incompatible secret type.
pub type SecretString = Secret<String>;

/// The signing-key algorithm a [`GeneratedKeyPair`] was generated with.
///
/// Only `Rsa2048` exists today (this module generates nothing else).
/// `src/actor/keys/repository.rs`'s `StoredSigningKey::algorithm` is
/// deliberately a plain `String` rather than this type — that task (2.3)
/// predates this one and noted `KeyMaterial` (here) would eventually own
/// the typed representation; wiring the repository layer to use
/// `KeyAlgorithm` is out of this task's boundary (`src/actor/keys/` minus
/// `repository.rs`) and left to whichever later task connects them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyAlgorithm {
    /// RSA, 2048-bit modulus, PKCS#1 v1.5 signatures (the algorithm this
    /// module's [`generate_keypair`] always produces).
    Rsa2048,
}

/// A freshly generated signing key pair (design.md's exact
/// `GeneratedKeyPair` interface): the public key as SPKI/PEM (safe to
/// publish, Requirement 4.6) and the private key as PKCS#8/PEM wrapped in a
/// [`SecretString`] (never printed in plaintext via `Debug`/`Display`,
/// Requirement 4.4).
///
/// `#[derive(Debug)]` is safe here specifically because `private_key_pem`'s
/// type (`SecretString` = `Secret<String>`) already redacts itself
/// regardless of the derive (`crate::config::secret`'s own
/// `derived_debug_on_containing_struct_does_not_leak_field` test proves
/// this pattern); `public_key_pem` is intentionally plain `String` since it
/// is public key material, safe to display as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedKeyPair {
    pub algorithm: KeyAlgorithm,
    pub public_key_pem: String,
    pub private_key_pem: SecretString,
}

/// Bridges `core-runtime`'s `&dyn Rng` (`fill_bytes` only) to the
/// `rand_core::RngCore + CryptoRng` (`CryptoRngCore`) bound the `rsa` crate
/// requires for key generation. Kept private to this module (design.md:
/// "境界の外には出さない").
struct RngAdapter<'a>(&'a dyn Rng);

impl<'a> RngCore for RngAdapter<'a> {
    /// Derived from `fill_bytes`, per `rand_core`'s own standard
    /// fill-based derivation helper (`rand_core::impls::next_u32_via_fill`),
    /// matching design.md's comment ("fill_bytes から導出（rand_core 標準
    /// 実装に準拠）") exactly.
    fn next_u32(&mut self) -> u32 {
        impls::next_u32_via_fill(self)
    }

    /// Derived from `fill_bytes`, same rationale as `next_u32` above.
    fn next_u64(&mut self) -> u64 {
        impls::next_u64_via_fill(self)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest);
    }

    /// `core-runtime`'s `Rng::fill_bytes` is infallible (it panics on an
    /// unrecoverable entropy failure rather than returning a `Result`, see
    /// `SystemRng`), so this always succeeds.
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), RandCoreError> {
        self.fill_bytes(dest);
        Ok(())
    }
}

/// Marker-only impl: asserts the injected `Rng` is CSPRNG-grade. See this
/// module's doc comment ("Assumptions carried over from design.md").
impl<'a> CryptoRng for RngAdapter<'a> {}

/// Generates a fresh RSA-2048 signing key pair using `rng` as the sole
/// source of randomness (Requirement 4.2), PEM-encoding the public key as
/// SPKI (Requirement 4.6) and the private key as PKCS#8 wrapped in a
/// [`SecretString`] (Requirement 4.4). Using the same deterministic `rng`
/// stream (e.g. `crate::runtime::rng::SeededRng` with a fixed seed)
/// reproduces the same key pair (Requirement 4.3).
pub fn generate_keypair(rng: &dyn Rng) -> Result<GeneratedKeyPair, AppError> {
    let mut adapter = RngAdapter(rng);

    let private_key = RsaPrivateKey::new(&mut adapter, RSA_KEY_BITS)
        .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
    let public_key = RsaPublicKey::from(&private_key);

    let private_key_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
    let public_key_pem = public_key
        .to_public_key_pem(LineEnding::LF)
        .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

    Ok(GeneratedKeyPair {
        algorithm: KeyAlgorithm::Rsa2048,
        public_key_pem,
        private_key_pem: Secret::new(private_key_pem.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};

    use super::*;
    use crate::runtime::rng::SeededRng;

    #[test]
    fn same_seed_reproduces_the_same_keypair() {
        let first = generate_keypair(&SeededRng::new(42)).expect("key generation must succeed");
        let second = generate_keypair(&SeededRng::new(42)).expect("key generation must succeed");

        assert_eq!(first.public_key_pem, second.public_key_pem);
        assert_eq!(
            first.private_key_pem.expose_secret(),
            second.private_key_pem.expose_secret()
        );
    }

    #[test]
    fn same_seed_reproduces_the_same_keypair_across_many_fills() {
        // Guards against a key generator that happens to consume just
        // enough of the RNG stream to look reproducible for a single call,
        // but drifts once more bytes are drawn than a shorter run needed.
        for seed in [1_u64, 2, 100, u64::MAX] {
            let first =
                generate_keypair(&SeededRng::new(seed)).expect("key generation must succeed");
            let second =
                generate_keypair(&SeededRng::new(seed)).expect("key generation must succeed");

            assert_eq!(
                first.public_key_pem, second.public_key_pem,
                "seed {seed} did not reproduce the same public key"
            );
            assert_eq!(
                first.private_key_pem.expose_secret(),
                second.private_key_pem.expose_secret(),
                "seed {seed} did not reproduce the same private key"
            );
        }
    }

    #[test]
    fn different_seeds_produce_different_keypairs() {
        let a = generate_keypair(&SeededRng::new(1)).expect("key generation must succeed");
        let b = generate_keypair(&SeededRng::new(2)).expect("key generation must succeed");

        assert_ne!(a.public_key_pem, b.public_key_pem);
        assert_ne!(
            a.private_key_pem.expose_secret(),
            b.private_key_pem.expose_secret()
        );
    }

    #[test]
    fn public_key_pem_is_valid_spki_pem() {
        let generated = generate_keypair(&SeededRng::new(7)).expect("key generation must succeed");

        assert!(
            generated
                .public_key_pem
                .starts_with("-----BEGIN PUBLIC KEY-----")
        );
        assert!(
            generated
                .public_key_pem
                .trim_end()
                .ends_with("-----END PUBLIC KEY-----")
        );

        // Round-trip through the SPKI/PEM decoder to prove this is not just
        // string formatting that happens to look right.
        let decoded = RsaPublicKey::from_public_key_pem(&generated.public_key_pem)
            .expect("generated public key must be valid SPKI/PEM");
        let re_encoded = decoded
            .to_public_key_pem(LineEnding::LF)
            .expect("re-encoding the decoded public key must succeed");
        assert_eq!(generated.public_key_pem, re_encoded);
    }

    #[test]
    fn private_key_pem_is_valid_pkcs8_pem() {
        let generated = generate_keypair(&SeededRng::new(7)).expect("key generation must succeed");
        let private_key_pem = generated.private_key_pem.expose_secret();

        assert!(private_key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert!(
            private_key_pem
                .trim_end()
                .ends_with("-----END PRIVATE KEY-----")
        );

        // Round-trip through the PKCS#8/PEM decoder to prove this is not
        // just string formatting that happens to look right.
        let decoded = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
            .expect("generated private key must be valid PKCS#8/PEM");
        let re_encoded = decoded
            .to_pkcs8_pem(LineEnding::LF)
            .expect("re-encoding the decoded private key must succeed");
        assert_eq!(private_key_pem.as_str(), re_encoded.as_str());
    }

    #[test]
    fn generated_keypair_is_algorithm_rsa2048() {
        let generated = generate_keypair(&SeededRng::new(3)).expect("key generation must succeed");
        assert_eq!(generated.algorithm, KeyAlgorithm::Rsa2048);
    }

    #[test]
    fn debug_does_not_leak_private_key_plaintext() {
        let generated = generate_keypair(&SeededRng::new(9)).expect("key generation must succeed");
        let private_key_plaintext = generated.private_key_pem.expose_secret().clone();

        let formatted = format!("{generated:?}");

        assert!(
            !formatted.contains(&private_key_plaintext),
            "Debug output leaked the private key PEM: {formatted}"
        );
        assert!(
            !formatted.contains("-----BEGIN PRIVATE KEY-----"),
            "Debug output leaked a private key PEM header: {formatted}"
        );
        // The public key PEM is not secret and is expected to still appear
        // (compared via its own `Debug` escaping, since `String`'s `Debug`
        // impl escapes the PEM's embedded newlines as literal `\n`).
        assert!(formatted.contains(&format!("{:?}", generated.public_key_pem)));
    }
}
