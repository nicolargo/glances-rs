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
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
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
        Some(expected) => {
            // Basic (any client) or a Bearer token minted by `POST /token`
            // (Glances v5 clients). Both are checked in constant time.
            let ok = match basic_password(&request) {
                Some(provided) => constant_time_eq(provided.as_bytes(), expected.as_bytes()),
                None => bearer_token(&request).is_some_and(|t| token_valid(expected, t, now())),
            };
            // constant_time_eq, never `==`: a naive compare leaks the secret
            // byte-by-byte through a timing side channel (§7.2).
            if ok {
                next.run(request).await
            } else {
                unauthorized()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Bearer tokens (`POST /api/5/token`, Glances v5 client compatibility)
// ---------------------------------------------------------------------------

/// Token lifetime. The Glances client transparently asks for a new token
/// once on a `401`, so a modest lifetime costs nothing.
const TOKEN_TTL_SECS: u64 = 3600;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `HMAC-SHA256(key = password, "glances-rs-token|<exp>")`, hex-encoded.
/// Stateless: nothing is stored server-side, a restart keeps tokens valid,
/// and changing the password revokes every outstanding token.
fn token_tag(password: &str, exp: u64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(password.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(format!("glances-rs-token|{exp}").as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A token is `<expiry unix secs>.<tag>`; the client treats it as opaque.
fn make_token(password: &str, now: u64) -> String {
    let exp = now + TOKEN_TTL_SECS;
    format!("{exp}.{}", token_tag(password, exp))
}

fn token_valid(password: &str, token: &str, now: u64) -> bool {
    let Some((exp, tag)) = token.split_once('.') else {
        return false;
    };
    let Ok(exp) = exp.parse::<u64>() else {
        return false;
    };
    constant_time_eq(tag.as_bytes(), token_tag(password, exp).as_bytes()) && exp > now
}

fn bearer_token(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// `POST /api/5/token` — trade the Basic credentials for a Bearer token.
/// `404` when no password is configured (nothing to authenticate: the
/// Glances client then goes on without a token). A Bearer token alone is
/// refused, so a token cannot be refreshed forever without the password.
pub async fn issue_token(State(app): State<Arc<AppState>>, request: Request) -> Response {
    let Some(expected) = app.config.server.password.as_deref() else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({ "detail": "authentication not configured" })),
        )
            .into_response();
    };
    match basic_password(&request) {
        Some(p) if constant_time_eq(p.as_bytes(), expected.as_bytes()) => axum::Json(json!({
            "access_token": make_token(expected, now()),
            "token_type": "bearer",
            "expires_in": TOKEN_TTL_SECS,
        }))
        .into_response(),
        _ => unauthorized(),
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
    fn token_roundtrip_expiry_and_tamper() {
        let t = make_token("pw", 1_000);
        assert!(token_valid("pw", &t, 1_000));
        assert!(token_valid("pw", &t, 1_000 + TOKEN_TTL_SECS - 1));
        // Expired.
        assert!(!token_valid("pw", &t, 1_000 + TOKEN_TTL_SECS));
        // Wrong password (e.g. rotated) revokes it.
        assert!(!token_valid("other", &t, 1_000));
        // Extending the expiry without the key fails.
        let (_, tag) = t.split_once('.').unwrap();
        assert!(!token_valid("pw", &format!("99999999999.{tag}"), 1_000));
        // Garbage.
        assert!(!token_valid("pw", "nodot", 0));
        assert!(!token_valid("pw", "x.y", 0));
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
