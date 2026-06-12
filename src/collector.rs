//! The lazy-with-wake-up collection engine (ARCHITECTURE.md §3, §5).
//!
//! Each plugin is independently `Idle` (no entry in the registry) or
//! `Active` (a loop task collecting at its refresh period). A collector
//! stops itself after `idle_cycles` refresh periods without a request —
//! there is no external cancellation: the idle self-check *is* the stop
//! mechanism, which keeps the registry the single source of truth.
//!
//! Race contract between `ensure_plugin` and `plugin_loop`: a request
//! bumps `last_request` *before* taking the registry lock, and a loop
//! about to stop re-checks idleness *under* that lock. Whichever side
//! wins the lock, the outcome is consistent — either the loop sees the
//! fresh request and keeps running, or it removed itself first and the
//! request spawns a fresh collector.

use crate::plugins::cpu::CpuPlugin;
use crate::plugins::load::LoadPlugin;
use crate::plugins::mem::MemPlugin;
use crate::plugins::network::NetworkPlugin;
use crate::plugins::{Plugin, PluginId};
use crate::state::{AppState, Collector};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Debug, PartialEq)]
pub enum EnsureError {
    /// Plugin not implemented or disabled — maps to `404`.
    NotRegistered,
    /// The first cycle was not published within the guard timeout — `503`.
    Timeout,
}

/// Wake the plugin if needed and return its snapshot. The triggering
/// request waits for the first published cycle (§3.1): the API never
/// returns null or empty data (§6.2).
pub async fn ensure_plugin(app: &Arc<AppState>, id: PluginId) -> Result<Value, EnsureError> {
    // The API layer filters unregistered plugins already; this is defense
    // in depth at the engine level.
    if !app.is_registered(id) {
        return Err(EnsureError::NotRegistered);
    }

    // Bump BEFORE locking the registry — see the race contract above.
    app.touch(id);

    let mut ready = {
        let mut collectors = app.collectors.lock().await;
        match collectors.get(&id) {
            Some(collector) => collector.ready.clone(),
            None => {
                let (tx, rx) = watch::channel(false);
                spawn_plugin(app, id, tx);
                collectors.insert(id, Collector { ready: rx.clone() });
                tracing::debug!(plugin = id.as_str(), "collector woken");
                rx
            }
        }
    };

    // Not `Receiver::wait_for`: its future holds a lock guard across an
    // await and is !Send, which would poison every handler future. This
    // loop never holds the borrow across the await.
    let first_cycle = async move {
        loop {
            if *ready.borrow_and_update() {
                return true;
            }
            if ready.changed().await.is_err() {
                // Sender dropped without ever publishing.
                return false;
            }
        }
    };
    match tokio::time::timeout(app.config.guard_timeout(), first_cycle).await {
        Ok(true) => app.snapshot(id).await.ok_or(EnsureError::Timeout),
        // Guard timeout elapsed, or the collector vanished silently.
        _ => Err(EnsureError::Timeout),
    }
}

/// `PluginId -> concrete loop task`. The only place that knows every
/// plugin type; monomorphizes `plugin_loop` per plugin.
fn spawn_plugin(app: &Arc<AppState>, id: PluginId, ready: watch::Sender<bool>) {
    let app = app.clone();
    match id {
        PluginId::Cpu => {
            tokio::spawn(plugin_loop(CpuPlugin::new(&app.config), app, ready));
        }
        PluginId::Load => {
            tokio::spawn(plugin_loop(LoadPlugin::new(&app.config), app, ready));
        }
        PluginId::Mem => {
            tokio::spawn(plugin_loop(MemPlugin::new(&app.config), app, ready));
        }
        PluginId::Network => {
            tokio::spawn(plugin_loop(NetworkPlugin::new(&app.config), app, ready));
        }
    }
}

/// One plugin's collection loop. The inter-cycle state is a local variable
/// owned by this task and passed to `collect()` by `&mut` — exclusive by
/// construction, no lock (§5.4).
pub async fn plugin_loop<P: Plugin>(plugin: P, app: Arc<AppState>, ready: watch::Sender<bool>) {
    let id = plugin.id();
    let refresh = plugin.refresh();
    let idle_timeout = app.config.idle_timeout_for(id.as_str());
    let mut state = P::State::default();

    tracing::debug!(plugin = id.as_str(), "collector started");
    loop {
        let value = plugin.collect(&mut state).await;
        app.publish(id, value).await;
        ready.send_replace(true);

        tokio::time::sleep(refresh).await;

        if app.idle_for(id) >= idle_timeout {
            let mut collectors = app.collectors.lock().await;
            // Re-check under the lock: a request may have arrived since
            // (it bumps last_request before locking — race contract).
            if app.idle_for(id) >= idle_timeout {
                collectors.remove(&id);
                tracing::debug!(plugin = id.as_str(), "collector stopped (idle)");
                // The snapshot intentionally stays in the store (§3.2).
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn fast_config() -> Config {
        Config::from_toml(
            r#"
            [collect]
            refresh = 0.02
            idle_cycles = 2
            guard_timeout = 0.1
            "#,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn disabled_plugin_is_not_registered() {
        let mut config = fast_config();
        config.plugins.entry("cpu".into()).or_default().enabled = Some(false);
        let app = AppState::new(config);
        assert_eq!(
            ensure_plugin(&app, PluginId::Cpu).await,
            Err(EnsureError::NotRegistered)
        );
    }

    #[tokio::test]
    async fn guard_timeout_yields_timeout_error() {
        let app = AppState::new(fast_config());
        // A collector that never publishes: keep the sender alive but
        // never set it to true.
        let (tx, rx) = watch::channel(false);
        app.collectors
            .lock()
            .await
            .insert(PluginId::Mem, Collector { ready: rx });

        let result = ensure_plugin(&app, PluginId::Mem).await;
        assert_eq!(result, Err(EnsureError::Timeout));
        drop(tx);
    }

    #[tokio::test]
    async fn wake_collect_idle_stop_and_rewake() {
        let app = AppState::new(fast_config());

        // Cold wake: waits for the first cycle, returns real data.
        let value = ensure_plugin(&app, PluginId::Mem).await.unwrap();
        assert!(value.is_object());
        assert_eq!(app.active_collectors().await, 1);

        // No requests: the collector stops after the idle timeout
        // (2 cycles of 20 ms), but the snapshot is retained (§3.2).
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(app.active_collectors().await, 0);
        assert!(app.snapshot(PluginId::Mem).await.is_some());

        // Re-wake works.
        let value = ensure_plugin(&app, PluginId::Mem).await.unwrap();
        assert!(value.is_object());
        assert_eq!(app.active_collectors().await, 1);
    }
}
