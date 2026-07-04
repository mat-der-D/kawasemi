//! `KeyCipher` (design.md "Crypto / 暗号層" -> "KeyCipher"; Requirements 4.4,
//! 4.5; task 3.2): the at-rest sealing/opening boundary for a signing key's
//! private key material, AEAD-sealed with a boot-config Key-Encryption-Key
//! (KEK) and a nonce drawn from the injected `core-runtime` random-byte
//! boundary ([`crate::runtime::rng::Rng`]), so a production AEAD
//! implementation and a deterministic test run (via
//! [`crate::runtime::rng::SeededRng`]) are interchangeable behind the same
//! trait (design.md's "差し替え可能境界" constraint).
//!
//! Scope: this module owns exactly the [`KeyCipher`] trait and its
//! production implementation ([`ChaCha20Poly1305KeyCipher`]), plus the
//! [`SecretSlice`] plaintext-input wrapper the trait's `seal` takes. It does
//! not decide *when* a key is sealed/opened (`SigningKeyService`, task 4.1)
//! or how the KEK is sourced from live boot configuration (`src/config/`,
//! task 6.1) — here the KEK is only a constructor parameter of type
//! [`Kek`] (`Secret<[u8; 32]>`).
//!
//! ## AEAD choice: `chacha20poly1305`
//! `ChaCha20Poly1305` (RFC 8439) is a well-reviewed, pure-Rust,
//! constant-time AEAD construction from the RustCrypto project. It is added
//! with `default-features = false, features = ["alloc"]` specifically to
//! avoid pulling in the crate's own `getrandom`/`rand_core` Cargo features:
//! nonce generation here always goes through the already-injected `&dyn Rng`
//! boundary (`seal`'s own signature), so a second RNG abstraction/dependency
//! is unnecessary — matching the precedent set by task 3.1's `material.rs`,
//! which took care to avoid a `rand_core` version clash between `rsa` and
//! `sqlx`. With these features disabled, `chacha20poly1305` v0.11.0 (and its
//! transitive deps `aead`, `cipher`, `inout`, `poly1305`,
//! `universal-hash`) resolve cleanly with no `rand_core`/`getrandom` pulled
//! in at all, confirmed by inspecting `Cargo.lock` after `cargo add`.
//!
//! ## Wire format
//! `seal`'s output is `nonce (12 bytes) || AEAD ciphertext+tag`. The nonce
//! must accompany the ciphertext to decrypt it later (it is not secret, only
//! required to be unique per seal under a given KEK), so it is prefixed
//! rather than stored out-of-band.

use std::fmt;

use axum::http::StatusCode;
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};

use super::material::SecretString;
use crate::config::Secret;
use crate::error::AppError;
use crate::runtime::rng::Rng;

/// Length in bytes of a `ChaCha20Poly1305` nonce (96 bits, RFC 8439).
const NONCE_LEN: usize = 12;

/// Key-Encryption-Key: a fixed-size 256-bit key wrapped in [`Secret`] so it
/// never leaks via `Debug`/`Display`. Sourced from boot configuration by a
/// later task (6.1); here it is only ever a constructor parameter.
pub type Kek = Secret<[u8; 32]>;

/// Plaintext byte payload to be sealed, wrapped in [`Secret`] so the caller
/// cannot accidentally format/log the plaintext private key bytes on the way
/// into [`KeyCipher::seal`]. Design.md references this type by name
/// (`SecretSlice`) but does not define it anywhere else in the codebase, so
/// it is defined here, colocated with its sole use.
pub type SecretSlice = Secret<Vec<u8>>;

