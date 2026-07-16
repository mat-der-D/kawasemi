//! Secret-masking wrapper type (Requirement 2.5).
//!
//! `Secret<T>` holds a value that must never be exposed via `Debug`,
//! `Display`, or structured log output. `tracing`'s field-recording paths
//! (including the `?field` / `Value::record` machinery) format non-primitive
//! values through `std::fmt::Debug`, so masking `Debug` (and `Display`)
//! here is sufficient to keep a `Secret<T>` from leaking through `tracing`
//! as well — including when it is a field of a struct that derives `Debug`,
//! since the derive calls each field's own `Debug` impl rather than
//! re-serializing the inner value itself.
//!
//! The real value remains reachable via [`Secret::expose_secret`] for
//! legitimate use (e.g. handing a database URL to a connection pool). The
//! name is deliberately loud so accidental use stands out at the call site.

use std::fmt;

/// Fixed redaction marker printed in place of the wrapped value by both the
/// `Debug` and `Display` impls.
const REDACTED: &str = "Secret(***)";

/// Wraps a value of type `T` so that formatting it (`{:?}` or `{}`) never
/// prints the real contents, while still allowing deliberate access via
/// [`Secret::expose_secret`].
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps `value` as a secret.
    pub fn new(value: T) -> Self {
        Secret(value)
    }

    /// Returns a reference to the real underlying value. Named explicitly
    /// (rather than implementing `Deref`, which would allow the value to
    /// leak through implicit coercions) so every read is a visible,
    /// deliberate call site.
    pub fn expose_secret(&self) -> &T {
        &self.0
    }
}

/// Never prints the wrapped value, regardless of `T`. Because this impl
/// does not require `T: Debug`, there is no way for a caller to derive an
/// impl that accidentally re-exposes it.
impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Never prints the wrapped value, regardless of `T`, matching the `Debug`
/// impl above.
impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAINTEXT: &str = "supersecret-db-password";

    #[test]
    fn debug_does_not_expose_value() {
        let secret = Secret::new(PLAINTEXT.to_string());
        let formatted = format!("{:?}", secret);
        assert!(
            !formatted.contains(PLAINTEXT),
            "Debug output leaked the plaintext secret: {formatted}"
        );
        assert_eq!(formatted, REDACTED);
    }

    #[test]
    fn display_does_not_expose_value() {
        let secret = Secret::new(PLAINTEXT.to_string());
        let formatted = format!("{}", secret);
        assert!(
            !formatted.contains(PLAINTEXT),
            "Display output leaked the plaintext secret: {formatted}"
        );
        assert_eq!(formatted, REDACTED);
    }

    #[test]
    fn expose_secret_returns_the_real_value() {
        let secret = Secret::new(PLAINTEXT.to_string());
        assert_eq!(secret.expose_secret().as_str(), PLAINTEXT);
    }

    /// A struct that derives `Debug` (like `DatabaseConfig` does) must not
    /// leak a `Secret<T>` field's contents when the whole struct is
    /// formatted — the derive calls the field's own `Debug` impl.
    // The field is exercised via the struct's `Debug` derive under test
    // below, not read directly, which dead-code analysis doesn't detect.
    #[derive(Debug)]
    #[allow(dead_code)]
    struct HoldsASecret {
        password: Secret<String>,
    }

    #[test]
    fn derived_debug_on_containing_struct_does_not_leak_field() {
        let holder = HoldsASecret {
            password: Secret::new(PLAINTEXT.to_string()),
        };
        let formatted = format!("{:?}", holder);
        assert!(
            !formatted.contains(PLAINTEXT),
            "derived Debug on containing struct leaked the field: {formatted}"
        );
        assert!(formatted.contains(REDACTED));
    }

    /// `tracing`'s `?field` recording path formats values through
    /// `std::fmt::Debug` via a trait object (the same mechanism
    /// `tracing::field::debug` uses internally). Exercising that path
    /// directly proves masking survives going through `dyn Debug`, not just
    /// a concrete-type `format!` call.
    #[test]
    fn formatting_through_dyn_debug_does_not_leak_value() {
        let secret = Secret::new(PLAINTEXT.to_string());
        let as_debug: &dyn fmt::Debug = &secret;
        let formatted = format!("{as_debug:?}");
        assert!(
            !formatted.contains(PLAINTEXT),
            "dyn Debug formatting leaked the plaintext secret: {formatted}"
        );
        assert_eq!(formatted, REDACTED);
    }
}
