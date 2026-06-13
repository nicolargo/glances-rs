//! Security layer (ARCHITECTURE.md §7): Basic auth, trusted-host, CORS.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::prelude::{BASE64_STANDARD, Engine as _};
use glances_rs::config::Config;
use glances_rs::server::build_router;
use glances_rs::state::AppState;
use tower::ServiceExt;

fn router_with(toml: &str) -> Router {
    build_router(AppState::new(Config::from_toml(toml).unwrap()))
}

async fn send(router: &Router, request: Request<Body>) -> (StatusCode, axum::http::HeaderMap) {
    let response = router.clone().oneshot(request).await.unwrap();
    (response.status(), response.headers().clone())
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

// --- Basic auth (§7.2) ------------------------------------------------------

#[tokio::test]
async fn no_password_allows_requests() {
    // Default config: loopback, no password. The §7.1 startup check proved
    // the bind is loopback, so the auth layer lets the request through.
    let (status, _) = send(&router_with(""), get("/api/5/mem")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn password_set_without_credentials_is_401_with_challenge() {
    let router = router_with("[server]\npassword = \"s3cret\"");
    let (status, headers) = send(&router, get("/api/5/mem")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers.get(header::WWW_AUTHENTICATE).unwrap(),
        "Basic realm=\"glances-rs\""
    );
}

#[tokio::test]
async fn wrong_password_is_401() {
    let router = router_with("[server]\npassword = \"s3cret\"");
    let creds = BASE64_STANDARD.encode("admin:wrong");
    let req = Request::get("/api/5/mem")
        .header(header::AUTHORIZATION, format!("Basic {creds}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&router, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn correct_password_is_200_regardless_of_username() {
    let router = router_with("[server]\npassword = \"s3cret\"");
    let creds = BASE64_STANDARD.encode("anyuser:s3cret");
    let req = Request::get("/api/5/mem")
        .header(header::AUTHORIZATION, format!("Basic {creds}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&router, req).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn probes_are_reachable_without_auth_even_with_a_password() {
    // The §6.4 invariant: probes live outside the authenticated sub-router.
    let router = router_with("[server]\npassword = \"s3cret\"");
    for path in ["/status", "/healthz"] {
        let (status, _) = send(&router, get(path)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
    }
}

// --- Trusted host (§7.4) ----------------------------------------------------

#[tokio::test]
async fn spoofed_host_is_rejected() {
    let req = Request::get("/api/5/mem")
        .header(header::HOST, "evil.example.com")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&router_with(""), req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn trusted_host_with_port_is_accepted() {
    let req = Request::get("/api/5/mem")
        .header(header::HOST, "127.0.0.1:61208")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&router_with(""), req).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn configured_host_is_accepted() {
    let router = router_with("[security]\ntrusted_hosts = [\"monitor.example.com\"]");
    let req = Request::get("/api/5/mem")
        .header(header::HOST, "monitor.example.com")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&router, req).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn probes_ignore_the_host_check() {
    let req = Request::get("/status")
        .header(header::HOST, "evil.example.com")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&router_with(""), req).await;
    assert_eq!(status, StatusCode::OK);
}

// --- CORS (§7.3) ------------------------------------------------------------

#[tokio::test]
async fn cors_closed_by_default() {
    let req = Request::get("/api/5/mem")
        .header(header::ORIGIN, "https://dash.example.com")
        .body(Body::empty())
        .unwrap();
    let (status, headers) = send(&router_with(""), req).await;
    assert_eq!(status, StatusCode::OK);
    // Empty allow-list ⇒ no CORS headers, never a wildcard.
    assert!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}

#[tokio::test]
async fn cors_allows_a_listed_origin() {
    let router = router_with("[security]\ncors_origins = [\"https://dash.example.com\"]");
    let req = Request::get("/api/5/mem")
        .header(header::ORIGIN, "https://dash.example.com")
        .body(Body::empty())
        .unwrap();
    let (status, headers) = send(&router, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "https://dash.example.com"
    );
}
