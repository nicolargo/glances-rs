//! `load` plugin — instantaneous, scalar. Payload shape: docs/api.md §5.2.
//!
//! On Windows `sysinfo` emulates the load average through PDH counters and
//! may legitimately report zeros: degraded values, identical shape.

use super::{Clock, Plugin, PluginId, envelope};
use crate::config::Config;
use serde_json::{Value, json};
use std::time::Duration;
use sysinfo::{CpuRefreshKind, System};

pub struct LoadPlugin {
    refresh: Duration,
    cpucore: usize,
}

impl LoadPlugin {
    pub fn new(config: &Config) -> Self {
        Self {
            refresh: config.refresh_for(PluginId::Load.as_str()),
            cpucore: logical_core_count(),
        }
    }
}

pub(crate) fn logical_core_count() -> usize {
    let mut sys = System::new();
    sys.refresh_cpu_list(CpuRefreshKind::nothing());
    sys.cpus().len()
}

#[async_trait::async_trait]
impl Plugin for LoadPlugin {
    type State = Clock;

    fn id(&self) -> PluginId {
        PluginId::Load
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    async fn collect(&self, clock: &mut Clock) -> Value {
        let load = System::load_average();
        envelope(
            json!({
                "min1": load.one,
                "min5": load.five,
                "min15": load.fifteen,
                "cpucore": self.cpucore,
            }),
            clock.tick(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collect_matches_the_frozen_schema() {
        let plugin = LoadPlugin::new(&Config::default());
        let value = plugin.collect(&mut Clock::default()).await;

        let obj = value.as_object().expect("load payload is an object");
        for field in ["min1", "min5", "min15", "cpucore"] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
        // The v5 envelope adds these to every plugin.
        assert!(obj.contains_key("time_since_update"));
        assert_eq!(obj["_levels"], json!({}));
        assert!(obj["cpucore"].as_u64().unwrap() > 0);
        // Degraded platforms report 0.0, never a negative or missing value.
        assert!(obj["min1"].as_f64().unwrap() >= 0.0);
    }
}
