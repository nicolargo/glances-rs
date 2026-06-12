//! `cpu` plugin — rate, scalar. Payload shape: docs/api.md §5.3.
//!
//! Two warm-up mechanisms are layered here (ARCHITECTURE.md §4): the
//! plugin's own self-bootstrap (§5.5) and `sysinfo`'s minimum CPU-refresh
//! interval. The Phase 1 spike showed that below that minimum (200 ms on
//! Linux/macOS/Windows) `refresh_cpu_usage()` is *silently skipped* and
//! the reading keeps a bogus value — hence `RATE_WARMUP` (250 ms) and the
//! sleep on the cold path. Subsequent cycles are `refresh` apart, which the
//! config cannot make shorter than valid data requires without the reading
//! merely going stale (never bogus, since the baseline is then real).

use super::{Plugin, PluginId, RATE_WARMUP, round1, round3};
use crate::config::Config;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use sysinfo::System;

pub struct CpuPlugin {
    refresh: Duration,
}

impl CpuPlugin {
    pub fn new(config: &Config) -> Self {
        Self {
            refresh: config.refresh_for(PluginId::Cpu.as_str()),
        }
    }
}

/// Inter-cycle memory: the `sysinfo` handle carries the previous sample
/// internally; `last` is the measured timestamp of that sample.
pub struct CpuState {
    sys: System,
    last: Option<Instant>,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            sys: System::new(),
            last: None,
        }
    }
}

#[async_trait::async_trait]
impl Plugin for CpuPlugin {
    type State = CpuState;

    fn id(&self) -> PluginId {
        PluginId::Cpu
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    async fn collect(&self, state: &mut CpuState) -> Value {
        if state.last.is_none() {
            // Self-bootstrap (§5.5), cold path only: take the baseline
            // sample and wait out sysinfo's minimum interval so the first
            // response carries a real percentage, not a bogus one.
            state.sys.refresh_cpu_usage();
            state.last = Some(Instant::now());
            tokio::time::sleep(RATE_WARMUP).await;
        }

        state.sys.refresh_cpu_usage();
        let now = Instant::now();
        // Measured elapsed time (§5.4) — never the nominal refresh.
        let elapsed = now
            .duration_since(state.last.expect("set above"))
            .as_secs_f64();
        state.last = Some(now);

        json!({
            "total": round1(f64::from(state.sys.global_cpu_usage())),
            "cpucore": state.sys.cpus().len(),
            "time_since_update": round3(elapsed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_collect_bootstraps_and_returns_a_real_rate() {
        let plugin = CpuPlugin::new(&Config::default());
        let mut state = CpuState::default();

        let t0 = Instant::now();
        let value = plugin.collect(&mut state).await;
        // The cold path pays the warm-up...
        assert!(t0.elapsed() >= RATE_WARMUP);

        let obj = value.as_object().expect("cpu payload is an object");
        for field in ["total", "cpucore", "time_since_update"] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
        let total = obj["total"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&total), "total = {total}");
        assert!(obj["cpucore"].as_u64().unwrap() > 0);
        // time_since_update is the measured warm-up interval.
        assert!(obj["time_since_update"].as_f64().unwrap() >= RATE_WARMUP.as_secs_f64());
    }

    #[tokio::test]
    async fn second_collect_is_warm_and_measures_real_elapsed() {
        let plugin = CpuPlugin::new(&Config::default());
        let mut state = CpuState::default();
        plugin.collect(&mut state).await;

        tokio::time::sleep(Duration::from_millis(250)).await;
        let t0 = Instant::now();
        let value = plugin.collect(&mut state).await;
        // Warm path: no warm-up sleep.
        assert!(t0.elapsed() < RATE_WARMUP);
        let elapsed = value["time_since_update"].as_f64().unwrap();
        assert!(elapsed >= 0.25, "elapsed = {elapsed}");
    }
}
