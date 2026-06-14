//! `fs` plugin — disk-space usage. Collection, **instantaneous** (no rate):
//! one item per mounted filesystem, primary key `mnt_point` (§8.1), filtered
//! by the shared `show`/`hide` regex. Payload: docs/api.md §5.8.
//!
//! Sourced from `sysinfo::Disks` (cross-platform, like `network` uses
//! `Networks`). `used`/`percent` are derived as `size - free` (free is the
//! space available to the caller); this slightly overstates usage versus
//! psutil's root-reserve-aware percent — revisit when alerting needs exact
//! thresholds. `key` and `options` (present in Glances) are omitted, as is
//! already the case for `network`'s `key`.

use super::filter::KeyFilter;
use super::{Plugin, PluginId, round1};
use crate::config::Config;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;
use sysinfo::Disks;

pub struct FsPlugin {
    refresh: Duration,
    filter: KeyFilter,
    /// Mount point -> operator-defined alias.
    alias: HashMap<String, String>,
}

impl FsPlugin {
    pub fn new(config: &Config) -> Self {
        let plugin = config.plugins.get(PluginId::Fs.as_str());
        Self {
            refresh: config.refresh_for(PluginId::Fs.as_str()),
            filter: KeyFilter::new(
                plugin.map(|p| p.show.as_slice()).unwrap_or_default(),
                plugin.map(|p| p.hide.as_slice()).unwrap_or_default(),
            ),
            alias: plugin.map(|p| p.alias.clone()).unwrap_or_default(),
        }
    }
}

pub struct FsState {
    disks: Disks,
}

impl Default for FsState {
    fn default() -> Self {
        Self {
            disks: Disks::new(),
        }
    }
}

/// `used` and `percent` from total size and caller-available free space.
/// `percent = (size - free) / size`, rounded to Glances' 1 decimal.
fn usage(size: u64, free: u64) -> (u64, f64) {
    let used = size.saturating_sub(free);
    let percent = if size == 0 {
        0.0
    } else {
        round1(used as f64 / size as f64 * 100.0)
    };
    (used, percent)
}

#[async_trait::async_trait]
impl Plugin for FsPlugin {
    type State = FsState;

    fn id(&self) -> PluginId {
        PluginId::Fs
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    async fn collect(&self, state: &mut FsState) -> Value {
        // Re-list each cycle so freshly (un)mounted filesystems are tracked.
        state.disks.refresh(true);

        let mut items: Vec<Value> = state
            .disks
            .iter()
            .filter_map(|disk| {
                let mnt = disk.mount_point().to_string_lossy().into_owned();
                // Filter first (§8.1): a hidden filesystem neither appears
                // in the JSON nor costs any further work.
                if !self.filter.shown(&mnt) {
                    return None;
                }
                let size = disk.total_space();
                let (used, percent) = usage(size, disk.available_space());
                // alias is always present (null when unset), mirroring network.
                let alias = self
                    .alias
                    .get(&mnt)
                    .map(|a| json!(a))
                    .unwrap_or(Value::Null);
                Some(json!({
                    "device_name": disk.name().to_string_lossy(),
                    "fs_type": disk.file_system().to_string_lossy(),
                    "mnt_point": mnt,
                    "size": size,
                    "used": used,
                    "free": disk.available_space(),
                    "percent": percent,
                    "alias": alias,
                }))
            })
            .collect();
        items.sort_by(|a, b| a["mnt_point"].as_str().cmp(&b["mnt_point"].as_str()));
        Value::Array(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_computes_used_and_percent() {
        // 1000 total, 250 free -> 750 used, 75.0%.
        assert_eq!(usage(1000, 250), (750, 75.0));
        // Empty/zero-size filesystem: no division by zero.
        assert_eq!(usage(0, 0), (0, 0.0));
        // free > size (shouldn't happen) clamps used to 0.
        assert_eq!(usage(100, 200), (0, 0.0));
    }

    #[tokio::test]
    async fn collect_is_an_array_of_well_formed_items() {
        let plugin = FsPlugin::new(&Config::default());
        let mut state = FsState::default();
        let value = plugin.collect(&mut state).await;
        let items = value.as_array().expect("fs payload is an array");
        for item in items {
            for field in [
                "device_name",
                "fs_type",
                "mnt_point",
                "size",
                "used",
                "free",
                "percent",
                "alias",
            ] {
                assert!(item.get(field).is_some(), "missing field {field}: {item}");
            }
            let percent = item["percent"].as_f64().unwrap();
            assert!((0.0..=100.0).contains(&percent), "percent = {percent}");
        }
    }
}
