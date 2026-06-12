//! `network` plugin — rate, collection (one item per interface), with
//! `show`/`hide` filtering on the interface name (ARCHITECTURE.md §8.1).
//!
//! Inter-cycle state must memorize only the current sample — never a merge
//! of old and new — so that dead interfaces do not accumulate (§8.1).
//!
//! Implemented in Phase 4 (DEVELOPMENT_PLAN.md).
