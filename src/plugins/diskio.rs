//! `diskio` plugin — rate **and** collection: one item per disk, primary key
//! `disk_name` (§8.1). Payload: docs/api.md §5.9.
//!
//! Glances v5 shape: the items live under `data`, each carrying the four
//! counters as **plain per-second rates** (`read_count`/`write_count`/
//! `read_bytes`/`write_bytes`), with a single top-level `time_since_update`
//! (rate plugin) and `_levels` on the envelope. Loop and ram devices are
//! hidden by default.
//!
//! Linux-only: counters come from `/proc/diskstats`. `sysinfo` exposes no
//! per-disk I/O, so other platforms return an empty `data` array.

use super::filter::{KeyFilter, hide_or_default};
use super::{Plugin, PluginId, envelope};
use crate::config::Config;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;

#[cfg(target_os = "linux")]
use super::{RATE_WARMUP, round1};
#[cfg(target_os = "linux")]
use std::time::Instant;

/// Default `hide` when the operator configures none: virtual loop devices.
const DEFAULT_HIDE: &[&str] = &["loop.*", "/dev/loop.*"];

/// Cumulative `(read_count, write_count, read_bytes, write_bytes)`.
#[cfg(target_os = "linux")]
type Counters = (u64, u64, u64, u64);

pub struct DiskioPlugin {
    refresh: Duration,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    filter: KeyFilter,
    /// Disk name -> operator-defined alias.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    alias: HashMap<String, String>,
}

impl DiskioPlugin {
    pub fn new(config: &Config) -> Self {
        let plugin = config.plugins.get(PluginId::Diskio.as_str());
        let show = plugin.map(|p| p.show.clone()).unwrap_or_default();
        let hide = hide_or_default(
            plugin.map(|p| p.hide.clone()).unwrap_or_default(),
            DEFAULT_HIDE,
        );
        Self {
            refresh: config.refresh_for(PluginId::Diskio.as_str()),
            filter: KeyFilter::new(&show, &hide),
            alias: plugin.map(|p| p.alias.clone()).unwrap_or_default(),
        }
    }
}

#[derive(Default)]
pub struct DiskioState {
    #[cfg(target_os = "linux")]
    previous: HashMap<String, Counters>,
    #[cfg(target_os = "linux")]
    last: Option<Instant>,
}

#[async_trait::async_trait]
impl Plugin for DiskioPlugin {
    type State = DiskioState;

    fn id(&self) -> PluginId {
        PluginId::Diskio
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    #[cfg(target_os = "linux")]
    async fn collect(&self, state: &mut DiskioState) -> Value {
        if state.last.is_none() {
            // Self-bootstrap (§5.5), cold path only: baseline sample, then
            // wait so the first response carries real rates, not zeros.
            state.previous = self.sample();
            state.last = Some(Instant::now());
            tokio::time::sleep(RATE_WARMUP).await;
        }

        let now = Instant::now();
        // Measured elapsed time (§5.4) — never the nominal refresh.
        let elapsed = now
            .duration_since(state.last.expect("set above"))
            .as_secs_f64();
        state.last = Some(now);

        let current = self.sample();
        let (items, previous) = step(
            std::mem::take(&mut state.previous),
            current,
            elapsed,
            &self.alias,
        );
        state.previous = previous;
        envelope(Value::Array(items), Some(elapsed))
    }

    #[cfg(not(target_os = "linux"))]
    async fn collect(&self, _state: &mut DiskioState) -> Value {
        // No per-disk I/O counters off Linux (sysinfo does not expose them):
        // an empty list and no measured interval, hence no time_since_update.
        envelope(Value::Array(Vec::new()), None)
    }
}

#[cfg(target_os = "linux")]
impl DiskioPlugin {
    /// The current `/proc/diskstats` sample, filtered (§8.1: filtering before
    /// rate computation, so a hidden disk neither appears nor costs a diff).
    fn sample(&self) -> HashMap<String, Counters> {
        super::linux::read_diskstats()
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| self.filter.shown(name))
            .collect()
    }
}

