//! REST API route handlers (ARCHITECTURE.md §6). Status-code grid (§6.2):
//! `200` always carries real data, `404` unknown/unavailable plugin,
//! `503` collection did not start within the guard timeout.

pub mod security;

use crate::collector::{EnsureError, ensure_plugin};
use crate::plugins::PluginId;
use crate::plugins::fields::{FieldInfo, fields};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use bytes::Bytes;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use tokio::task::JoinSet;

/// The `/api/5` sub-router, wrapped in the §7 security layers (auth, CORS,
/// trusted host). The probes are merged separately at the top level, so
/// they are never covered by these layers (§6.4).
pub fn api_router(app: Arc<AppState>) -> Router {
    let routes = Router::new()
        .route("/api/5/pluginslist", get(plugins_list))
        .route("/api/5/all", get(all_stats))
        .route("/api/5/alert", get(alert_history))
        .route("/api/5/{plugin}/info", get(plugin_info))
        .route("/api/5/{plugin}", get(plugin_stats))
        .with_state(app.clone());
    security::apply_security(routes, app)
}

/// `GET /api/5/pluginslist` — sorted names of the plugins this server
/// exposes (implemented and enabled). Cheap: names only, no wake-up.
async fn plugins_list(State(app): State<Arc<AppState>>) -> Json<Vec<&'static str>> {
    let mut names: Vec<&'static str> = PluginId::ALL
        .into_iter()
        .filter(|id| app.is_registered(*id))
        .map(PluginId::as_str)
        .collect();
    names.sort_unstable();
    Json(names)
}

/// `GET /api/5/all` — every registered plugin at once, as an object keyed
/// by plugin name (matching Glances' `store.as_dict()`).
///
/// Plugins are woken **concurrently** (§5.2): the latency is the slowest
/// plugin's warm-up, not the sum. **Partial-failure policy** (§6.3): a
/// plugin that errs (timeout or not-registered) is simply absent from the
/// object and the response is still `200` — an aggregate route must not
/// collapse for one slow component.
async fn all_stats(State(app): State<Arc<AppState>>) -> Response {
    let mut set = JoinSet::new();
    for id in PluginId::ALL {
        if !app.is_registered(id) {
            continue;
        }
        let app = app.clone();
        set.spawn(async move { (id, ensure_plugin(&app, id).await) });
    }

    let mut parts: Vec<(&'static str, Bytes)> = Vec::new();
    while let Some(joined) = set.join_next().await {
        if let Ok((id, Ok(body))) = joined {
            parts.push((id.as_str(), body));
        }
    }
    // Keys must come out sorted, matching the previous BTreeMap-backed
    // serde_json::Map key order (no preserve_order feature).
    parts.sort_by_key(|(name, _)| *name);

    // Each `body` is already a serialized JSON object; compose the
    // aggregate without re-parsing or re-serializing any plugin.
    let mut out = Vec::with_capacity(
        2 + parts
            .iter()
            .map(|(n, b)| n.len() + b.len() + 4)
            .sum::<usize>(),
    );
    out.push(b'{');
    for (i, (name, body)) in parts.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.push(b'"');
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b"\":");
        out.extend_from_slice(body);
    }
    out.push(b'}');
    (
        [(header::CONTENT_TYPE, "application/json")],
        Bytes::from(out),
    )
        .into_response()
}

/// `GET /api/5/alert` — the alert event journal (spec §4.4). Read-only: it
/// never wakes or waits on a collector (like `pluginslist`), and returns `200`
/// with a JSON array, `[]` when empty.
async fn alert_history(State(app): State<Arc<AppState>>) -> Json<Vec<Value>> {
    Json(app.alerts.history())
}

/// `GET /api/5/{plugin}` — single dynamic route for every plugin (§6.1).
async fn plugin_stats(State(app): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let Some(id) = PluginId::parse(&name).filter(|id| app.is_registered(*id)) else {
        return not_found(&name);
    };
    match ensure_plugin(&app, id).await {
        Ok(body) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
        Err(EnsureError::NotRegistered) => not_found(&name),
        Err(EnsureError::Timeout) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "detail":
                    format!("plugin '{name}': collection did not start within the guard timeout")
            })),
        )
            .into_response(),
    }
}

/// `GET /api/5/{plugin}/info` — the static field schema (design spec
/// 2026-08-02). Inert like `pluginslist`/`alert`: no wake, no store, no
/// `503`. `default_thresholds` reflects the operator's configured global
/// thresholds (config-only; no built-in defaults ship).
async fn plugin_info(State(app): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let Some(id) = PluginId::parse(&name).filter(|id| app.is_registered(*id)) else {
        return not_found(&name);
    };
    let mut out = Map::new();
    for fi in fields(id) {
        out.insert(fi.field.to_owned(), field_schema(&app, id, fi));
    }
    Json(Value::Object(out)).into_response()
}

fn field_schema(app: &AppState, id: PluginId, fi: &FieldInfo) -> Value {
    let mut m = Map::new();
    m.insert("description".into(), json!(fi.description));
    m.insert("unit".into(), json!(fi.unit.as_str()));
    if let Some(s) = fi.short_name {
        m.insert("short_name".into(), json!(s));
    }
    if fi.primary_key {
        m.insert("primary_key".into(), json!(true));
    }
    if fi.rate {
        m.insert("rate".into(), json!(true));
    }
    if fi.internal {
        m.insert("internal".into(), json!(true));
    }
    if fi.watched {
        m.insert("watched".into(), json!(true));
        m.insert("watch_direction".into(), json!(fi.direction.as_str()));
        m.insert("prominent".into(), json!(fi.prominent));
        if let Some(dt) = configured_thresholds(app, id, fi.field) {
            m.insert("default_thresholds".into(), dt);
        }
    }
    if let Some(nb) = fi.normalize_by {
        m.insert("normalize_by".into(), json!(nb));
    }
    Value::Object(m)
}

/// The operator's configured global thresholds for `field` (`Some` limits
/// only), or `None` when unconfigured. Per-item overrides are not reflected —
/// `/info` is a per-field schema.
fn configured_thresholds(app: &AppState, id: PluginId, field: &str) -> Option<Value> {
    let t = app.config.plugins.get(id.as_str())?.thresholds.get(field)?;
    let mut m = Map::new();
    if let Some(c) = t.careful {
        m.insert("careful".into(), json!(c));
    }
    if let Some(w) = t.warning {
        m.insert("warning".into(), json!(w));
    }
    if let Some(cr) = t.critical {
        m.insert("critical".into(), json!(cr));
    }
    (!m.is_empty()).then_some(Value::Object(m))
}

fn not_found(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "detail": format!("unknown plugin '{name}'") })),
    )
        .into_response()
}
