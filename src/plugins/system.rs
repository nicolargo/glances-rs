//! `system` plugin — instantaneous host identity. Payload: docs/api.md §5.5.
//!
//! `State = ()`: the values barely change, but the plugin still lives in the
//! lazy engine like any other. On Linux the `linux_distro` field is read from
//! `/etc/os-release` (the Glances source); other platforms omit it.

use super::{Clock, Plugin, PluginId, envelope};
use crate::config::Config;
use serde_json::{Value, json};
use std::time::Duration;
use sysinfo::System;

pub struct SystemPlugin {
    refresh: Duration,
}

impl SystemPlugin {
    pub fn new(config: &Config) -> Self {
        Self {
            refresh: config.refresh_for(PluginId::System.as_str()),
        }
    }
}

/// `os_name` as Glances reports it (Python's `platform.system()`): a
/// capitalized family name, with `macos` spelled `Darwin` to match.
fn os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Darwin",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        "openbsd" => "OpenBSD",
        "netbsd" => "NetBSD",
        other => other,
    }
}

/// `platform` field — pointer width, mirroring `platform.architecture()[0]`.
fn platform_bits() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "64bit"
    } else if cfg!(target_pointer_width = "32") {
        "32bit"
    } else {
        "unknown"
    }
}

#[async_trait::async_trait]
impl Plugin for SystemPlugin {
    type State = Clock;

    fn id(&self) -> PluginId {
        PluginId::System
    }

    fn refresh(&self) -> Duration {
        self.refresh
    }

    #[cfg(target_os = "linux")]
    async fn collect(&self, clock: &mut Clock) -> Value {
        let hostname = System::host_name().unwrap_or_default();
        let os_name = os_name();
        let platform = platform_bits();
        // platform.release() == the kernel release on Linux.
        let os_version = System::kernel_version().unwrap_or_default();
        let linux_distro = super::linux::read_os_release().unwrap_or_default();
        // Glances composition: "<distro> <platform> / <os_name> <os_version>".
        let hr_name = format!("{linux_distro} {platform} / {os_name} {os_version}");
        envelope(
            json!({
                "os_name": os_name,
                "hostname": hostname,
                "platform": platform,
                "os_version": os_version,
                "linux_distro": linux_distro,
                "hr_name": hr_name,
            }),
            clock.tick(),
        )
    }

    #[cfg(not(target_os = "linux"))]
    async fn collect(&self, clock: &mut Clock) -> Value {
        let hostname = System::host_name().unwrap_or_default();
        let os_name = os_name();
        let platform = platform_bits();
        let os_version = System::os_version().unwrap_or_default();
        // No `linux_distro` off Linux; Glances composes hr_name differently.
        let hr_name = format!("{os_name} {os_version} {platform}");
        envelope(
            json!({
                "os_name": os_name,
                "hostname": hostname,
                "platform": platform,
                "os_version": os_version,
                "hr_name": hr_name,
            }),
            clock.tick(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collect_matches_the_frozen_schema() {
        let plugin = SystemPlugin::new(&Config::default());
        let value = plugin.collect(&mut Clock::default()).await;

        let obj = value.as_object().expect("system payload is an object");
        for field in ["os_name", "hostname", "platform", "os_version", "hr_name"] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
        assert!(matches!(obj["platform"].as_str(), Some("64bit" | "32bit")));
        // hr_name embeds the platform and os_name — a cheap consistency check.
        let hr = obj["hr_name"].as_str().unwrap();
        assert!(hr.contains(obj["platform"].as_str().unwrap()));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_payload_carries_linux_distro() {
        let plugin = SystemPlugin::new(&Config::default());
        let value = plugin.collect(&mut Clock::default()).await;
        assert!(value.as_object().unwrap().contains_key("linux_distro"));
    }
}
