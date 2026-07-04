//! Secret-value wrapper (Requirement 2.5).
//!
//! `Secret<T>` holds a sensitive configuration value (for example a
//! database connection string that embeds credentials) and guarantees that
//! `Debug` and `Display` never render the plaintext, even if the value is
//! logged incidentally (e.g. via `{:?}` in a `tracing` event or a panic
//! message). Reaching the plaintext requires an explicit, named call to
//! [`Secret::expose`].

use std::fmt;

/// A masked wrapper around a secret-bearing value of type `T`.
///
/// `Secret<T>` deliberately does not implement `Deref`, `AsRef`, or any
/// other trait that would let the inner value leak implicitly into a
/// formatter or a generic logging call. The only way to read the value is
/// [`Secret::expose`], which makes every access site explicit and
/// grep-able.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps `value` as a secret.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Returns a reference to the wrapped plaintext value.
    ///
    /// Callers must have a specific, deliberate need to read the secret
    /// (e.g. actually opening the database connection); the name is chosen
    /// to make every call site stand out under review or `grep`.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Consumes the wrapper and returns the plaintext value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

const MASK: &str = "***REDACTED***";

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({MASK})")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{MASK}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_does_not_contain_plaintext() {
        let secret = Secret::new("postgres://user:sup3r-secret-pw@host/db".to_string());

        let debug_output = format!("{secret:?}");

        assert!(
            !debug_output.contains("sup3r-secret-pw"),
            "Debug output leaked the secret: {debug_output}"
        );
        assert!(debug_output.contains(MASK));
    }

    #[test]
    fn display_output_does_not_contain_plaintext() {
        let secret = Secret::new("postgres://user:sup3r-secret-pw@host/db".to_string());

        let display_output = format!("{secret}");

        assert!(
            !display_output.contains("sup3r-secret-pw"),
            "Display output leaked the secret: {display_output}"
        );
        assert!(display_output.contains(MASK));
    }

    #[test]
    fn expose_returns_the_original_plaintext() {
        let secret = Secret::new("postgres://user:sup3r-secret-pw@host/db".to_string());

        assert_eq!(secret.expose(), "postgres://user:sup3r-secret-pw@host/db");
    }

    #[test]
    fn into_inner_returns_the_original_plaintext() {
        let secret = Secret::new("postgres://user:sup3r-secret-pw@host/db".to_string());

        assert_eq!(secret.into_inner(), "postgres://user:sup3r-secret-pw@host/db");
    }

    #[test]
    fn cloned_secret_still_masks_on_debug() {
        let secret = Secret::new("sup3r-secret-pw".to_string());
        let cloned = secret.clone();

        assert!(!format!("{cloned:?}").contains("sup3r-secret-pw"));
    }
}
