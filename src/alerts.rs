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

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(c: f64, w: f64, cr: f64) -> Effective {
        Effective { careful: Some(c), warning: Some(w), critical: Some(cr) }
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
        let t = Effective { careful: None, warning: Some(80.0), critical: Some(90.0) };
        assert_eq!(compute_level(85.0, &t, Direction::High), Level::Warning);
        assert_eq!(compute_level(50.0, &t, Direction::High), Level::Ok);
    }
}
