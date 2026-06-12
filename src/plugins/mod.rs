//! The `Plugin` trait and `PluginId` (ARCHITECTURE.md §5.3), plus the four
//! v1 plugins: `mem`, `cpu`, `load`, `network` (§8).

pub mod cpu;
pub mod load;
pub mod mem;
pub mod network;

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginId {
    Cpu,
    Load,
    Mem,
    Network,
}

impl PluginId {
    /// Every plugin name in the v1 contract.
    pub const ALL: [PluginId; 4] = [
        PluginId::Cpu,
        PluginId::Load,
        PluginId::Mem,
        PluginId::Network,
    ];

    /// Plugins implemented so far. Grows to `ALL` in Phase 4; the API
    /// answers 404 for contract names that are not implemented yet.
    pub const IMPLEMENTED: [PluginId; 1] = [PluginId::Mem];

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
