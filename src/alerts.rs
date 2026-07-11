//! Glances-v5-compatible alerting (spec docs/superpowers/specs/2026-06-14-alerting-design.md).
//!
//! `Alerts` is a shared component in `AppState`. Each plugin loop calls
//! `observe()` once per cycle: it rewrites the envelope's `_levels` from the
//! configured thresholds (raw, instantaneous) and records `min_duration`-
//! debounced level transitions into a bounded event journal served by
//! `/api/5/alert`. State lives here, not in the plugin loop's `State`, because
//! it must survive a collector going idle and waking again (spec §3.2).

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
}
