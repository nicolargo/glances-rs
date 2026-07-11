//! Glances-v5-compatible alerting (spec docs/superpowers/specs/2026-06-14-alerting-design.md).
//!
//! `Alerts` is a shared component in `AppState`. Each plugin loop calls
//! `observe()` once per cycle: it rewrites the envelope's `_levels` from the
//! configured thresholds (raw, instantaneous) and records `min_duration`-
//! debounced level transitions into a bounded event journal served by
//! `/api/5/alert`. State lives here, not in the plugin loop's `State`, because
//! it must survive a collector going idle and waking again (spec §3.2).

use std::time::{Duration, Instant};

use crate::config::{Config, Thresholds};
use crate::plugins::PluginId;

/// Alert severity. `Ok` is the default committed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    #[default]
    Ok,
    Careful,
    Warning,
    Critical,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Careful => "careful",
            Level::Warning => "warning",
            Level::Critical => "critical",
        }
    }
}

/// Whether a field alerts on high values (cpu%, fs%) or low ones (free
/// space). Every v0.3.0 field is `High`; `Low` is engine-complete and tested
/// so a low-direction field computes correctly the day it is added (spec §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    High,
    Low,
}

/// Resolved limits for one `(item, field)` after the global+item merge.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Effective {
    careful: Option<f64>,
    warning: Option<f64>,
    critical: Option<f64>,
}

/// Highest breached limit, honouring `direction`. Checked critical→careful so
/// the ordering invariant (`careful <= warning <= critical`) makes the first
/// match the worst breach for both directions.
pub(crate) fn compute_level(value: f64, t: &Effective, dir: Direction) -> Level {
    let breached = |limit: Option<f64>| match (limit, dir) {
        (Some(l), Direction::High) => value >= l,
        (Some(l), Direction::Low) => value <= l,
        (None, _) => false,
    };
    if breached(t.critical) {
        Level::Critical
    } else if breached(t.warning) {
        Level::Warning
    } else if breached(t.careful) {
        Level::Careful
    } else {
        Level::Ok
    }
}

/// Per-`(plugin, key, field)` hysteresis state (spec §4.1). Mirrors Glances
/// `_AlertState`.
#[derive(Default)]
pub(crate) struct AlertState {
    committed_level: Level,
    pending_level: Option<Level>,
    pending_since: Option<Instant>,
    has_committed: bool,
}

/// A committed level change, the seed of an alert event.
pub(crate) struct Transition {
    pub previous: Level,
    pub new: Level,
    pub is_initial: bool,
}

/// Advance the state machine for one observation. Returns `Some` only when a
/// transition commits — i.e. the observed level differs from the committed
/// one and has persisted for `min_duration` (spec §2 `_reconcile`).
pub(crate) fn reconcile(
    state: &mut AlertState,
    observed: Level,
    now: Instant,
    min_duration: Duration,
) -> Option<Transition> {
    if observed == state.committed_level {
        state.pending_level = None;
        state.pending_since = None;
        state.has_committed = true;
        return None;
    }
    let commit = |state: &mut AlertState| -> Transition {
        let previous = state.committed_level;
        let is_initial = !state.has_committed;
        state.committed_level = observed;
        state.has_committed = true;
        state.pending_level = None;
        state.pending_since = None;
        Transition {
            previous,
            new: observed,
            is_initial,
        }
    };
    if min_duration.is_zero() {
        return Some(commit(state));
    }
    if state.pending_level == Some(observed) {
        if now.duration_since(state.pending_since.expect("set with pending_level")) >= min_duration
        {
            return Some(commit(state));
        }
        return None;
    }
    // Fresh debounce window for a newly-observed level.
    state.pending_level = Some(observed);
    state.pending_since = Some(now);
    None
}

