//! `network` plugin — rate, collection: one item per interface, primary
//! key `interface_name` (ARCHITECTURE.md §8.1). Payload: docs/api.md §5.4.
//!
//! On Linux each item also carries `is_up` and `speed` (from
//! `/sys/class/net`). `alias` comes from `[plugins.network].alias` on every
//! platform (null when unset), mirroring Glances.

use super::filter::KeyFilter;
use super::{Plugin, PluginId, RATE_WARMUP, round1, round3};
use crate::config::Config;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use sysinfo::Networks;

/// Cumulative (rx, tx) byte counters for one interface.
type Counters = (u64, u64);

pub struct NetworkPlugin {
    refresh: Duration,
    filter: KeyFilter,
    /// Interface name -> operator-defined alias.
    alias: HashMap<String, String>,
}

impl NetworkPlugin {
    pub fn new(config: &Config) -> Self {
        let plugin = config.plugins.get(PluginId::Network.as_str());
        Self {
            refresh: config.refresh_for(PluginId::Network.as_str()),
            filter: KeyFilter::new(
                plugin.map(|p| p.show.as_slice()).unwrap_or_default(),
                plugin.map(|p| p.hide.as_slice()).unwrap_or_default(),
            ),
            alias: plugin.map(|p| p.alias.clone()).unwrap_or_default(),
        }
    }
}

/// Per-interface status. `None` fields are simply omitted from the JSON,
/// which is how non-Linux platforms degrade (no `is_up`/`speed`).
#[derive(Default, Clone, Copy)]
struct IfaceMeta {
    is_up: Option<bool>,
    speed: Option<u64>,
}

pub struct NetworkState {
    networks: Networks,
    previous: HashMap<String, Counters>,
    last: Option<Instant>,
}

impl Default for NetworkState {
    fn default() -> Self {
        Self {
            networks: Networks::new(),
            previous: HashMap::new(),
            last: None,
        }
    }
}

#[async_trait::async_trait]
impl Plugin for NetworkPlugin {
    type State = NetworkState;

