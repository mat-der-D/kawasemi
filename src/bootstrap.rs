//! Composition root for the application.
//!
//! `bootstrap()` will grow, in later tasks, into the sequential startup
//! pipeline described in the core-runtime design (config -> telemetry ->
//! db pool -> migrations -> runtime context -> http server). At this stage
//! (task 1.1) it is an intentional no-op placeholder: it exists purely so
//! that `src/main.rs` has a single, stable entry point to call and convert
//! into a process exit code, and so subsequent tasks can incrementally fill
//! in its body without changing `main.rs`.

/// Errors that can occur during application bootstrap.
///
/// This is a minimal placeholder. Later tasks (config loading, telemetry
/// init, db pool establishment, migrations, server startup) will extend
/// this type or replace it with the unified `AppError` type (Requirement 6),
/// as appropriate for the composition root's error reporting needs.
#[derive(Debug)]
pub struct BootstrapError(pub String);

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BootstrapError {}

/// Runs the application composition root.
///
/// Placeholder for task 1.1: does not yet load config, initialize
/// telemetry, establish a db pool, run migrations, build a runtime
/// context, or start an http server. Those responsibilities belong to
/// later tasks (2.1, 3.1, 4.1, 4.2, 5.x, 7.1-7.4) and will be composed
/// here incrementally.
pub async fn bootstrap() -> Result<(), BootstrapError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_succeeds_in_default_placeholder_state() {
        assert!(bootstrap().await.is_ok());
    }
}