/// Resolve the effective limits for `(plugin, item, field)`: item-specific
/// wins over global, **per limit key** (spec §4.5). Returns `None` when no
/// limit is configured — the field then produces no `_levels` entry.
pub(crate) fn resolve(
    config: &Config,
    id: PluginId,
    item: Option<&str>,
    field: &str,
) -> Option<Effective> {
    let pc = config.plugins.get(id.as_str())?;
    let global = pc.thresholds.get(field);
    let specific = item
        .and_then(|i| pc.thresholds_by_item.get(i))
        .and_then(|m| m.get(field));
    let pick =
        |get: fn(&Thresholds) -> Option<f64>| specific.and_then(get).or(global.and_then(get));
    let e = Effective {
        careful: pick(|t| t.careful),
        warning: pick(|t| t.warning),
        critical: pick(|t| t.critical),
    };
    if e.careful.is_none() && e.warning.is_none() && e.critical.is_none() {
        None
    } else {
        Some(e)
    }
}

/// One alertable field's static metadata (spec §4.6). `prominent` is copied
/// verbatim from Glances v5 for UI parity; `direction` is `High` for every
/// v0.3.0 field; `normalize_by` names a divisor field for rate-vs-capacity
/// comparison (network only).
pub(crate) struct AlertField {
    pub field: &'static str,
    pub prominent: bool,
    pub direction: Direction,
    pub normalize_by: Option<&'static str>,
}

const fn af(field: &'static str, prominent: bool) -> AlertField {
    AlertField {
        field,
        prominent,
        direction: Direction::High,
        normalize_by: None,
    }
}

const MEM_FIELDS: &[AlertField] = &[af("percent", true)];
const FS_FIELDS: &[AlertField] = &[af("percent", false)];
const LOAD_FIELDS: &[AlertField] = &[af("min5", false), af("min15", true)];
const MEMSWAP_FIELDS: &[AlertField] = &[af("percent", true), af("sin", false), af("sout", false)];
const DISKIO_FIELDS: &[AlertField] = &[af("read_bytes", false), af("write_bytes", false)];
const CPU_FIELDS: &[AlertField] = &[
    af("total", true),
    af("system", false),
    af("user", false),
    af("iowait", false),
    af("steal", true),
    af("ctx_switches", true),
];
const NETWORK_FIELDS: &[AlertField] = &[
    AlertField {
        field: "bytes_recv",
        prominent: false,
        direction: Direction::High,
        normalize_by: Some("bytes_speed_rate_per_sec"),
    },
    AlertField {
        field: "bytes_sent",
        prominent: false,
        direction: Direction::High,
        normalize_by: Some("bytes_speed_rate_per_sec"),
    },
];
const EMPTY_FIELDS: &[AlertField] = &[];

