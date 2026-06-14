//! `uptime` plugin — instantaneous. Payload: docs/api.md §5.6.
//!
//! Unlike every other plugin, the REST payload is a **bare JSON string**, not
//! an object: this matches what Glances v5 serializes (its uptime stat is a
//! `str(timedelta)`). `{"seconds": N}` is the Glances *export* shape, not the
//! REST one — keep the string for client parity.

use super::{Plugin, PluginId};
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

/// Seconds since boot formatted like Python's `str(timedelta)` (the exact
/// shape Glances emits): `"H:MM:SS"`, or `"N day[s], H:MM:SS"` past 24 h. The
/// hour field is not zero-padded; minutes and seconds are.
pub(crate) fn format_uptime(total_secs: u64) -> String {
    let days = total_secs / 86_400;
    let rem = total_secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    match days {
        0 => format!("{h}:{m:02}:{s:02}"),
        1 => format!("1 day, {h}:{m:02}:{s:02}"),
        n => format!("{n} days, {h}:{m:02}:{s:02}"),
    }
}

#[async_trait::async_trait]
impl Plugin for UptimePlugin {
    type State = ();

    fn id(&self) -> PluginId {
        PluginId::Uptime
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    async fn collect(&self, _state: &mut ()) -> Value {
        json!(format_uptime(System::uptime()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_uptime_matches_python_timedelta() {
        assert_eq!(format_uptime(0), "0:00:00");
        assert_eq!(format_uptime(45_296), "12:34:56");
        // 1 day, 1:15:30
        assert_eq!(format_uptime(86_400 + 4_530), "1 day, 1:15:30");
        // plural "days" past 48 h, with a zero clock
        assert_eq!(format_uptime(3 * 86_400), "3 days, 0:00:00");
    }

    #[tokio::test]
    async fn collect_returns_a_clock_string() {
        let plugin = UptimePlugin::new(&Config::default());
        let value = plugin.collect(&mut ()).await;
        let s = value.as_str().expect("uptime payload is a JSON string");
        assert!(s.contains(':'), "expected a H:MM:SS clock, got {s:?}");
    }
}