    fn id(&self) -> PluginId {
        PluginId::Network
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    async fn collect(&self, state: &mut NetworkState) -> Value {
        if state.last.is_none() {
            // Self-bootstrap (§5.5), cold path only: take the baseline
            // sample so the first response carries real rates, not an
            // empty array.
            state.networks.refresh(true);
            state.previous = sample(&state.networks, &self.filter);
            state.last = Some(Instant::now());
            tokio::time::sleep(RATE_WARMUP).await;
        }

        state.networks.refresh(true);
        let now = Instant::now();
        // Measured elapsed time (§5.4) — never the nominal refresh.
        let elapsed = now
            .duration_since(state.last.expect("set above"))
            .as_secs_f64();
        state.last = Some(now);

        // Filtering happens here, BEFORE rate computation (§8.1): a hidden
        // interface neither appears in the JSON nor costs a diff.
        let current = sample(&state.networks, &self.filter);
        let meta = gather_meta(current.keys());
        let (items, previous) = step(
            std::mem::take(&mut state.previous),
            current,
            elapsed,
            &meta,
            &self.alias,
        );
        state.previous = previous;
        Value::Array(items)
    }
}

fn sample(networks: &Networks, filter: &KeyFilter) -> HashMap<String, Counters> {
    networks
        .iter()
        .filter(|(name, _)| filter.shown(name))
        .map(|(name, data)| {
            (
                name.clone(),
                (data.total_received(), data.total_transmitted()),
            )
        })
        .collect()
}

/// Status of each named interface. Linux reads `/sys/class/net`; other
/// platforms return empty metadata, so `is_up`/`speed` are omitted.
#[cfg(target_os = "linux")]
fn gather_meta<'a>(names: impl Iterator<Item = &'a String>) -> HashMap<String, IfaceMeta> {
    names
        .map(|name| {
            let m = super::linux::read_iface_meta(name);
            (
                name.clone(),
                IfaceMeta {
                    is_up: Some(m.is_up),
                    speed: Some(m.speed),
                },
            )
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn gather_meta<'a>(_names: impl Iterator<Item = &'a String>) -> HashMap<String, IfaceMeta> {
    HashMap::new()
}

/// One rate step. Returns the JSON items and the next inter-cycle state.
///
/// The returned state is ONLY the current sample — never a merge of old
/// and new. Merging would let dead interfaces accumulate in `previous`
/// forever: a slow memory leak, ironic for a footprint-focused project.
/// Disappearing interfaces must vanish from both the output and the
/// inter-cycle state in the same cycle (§8.1).
fn step(
    previous: HashMap<String, Counters>,
    current: HashMap<String, Counters>,
    elapsed: f64,
    meta: &HashMap<String, IfaceMeta>,
    alias: &HashMap<String, String>,
) -> (Vec<Value>, HashMap<String, Counters>) {
    let mut items: Vec<Value> = current
        .iter()
        .filter_map(|(name, &(rx, tx))| {
            // Appearing interface: no reference sample yet — skip this
            // cycle, it gets a rate next cycle (§5.4).
            let &(prev_rx, prev_tx) = previous.get(name)?;
            // saturating_sub (§5.4): on reboot or counter wrap the new
            // value can be lower than the old one — clamp to 0.
            let recv = rx.saturating_sub(prev_rx);
            let sent = tx.saturating_sub(prev_tx);
            let all = recv.saturating_add(sent);
            let mut item = json!({
                "interface_name": name,
                // alias is always present (null when unset), matching Glances.
                "alias": alias.get(name).map(|a| json!(a)).unwrap_or(Value::Null),
                "bytes_recv": recv,
                "bytes_recv_gauge": rx,
                "bytes_recv_rate_per_sec": per_sec(recv, elapsed),
                "bytes_sent": sent,
                "bytes_sent_gauge": tx,
                "bytes_sent_rate_per_sec": per_sec(sent, elapsed),
                "bytes_all": all,
                "bytes_all_gauge": rx.saturating_add(tx),
                "bytes_all_rate_per_sec": per_sec(all, elapsed),
                "time_since_update": round3(elapsed),
            });
            if let Some(m) = meta.get(name) {
                if let Some(is_up) = m.is_up {
                    item["is_up"] = json!(is_up);
                }
                if let Some(speed) = m.speed {
                    item["speed"] = json!(speed);
                }
            }
            Some(item)
        })
        .collect();
    items.sort_by(|a, b| {
        a["interface_name"]
            .as_str()
            .cmp(&b["interface_name"].as_str())
    });
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

    fn counters(pairs: &[(&str, u64, u64)]) -> HashMap<String, Counters> {
        pairs
            .iter()
            .map(|&(name, rx, tx)| (name.to_string(), (rx, tx)))
            .collect()
    }

    fn no_meta() -> HashMap<String, IfaceMeta> {
        HashMap::new()
    }

    fn no_alias() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn nominal_rate() {
        let prev = counters(&[("eth0", 1_000, 2_000)]);
        let cur = counters(&[("eth0", 2_000, 2_500)]);
        let (items, _) = step(prev, cur, 2.0, &no_meta(), &no_alias());
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item["interface_name"], "eth0");
        assert_eq!(item["bytes_recv"], 1_000);
        assert_eq!(item["bytes_recv_gauge"], 2_000);
        assert_eq!(item["bytes_recv_rate_per_sec"], 500.0);
        assert_eq!(item["bytes_sent"], 500);
        assert_eq!(item["bytes_all"], 1_500);
        assert_eq!(item["bytes_all_rate_per_sec"], 750.0);
        assert_eq!(item["time_since_update"], 2.0);
        // alias is always present, null when unset.
        assert_eq!(item["alias"], Value::Null);
    }

    #[test]
    fn counter_rollback_clamps_to_zero() {
        // Reboot or 32-bit wrap: the new counter is lower than the old.
        let prev = counters(&[("eth0", 5_000, 5_000)]);
        let cur = counters(&[("eth0", 100, 200)]);
        let (items, _) = step(prev, cur, 2.0, &no_meta(), &no_alias());
        assert_eq!(items[0]["bytes_recv"], 0);
        assert_eq!(items[0]["bytes_sent"], 0);
        assert_eq!(items[0]["bytes_recv_rate_per_sec"], 0.0);
    }

    #[test]
    fn appearing_interface_is_skipped_for_one_cycle() {
        let prev = counters(&[("eth0", 1_000, 1_000)]);
        let cur = counters(&[("eth0", 1_100, 1_100), ("eth1", 50, 50)]);
        let (items, previous) = step(prev, cur, 2.0, &no_meta(), &no_alias());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["interface_name"], "eth0");
        // ...but it is referenced for the next cycle.
        assert!(previous.contains_key("eth1"));
    }

    #[test]
    fn disappearing_interface_vanishes_from_output_and_state() {
        let prev = counters(&[("eth0", 1_000, 1_000), ("ppp0", 9_000, 9_000)]);
        let cur = counters(&[("eth0", 1_100, 1_100)]);
        let (items, previous) = step(prev, cur, 2.0, &no_meta(), &no_alias());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["interface_name"], "eth0");
        // The anti-leak rule: previous == current sample, no merge.
        assert!(!previous.contains_key("ppp0"));
        assert_eq!(previous.len(), 1);
    }

    #[test]
    fn output_is_sorted_by_interface_name() {
        let prev = counters(&[("lo", 0, 0), ("eth0", 0, 0), ("wlan0", 0, 0)]);
        let cur = counters(&[("lo", 1, 1), ("eth0", 1, 1), ("wlan0", 1, 1)]);
        let (items, _) = step(prev, cur, 1.0, &no_meta(), &no_alias());
        let names: Vec<&str> = items
            .iter()
            .map(|i| i["interface_name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["eth0", "lo", "wlan0"]);
    }

    #[test]
    fn meta_and_alias_are_injected() {
        let prev = counters(&[("eth0", 0, 0)]);
        let cur = counters(&[("eth0", 1, 1)]);
        let meta = HashMap::from([(
            "eth0".to_string(),
            IfaceMeta {
                is_up: Some(true),
                speed: Some(1_048_576_000),
            },
        )]);
        let alias = HashMap::from([("eth0".to_string(), "LAN".to_string())]);
        let (items, _) = step(prev, cur, 1.0, &meta, &alias);
        assert_eq!(items[0]["is_up"], true);
        assert_eq!(items[0]["speed"], 1_048_576_000u64);
        assert_eq!(items[0]["alias"], "LAN");
    }
}
