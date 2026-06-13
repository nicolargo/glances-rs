//! The `Plugin` trait and `PluginId` (ARCHITECTURE.md §5.3), plus the four
//! v1 plugins: `mem`, `cpu`, `load`, `network` (§8).

pub mod cpu;
pub mod load;
pub mod mem;
pub mod network;

#[cfg(target_os = "linux")]
pub mod linux;

use std::time::Duration;

/// Warm-up delay for rate plugins' self-bootstrap (§5.5): `sysinfo`'s
/// minimum CPU-refresh interval (200 ms on Linux/macOS/Windows) plus a
/// margin, because the failure mode at the boundary is *silent* — a too
/// short delay returns bogus data, not an error (docs/api.md §6).
pub const RATE_WARMUP: Duration = Duration::from_millis(250);

/// Round to 1 decimal, the Glances convention for percentages and rates.
pub(crate) fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Round to 3 decimals, used for `time_since_update`.
pub(crate) fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginId {
    Cpu,
    Load,
    Mem,
    Network,
}

impl PluginId {
    /// Every plugin in the v1 contract — all implemented since Phase 4.
    pub const ALL: [PluginId; 4] = [
        PluginId::Cpu,
        PluginId::Load,
        PluginId::Mem,
        PluginId::Network,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PluginId::Cpu => "cpu",
            PluginId::Load => "load",
            PluginId::Mem => "mem",
            PluginId::Network => "network",
        }
    }

    /// `&str -> PluginId`; `None` maps to `404` in the API layer (§6.1).
    pub fn parse(name: &str) -> Option<PluginId> {
        match name {
            "cpu" => Some(PluginId::Cpu),
            "load" => Some(PluginId::Load),
            "mem" => Some(PluginId::Mem),
            "network" => Some(PluginId::Network),
            _ => None,
        }
    }
}

/// One collectable metric source (§5.3). Implementations are stateless
/// objects: all inter-cycle memory lives in `State`, owned by the loop
/// task and passed back by `&mut` — exclusive by construction, no lock.
#[async_trait::async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Inter-cycle memory. `()` for an instantaneous plugin, a raw-sample
    /// type for a rate plugin (§5.4).
    type State: Default + Send;

    fn id(&self) -> PluginId;

    /// Collection period, fixed at construction from the config.
    fn refresh(&self) -> Duration;

    /// One collection cycle: update `state`, return the public JSON
    /// (shape frozen in docs/api.md §5).
    async fn collect(&self, state: &mut Self::State) -> serde_json::Value;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_names() {
        for id in PluginId::ALL {
            assert_eq!(PluginId::parse(id.as_str()), Some(id));
        }
    }

    #[test]
    fn parse_unknown_names() {
        assert_eq!(PluginId::parse("bogus"), None);
        assert_eq!(PluginId::parse("MEM"), None);
        assert_eq!(PluginId::parse(""), None);
        // "all" is the aggregate route, not a plugin name.
        assert_eq!(PluginId::parse("all"), None);
    }
}
