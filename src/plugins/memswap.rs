//! `memswap` plugin — swap usage. Payload: docs/api.md §5.7.
//!
//! A **part-rate** plugin: `total`/`used`/`free`/`percent` are instantaneous,
//! while `sin`/`sout` are cumulative byte counters. Following Glances, the
//! counters are emitted **raw** (not as a server-side per-second rate) next to
//! a measured `time_since_update`, so a client derives the rate from two
//! samples. Because nothing is diffed server-side there is no §5.5 warm-up to
//! pay — only the previous `Instant` is kept, to fill `time_since_update`.
//!
//! On Linux the full field set (`sin`/`sout` from `/proc/vmstat`) is built;
//! other platforms degrade to the `sysinfo` subset without `sin`/`sout`.

#[cfg(not(target_os = "linux"))]
use super::round1;
use super::{Plugin, PluginId, round3};
use crate::config::Config;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
#[cfg(not(target_os = "linux"))]
use sysinfo::System;

pub struct MemSwapPlugin {
    refresh: Duration,
}

impl MemSwapPlugin {
    pub fn new(config: &Config) -> Self {
        Self {
            refresh: config.refresh_for(PluginId::MemSwap.as_str()),
        }
    }
}

/// Inter-cycle memory: only the timestamp of the previous cycle, used to
/// measure `time_since_update` (§5.4). The cumulative `sin`/`sout` are read
/// fresh each cycle and need no previous sample.
#[derive(Default)]
pub struct MemSwapState {
    last: Option<Instant>,
    #[cfg(not(target_os = "linux"))]
    sys: System,
}

impl MemSwapState {
    /// Measured seconds since the previous cycle (0.0 on the first one, as
    /// Glances reports it), advancing the stored timestamp.
    fn elapsed(&mut self) -> f64 {
        let now = Instant::now();
        let elapsed = self
            .last
            .map_or(0.0, |l| now.duration_since(l).as_secs_f64());
        self.last = Some(now);
        elapsed
    }
}

#[async_trait::async_trait]
impl Plugin for MemSwapPlugin {
    type State = MemSwapState;

    fn id(&self) -> PluginId {
        PluginId::MemSwap
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    #[cfg(target_os = "linux")]
    async fn collect(&self, state: &mut MemSwapState) -> Value {
        let elapsed = state.elapsed();
        let Some(s) = super::linux::read_swap() else {
            // /proc unreadable — degrade rather than fail the cycle.
            return json!({
                "total": 0, "used": 0, "free": 0, "percent": 0.0,
                "sin": 0, "sout": 0, "time_since_update": round3(elapsed),
            });
        };
        json!({
            "total": s.total,
            "used": s.used,
            "free": s.free,
            "percent": s.percent,
            "sin": s.sin,
            "sout": s.sout,
            "time_since_update": round3(elapsed),
        })
    }

    #[cfg(not(target_os = "linux"))]
    async fn collect(&self, state: &mut MemSwapState) -> Value {
        let elapsed = state.elapsed();
        state.sys.refresh_memory();
        let total = state.sys.total_swap();
        let free = state.sys.free_swap();
        let used = state.sys.used_swap();
        let percent = if total == 0 {
            0.0
        } else {
            round1(used as f64 / total as f64 * 100.0)
        };
        // No sin/sout off Linux: sysinfo does not expose the swap counters,
        // so they degrade out, like mem's active/inactive (docs/api.md §2).
        json!({
            "total": total,
            "used": used,
            "free": free,
            "percent": percent,
            "time_since_update": round3(elapsed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collect_matches_the_frozen_schema() {
        let plugin = MemSwapPlugin::new(&Config::default());
        let mut state = MemSwapState::default();
        let value = plugin.collect(&mut state).await;

        let obj = value.as_object().expect("memswap payload is an object");
        for field in ["total", "used", "free", "percent", "time_since_update"] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
        let percent = obj["percent"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&percent), "percent = {percent}");
        // First cycle: Glances-style zero elapsed.
        assert_eq!(obj["time_since_update"].as_f64().unwrap(), 0.0);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_payload_carries_sin_sout() {
        let plugin = MemSwapPlugin::new(&Config::default());
        let mut state = MemSwapState::default();
        let value = plugin.collect(&mut state).await;
        let obj = value.as_object().unwrap();
        assert!(obj.contains_key("sin"));
        assert!(obj.contains_key("sout"));
    }

    #[tokio::test]
    async fn second_cycle_measures_real_elapsed() {
        let plugin = MemSwapPlugin::new(&Config::default());
        let mut state = MemSwapState::default();
        plugin.collect(&mut state).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        let value = plugin.collect(&mut state).await;
        assert!(value["time_since_update"].as_f64().unwrap() >= 0.03);
    }
}
