//! glances-rs — lightweight monitoring server with a Glances-compatible
//! REST API and a minimal CPU/RAM footprint.
//!
//! Design rationale lives in `ARCHITECTURE.md`; the implementation roadmap
//! in `DEVELOPMENT_PLAN.md`.

pub mod api;
pub mod collector;
pub mod config;
pub mod plugins;
pub mod server;
pub mod state;

/// Application entry point, called by `main`.
///
/// Placeholder until Phase 2 wires config loading and server startup.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn run_returns_ok() {
        assert!(super::run().await.is_ok());
    }
}
