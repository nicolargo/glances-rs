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
use serde_json::json;
use std::sync::Arc;

/// The `/api/5` sub-router. The Phase 6 middleware stack (auth, CORS,
/// trusted host) wraps exactly this router — never the probes.
pub fn api_router(app: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/5/pluginslist", get(plugins_list))
        .route("/api/5/{plugin}", get(plugin_stats))
        .with_state(app)
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
