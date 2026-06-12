use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match glances_rs::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("glances-rs: {err}");
            ExitCode::FAILURE
        }
    }
}
