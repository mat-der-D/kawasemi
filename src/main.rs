//! Entrypoint. Calls `bootstrap()` and converts its `Result` into a process
//! exit code (Requirement 1.1): `Err` prints the aggregated `BootstrapError`
//! (whose `Display` already renders the failing stage's own diagnostic
//! detail, e.g. which config field was missing/malformed, or the underlying
//! `sqlx` error) and exits non-zero (Requirement 1.2); `Ok` exits zero
//! (Requirement 1.5, once graceful shutdown completes).
//!
//! `bootstrap()` itself now lives on the `kawasemi` library target
//! (`src/bootstrap.rs`, task 7.4) rather than as a binary-local module, so
//! this file only imports and calls it — see `src/lib.rs`'s doc comment for
//! why.

use kawasemi::bootstrap::bootstrap;

#[tokio::main]
async fn main() {
    if let Err(err) = bootstrap().await {
        eprintln!("kawasemi: fatal error during startup: {err}");
        std::process::exit(1);
    }

    std::process::exit(0);
}
