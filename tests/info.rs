//! `/api/5/<plugin>/info` — static field schema (design spec 2026-08-02).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use glances_rs::config::Config;
use glances_rs::server::build_router;
use glances_rs::state::AppState;
use serde_json::Value;
use tower::ServiceExt;

async fn info(config: Config, plugin: &str) -> (StatusCode, Value) {
    let router = build_router(AppState::new(config));
    let resp = router
        .oneshot(
            Request::get(format!("/api/5/{plugin}/info"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

#[tokio::test]
async fn mem_info_scalar_shape() {
    let (status, body) = info(Config::default(), "mem").await;
    assert_eq!(status, StatusCode::OK);
    let percent = &body["percent"];
    assert_eq!(
        percent["description"],
        "Percentage usage calculated as (total - available) / total * 100."
    );
    assert_eq!(percent["unit"], "percent");
    assert_eq!(percent["watched"], true);
    assert_eq!(percent["watch_direction"], "high");
    assert_eq!(percent["prominent"], true);
    // no built-in default thresholds ship (config-only).
    assert!(percent.get("default_thresholds").is_none());
    // short_name only where defined.
    assert_eq!(body["available"]["short_name"], "avail");
    assert!(body["total"].get("short_name").is_none());
    // scalar plugin: no primary_key anywhere.
    assert!(
        body.as_object()
            .unwrap()
            .values()
            .all(|f| f.get("primary_key").is_none())
    );
}

#[tokio::test]
async fn network_info_collection_shape() {
    let (status, body) = info(Config::default(), "network").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["interface_name"]["primary_key"], true);
    assert_eq!(body["interface_name"]["unit"], "string");
    let recv = &body["bytes_recv"];
    assert_eq!(recv["rate"], true);
    assert_eq!(recv["watched"], true);
    assert_eq!(recv["prominent"], false);
    assert_eq!(recv["normalize_by"], "bytes_speed_rate_per_sec");
    assert_eq!(recv["unit"], "bytespers");
    // glances-rs-only field is described.
    assert!(body["bytes_all"]["description"].is_string());
}

#[tokio::test]
async fn cpu_info_internal_and_no_unimplemented_keys() {
    let (_, body) = info(Config::default(), "cpu").await;
    assert_eq!(body["cpucore"]["internal"], true);
    for (_, f) in body.as_object().unwrap() {
        assert!(f.get("history").is_none(), "history must not be emitted");
        assert!(
            f.get("strict_thresholds").is_none(),
            "strict_thresholds must not be emitted"
        );
    }
    // parity fix visible in /info: ctx_switches normalizes by cpucore.
    assert_eq!(body["ctx_switches"]["normalize_by"], "cpucore");
    assert_eq!(body["iowait"]["prominent"], true);
    assert_eq!(body["steal"]["prominent"], false);
}

#[tokio::test]
async fn default_thresholds_reflects_config() {
    let config =
        Config::from_toml("[plugins.mem.thresholds.percent]\nwarning = 75.0\ncritical = 90.0\n")
            .unwrap();
    let (_, body) = info(config, "mem").await;
    let dt = &body["percent"]["default_thresholds"];
    assert_eq!(dt["warning"], 75.0);
    assert_eq!(dt["critical"], 90.0);
    // partial: careful was not configured -> absent.
    assert!(dt.get("careful").is_none());
}

#[tokio::test]
async fn unknown_plugin_is_404() {
    let (status, _) = info(Config::default(), "nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn info_never_wakes_a_collector() {
    let app = AppState::new(Config::default());
    let router = build_router(app.clone());
    let _ = router
        .oneshot(Request::get("/api/5/mem/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(app.active_collectors().await, 0);
}

// Load-bearing invariant (all platforms): no emitted field is undocumented.
#[tokio::test]
async fn every_emitted_field_is_documented() {
    for plugin in ["mem", "fs"] {
        let router = build_router(AppState::new(Config::default()));
        let data_resp = router
            .oneshot(
                Request::get(format!("/api/5/{plugin}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(data_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let data: Value = serde_json::from_slice(&bytes).unwrap();
        let (_, schema) = info(Config::default(), plugin).await;
        let described: std::collections::HashSet<&str> = schema
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        // Scalar: fields at top level. Collection: under "data"[0].
        let sample = data.get("data").and_then(|d| d.get(0)).unwrap_or(&data);
        for key in sample.as_object().unwrap().keys() {
            if key == "time_since_update" || key == "_levels" {
                continue;
            }
            assert!(
                described.contains(key.as_str()),
                "{plugin}.{key} undocumented"
            );
        }
    }
}
