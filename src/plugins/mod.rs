//! The `Plugin` trait and `PluginId` (ARCHITECTURE.md §5.3), plus the four
//! v1 plugins: `mem`, `cpu`, `load`, `network` (§8).
//!
//! Implemented in Phases 3–4 (DEVELOPMENT_PLAN.md).

pub mod cpu;
pub mod load;
pub mod mem;
pub mod network;
