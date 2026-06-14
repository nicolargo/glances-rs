//! `mem` plugin — instantaneous, scalar. Payload shape: docs/api.md §5.1.
//!
//! On Linux the full Glances v5 field set is read from `/proc/meminfo`
//! (adding `active`/`inactive`/`buffers`/`cached`). On other platforms the
//! payload degrades to the `total`/`available`/`percent`/`used`/`free`
//! subset that `sysinfo` exposes portably.

#[cfg(not(target_os = "linux"))]
use super::round1;
use super::{Plugin, PluginId, envelope};
use crate::config::Config;
use serde_json::{Value, json};
use std::time::Duration;
#[cfg(not(target_os = "linux"))]
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

#[derive(Default)]
pub struct MemState {
    /// `sysinfo` handle kept across cycles on the degraded path; the Linux
    /// path reads `/proc/meminfo` directly and needs no state. `mem` is
    /// instantaneous, so there is no `time_since_update` clock either.
    #[cfg(not(target_os = "linux"))]
    sys: System,
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

    #[cfg(target_os = "linux")]
    async fn collect(&self, _state: &mut MemState) -> Value {
        let Some(m) = super::linux::read_meminfo() else {
            // /proc/meminfo unreadable — degrade to the minimal subset.
            return envelope(
                json!({ "total": 0, "available": 0, "percent": 0.0, "used": 0, "free": 0 }),
            );
        };
        envelope(json!({
            "total": m.total,
            "available": m.available,
            "percent": m.percent,
            "used": m.used,
            "free": m.free,
            "active": m.active,
            "inactive": m.inactive,
            "buffers": m.buffers,
            "cached": m.cached,
        }))
    }

    #[cfg(not(target_os = "linux"))]
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
        envelope(json!({
            "total": total,
            "available": available,
            "percent": percent,
            "used": state.sys.used_memory(),
            "free": state.sys.free_memory(),
        }))
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_payload_has_the_full_field_set() {
        let plugin = MemPlugin::new(&Config::default());
        let mut state = MemState::default();
        let value = plugin.collect(&mut state).await;
        let obj = value.as_object().unwrap();
        for field in [
            "total",
            "available",
            "percent",
            "used",
            "free",
            "active",
            "inactive",
            "buffers",
            "cached",
        ] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
    }
}
