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

// --- Bearer token (`POST /api/5/token`, Glances v5 client) ------------------

fn post_token(auth: Option<String>) -> Request<Body> {
    let mut b = Request::post("/api/5/token");
    if let Some(a) = auth {
        b = b.header(header::AUTHORIZATION, a);
    }
    b.body(Body::empty()).unwrap()
}

fn basic(user: &str, pass: &str) -> String {
    format!("Basic {}", BASE64_STANDARD.encode(format!("{user}:{pass}")))
}

async fn mint(router: &Router) -> String {
    let resp = router
        .clone()
        .oneshot(post_token(Some(basic("glances", "s3cret"))))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["token_type"], "bearer");
    assert!(body["expires_in"].as_u64().unwrap() > 0);
    body["access_token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn token_without_password_configured_is_404() {
    // The Glances client treats 404 as "no auth": it proceeds without token.
    let (status, _) = send(&router_with(""), post_token(None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn token_requires_the_basic_password() {
    let router = router_with("[server]\npassword = \"s3cret\"");
    assert_eq!(
        send(&router, post_token(None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&router, post_token(Some(basic("glances", "wrong"))))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn bearer_token_authenticates_api_requests() {
    let router = router_with("[server]\npassword = \"s3cret\"");
    let token = mint(&router).await;
    let req = Request::get("/api/5/pluginslist")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&router, req).await.0, StatusCode::OK);
}

#[tokio::test]
async fn forged_or_foreign_bearer_tokens_are_401() {
    let router = router_with("[server]\npassword = \"s3cret\"");
    let token = mint(&router).await;
    // Same token against a server with another password.
    let other = router_with("[server]\npassword = \"different\"");
    for (r, t) in [(&router, "9999999999.deadbeef"), (&other, token.as_str())] {
        let req = Request::get("/api/5/pluginslist")
            .header(header::AUTHORIZATION, format!("Bearer {t}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(r, req).await.0, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn a_bearer_token_cannot_mint_another_token() {
    let router = router_with("[server]\npassword = \"s3cret\"");
    let token = mint(&router).await;
    let (status, _) = send(&router, post_token(Some(format!("Bearer {token}")))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
