//! Security middleware (ARCHITECTURE.md §7) for the `/api/5` sub-router:
//! HTTP Basic auth (constant-time comparison), a CORS allow-list (never a
//! wildcard), and trusted-`Host` validation. The probes live in a separate
//! router and are never wrapped by these layers (§6.4).

use crate::state::AppState;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use base64::prelude::{BASE64_STANDARD, Engine as _};
use constant_time_eq::constant_time_eq;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

/// Wrap the `/api/5` routes with the §7 security layers. Execution order on
/// a request is CORS → trusted-host → auth → handler (the last `.layer`
/// added is the outermost, so it runs first).
pub fn apply_security(router: Router, app: Arc<AppState>) -> Router {
    router
        .layer(from_fn_with_state(app.clone(), require_auth)) // innermost
        .layer(from_fn_with_state(app.clone(), require_trusted_host))
        .layer(cors_layer(&app.config.security.cors_origins)) // outermost
}

// ---------------------------------------------------------------------------
// Basic auth (§7.2)
// ---------------------------------------------------------------------------

async fn require_auth(State(app): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    match app.config.server.password.as_deref() {
        // No password configured ⇒ allow. This is safe *only* because the
        // §7.1 startup check (server::check_security) already refused to
        // start on a non-loopback bind without a password — so reaching
        // here with no password means the server is loopback-only. Do not
        // "fix" this into a deny without revisiting that invariant.
        None => next.run(request).await,
        Some(expected) => match basic_password(&request) {
            Some(provided) if constant_time_eq(provided.as_bytes(), expected.as_bytes()) => {
                next.run(request).await
            }
            // constant_time_eq, never `==`: a naive compare leaks the secret
            // byte-by-byte through a timing side channel (§7.2).
            _ => unauthorized(),
        },
    }
}

/// Extract the password from an `Authorization: Basic <base64(user:pass)>`
/// header. The username is accepted as-is — only the password is checked,
/// matching the password-only config model.
fn basic_password(request: &Request) -> Option<String> {
    let value = request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = BASE64_STANDARD.decode(encoded.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (_user, password) = text.split_once(':')?;
    Some(password.to_owned())
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        // The challenge header is mandatory on a 401 (§7.2).
        [(header::WWW_AUTHENTICATE, "Basic realm=\"glances-rs\"")],
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Trusted host (§7.4)
// ---------------------------------------------------------------------------

async fn require_trusted_host(
    State(app): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    if host_allowed(host, &app.config.security.trusted_hosts) {
        next.run(request).await
    } else {
        (StatusCode::BAD_REQUEST, "host not allowed").into_response()
    }
}

/// A *present* `Host` must be on the allow-list. A missing `Host` is allowed
/// — the threat (DNS rebinding) always carries a forged host value, so
/// there is nothing to reject when the header is absent, and HTTP clients
/// send one in practice. An empty allow-list disables the check.
fn host_allowed(host: Option<&str>, trusted: &[String]) -> bool {
    match host {
        None => true,
        Some(_) if trusted.is_empty() => true,
        Some(host) => {
            let bare = strip_port(host);
            trusted.iter().any(|t| t == host || t == bare)
        }
    }
}

fn strip_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        // IPv6 literal: "[::1]:80" or "[::1]".
        return rest.split(']').next().unwrap_or(host);
    }
    host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
}

// ---------------------------------------------------------------------------
// CORS (§7.3)
// ---------------------------------------------------------------------------

/// CORS from an explicit allow-list, **never** a wildcard (the wildcard +
/// credentials combination is a Glances CVE). An empty list leaves CORS
/// fully closed: no `Access-Control-*` headers are emitted, which is right
/// for non-browser clients (scripts, Prometheus).
fn cors_layer(origins: &[String]) -> CorsLayer {
    if origins.is_empty() {
        return CorsLayer::new();
    }
    let allowed: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
    CorsLayer::new()
        .allow_origin(allowed)
        .allow_methods([Method::GET])
        .allow_headers([header::AUTHORIZATION])
        // Safe with an explicit origin list (never with a wildcard).
        .allow_credentials(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn request_with_auth(value: &str) -> Request {
        Request::builder()
            .header(header::AUTHORIZATION, value)
            .body(Body::empty())
            .unwrap()
    }

    #[test]
    fn basic_password_extracts_the_password_part() {
        // base64("admin:s3cret")
        let encoded = BASE64_STANDARD.encode("admin:s3cret");
        let req = request_with_auth(&format!("Basic {encoded}"));
        assert_eq!(basic_password(&req).as_deref(), Some("s3cret"));
    }

    #[test]
    fn basic_password_handles_a_password_containing_a_colon() {
        let encoded = BASE64_STANDARD.encode("user:a:b:c");
        let req = request_with_auth(&format!("Basic {encoded}"));
        assert_eq!(basic_password(&req).as_deref(), Some("a:b:c"));
    }

    #[test]
    fn basic_password_rejects_malformed_headers() {
        assert_eq!(basic_password(&request_with_auth("Bearer xyz")), None);
        assert_eq!(basic_password(&request_with_auth("Basic !!!notb64")), None);
        // No colon ⇒ not a valid user:pass pair.
        let no_colon = BASE64_STANDARD.encode("nopassword");
        assert_eq!(
            basic_password(&request_with_auth(&format!("Basic {no_colon}"))),
            None
        );
    }

    #[test]
    fn host_allowed_rules() {
        let trusted = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        // Missing host is allowed (nothing to spoof).
        assert!(host_allowed(None, &trusted));
        // Exact and port-stripped matches.
        assert!(host_allowed(Some("localhost"), &trusted));
        assert!(host_allowed(Some("127.0.0.1:61208"), &trusted));
        // A forged host is rejected.
        assert!(!host_allowed(Some("evil.example.com"), &trusted));
        // Empty allow-list disables the check.
        assert!(host_allowed(Some("anything"), &[]));
    }

    #[test]
    fn strip_port_handles_ipv4_and_ipv6() {
        assert_eq!(strip_port("127.0.0.1:80"), "127.0.0.1");
        assert_eq!(strip_port("localhost"), "localhost");
        assert_eq!(strip_port("[::1]:80"), "::1");
        assert_eq!(strip_port("[::1]"), "::1");
    }
}
