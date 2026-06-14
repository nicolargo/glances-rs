//! `uptime` plugin — instantaneous. Payload: docs/api.md §5.6.
//!
//! Glances v5 returns `{"seconds": <int>}` wrapped in the standard envelope
//! (`time_since_update` + `_levels`). The earlier `str(timedelta)` string was
//! the v4 shape; v5 serializes the integer seconds.

use super::{Clock, Plugin, PluginId, envelope};
use crate::config::Config;
use serde_json::{Value, json};
use std::time::Duration;
use sysinfo::System;

pub struct UptimePlugin {
    refresh: Duration,
}

impl UptimePlugin {
    pub fn new(config: &Config) -> Self {
        Self {
            refresh: config.refresh_for(PluginId::Uptime.as_str()),
        }
    }
}

#[derive(Default)]
pub struct UptimeState {
    clock: Clock,
}

#[async_trait::async_trait]
impl Plugin for UptimePlugin {
    type State = UptimeState;

    fn id(&self) -> PluginId {
        PluginId::Uptime
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    async fn collect(&self, state: &mut UptimeState) -> Value {
        let tsu = state.clock.tick();
        envelope(json!({ "seconds": System::uptime() }), tsu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collect_matches_the_v5_envelope() {
        let plugin = UptimePlugin::new(&Config::default());
        let mut state = UptimeState::default();
        let value = plugin.collect(&mut state).await;

        let obj = value.as_object().expect("uptime payload is an object");
        // seconds is an integer count, plus the standard envelope fields.
        assert!(obj["seconds"].is_u64(), "seconds: {value}");
        assert!(obj.contains_key("time_since_update"));
        assert_eq!(obj["_levels"], json!({}));
    }
}
