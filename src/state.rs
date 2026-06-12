//! Shared application state — three distinct synchronization primitives,
//! each matched to its access pattern (ARCHITECTURE.md §5.1). Do not
//! collapse them into one lock:
//!
//! - the snapshot store: Tokio `RwLock` (many readers, one writer);
//! - per-plugin last-request timestamps: `AtomicI64` (lock-free, written
//!   on every request);
//! - the active-collector registry: Tokio `Mutex` (guards only the rare
//!   `Idle -> Active` transition).

use crate::config::Config;
use crate::plugins::PluginId;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, watch};

/// Registry entry for an active collector. `ready` becomes `true` when the
/// collector publishes its first cycle of this activity period.
pub struct Collector {
    pub ready: watch::Receiver<bool>,
}

pub struct AppState {
    pub config: Config,
    /// Monotonic origin for the `last_request` timestamps — immune to
    /// system clock changes.
    started: Instant,
    store: RwLock<HashMap<PluginId, Value>>,
    last_request: HashMap<PluginId, AtomicI64>,
    pub(crate) collectors: Mutex<HashMap<PluginId, Collector>>,
}

impl AppState {
    pub fn new(config: Config) -> Arc<Self> {
        let last_request = PluginId::ALL
            .iter()
            .map(|id| (*id, AtomicI64::new(0)))
            .collect();
        Arc::new(Self {
            config,
            started: Instant::now(),
            store: RwLock::new(HashMap::new()),
            last_request,
            collectors: Mutex::new(HashMap::new()),
        })
    }

    /// Is this plugin served by the API? Implemented in this build *and*
    /// enabled by the operator (fine-grained exposure).
    pub fn is_registered(&self, id: PluginId) -> bool {
        PluginId::IMPLEMENTED.contains(&id) && self.config.plugin_enabled(id.as_str())
    }

    /// Record "a request for this plugin happened now". Lock-free: called
    /// on every request.
    pub fn touch(&self, id: PluginId) {
        self.last_request[&id].store(self.now_millis(), Ordering::Relaxed);
    }

    /// Time since the last request for this plugin.
    pub fn idle_for(&self, id: PluginId) -> Duration {
        let last = self.last_request[&id].load(Ordering::Relaxed);
        Duration::from_millis(self.now_millis().saturating_sub(last).max(0) as u64)
    }

    /// Publish a collection cycle (the loop is the store's only writer).
    pub async fn publish(&self, id: PluginId, value: Value) {
        self.store.write().await.insert(id, value);
    }

    /// Last published snapshot. Intentionally survives collector stops
    /// (§3.2): the memory cost is a few KB and rates can restart instantly.
    pub async fn snapshot(&self, id: PluginId) -> Option<Value> {
        self.store.read().await.get(&id).cloned()
    }

    /// Number of currently active collectors (used by tests to assert the
    /// "observably idle" exit criterion).
    pub async fn active_collectors(&self) -> usize {
        self.collectors.lock().await.len()
    }

    fn now_millis(&self) -> i64 {
        self.started.elapsed().as_millis() as i64
    }
}
