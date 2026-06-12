//! axum `Router` construction and startup, including the §7.1 startup check.

use crate::config::Config;
use crate::state::AppState;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

#[derive(Debug)]
pub enum StartupError {
    /// §7.1: non-loopback bind without a password — refuse to start.
    OpenBindWithoutPassword(IpAddr),
    Bind(SocketAddr, std::io::Error),
    Serve(std::io::Error),
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenBindWithoutPassword(bind) => write!(
                f,
                "refusing to start: bind address {bind} is reachable from the network \
                 but no password is configured. Set [server].password, or bind to a \
                 loopback address (ARCHITECTURE.md §7.1)"
            ),
            Self::Bind(addr, err) => write!(f, "cannot bind {addr}: {err}"),
            Self::Serve(err) => write!(f, "server error: {err}"),
        }
    }
}

impl std::error::Error for StartupError {}

/// The §7.1 grid has four cases; the only refusal is a non-loopback bind
/// with no password. A hard error — not a log warning — is what turns
/// "closed by default" from an intention into a guarantee.
pub fn check_security(bind: IpAddr, password: Option<&str>) -> Result<(), StartupError> {
    if !bind.is_loopback() && password.is_none() {
        return Err(StartupError::OpenBindWithoutPassword(bind));
    }
    Ok(())
}

/// Liveness probes (§6.4): inert by construction. They live in their own
/// sub-router, merged outside the `/api/5` middleware stack, so the auth,
/// CORS and trusted-host layers added in Phase 6 can never cover them —
/// and they never touch plugin state, so they can never wake a collector.
fn probes_router() -> Router {
    Router::new()
        .route("/status", get(|| async { StatusCode::OK }))
        .route("/healthz", get(|| async { StatusCode::OK }))
}

/// Full application router: inert probes + the `/api/5` sub-router.
pub fn build_router(app: Arc<AppState>) -> Router {
    Router::new()
        .merge(probes_router())
        .merge(crate::api::api_router(app))
}

/// Run the §7.1 check, bind, and serve until SIGINT/SIGTERM.
pub async fn serve(config: Config) -> Result<(), StartupError> {
    check_security(config.server.bind, config.server.password.as_deref())?;

    let addr = SocketAddr::new(config.server.bind, config.server.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| StartupError::Bind(addr, err))?;
    tracing::info!("listening on http://{addr}");

    let app = AppState::new(config);
    axum::serve(listener, build_router(app))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(StartupError::Serve)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;

    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    // The four §7.1 cases.
    #[test]
    fn loopback_without_password_is_ok() {
        assert!(check_security(ip("127.0.0.1"), None).is_ok());
        assert!(check_security(IpAddr::V6(Ipv6Addr::LOCALHOST), None).is_ok());
    }

    #[test]
    fn loopback_with_password_is_ok() {
        assert!(check_security(ip("127.0.0.1"), Some("secret")).is_ok());
    }

    #[test]
    fn non_loopback_with_password_is_ok() {
        assert!(check_security(ip("0.0.0.0"), Some("secret")).is_ok());
        assert!(check_security(ip("192.168.1.10"), Some("secret")).is_ok());
    }

    #[test]
    fn non_loopback_without_password_refuses_to_start() {
        assert!(matches!(
            check_security(ip("0.0.0.0"), None),
            Err(StartupError::OpenBindWithoutPassword(_))
        ));
        assert!(matches!(
            check_security(ip("192.168.1.10"), None),
            Err(StartupError::OpenBindWithoutPassword(_))
        ));
        assert!(matches!(
            check_security(IpAddr::V6(Ipv6Addr::UNSPECIFIED), None),
            Err(StartupError::OpenBindWithoutPassword(_))
        ));
    }
}
