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
async fn all_contract_plugins_answer_cold_with_real_data() {
    let (router, _) = make_app("");

    // Objects for the scalar plugins...
    for path in ["/api/5/cpu", "/api/5/load", "/api/5/mem"] {
        let (status, value) = get(&router, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(value.is_object(), "{path}: {value}");
    }
    // ...an array for the collection plugin.
    let (status, value) = get(&router, "/api/5/network").await;
    assert_eq!(status, StatusCode::OK);
    assert!(value.is_array());
}

#[tokio::test]
async fn cold_cpu_carries_a_real_rate() {
    // The §5.5 warm-up promise: the first response is a valid percentage,
    // not a bogus or empty reading.
    let (router, _) = make_app("");
    let (status, value) = get(&router, "/api/5/cpu").await;
    assert_eq!(status, StatusCode::OK);
    let total = value["total"].as_f64().unwrap();
    assert!((0.0..=100.0).contains(&total), "total = {total}");
    assert!(value["time_since_update"].as_f64().unwrap() > 0.0);
}

#[tokio::test]
async fn cold_network_carries_rates_for_existing_interfaces() {
    // Same promise for the collection plugin: the self-bootstrap means
    // the first response already has one item per (visible) interface.
    let (router, _) = make_app("");
    let (status, value) = get(&router, "/api/5/network").await;
    assert_eq!(status, StatusCode::OK);
    let items = value.as_array().unwrap();
    assert!(!items.is_empty(), "expected at least one interface");
    for item in items {
        for field in [
            "interface_name",
            "bytes_recv",
            "bytes_recv_gauge",
            "bytes_recv_rate_per_sec",
            "bytes_sent",
            "bytes_all",
            "time_since_update",
        ] {
            assert!(!item[field].is_null(), "missing field {field}: {item}");
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_cpu_and_network_match_the_full_glances_field_set() {
    let (router, _) = make_app("");

    let (_, cpu) = get(&router, "/api/5/cpu").await;
    for field in [
        "user",
        "system",
        "idle",
        "iowait",
        "ctx_switches",
        "interrupts",
    ] {
        assert!(!cpu[field].is_null(), "cpu missing {field}: {cpu}");
    }

    let (_, net) = get(&router, "/api/5/network").await;
    let item = &net.as_array().unwrap()[0];
    for field in ["is_up", "speed", "alias"] {
        assert!(
            item.get(field).is_some(),
            "network item missing {field}: {item}"
        );
    }
}

#[tokio::test]
async fn network_alias_from_config_is_surfaced() {
    // The container always has a loopback interface; alias it.
    let (router, _) = make_app("[plugins.network.alias]\nlo = \"loopback\"");
    let (_, value) = get(&router, "/api/5/network").await;
    let lo = value
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["interface_name"] == "lo");
    if let Some(lo) = lo {
        assert_eq!(lo["alias"], "loopback");
    }
}

#[tokio::test]
async fn network_hide_filter_is_applied() {
    // The container always has a loopback interface; hide it.
    let (router, _) = make_app("[plugins.network]\nhide = [\"^lo$\"]");
    let (_, value) = get(&router, "/api/5/network").await;
    let names: Vec<&str> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["interface_name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"lo"), "lo should be hidden: {names:?}");
}

#[tokio::test]
async fn disabled_plugin_is_404_and_absent_from_pluginslist() {
    let (router, _) = make_app("[plugins.mem]\nenabled = false");
    let (status, _) = get(&router, "/api/5/mem").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, value) = get(&router, "/api/5/pluginslist").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, serde_json::json!(["cpu", "load", "network"]));
}

#[tokio::test]
async fn pluginslist_lists_enabled_plugins() {
    let (router, app) = make_app("");
    let (status, value) = get(&router, "/api/5/pluginslist").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, serde_json::json!(["cpu", "load", "mem", "network"]));
    // pluginslist is names-only: it must not wake anything.
    assert_eq!(app.active_collectors().await, 0);
}
