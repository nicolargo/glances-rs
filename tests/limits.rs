//! `/api/5/<plugin>/limits` and `/api/5/all/limits` — the thresholds in force.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use glances_rs::config::Config;
use glances_rs::server::build_router;
use glances_rs::state::AppState;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn limits(config: Config, scope: &str) -> (StatusCode, Value) {
    let router = build_router(AppState::new(config));
    let resp = router
        .oneshot(
            Request::get(format!("/api/5/{scope}/limits"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

fn cfg(toml: &str) -> Config {
    Config::from_toml(toml).unwrap()
}

#[tokio::test]
async fn empty_by_default() {
    // No built-in thresholds ship by default: nothing is in force.
    let (status, body) = limits(Config::default(), "mem").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({}));
    let (status, body) = limits(Config::default(), "all").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({}));
}

#[tokio::test]
async fn configured_thresholds_are_listed_partial_aware() {
    let config = cfg("[plugins.mem.thresholds.percent]\nwarning = 75.0\ncritical = 90.0\n");
    let (_, body) = limits(config, "mem").await;
    assert_eq!(
        body,
        json!({ "percent": { "warning": 75.0, "critical": 90.0 } })
    );
}

#[tokio::test]
async fn builtin_defaults_show_up_when_enabled() {
    let config = cfg("[alerts]\nuse_builtin_defaults = true\n");
    let (_, body) = limits(config.clone(), "mem").await;
    assert_eq!(
        body,
        json!({ "percent": { "careful": 50.0, "warning": 70.0, "critical": 90.0 } })
    );
    // A plugin with no watched field (system) or no default (diskio) is `{}`.
    for plugin in ["system", "diskio"] {
        let (status, body) = limits(config.clone(), plugin).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({}), "{plugin}");
    }
    // /all/limits lists only plugins that have something, never an empty one.
    let (_, all) = limits(config, "all").await;
    let all = all.as_object().unwrap();
    for plugin in ["cpu", "load", "mem", "memswap", "fs", "network"] {
        assert!(all.contains_key(plugin), "missing {plugin}");
    }
    for plugin in ["system", "uptime", "diskio"] {
        assert!(!all.contains_key(plugin), "{plugin} must be left out");
    }
    assert_eq!(all["network"]["bytes_recv"]["critical"], 0.9);
}

#[tokio::test]
async fn configured_block_replaces_the_builtin_one() {
    let config = cfg(
        "[alerts]\nuse_builtin_defaults = true\n[plugins.mem.thresholds.percent]\ncritical = 99.0\n",
    );
    let (_, body) = limits(config, "mem").await;
    assert_eq!(body, json!({ "percent": { "critical": 99.0 } }));
}

#[tokio::test]
async fn per_item_overrides_sit_under_underscore_per_item() {
    let config = cfg(
        "[plugins.fs.thresholds.percent]\nwarning = 80.0\ncritical = 90.0\n\
         [plugins.fs.thresholds_by_item.\"/home\".percent]\ncritical = 95.0\n",
    );
    let (_, body) = limits(config, "fs").await;
    assert_eq!(
        body["percent"],
        json!({ "warning": 80.0, "critical": 90.0 })
    );
    assert_eq!(
        body["_per_item"],
        json!({ "/home": { "percent": { "critical": 95.0 } } })
    );
    // No override anywhere -> the key is omitted.
    let (_, body) = limits(Config::default(), "fs").await;
    assert!(body.get("_per_item").is_none());
}

#[tokio::test]
async fn threshold_on_a_non_watched_field_is_not_a_limit() {
    // `mem.free` is not alertable: it never drives `_levels`, so it is not listed.
    let config = cfg("[plugins.mem.thresholds.free]\ncritical = 1.0\n");
    let (_, body) = limits(config, "mem").await;
    assert_eq!(body, json!({}));
}

#[tokio::test]
async fn unknown_or_disabled_plugin_is_404() {
    let (status, _) = limits(Config::default(), "nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let config = cfg("[plugins.mem]\nenabled = false\n");
    let (status, _) = limits(config.clone(), "mem").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A disabled plugin is also absent from /all/limits.
    let config = cfg("[plugins.mem]\nenabled = false\n[alerts]\nuse_builtin_defaults = true\n");
    let (_, all) = limits(config, "all").await;
    assert!(all.get("mem").is_none());
}