/// At-rest sealing/opening boundary for private key material (Requirements
/// 4.4, 4.5). A production implementation ([`ChaCha20Poly1305KeyCipher`])
/// and a deterministic test run (using the same implementation driven by
/// [`crate::runtime::rng::SeededRng`]) are interchangeable behind this
/// trait.
pub trait KeyCipher: Send + Sync {
    /// Seals `plaintext` for at-rest storage, drawing a fresh nonce from
    /// `rng`. Returns opaque sealed bytes (see module docs for the wire
    /// format) that never contain `plaintext` verbatim.
    fn seal(&self, plaintext: &SecretSlice, rng: &dyn Rng) -> Result<Vec<u8>, AppError>;

    /// Reverses [`KeyCipher::seal`], recovering the original plaintext as a
    /// UTF-8 string wrapped in [`SecretString`]. Fails if `sealed` was
    /// tampered with (AEAD authentication failure) or is malformed.
    fn open(&self, sealed: &[u8]) -> Result<SecretString, AppError>;
}

/// Marker error for sealed bytes too short to even contain a nonce.
#[derive(Debug)]
struct SealedCiphertextTooShort;

impl fmt::Display for SealedCiphertextTooShort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sealed ciphertext is shorter than the {NONCE_LEN}-byte nonce prefix"
        )
    }
}

impl std::error::Error for SealedCiphertextTooShort {}

/// Production [`KeyCipher`] implementation: `ChaCha20Poly1305` AEAD sealing
/// under a fixed 256-bit KEK, with a nonce drawn from the injected `Rng` on
/// every `seal` call (never reused, per AEAD nonce-uniqueness requirements).
pub struct ChaCha20Poly1305KeyCipher {
    kek: Kek,
}

impl ChaCha20Poly1305KeyCipher {
    /// Builds a cipher bound to `kek` (Requirement 4.5's "起動設定の
    /// `Secret<T>`" KEK; sourced from live boot config by a later task, 6.1
    /// — here just an injected constructor parameter).
    pub fn new(kek: Kek) -> Self {
        Self { kek }
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(&Key::from(*self.kek.expose_secret()))
    }
}

impl KeyCipher for ChaCha20Poly1305KeyCipher {
    fn seal(&self, plaintext: &SecretSlice, rng: &dyn Rng) -> Result<Vec<u8>, AppError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from(nonce_bytes);

