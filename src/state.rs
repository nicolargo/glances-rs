//! Shared application state: the snapshot store (Tokio `RwLock`), per-plugin
//! `last_request` timestamps (`AtomicI64`), and the active-collector registry
//! (`Mutex`) — three distinct primitives by design (ARCHITECTURE.md §5.1).
//!
//! Implemented in Phase 3 (DEVELOPMENT_PLAN.md).
