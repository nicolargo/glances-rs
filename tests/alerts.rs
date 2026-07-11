//! Alerting end-to-end (spec §4): _levels decoration + the /api/5/alert journal.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use glances_rs::config::Config;
use glances_rs::server::build_router;
use glances_rs::state::AppState;
use serde_json::Value;
use tower::ServiceExt;

async fn get_json(router: axum::Router, path: &str) -> (StatusCode, Value) {
    let resp = router
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

// Fast engine + a mem threshold that a real machine will not breach, so the
// level is deterministic ("ok") without depending on host load.
fn config_mem_threshold() -> Config {
    Config::from_toml(
        r#"
        [collect]
        refresh = 0.05
        idle_cycles = 5
        guard_timeout = 2.0
        [alerts]
        min_duration_seconds = 0.0
        [plugins.mem.thresholds.percent]
        critical = 100.0
        "#,
    )
    .unwrap()
}

#[tokio::test]
async fn mem_payload_carries_levels_when_threshold_configured() {
    let router = build_router(AppState::new(config_mem_threshold()));
    let (status, body) = get_json(router, "/api/5/mem").await;
    assert_eq!(status, StatusCode::OK);
    // _levels has an entry for percent, with the prominent flag.
    assert_eq!(body["_levels"]["percent"]["level"], "ok");
    assert_eq!(body["_levels"]["percent"]["prominent"], true);
}

#[tokio::test]
async fn alert_route_is_empty_array_without_thresholds() {
    let router = build_router(AppState::new(Config::default()));
    let (status, body) = get_json(router, "/api/5/alert").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Array(vec![]));
}

#[tokio::test]
async fn alert_route_records_an_event_when_a_threshold_is_breached() {
    // critical = 0 percent -> any real usage breaches immediately; min_duration
    // 0 commits on the first cycle.
    let config = Config::from_toml(
        r#"
        [collect]
        refresh = 0.05
        guard_timeout = 2.0
        [alerts]
        min_duration_seconds = 0.0
        [plugins.mem.thresholds.percent]
        critical = 0.0
        "#,
    )
    .unwrap();
    let app = AppState::new(config);
    let router = build_router(app.clone());

    // Wake mem and let one cycle publish + observe.
    let _ = get_json(router.clone(), "/api/5/mem").await;

    let (status, body) = get_json(router, "/api/5/alert").await;
    assert_eq!(status, StatusCode::OK);
    let events = body.as_array().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e["plugin"] == "mem" && e["field"] == "percent" && e["level"] == "critical"),
        "expected a mem/percent critical event, got {body}"
    );
    let e = events.iter().find(|e| e["plugin"] == "mem").unwrap();
    assert_eq!(e["key"], Value::Null); // scalar plugin
    assert!(e["ts"].as_str().unwrap().ends_with('Z'));
    assert!(e["is_initial"].is_boolean());
}

#[tokio::test]
async fn alert_route_never_wakes_a_collector() {
    let app = AppState::new(Config::default());
    let router = build_router(app.clone());
    let _ = get_json(router, "/api/5/alert").await;
    // Like the probes (§6.4), the journal route is inert.
    assert_eq!(app.active_collectors().await, 0);
}
