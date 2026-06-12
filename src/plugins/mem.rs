//! `mem` plugin — instantaneous, scalar. Payload shape: docs/api.md §5.1.

use super::{Plugin, PluginId, round1};
use crate::config::Config;
use serde_json::{Value, json};
use std::time::Duration;
use sysinfo::System;

pub struct MemPlugin {
    refresh: Duration,
}

impl MemPlugin {
    pub fn new(config: &Config) -> Self {
        Self {
            refresh: config.refresh_for(PluginId::Mem.as_str()),
        }
    }
}

/// Keeps the `sysinfo` handle across cycles instead of re-allocating one
/// per collection.
pub struct MemState {
    sys: System,
}

impl Default for MemState {
    fn default() -> Self {
        Self { sys: System::new() }
    }
}

#[async_trait::async_trait]
impl Plugin for MemPlugin {
    type State = MemState;

    fn id(&self) -> PluginId {
        PluginId::Mem
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    async fn collect(&self, state: &mut MemState) -> Value {
        state.sys.refresh_memory();
        let total = state.sys.total_memory();
        let available = state.sys.available_memory();
        let percent = if total == 0 {
            0.0
        } else {
            // The Glances formula: (total - available) / total.
            round1(total.saturating_sub(available) as f64 / total as f64 * 100.0)
        };
        json!({
            "total": total,
            "available": available,
            "percent": percent,
            "used": state.sys.used_memory(),
            "free": state.sys.free_memory(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collect_matches_the_frozen_schema() {
        let plugin = MemPlugin::new(&Config::default());
        let mut state = MemState::default();
        let value = plugin.collect(&mut state).await;

        let obj = value.as_object().expect("mem payload is an object");
        for field in ["total", "available", "percent", "used", "free"] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
        assert!(obj["total"].as_u64().unwrap() > 0);
        let percent = obj["percent"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&percent), "percent = {percent}");
    }
}