/// One rate step. Returns the JSON items and the next inter-cycle state.
///
/// The returned state is ONLY the current sample — never a merge of old and
/// new. Merging would let removed disks accumulate in `previous` forever: a
/// slow memory leak (§8.1). A disk gone from the current sample drops out of
/// both the output and the state in the same cycle.
#[cfg(target_os = "linux")]
fn step(
    previous: HashMap<String, Counters>,
    current: HashMap<String, Counters>,
    elapsed: f64,
    alias: &HashMap<String, String>,
) -> (Vec<Value>, HashMap<String, Counters>) {
    let mut items: Vec<Value> = current
        .iter()
        .filter_map(|(name, &(rc, wc, rb, wb))| {
            // Appearing disk: no reference sample yet — skip this cycle, it
            // gets a rate next cycle (§5.4).
            let &(prc, pwc, prb, pwb) = previous.get(name)?;
            // saturating_sub (§5.4): on reboot the new counter can be lower.
            let mut item = json!({
                "disk_name": name,
                "read_count": per_sec(rc.saturating_sub(prc), elapsed),
                "write_count": per_sec(wc.saturating_sub(pwc), elapsed),
                "read_bytes": per_sec(rb.saturating_sub(prb), elapsed),
                "write_bytes": per_sec(wb.saturating_sub(pwb), elapsed),
            });
            // alias only when configured for this disk (matching Glances v5).
            if let Some(a) = alias.get(name) {
                item["alias"] = json!(a);
            }
            Some(item)
        })
        .collect();
    items.sort_by(|a, b| a["disk_name"].as_str().cmp(&b["disk_name"].as_str()));
    (items, current)
}

/// Per-second rate of a counter delta over the measured interval.
#[cfg(target_os = "linux")]
fn per_sec(delta: u64, elapsed: f64) -> f64 {
    if elapsed > 0.0 {
        round1(delta as f64 / elapsed)
    } else {
        0.0
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn counters(pairs: &[(&str, Counters)]) -> HashMap<String, Counters> {
        pairs.iter().map(|&(n, c)| (n.to_string(), c)).collect()
    }

    fn no_alias() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn nominal_rates_are_per_second() {
        let prev = counters(&[("sda", (100, 200, 4_000, 8_000))]);
        let cur = counters(&[("sda", (110, 230, 6_000, 9_000))]);
        let (items, _) = step(prev, cur, 2.0, &no_alias());
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item["disk_name"], "sda");
        assert_eq!(item["read_count"], 5.0); // (110-100)/2
        assert_eq!(item["write_count"], 15.0); // (230-200)/2
        assert_eq!(item["read_bytes"], 1_000.0); // (6000-4000)/2
        assert_eq!(item["write_bytes"], 500.0); // (9000-8000)/2
        // No gauge / rate_per_sec / per-item time_since_update in v5
        // (time_since_update lives once at the envelope top level).
        assert!(item.get("read_count_gauge").is_none());
        assert!(item.get("time_since_update").is_none());
        // No alias key when none configured.
        assert!(item.get("alias").is_none());
    }

    #[test]
    fn counter_rollback_clamps_to_zero() {
        let prev = counters(&[("sda", (5_000, 5_000, 5_000, 5_000))]);
        let cur = counters(&[("sda", (100, 100, 100, 100))]);
        let (items, _) = step(prev, cur, 2.0, &no_alias());
        assert_eq!(items[0]["read_count"], 0.0);
        assert_eq!(items[0]["write_bytes"], 0.0);
    }

    #[test]
    fn appearing_disk_is_skipped_for_one_cycle() {
        let prev = counters(&[("sda", (10, 10, 10, 10))]);
        let cur = counters(&[("sda", (20, 20, 20, 20)), ("sdb", (5, 5, 5, 5))]);
        let (items, previous) = step(prev, cur, 1.0, &no_alias());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["disk_name"], "sda");
        // ...but it is referenced for the next cycle.
        assert!(previous.contains_key("sdb"));
    }

    #[test]
    fn disappearing_disk_vanishes_from_output_and_state() {
        let prev = counters(&[("sda", (10, 10, 10, 10)), ("sdb", (9, 9, 9, 9))]);
        let cur = counters(&[("sda", (20, 20, 20, 20))]);
        let (items, previous) = step(prev, cur, 1.0, &no_alias());
        assert_eq!(items.len(), 1);
        // The anti-leak rule: previous == current sample, no merge.
        assert!(!previous.contains_key("sdb"));
        assert_eq!(previous.len(), 1);
    }

    #[test]
    fn alias_is_injected_and_output_sorted() {
        let prev = counters(&[("sdb", (0, 0, 0, 0)), ("sda", (0, 0, 0, 0))]);
        let cur = counters(&[("sdb", (1, 1, 1, 1)), ("sda", (1, 1, 1, 1))]);
        let alias = HashMap::from([("sda".to_string(), "root-disk".to_string())]);
        let (items, _) = step(prev, cur, 1.0, &alias);
        let names: Vec<&str> = items
            .iter()
            .map(|i| i["disk_name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["sda", "sdb"]);
        assert_eq!(items[0]["alias"], "root-disk");
        assert!(items[1].get("alias").is_none());
    }
}