/// Alertable fields per plugin. Only these emit `_levels`, and only when a
/// threshold is configured (spec §4.6). Empty slice = nothing to alert on.
pub(crate) fn alert_fields(id: PluginId) -> &'static [AlertField] {
    match id {
        PluginId::Mem => MEM_FIELDS,
        PluginId::Fs => FS_FIELDS,
        PluginId::Load => LOAD_FIELDS,
        PluginId::MemSwap => MEMSWAP_FIELDS,
        PluginId::Diskio => DISKIO_FIELDS,
        PluginId::Cpu => CPU_FIELDS,
        PluginId::Network => NETWORK_FIELDS,
        PluginId::System | PluginId::Uptime => EMPTY_FIELDS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(c: f64, w: f64, cr: f64) -> Effective {
        Effective {
            careful: Some(c),
            warning: Some(w),
            critical: Some(cr),
        }
    }

    #[test]
    fn high_direction_ladder() {
        let t = eff(50.0, 70.0, 90.0);
        assert_eq!(compute_level(45.0, &t, Direction::High), Level::Ok);
        assert_eq!(compute_level(60.0, &t, Direction::High), Level::Careful);
        assert_eq!(compute_level(75.0, &t, Direction::High), Level::Warning);
        assert_eq!(compute_level(95.0, &t, Direction::High), Level::Critical);
        // boundary is inclusive (>=)
        assert_eq!(compute_level(90.0, &t, Direction::High), Level::Critical);
    }

    #[test]
    fn low_direction_ladder() {
        // free-space style: careful=20, warning=10, critical=5 (low = worse)
        let t = eff(20.0, 10.0, 5.0);
        assert_eq!(compute_level(25.0, &t, Direction::Low), Level::Ok);
        assert_eq!(compute_level(15.0, &t, Direction::Low), Level::Careful);
        assert_eq!(compute_level(8.0, &t, Direction::Low), Level::Warning);
        assert_eq!(compute_level(3.0, &t, Direction::Low), Level::Critical);
    }

    #[test]
    fn partial_subset_only_uses_present_limits() {
        let t = Effective {
            careful: None,
            warning: Some(80.0),
            critical: Some(90.0),
        };
        assert_eq!(compute_level(85.0, &t, Direction::High), Level::Warning);
        assert_eq!(compute_level(50.0, &t, Direction::High), Level::Ok);
    }

    #[test]
    fn resolve_merges_item_over_global_per_limit() {
        let config = Config::from_toml(
            r#"
            [plugins.fs.thresholds.percent]
            careful = 70.0
            warning = 80.0
            [plugins.fs.thresholds_by_item."/".percent]
            critical = 95.0
            "#,
        )
        .unwrap();
        // item "/" inherits careful+warning from global, adds critical.
        let e = resolve(&config, PluginId::Fs, Some("/"), "percent").unwrap();
        assert_eq!(e.careful, Some(70.0));
        assert_eq!(e.warning, Some(80.0));
        assert_eq!(e.critical, Some(95.0));
        // item "/home" (no override) sees global only, critical unset.
        let e2 = resolve(&config, PluginId::Fs, Some("/home"), "percent").unwrap();
        assert_eq!(e2.critical, None);
        // unconfigured field -> None (no _levels entry).
        assert!(resolve(&config, PluginId::Fs, Some("/"), "size").is_none());
    }

    #[test]
    fn alert_fields_match_emitted_payload_fields() {
        // Spot-check the static table against the spec §4.6 prominent values.
        let mem = alert_fields(PluginId::Mem);
        assert_eq!(mem.len(), 1);
        assert_eq!(mem[0].field, "percent");
        assert!(mem[0].prominent);

        let fs = alert_fields(PluginId::Fs);
        assert_eq!(fs[0].field, "percent");
        assert!(!fs[0].prominent);

        let net = alert_fields(PluginId::Network);
        assert!(
            net.iter()
                .all(|f| f.normalize_by == Some("bytes_speed_rate_per_sec"))
        );
        assert!(net.iter().any(|f| f.field == "bytes_recv"));

        // scalar/no-numeric plugins have no alertable fields.
        assert!(alert_fields(PluginId::System).is_empty());
        assert!(alert_fields(PluginId::Uptime).is_empty());
    }

    #[test]
    fn reconcile_debounces_then_commits() {
        use std::time::{Duration, Instant};
        let md = Duration::from_millis(50);
        let mut s = AlertState::default();
        let t0 = Instant::now();

        // ok == committed ok: no event, marks has_committed.
        assert!(reconcile(&mut s, Level::Ok, t0, md).is_none());
        // first warning: starts pending, no commit yet.
        assert!(reconcile(&mut s, Level::Warning, t0, md).is_none());
        // same warning, before window elapses: still pending.
        assert!(reconcile(&mut s, Level::Warning, t0 + Duration::from_millis(10), md).is_none());
        // after window: commit -> transition ok->warning, is_initial=false
        // (an ok was already committed first).
        let tr = reconcile(&mut s, Level::Warning, t0 + Duration::from_millis(60), md).unwrap();
        assert_eq!(tr.previous, Level::Ok);
        assert_eq!(tr.new, Level::Warning);
        assert!(!tr.is_initial);
        // return to ok commits immediately on next persisted observation window.
        assert!(reconcile(&mut s, Level::Ok, t0 + Duration::from_millis(60), md).is_none());
        let back = reconcile(&mut s, Level::Ok, t0 + Duration::from_millis(120), md).unwrap();
        assert_eq!(back.previous, Level::Warning);
        assert_eq!(back.new, Level::Ok);
    }

    #[test]
    fn reconcile_zero_min_duration_commits_immediately() {
        use std::time::{Duration, Instant};
        let mut s = AlertState::default();
        let tr = reconcile(&mut s, Level::Critical, Instant::now(), Duration::ZERO).unwrap();
        assert_eq!(tr.new, Level::Critical);
        assert!(tr.is_initial); // first commit, no prior ok observed
    }
}
