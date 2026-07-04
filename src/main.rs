use std::process::ExitCode;

use kawasemi::bootstrap::bootstrap;

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
