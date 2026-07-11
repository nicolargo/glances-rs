//! REST API route handlers (ARCHITECTURE.md §6). Status-code grid (§6.2):
//! `200` always carries real data, `404` unknown/unavailable plugin,
//! `503` collection did not start within the guard timeout.

pub mod security;

use crate::collector::{EnsureError, ensure_plugin};
use crate::plugins::PluginId;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
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
async fn all_stats(State(app): State<Arc<AppState>>) -> Json<Map<String, Value>> {
    let mut set = JoinSet::new();
    for id in PluginId::ALL {
        if !app.is_registered(id) {
            continue;
        }
        let app = app.clone();
        set.spawn(async move { (id, ensure_plugin(&app, id).await) });
    }

    let mut out = Map::new();
    while let Some(joined) = set.join_next().await {
        // serde_json::Map is a BTreeMap here (no preserve_order feature),
        // so keys come out sorted regardless of completion order.
        if let Ok((id, Ok(value))) = joined {
            out.insert(id.as_str().to_owned(), value);
        }
    }
    Json(out)
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
        Ok(value) => Json(value).into_response(),
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

fn not_found(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "detail": format!("unknown plugin '{name}'") })),
    )
        .into_response()
}
