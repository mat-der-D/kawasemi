mod bootstrap;
mod config;

use std::process::ExitCode;

use bootstrap::bootstrap;

#[tokio::main]
async fn main() -> ExitCode {
    match bootstrap().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("fatal: {err}");
            ExitCode::FAILURE
        }
    }
}
