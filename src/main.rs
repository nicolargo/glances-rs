use std::process::ExitCode;

// A `current_thread` runtime: glances-rs' workload is trivial and I/O-bound
// (a handful of `/proc`/`sysinfo` reads plus a few KB of JSON), so the default
// multi-thread runtime's one-worker-per-core (16 idle threads on a 16-core
// host) buys nothing and costs RSS. The §5.2 concurrent `/all` wake is async
// concurrency, not parallelism — it runs identically on a single thread.
// Measured: −18% RSS at rest, −47% RSS under 100 req/s. (ARCHITECTURE.md §9)
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match glances_rs::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("glances-rs: {err}");
            ExitCode::FAILURE
        }
    }
}
