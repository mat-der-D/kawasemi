//! Entrypoint. Calls `bootstrap()` and converts its `Result` into a process
//! exit code (Requirement 1.1; full startup/shutdown behavior lands in
//! later tasks, notably 7.4).

mod bootstrap;

use bootstrap::bootstrap;

#[tokio::main]
async fn main() {
    if let Err(err) = bootstrap().await {
        eprintln!("kawasemi: fatal error during startup: {err}");
        std::process::exit(1);
    }

    std::process::exit(0);
}
