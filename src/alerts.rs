//! Glances-v5-compatible alerting (spec docs/superpowers/specs/2026-06-14-alerting-design.md).
//!
//! `Alerts` is a shared component in `AppState`. Each plugin loop calls
//! `observe()` once per cycle: it rewrites the envelope's `_levels` from the
//! configured thresholds (raw, instantaneous) and records `min_duration`-
//! debounced level transitions into a bounded event journal served by
//! `/api/5/alert`. State lives here, not in the plugin loop's `State`, because
//! it must survive a collector going idle and waking again (spec §3.2).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{Config, Thresholds};
use crate::plugins::PluginId;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

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

/// Format a wall-clock instant as `YYYY-MM-DDThh:mm:ssZ` (UTC, second
/// precision). Hand-rolled to avoid a chrono/time dependency (footprint
/// mandate). The event `ts` uses this; durations use `Instant` elsewhere.
pub(crate) fn iso8601_utc(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    // civil_from_days: days since 1970-01-01 -> (year, month, day).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

type StateKey = (PluginId, Option<String>, String);

struct Inner {
    history: VecDeque<Value>,
    state: HashMap<StateKey, AlertState>,
    last_seen: HashMap<PluginId, Instant>,
}

/// Shared alert engine: hysteresis state + the bounded event journal. Lives in
/// `AppState`; fed by every plugin loop via `observe` (spec §3.1, §4.1). One
/// `Mutex` guards a short, non-async critical section.
pub struct Alerts {
    inner: Mutex<Inner>,
    hostname: String,
}

impl Alerts {
    /// Build from runtime config: capture the hostname once for event payloads.
    pub fn new() -> Self {
        let hostname = sysinfo::System::host_name().unwrap_or_default();
        Self::for_tests_impl(hostname)
    }

    #[cfg(test)]
    fn for_tests(hostname: &str) -> Self {
        Self::for_tests_impl(hostname.to_string())
    }

    fn for_tests_impl(hostname: String) -> Self {
        Self {
            inner: Mutex::new(Inner {
                history: VecDeque::new(),
                state: HashMap::new(),
                last_seen: HashMap::new(),
            }),
            hostname,
        }
    }

    #[cfg(test)]
    fn state_len(&self) -> usize {
        self.inner.lock().unwrap().state.len()
    }

    /// Snapshot of the event journal, most-recent last (spec §4.4).
    pub fn history(&self) -> Vec<Value> {
        self.inner.lock().unwrap().history.iter().cloned().collect()
    }

    /// One cycle's worth of alerting for `id`'s freshly collected `value`:
    /// rewrite `_levels` (raw, instantaneous) and append any committed level
    /// transitions to the journal. No-op for plugins with no alertable fields.
    pub fn observe(&self, config: &Config, id: PluginId, value: &mut Value) {
        let fields = alert_fields(id);
        if fields.is_empty() {
            return;
        }
        let min_duration = effective_min_duration(config, id);
        let history_size = config.alerts.history_size;
        let now = Instant::now();
        let now_sys = SystemTime::now();
        let refresh = config.refresh_for(id.as_str());

        let mut guard = self.inner.lock().unwrap();
        let inner = &mut *guard;

        // Idle-gap reset (§5.2): if the loop had stopped and re-woken, drop any
        // stale pending windows for this plugin (keep committed_level).
        let gap_reset = match inner.last_seen.get(&id) {
            Some(&prev) => now.duration_since(prev) > refresh * 2,
            None => false,
        };
        inner.last_seen.insert(id, now);
        if gap_reset {
            for (k, st) in inner.state.iter_mut() {
                if k.0 == id {
                    st.pending_level = None;
                    st.pending_since = None;
                }
            }
        }

        // Compute levels per (item, field), collect observations + the new
        // `_levels` JSON, and (for collections) the set of live item keys.
        let mut observations: Vec<(Option<String>, &'static AlertField, Level, f64)> = Vec::new();

        match id.key_field() {
            None => {
                // Scalar plugin: fields at the payload top level.
                let mut levels = Map::new();
                for af in fields {
                    if let Some((level, raw)) = level_for(config, id, None, af, value) {
                        levels.insert(af.field.to_string(), level_entry(level, af.prominent));
                        observations.push((None, af, level, raw));
                    }
                }
                value["_levels"] = Value::Object(levels);
            }
            Some(pk_field) => {
                // Collection plugin: one sub-map per item, keyed by pk value.
                let mut levels = Map::new();
                let mut live: Vec<String> = Vec::new();
                if let Some(items) = value.get("data").and_then(Value::as_array) {
                    for item in items {
                        let Some(pk) = item.get(pk_field).and_then(json_key) else {
                            continue;
                        };
                        live.push(pk.clone());
                        let mut field_levels = Map::new();
                        for af in fields {
                            if let Some((level, raw)) = level_for(config, id, Some(&pk), af, item) {
                                field_levels
                                    .insert(af.field.to_string(), level_entry(level, af.prominent));
                                observations.push((Some(pk.clone()), af, level, raw));
                            }
                        }
                        if !field_levels.is_empty() {
                            levels.insert(pk, Value::Object(field_levels));
                        }
                    }
                }
                value["_levels"] = Value::Object(levels);
                // §6 pruning: drop hysteresis state for items no longer present.
                inner.state.retain(|k, _| {
                    k.0 != id
                        || k.1
                            .as_deref()
                            .is_none_or(|key| live.iter().any(|l| l == key))
                });
            }
        }

        // Reconcile each observation; append committed transitions as events.
        for (key, af, level, raw) in observations {
            let sk: StateKey = (id, key.clone(), af.field.to_string());
            let st = inner.state.entry(sk).or_default();
            if let Some(tr) = reconcile(st, level, now, min_duration) {
                let event = build_event(&self.hostname, id, key.as_deref(), af, &tr, raw, now_sys);
                inner.history.push_back(event);
                while inner.history.len() > history_size {
                    inner.history.pop_front();
                }
            }
        }
    }
}

impl Default for Alerts {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-plugin effective hysteresis window: per-plugin override or the global
/// default (spec §5.3).
fn effective_min_duration(config: &Config, id: PluginId) -> Duration {
    let secs = config
        .plugins
        .get(id.as_str())
        .and_then(|p| p.min_duration_seconds)
        .unwrap_or(config.alerts.min_duration_seconds);
    Duration::from_secs_f64(secs)
}

/// Compute the level for one alertable field of one item (or scalar payload).
/// Returns `(level, raw_value)` or `None` when there is no threshold or the
/// `normalize_by` divisor is missing/zero (spec §4.6). `raw_value` is the
/// undivided field value, carried verbatim into the event (Glances semantics).
fn level_for(
    config: &Config,
    id: PluginId,
    item: Option<&str>,
    af: &AlertField,
    stats: &Value,
) -> Option<(Level, f64)> {
    let raw = stats.get(af.field).and_then(Value::as_f64)?;
    let thresholds = resolve(config, id, item, af.field)?;
    let compared = match af.normalize_by {
        None => raw,
        Some(divisor_field) => {
            let divisor = stats.get(divisor_field).and_then(Value::as_f64)?;
            if !divisor.is_finite() || divisor == 0.0 {
                return None;
            }
            raw / divisor
        }
    };
    Some((compute_level(compared, &thresholds, af.direction), raw))
}

/// `{ "level": "...", "prominent": <bool> }` (spec §4.3).
fn level_entry(level: Level, prominent: bool) -> Value {
    json!({ "level": level.as_str(), "prominent": prominent })
}

/// Convert a primary-key JSON value to its string form for keying.
fn json_key(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Build the public alert-event JSON (parity with Glances `_build_event`).
fn build_event(
    hostname: &str,
    id: PluginId,
    key: Option<&str>,
    af: &AlertField,
    tr: &Transition,
    value: f64,
    ts: SystemTime,
) -> Value {
    json!({
        "ts": iso8601_utc(ts),
        "plugin": id.as_str(),
        "key": key,
        "field": af.field,
        "level": tr.new.as_str(),
        "previous_level": tr.previous.as_str(),
        "value": value,
        "prominent": af.prominent,
        "is_initial": tr.is_initial,
        "hostname": hostname,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(toml: &str) -> Config {
        Config::from_toml(toml).unwrap()
    }

    #[test]
    fn observe_writes_scalar_levels_and_no_event_until_min_duration() {
        let config = cfg("[alerts]\nmin_duration_seconds = 0.05\n\
                          [plugins.mem.thresholds.percent]\nwarning = 70.0\ncritical = 90.0\n");
        let alerts = Alerts::for_tests("host1");
        let mut payload = json!({ "percent": 95.0, "time_since_update": 1.0, "_levels": {} });

        alerts.observe(&config, PluginId::Mem, &mut payload);
        // _levels is rewritten raw, immediately.
        assert_eq!(payload["_levels"]["percent"]["level"], "critical");
        assert_eq!(payload["_levels"]["percent"]["prominent"], true);
        // but no event yet (min_duration not elapsed).
        assert!(alerts.history().is_empty());
    }

    #[test]
    fn observe_skips_unconfigured_and_non_alertable_fields() {
        let config = cfg(""); // no thresholds at all
        let alerts = Alerts::for_tests("host1");
        let mut payload = json!({ "percent": 95.0, "_levels": {} });
        alerts.observe(&config, PluginId::Mem, &mut payload);
        assert_eq!(payload["_levels"], json!({})); // config-only: nothing
        assert!(alerts.history().is_empty());
    }

    #[test]
    fn observe_collection_keys_levels_by_primary_key() {
        let config = cfg(r#"
            [alerts]
            min_duration_seconds = 0.0
            [plugins.fs.thresholds.percent]
            critical = 90.0
        "#);
        let alerts = Alerts::for_tests("h");
        let mut payload = json!({
            "data": [
                { "mnt_point": "/", "percent": 95.0 },
                { "mnt_point": "/home", "percent": 10.0 }
            ],
            "_levels": {}
        });
        alerts.observe(&config, PluginId::Fs, &mut payload);
        assert_eq!(payload["_levels"]["/"]["percent"]["level"], "critical");
        assert_eq!(payload["_levels"]["/home"]["percent"]["level"], "ok");
        // min_duration 0 -> the "/" breach commits an event immediately.
        let h = alerts.history();
        assert!(
            h.iter()
                .any(|e| e["plugin"] == "fs" && e["key"] == "/" && e["level"] == "critical")
        );
    }

    #[test]
    fn observe_normalize_by_skips_when_divisor_zero() {
        let config = cfg(r#"
            [plugins.network.thresholds.bytes_recv]
            warning = 0.8
        "#);
        let alerts = Alerts::for_tests("h");
        // capacity 0 (unknown link speed) -> no level entry for bytes_recv.
        let mut payload = json!({
            "data": [{ "interface_name": "eth0", "bytes_recv": 9999.0, "bytes_speed_rate_per_sec": 0 }],
            "_levels": {}
        });
        alerts.observe(&config, PluginId::Network, &mut payload);
        assert_eq!(payload["_levels"], json!({}));
    }

    #[test]
    fn observe_prunes_state_for_disappeared_items() {
        let config = cfg("[alerts]\nmin_duration_seconds = 0.0\n\
                          [plugins.fs.thresholds.percent]\ncritical = 90.0\n");
        let alerts = Alerts::for_tests("h");
        let mut p1 = json!({ "data": [{ "mnt_point": "/usb", "percent": 95.0 }], "_levels": {} });
        alerts.observe(&config, PluginId::Fs, &mut p1);
        assert_eq!(alerts.state_len(), 1);
        // next cycle /usb is gone -> its hysteresis state must be pruned (§6).
        let mut p2 = json!({ "data": [], "_levels": {} });
        alerts.observe(&config, PluginId::Fs, &mut p2);
        assert_eq!(alerts.state_len(), 0);
    }

    #[test]
    fn idle_gap_reset_prevents_stale_commit() {
        // refresh = 1ms -> idle-gap threshold (2 * refresh) = 2ms;
        // min_duration = 5ms. Both are dwarfed by the 30ms sleep below, so
        // the test is not flaky in either direction.
        let config = cfg("[collect]\nrefresh = 0.001\n\
             [alerts]\nmin_duration_seconds = 0.005\n\
             [plugins.mem.thresholds.percent]\ncritical = 0.0\n");
        let alerts = Alerts::for_tests("host1");

        // Cycle 1: breaching payload starts a pending window; no event yet.
        let mut p1 = json!({ "percent": 95.0, "_levels": {} });
        alerts.observe(&config, PluginId::Mem, &mut p1);
        assert!(alerts.history().is_empty());

        // Sleep far longer than both the 2ms gap threshold and the 5ms
        // min_duration.
        std::thread::sleep(Duration::from_millis(30));

        // Cycle 2: same breaching payload. The 30ms gap (> 2ms) triggers the
        // idle-gap reset (§5.2), clearing the pending window and restarting
        // it, so the level does NOT commit here. Without the reset, the
        // 30ms-old pending window would already exceed the 5ms min_duration
        // and wrongly commit an event -- the still-empty history proves the
        // reset fired.
        let mut p2 = json!({ "percent": 95.0, "_levels": {} });
        alerts.observe(&config, PluginId::Mem, &mut p2);
        assert!(alerts.history().is_empty());
    }

    #[test]
    fn history_ring_evicts_oldest() {
        let config = cfg("[alerts]\nhistory_size = 2\nmin_duration_seconds = 0.0\n\
             [plugins.mem.thresholds.percent]\nwarning = 50.0\ncritical = 90.0\n");
        let alerts = Alerts::for_tests("host1");

        // Four distinct committed transitions: critical -> ok -> critical -> ok.
        for percent in [95.0, 10.0, 95.0, 10.0] {
            let mut payload = json!({ "percent": percent, "_levels": {} });
            alerts.observe(&config, PluginId::Mem, &mut payload);
        }

        let h = alerts.history();
        // Ring bounded to history_size even though 4 events were appended.
        assert_eq!(h.len(), 2);
        // Retained events are the two most recent, most-recent-last.
        assert_eq!(h.first().unwrap()["level"], "critical");
        assert_eq!(h.last().unwrap()["level"], "ok");
    }

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

    #[test]
    fn iso8601_formats_known_instants() {
        use std::time::{Duration, UNIX_EPOCH};
        assert_eq!(iso8601_utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        // 2021-01-01T00:00:00Z = 1_609_459_200 s
        let t = UNIX_EPOCH + Duration::from_secs(1_609_459_200);
        assert_eq!(iso8601_utc(t), "2021-01-01T00:00:00Z");
        // 2026-06-14T12:34:56Z = 1_781_440_496 s
        let t2 = UNIX_EPOCH + Duration::from_secs(1_781_440_496);
        assert_eq!(iso8601_utc(t2), "2026-06-14T12:34:56Z");
    }
}
