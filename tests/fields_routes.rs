//! `/api/5/<plugin>/<field>` and `/api/5/<plugin>/top/<n>` (Glances v5
//! sub-resource routes) plus the `/info` `history` flag.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use glances_rs::config::Config;
use glances_rs::server::build_router;
use glances_rs::state::AppState;
use serde_json::Value;
use tower::ServiceExt;

async fn get(path: &str) -> (StatusCode, Value) {
    let router = build_router(AppState::new(Config::default()));
    let resp = router
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn object_plugin_field_is_a_single_key_object() {
    let (status, body) = get("/api/5/load/cpucore").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_object().unwrap().len(), 1);
    assert!(body["cpucore"].is_number());
}

#[tokio::test]
async fn envelope_keys_are_addressable_like_in_glances() {
    let (status, body) = get("/api/5/mem/time_since_update").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["time_since_update"].is_number());
}

#[tokio::test]
async fn collection_plugin_field_is_a_list_of_values() {
    let (status, body) = get("/api/5/network/interface_name").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["interface_name"].is_array());
}

#[tokio::test]
async fn unknown_field_and_unknown_plugin_are_404() {
    assert_eq!(get("/api/5/mem/nope").await.0, StatusCode::NOT_FOUND);
    assert_eq!(get("/api/5/nope/total").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn top_returns_a_bare_array_capped_at_n() {
    let (status, body) = get("/api/5/network/top/1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().len() <= 1);
}

#[tokio::test]
async fn top_status_codes() {
    assert_eq!(get("/api/5/mem/top/1").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        get("/api/5/network/top/0").await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        get("/api/5/network/top/x").await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn static_routes_still_win_over_the_field_route() {
    // `info` is a field-shaped segment: it must keep serving the schema.
    let (status, body) = get("/api/5/mem/info").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["total"]["unit"].is_string());
}

#[tokio::test]
async fn info_carries_history_flag() {
    let (_, body) = get("/api/5/mem/info").await;
    assert_eq!(body["percent"]["history"], true);
    assert!(body["total"].get("history").is_none());
}
