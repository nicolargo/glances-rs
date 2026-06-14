//! `uptime` plugin — instantaneous. Payload: docs/api.md §5.6.
//!
//! Glances v5 returns `{"seconds": <int>}` wrapped in the standard envelope
//! (`_levels`; no `time_since_update`, as uptime is instantaneous). The earlier
//! `str(timedelta)` string was the v4 shape; v5 serializes the integer seconds.

use super::{Plugin, PluginId, envelope};
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
pub struct UptimeState {}

#[async_trait::async_trait]
impl Plugin for UptimePlugin {
    type State = UptimeState;

    fn id(&self) -> PluginId {
        PluginId::Uptime
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    async fn collect(&self, _state: &mut UptimeState) -> Value {
        envelope(json!({ "seconds": System::uptime() }))
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
        // seconds is an integer count; the envelope adds _levels but, since
        // uptime is instantaneous, no time_since_update.
        assert!(obj["seconds"].is_u64(), "seconds: {value}");
        assert!(obj.get("time_since_update").is_none());
        assert_eq!(obj["_levels"], json!({}));
    }
}
