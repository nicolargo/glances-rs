//! Probe routes (§6.4): always 200, no auth, no plugin wake-up.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use glances_rs::config::Config;
use glances_rs::server::build_router;
use glances_rs::state::AppState;
use tower::ServiceExt;

async fn get(path: &str) -> StatusCode {
    let router = build_router(AppState::new(Config::default()));
    let response = router
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    response.status()
}

#[tokio::test]
async fn status_responds_200() {
    assert_eq!(get("/status").await, StatusCode::OK);
}

#[tokio::test]
async fn healthz_responds_200() {
    assert_eq!(get("/healthz").await, StatusCode::OK);
}

#[tokio::test]
async fn unknown_route_is_404() {
    assert_eq!(get("/nope").await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn probes_work_even_with_a_password_configured() {
    // Once auth lands (Phase 6) this test guards the §6.4 invariant:
    // probes are outside the authenticated sub-router.
    let mut config = Config::default();
    config.server.password = Some("secret".into());
    let response = build_router(AppState::new(config))
        .oneshot(Request::get("/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn probes_never_wake_a_collector() {
    let app = AppState::new(Config::default());
    let router = build_router(app.clone());
    for path in ["/status", "/healthz"] {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(app.active_collectors().await, 0);
}
