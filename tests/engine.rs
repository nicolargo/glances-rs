//! The collection engine's HTTP contract (ARCHITECTURE.md §3, §6):
//! lazy wake-up, first-request-waits, idle stop with retained store,
//! 404 semantics, fine-grained exposure.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use glances_rs::config::Config;
use glances_rs::plugins::PluginId;
use glances_rs::server::build_router;
use glances_rs::state::AppState;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

fn fast_config(extra: &str) -> Config {
    Config::from_toml(&format!(
        r#"
        [collect]
        refresh = 0.02
        idle_cycles = 2
        guard_timeout = 1.0
        {extra}
        "#
    ))
    .unwrap()
}

fn make_app(extra: &str) -> (Router, Arc<AppState>) {
    let app = AppState::new(fast_config(extra));
    (build_router(app.clone()), app)
}

async fn get(router: &Router, path: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn cold_request_waits_and_returns_real_data() {
    let (router, app) = make_app("");
    assert_eq!(app.active_collectors().await, 0);

    let (status, value) = get(&router, "/api/5/mem").await;
    assert_eq!(status, StatusCode::OK);
    // Never null/empty (§6.2): the request waited for the first cycle.
    assert!(value.is_object());
    assert!(value["total"].as_u64().unwrap() > 0);
    assert_eq!(app.active_collectors().await, 1);
}

#[tokio::test]
async fn collector_stops_when_idle_but_store_is_retained() {
    let (router, app) = make_app("");
    let (status, _) = get(&router, "/api/5/mem").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.active_collectors().await, 1);

    // 2 cycles of 20 ms without requests: the collector must stop...
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(app.active_collectors().await, 0);
    // ...but the snapshot survives the stop (§3.2).
    assert!(app.snapshot(PluginId::Mem).await.is_some());

    // Re-wake: the same route works again.
    let (status, value) = get(&router, "/api/5/mem").await;
    assert_eq!(status, StatusCode::OK);
    assert!(value.is_object());
    assert_eq!(app.active_collectors().await, 1);
}

#[tokio::test]
async fn repeated_requests_keep_the_collector_alive() {
    let (router, app) = make_app("");
    for _ in 0..5 {
        let (status, _) = get(&router, "/api/5/mem").await;
        assert_eq!(status, StatusCode::OK);
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    assert_eq!(app.active_collectors().await, 1);
}

#[tokio::test]
async fn unknown_plugin_is_404() {
    let (router, _) = make_app("");
    let (status, value) = get(&router, "/api/5/bogus").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(value["detail"].is_string());
}

#[tokio::test]
async fn contract_plugin_not_yet_implemented_is_404() {
    // cpu/load/network exist in the contract but land in Phase 4.
    let (router, _) = make_app("");
    let (status, _) = get(&router, "/api/5/cpu").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn disabled_plugin_is_404_and_absent_from_pluginslist() {
    let (router, _) = make_app("[plugins.mem]\nenabled = false");
    let (status, _) = get(&router, "/api/5/mem").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, value) = get(&router, "/api/5/pluginslist").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, serde_json::json!([]));
}

#[tokio::test]
async fn pluginslist_lists_enabled_plugins() {
    let (router, app) = make_app("");
    let (status, value) = get(&router, "/api/5/pluginslist").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, serde_json::json!(["mem"]));
    // pluginslist is names-only: it must not wake anything.
    assert_eq!(app.active_collectors().await, 0);
}
