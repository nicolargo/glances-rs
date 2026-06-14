//! `diskio` plugin — rate **and** collection: one item per disk, primary key
//! `disk_name` (§8.1), read/write counters diffed into rates. Payload:
//! docs/api.md §5.9.
//!
//! Linux-only: the counters come from `/proc/diskstats`. `sysinfo` exposes no
//! per-disk I/O, so other platforms return an empty array (degraded). The four
//! core counters (`read_count`/`write_count`/`read_bytes`/`write_bytes`) follow
//! the §4 rate convention; Glances' `read_time`/`write_time`/latency fields are
//! omitted (kept lean — they can be added later).

use super::filter::KeyFilter;
use super::{Plugin, PluginId, RATE_WARMUP, round1, round3};
use crate::config::Config;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Cumulative `(read_count, write_count, read_bytes, write_bytes)`.
type Counters = (u64, u64, u64, u64);

pub struct DiskioPlugin {
    refresh: Duration,
    filter: KeyFilter,
    /// Disk name -> operator-defined alias.
    alias: HashMap<String, String>,
}

impl DiskioPlugin {
    pub fn new(config: &Config) -> Self {
        let plugin = config.plugins.get(PluginId::Diskio.as_str());
        Self {
            refresh: config.refresh_for(PluginId::Diskio.as_str()),
            filter: KeyFilter::new(
                plugin.map(|p| p.show.as_slice()).unwrap_or_default(),
                plugin.map(|p| p.hide.as_slice()).unwrap_or_default(),
            ),
            alias: plugin.map(|p| p.alias.clone()).unwrap_or_default(),
        }
    }
}

#[derive(Default)]
pub struct DiskioState {
    previous: HashMap<String, Counters>,
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
        Value::Array(items)
    }

    #[cfg(not(target_os = "linux"))]
    async fn collect(&self, _state: &mut DiskioState) -> Value {
        // No per-disk I/O counters off Linux (sysinfo does not expose them).
        Value::Array(Vec::new())
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
            let read_count = rc.saturating_sub(prc);
            let write_count = wc.saturating_sub(pwc);
            let read_bytes = rb.saturating_sub(prb);
            let write_bytes = wb.saturating_sub(pwb);
            Some(json!({
                "disk_name": name,
                "read_count": read_count,
                "read_count_gauge": rc,
                "read_count_rate_per_sec": per_sec(read_count, elapsed),
                "write_count": write_count,
                "write_count_gauge": wc,
                "write_count_rate_per_sec": per_sec(write_count, elapsed),
                "read_bytes": read_bytes,
                "read_bytes_gauge": rb,
                "read_bytes_rate_per_sec": per_sec(read_bytes, elapsed),
                "write_bytes": write_bytes,
                "write_bytes_gauge": wb,
                "write_bytes_rate_per_sec": per_sec(write_bytes, elapsed),
                // alias always present (null when unset), as for network.
                "alias": alias.get(name).map(|a| json!(a)).unwrap_or(Value::Null),
                "time_since_update": round3(elapsed),
            }))
        })
        .collect();
    items.sort_by(|a, b| a["disk_name"].as_str().cmp(&b["disk_name"].as_str()));
    (items, current)
}

fn per_sec(delta: u64, elapsed: f64) -> f64 {
    if elapsed > 0.0 {
        round1(delta as f64 / elapsed)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters(pairs: &[(&str, Counters)]) -> HashMap<String, Counters> {
        pairs.iter().map(|&(n, c)| (n.to_string(), c)).collect()
    }

    fn no_alias() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn nominal_rate() {
        let prev = counters(&[("sda", (100, 200, 4_000, 8_000))]);
        let cur = counters(&[("sda", (110, 230, 6_000, 9_000))]);
        let (items, _) = step(prev, cur, 2.0, &no_alias());
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item["disk_name"], "sda");
        assert_eq!(item["read_count"], 10);
        assert_eq!(item["read_count_gauge"], 110);
        assert_eq!(item["read_count_rate_per_sec"], 5.0); // 10 / 2s
        assert_eq!(item["write_count"], 30);
        assert_eq!(item["read_bytes"], 2_000);
        assert_eq!(item["read_bytes_rate_per_sec"], 1_000.0);
        assert_eq!(item["write_bytes"], 1_000);
        assert_eq!(item["time_since_update"], 2.0);
        assert_eq!(item["alias"], Value::Null);
    }

    #[test]
    fn counter_rollback_clamps_to_zero() {
        let prev = counters(&[("sda", (5_000, 5_000, 5_000, 5_000))]);
        let cur = counters(&[("sda", (100, 100, 100, 100))]);
        let (items, _) = step(prev, cur, 2.0, &no_alias());
        assert_eq!(items[0]["read_count"], 0);
        assert_eq!(items[0]["write_bytes"], 0);
        assert_eq!(items[0]["read_bytes_rate_per_sec"], 0.0);
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
        assert_eq!(items[1]["alias"], Value::Null);
    }
}
