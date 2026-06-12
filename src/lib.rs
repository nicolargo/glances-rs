//! glances-rs — lightweight monitoring server with a Glances-compatible
//! REST API and a minimal CPU/RAM footprint.
//!
//! Design rationale lives in `ARCHITECTURE.md`; the implementation roadmap
//! in `DEVELOPMENT_PLAN.md`; the frozen API contract in `docs/api.md`.

pub mod api;
pub mod collector;
pub mod config;
pub mod plugins;
pub mod server;
pub mod state;

use config::Config;

const HELP: &str = "\
glances-rs — lightweight monitoring server

Usage: glances-rs [OPTIONS]

Options:
  -c, --config <PATH>  Configuration file (default: discovery order in docs/api.md)
  -h, --help           Print this help
  -V, --version        Print the version

Logging is controlled with RUST_LOG (default: info).";

/// Application entry point, called by `main`.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = config::parse_args(std::env::args().skip(1))?;
    if args.help {
        println!("{HELP}");
        return Ok(());
    }
    if args.version {
        println!("glances-rs {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    init_tracing();

    let (config, path) = Config::load(args.config)?;
    match &path {
        Some(path) => tracing::info!("configuration loaded from {}", path.display()),
        None => tracing::info!("no configuration file found, using built-in defaults"),
    }

    server::serve(config).await?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
}
