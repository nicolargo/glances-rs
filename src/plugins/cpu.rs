//! `cpu` plugin — rate, scalar. Payload shape: docs/api.md §5.3.
//!
//! On Linux the full Glances v5 field set is built by diffing two
//! `/proc/stat` samples (per-category percentages and the
//! ctx_switches/interrupts/soft_interrupts rates). On other platforms the
//! payload degrades to `total`/`cpucore`/`time_since_update`, since the
//! breakdown is not portably available.
//!
//! Two warm-up mechanisms are layered (ARCHITECTURE.md §4, §5.5): the
//! plugin's own self-bootstrap (a baseline sample + `RATE_WARMUP` sleep on
//! the cold path, so the first response carries real data) and, on the
//! non-Linux path, `sysinfo`'s minimum CPU-refresh interval — the Phase 1
//! spike showed a shorter delay silently keeps a bogus reading.

use super::load::logical_core_count;
use super::{Plugin, PluginId, RATE_WARMUP, envelope, round1};
use crate::config::Config;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
#[cfg(not(target_os = "linux"))]
use sysinfo::System;

pub struct CpuPlugin {
    refresh: Duration,
    cpucore: usize,
}

impl CpuPlugin {
    pub fn new(config: &Config) -> Self {
        Self {
            refresh: config.refresh_for(PluginId::Cpu.as_str()),
            cpucore: logical_core_count(),
        }
    }
}

/// Inter-cycle memory: the previous cumulative sample and the measured
/// timestamp it was taken at.
#[derive(Default)]
pub struct CpuState {
    last: Option<Instant>,
    #[cfg(target_os = "linux")]
    prev: Option<super::linux::CpuSample>,
    #[cfg(not(target_os = "linux"))]
    sys: System,
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

    #[cfg(target_os = "linux")]
    async fn collect(&self, state: &mut CpuState) -> Value {
        use super::linux;

        if state.prev.is_none() {
            // Self-bootstrap (§5.5), cold path only: baseline sample, then
            // wait so the first diff spans a real interval.
            state.prev = linux::read_proc_stat();
            state.last = Some(Instant::now());
            tokio::time::sleep(RATE_WARMUP).await;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(state.last.unwrap_or(now)).as_secs_f64();
        state.last = Some(now);

        let (prev, cur) = match (state.prev, linux::read_proc_stat()) {
            (Some(prev), Some(cur)) => (prev, cur),
            // /proc/stat unreadable — degrade rather than fail the cycle.
            _ => {
                return envelope(json!({ "cpucore": self.cpucore }), Some(elapsed));
            }
        };
        state.prev = Some(cur);

        let p = linux::cpu_percents(&prev, &cur);
        // Rates rounded to 1 decimal, the Glances convention (§docs/api.md §5.3).
        let rate = |delta: u64| {
            if elapsed > 0.0 {
                round1(delta as f64 / elapsed)
            } else {
                0.0
            }
        };
        envelope(
            json!({
                "total": p.total,
                "user": p.user,
                "system": p.system,
                "idle": p.idle,
                "nice": p.nice,
                "iowait": p.iowait,
                "irq": p.irq,
                "steal": p.steal,
                "guest": p.guest,
                "ctx_switches": rate(cur.ctxt.saturating_sub(prev.ctxt)),
                "interrupts": rate(cur.intr.saturating_sub(prev.intr)),
                "soft_interrupts": rate(cur.softirq_total.saturating_sub(prev.softirq_total)),
                // psutil reports 0 syscalls on Linux; mirror that.
                "syscalls": 0.0,
                "cpucore": self.cpucore,
            }),
            Some(elapsed),
        )
    }

    #[cfg(not(target_os = "linux"))]
    async fn collect(&self, state: &mut CpuState) -> Value {
        if state.last.is_none() {
            state.sys.refresh_cpu_usage();
            state.last = Some(Instant::now());
            tokio::time::sleep(RATE_WARMUP).await;
        }
        state.sys.refresh_cpu_usage();
        let now = Instant::now();
        let elapsed = now
            .duration_since(state.last.expect("set above"))
            .as_secs_f64();
        state.last = Some(now);

        envelope(
            json!({
                "total": round1(f64::from(state.sys.global_cpu_usage())),
                "cpucore": self.cpucore,
            }),
            Some(elapsed),
        )
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
        assert!(obj["time_since_update"].as_f64().unwrap() >= RATE_WARMUP.as_secs_f64());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_payload_has_the_full_breakdown() {
        let plugin = CpuPlugin::new(&Config::default());
        let mut state = CpuState::default();
        let value = plugin.collect(&mut state).await;
        let obj = value.as_object().unwrap();
        for field in [
            "total",
            "user",
            "system",
            "idle",
            "nice",
            "iowait",
            "irq",
            "steal",
            "guest",
            "ctx_switches",
            "interrupts",
            "soft_interrupts",
            "syscalls",
            "cpucore",
            "time_since_update",
        ] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
        let idle = obj["idle"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&idle), "idle = {idle}");
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
