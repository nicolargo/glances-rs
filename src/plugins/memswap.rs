//! `memswap` plugin — swap usage. Payload: docs/api.md §5.7.
//!
//! A **part-rate** plugin: `total`/`used`/`free`/`percent` are instantaneous,
//! while `sin`/`sout` are **per-second rates** (bytes swapped in/out per
//! second), computed by diffing the cumulative `/proc/vmstat` counters over
//! the measured interval — the Glances v5 shape. The payload is wrapped in the
//! standard envelope (`_levels`); Glances does not expose a top-level
//! `time_since_update` for memswap, so neither do we.
//!
//! On Linux the full field set is built; other platforms degrade to the
//! `sysinfo` swap subset without `sin`/`sout`.

use super::{Plugin, PluginId, envelope, round1};
use crate::config::Config;
use serde_json::{Value, json};
use std::time::Duration;

#[cfg(target_os = "linux")]
use super::RATE_WARMUP;
#[cfg(target_os = "linux")]
use std::time::Instant;
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

#[derive(Default)]
pub struct MemSwapState {
    /// Previous cumulative `(sin, sout)` for the rate diff (§5.4).
    #[cfg(target_os = "linux")]
    prev: Option<(u64, u64)>,
    #[cfg(target_os = "linux")]
    last: Option<Instant>,
    #[cfg(not(target_os = "linux"))]
    sys: System,
}

/// `(sin, sout)` per-second rates from two cumulative samples (§5.4:
/// `saturating_sub` for reboot/wrap, divide by the measured interval).
#[cfg(target_os = "linux")]
fn swap_rates(prev: (u64, u64), cur: (u64, u64), elapsed: f64) -> (f64, f64) {
    (
        per_sec(cur.0.saturating_sub(prev.0), elapsed),
        per_sec(cur.1.saturating_sub(prev.1), elapsed),
    )
}

#[cfg(target_os = "linux")]
fn per_sec(delta: u64, elapsed: f64) -> f64 {
    if elapsed > 0.0 {
        round1(delta as f64 / elapsed)
    } else {
        0.0
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
        if state.last.is_none() {
            // Self-bootstrap (§5.5), cold path only: baseline sin/sout, then
            // wait so the first response carries a real rate.
            state.prev = super::linux::read_swap().map(|s| (s.sin, s.sout));
            state.last = Some(Instant::now());
            tokio::time::sleep(RATE_WARMUP).await;
        }

        let now = Instant::now();
        let elapsed = now
            .duration_since(state.last.expect("set above"))
            .as_secs_f64();
        state.last = Some(now);

        let Some(s) = super::linux::read_swap() else {
            return envelope(
                json!({ "total": 0, "used": 0, "free": 0, "percent": 0.0, "sin": 0.0, "sout": 0.0 }),
            );
        };
        let (sin, sout) = match state.prev {
            Some(prev) => swap_rates(prev, (s.sin, s.sout), elapsed),
            None => (0.0, 0.0),
        };
        state.prev = Some((s.sin, s.sout));
        envelope(json!({
            "total": s.total,
            "used": s.used,
            "free": s.free,
            "percent": s.percent,
            "sin": sin,
            "sout": sout,
        }))
    }

    #[cfg(not(target_os = "linux"))]
    async fn collect(&self, state: &mut MemSwapState) -> Value {
        state.sys.refresh_memory();
        let total = state.sys.total_swap();
        let free = state.sys.free_swap();
        let used = state.sys.used_swap();
        let percent = if total == 0 {
            0.0
        } else {
            round1(used as f64 / total as f64 * 100.0)
        };
        // No sin/sout off Linux: sysinfo does not expose the swap counters.
        envelope(json!({
            "total": total,
            "used": used,
            "free": free,
            "percent": percent,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn swap_rates_are_per_second_and_clamp_on_rollback() {
        // 4096 bytes swapped in over 2 s -> 2048 B/s; sout unchanged.
        assert_eq!(
            swap_rates((1_000, 500), (1_000 + 4_096, 500), 2.0),
            (2_048.0, 0.0)
        );
        // Reboot: counter lower than before -> clamped to 0.
        assert_eq!(swap_rates((9_000, 9_000), (10, 10), 2.0), (0.0, 0.0));
    }

    #[tokio::test]
    async fn collect_matches_the_v5_envelope() {
        let plugin = MemSwapPlugin::new(&Config::default());
        let mut state = MemSwapState::default();
        let value = plugin.collect(&mut state).await;

        let obj = value.as_object().expect("memswap payload is an object");
        for field in ["total", "used", "free", "percent"] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
        // memswap carries sin/sout rates but no top-level time_since_update.
        assert!(obj.get("time_since_update").is_none());
        assert_eq!(obj["_levels"], json!({}));
        let percent = obj["percent"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&percent), "percent = {percent}");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_payload_carries_sin_sout_as_rates() {
        let plugin = MemSwapPlugin::new(&Config::default());
        let mut state = MemSwapState::default();
        let value = plugin.collect(&mut state).await;
        // sin/sout are floats (rates), not cumulative integers.
        assert!(value["sin"].is_number());
        assert!(value["sout"].is_number());
    }
}