        let ciphertext = self
            .cipher()
            .encrypt(&nonce, plaintext.expose_secret().as_slice())
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        let mut sealed = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        sealed.extend_from_slice(&nonce_bytes);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }

    fn open(&self, sealed: &[u8]) -> Result<SecretString, AppError> {
        if sealed.len() < NONCE_LEN {
            return Err(AppError::server(
                StatusCode::INTERNAL_SERVER_ERROR,
                SealedCiphertextTooShort,
            ));
        }
        let (nonce_bytes, ciphertext) = sealed.split_at(NONCE_LEN);
        let nonce_bytes: [u8; NONCE_LEN] = nonce_bytes
            .try_into()
            .expect("split_at(NONCE_LEN) guarantees an exact NONCE_LEN-byte slice");
        let nonce = Nonce::from(nonce_bytes);

        let plaintext_bytes = self
            .cipher()
            .decrypt(&nonce, ciphertext)
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;
        let plaintext = String::from_utf8(plaintext_bytes)
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(Secret::new(plaintext))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::rng::SeededRng;

    const PLAINTEXT_PEM: &str =
        "-----BEGIN PRIVATE KEY-----\nMIIExamplePlaintextKeyMaterial\n-----END PRIVATE KEY-----";

    fn cipher_under_test() -> ChaCha20Poly1305KeyCipher {
        ChaCha20Poly1305KeyCipher::new(Secret::new([7u8; 32]))
    }

    fn plaintext() -> SecretSlice {
        Secret::new(PLAINTEXT_PEM.as_bytes().to_vec())
    }

    #[test]
    fn seal_then_open_round_trips_the_original_plaintext() {
        let cipher = cipher_under_test();
        let rng = SeededRng::new(1);

        let sealed = cipher.seal(&plaintext(), &rng).expect("seal must succeed");
        let opened = cipher.open(&sealed).expect("open must succeed");

        assert_eq!(opened.expose_secret(), PLAINTEXT_PEM);
    }

    #[test]
    fn sealed_output_does_not_contain_the_plaintext_as_a_literal_substring() {
        let cipher = cipher_under_test();
        let rng = SeededRng::new(2);

        let sealed = cipher.seal(&plaintext(), &rng).expect("seal must succeed");

        // A real AEAD cipher's output must not contain the plaintext bytes
        // verbatim anywhere in the sealed bytes -- this would fail trivially
        // for a mock/no-op "cipher" that just copies or encodes plaintext.
        assert!(
            !sealed
                .windows(PLAINTEXT_PEM.len())
                .any(|window| window == PLAINTEXT_PEM.as_bytes()),
            "sealed output contained the plaintext verbatim"
        );
    }

    #[test]
    fn sealed_output_is_not_merely_the_plaintext_prefixed_with_a_nonce() {
        let cipher = cipher_under_test();
        let rng = SeededRng::new(3);

        let sealed = cipher.seal(&plaintext(), &rng).expect("seal must succeed");

        // Sanity check that real transformation happened beyond prefixing a
        // nonce: the tail of `sealed` (past the 12-byte nonce) must differ
        // from the plaintext bytes, and must be longer than the plaintext by
        // exactly the AEAD tag size (16 bytes for ChaCha20Poly1305), proving
        // an authentication tag was appended, not just an encoding pass.
        let ciphertext_and_tag = &sealed[NONCE_LEN..];
        assert_ne!(ciphertext_and_tag, PLAINTEXT_PEM.as_bytes());
        assert_eq!(ciphertext_and_tag.len(), PLAINTEXT_PEM.len() + 16);
    }

    #[test]
    fn tampering_with_sealed_bytes_causes_open_to_fail() {
        let cipher = cipher_under_test();
        let rng = SeededRng::new(4);

        let mut sealed = cipher.seal(&plaintext(), &rng).expect("seal must succeed");
        // Flip a bit well past the nonce, inside the ciphertext/tag region.
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;

        let result = cipher.open(&sealed);
        assert!(
            result.is_err(),
            "opening tampered sealed bytes must fail AEAD authentication"
        );
    }

    #[test]
    fn opening_with_the_wrong_kek_fails() {
        let sealer = ChaCha20Poly1305KeyCipher::new(Secret::new([7u8; 32]));
        let wrong_key_opener = ChaCha20Poly1305KeyCipher::new(Secret::new([9u8; 32]));
        let rng = SeededRng::new(5);

        let sealed = sealer.seal(&plaintext(), &rng).expect("seal must succeed");
        let result = wrong_key_opener.open(&sealed);

        assert!(
            result.is_err(),
            "opening with a different KEK must fail AEAD authentication"
        );
    }

    #[test]
    fn different_nonces_produce_different_sealed_bytes_for_the_same_plaintext() {
        let cipher = cipher_under_test();

        let sealed_a = cipher
            .seal(&plaintext(), &SeededRng::new(10))
            .expect("seal must succeed");
        let sealed_b = cipher
            .seal(&plaintext(), &SeededRng::new(20))
            .expect("seal must succeed");

        assert_ne!(sealed_a, sealed_b);
    }

    #[test]
    fn debug_of_opened_secret_string_does_not_expose_plaintext() {
        let cipher = cipher_under_test();
        let rng = SeededRng::new(6);

        let sealed = cipher.seal(&plaintext(), &rng).expect("seal must succeed");
        let opened = cipher.open(&sealed).expect("open must succeed");

        let formatted = format!("{opened:?}");
        assert!(
            !formatted.contains(PLAINTEXT_PEM),
            "Debug output leaked the opened plaintext: {formatted}"
        );
        assert!(!formatted.to_lowercase().contains("begin private key"));
    }
}
