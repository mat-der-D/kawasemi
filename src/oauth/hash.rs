//! Shared keyed-hashing primitive for OAuth secret material (client
//! secrets, authorization codes, access tokens) persisted by the
//! `*_repository.rs` modules (tasks 3.1-3.3), per design.md's Security
//! Considerations: "アクセストークン/認可コード/`client_secret` はいずれも
//! 同一規約でハッシュ保存し平文を永続化・ログ出力しない...資格情報・トークン
//! 照合はハッシュ化した値同士の定数時間比較" and Requirements 1.5, 3.6.
//!
//! Scope: this module owns exactly one shared primitive — a keyed hash
//! (HMAC-SHA256, keyed by `AppConfig.oauth.token_hash_key`, task 1.2) plus a
//! constant-time equality check against an already-hashed value — used by
//! `app_repository.rs` (task 3.1, `oauth_applications.client_secret_hash`),
//! and intended to be reused as-is (not re-derived) by `code_repository.rs`
//! (task 3.2, `oauth_authorization_codes.code_hash`) and
//! `token_repository.rs` (task 3.3, `oauth_access_tokens.token_hash`) — all
//! three columns are documented (`migrations/0003_oauth.sql`) as sharing
//! "the same hashing convention". This module does not decide *when* to
//! hash or which table/column a hash lands in — that is each repository
//! module's own concern.
//!
//! ## Why HMAC-SHA256 (keyed), not plain SHA-256
//! A plain unsalted/unkeyed SHA-256 digest of a high-entropy secret is
//! already infeasible to reverse, but `token_hash_key` (`src/config.rs`'s
//! `OauthConfig`) exists specifically so a database leak alone is
//! insufficient to let an attacker who does not also possess the
//! deployment's `token_hash_key` build a rainbow table / precomputed
//! dictionary against the hashed column offline — the digest is only
//! reproducible with the same keyed material. HMAC-SHA256 (RFC 2104) is the
//! standard, well-reviewed way to build a keyed hash from SHA-256.
//!
//! `hmac`/`digest` were already transitively resolved in `Cargo.lock` at
//! versions compatible with this crate's existing `sha2 = "0.11.0"`
//! dependency (`sqlx-postgres` depends on `hmac 0.13.0` over `digest
//! 0.11.3`, the same `digest` version `sha2 0.11.0` itself depends on), so
//! adding `hmac = "0.13.0"` as a direct dependency (`Cargo.toml`)
//! introduces no new supply-chain surface — it only exposes an
//! already-resolved transitive dependency directly.
//!
//! ## Constant-time comparison
//! [`verify_keyed_hash`] recomputes the HMAC and compares it to the stored
//! hash via [`subtle::ConstantTimeEq`], mirroring `pkce.rs::verify_pkce`'s
//! identical rationale and this crate's established comparison discipline
//! for anything hash-shaped and security-adjacent (design.md's Security
//! Considerations: "定数時間比較"; Requirement 1.5).

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::config::Secret;

/// Keyed hashing material for OAuth secret material (`client_secret`/
/// authorization-code/access-token values), sourced from
/// `AppConfig.oauth.token_hash_key` (`src/config.rs`, task 1.2). A type
/// alias (not a newtype) so callers can pass `&app_config.oauth.token_hash_key`
/// directly without an extra wrapping/unwrapping step.
pub type TokenHashKey = Secret<[u8; 32]>;

type HmacSha256 = Hmac<Sha256>;

/// Computes the keyed hash of `value` under `key` (HMAC-SHA256, 32-byte
/// output). Used both to derive the persisted hash column value at write
/// time (`client_secret_hash`/`code_hash`/`token_hash`) and to recompute it
/// from a caller-presented plaintext at verification time.
pub fn keyed_hash(key: &TokenHashKey, value: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key.expose_secret())
        .expect("HMAC-SHA256 accepts any key length, including this fixed 32-byte key");
    mac.update(value.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Returns `true` if `presented` hashes (under `key`) to exactly
/// `expected_hash`, comparing the two digests in constant time (Requirement
/// 1.5's "ハッシュ同士の定数時間比較", mirrored by Requirement 3.6 for
/// tokens/codes). Never compares plaintext to plaintext, and the comparison
/// itself never short-circuits on the first differing byte.
pub fn verify_keyed_hash(key: &TokenHashKey, presented: &str, expected_hash: &[u8]) -> bool {
    let computed = keyed_hash(key, presented);
    computed.ct_eq(expected_hash).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> TokenHashKey {
        Secret::new([byte; 32])
    }

    #[test]
    fn keyed_hash_is_deterministic_for_the_same_key_and_value() {
        assert_eq!(keyed_hash(&key(1), "s3cr3t"), keyed_hash(&key(1), "s3cr3t"));
    }

    #[test]
    fn keyed_hash_output_is_a_32_byte_sha256_sized_digest() {
        assert_eq!(keyed_hash(&key(1), "s3cr3t").len(), 32);
    }

    #[test]
    fn keyed_hash_differs_for_different_keys_given_the_same_value() {
        assert_ne!(
            keyed_hash(&key(1), "same-value"),
            keyed_hash(&key(2), "same-value")
        );
    }

    #[test]
    fn keyed_hash_differs_for_different_values_given_the_same_key() {
        assert_ne!(
            keyed_hash(&key(1), "value-a"),
            keyed_hash(&key(1), "value-b")
        );
    }

    #[test]
    fn keyed_hash_output_never_contains_the_plaintext_value_verbatim() {
        let plaintext = "a-fairly-long-plaintext-secret-value-1234567890";
        let digest = keyed_hash(&key(9), plaintext);
        assert!(
            !digest
                .windows(plaintext.len().min(digest.len()))
                .any(|window| *window == plaintext.as_bytes()[..window.len()]),
            "keyed hash output leaked the plaintext value verbatim"
        );
    }

    #[test]
    fn verify_keyed_hash_accepts_the_correct_key_and_plaintext() {
        let k = key(3);
        let hash = keyed_hash(&k, "correct-secret");
        assert!(verify_keyed_hash(&k, "correct-secret", &hash));
    }

    #[test]
    fn verify_keyed_hash_rejects_the_wrong_plaintext() {
        let k = key(3);
        let hash = keyed_hash(&k, "correct-secret");
        assert!(!verify_keyed_hash(&k, "wrong-secret", &hash));
    }

    #[test]
    fn verify_keyed_hash_rejects_the_wrong_key() {
        let hash = keyed_hash(&key(3), "correct-secret");
        assert!(!verify_keyed_hash(&key(4), "correct-secret", &hash));
    }

    #[test]
    fn verify_keyed_hash_rejects_a_truncated_hash() {
        let k = key(3);
        let mut hash = keyed_hash(&k, "correct-secret");
        hash.truncate(16);
        assert!(!verify_keyed_hash(&k, "correct-secret", &hash));
    }
}
