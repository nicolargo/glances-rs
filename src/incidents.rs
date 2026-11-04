//! Incident synthesis for `/api/5/alert/incidents` (port of Glances v5's
//! `alerts_incidents.py`, minus process `top`, which needs a process plugin).
//!
//! Collapses the transition journal into incidents: one opens on the first
//! transition away from `ok` for a `(plugin, key, field)` tuple and closes on
//! its transition back to `ok`; escalations mutate the open incident. Pure —
//! data in, data out — so it is testable without an engine.

use crate::alerts::parse_iso8601_utc;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// `(plugin, key, field)` — the identity of one alert.
pub type IncidentKey = (String, Option<String>, String);

/// What the engine reports as currently non-`ok`: the committed level and,
/// when known, the ISO-8601 instant the alert opened. The engine's unbounded
/// state is the authority here — the journal is a ring buffer, so an alert
/// can outlive its own opening event.
pub type Ongoing = HashMap<IncidentKey, (String, Option<String>)>;

struct Incident {
    key: IncidentKey,
    level: String,
    begin: Option<String>,
    end: Option<String>,
    ongoing: bool,
    partial: bool,
    prominent: bool,
}

/// Severity rank; keeps an incident's level monotonic (once `critical`, it
/// keeps reporting `critical` even if it later de-escalates).
fn rank(level: &str) -> u8 {
    match level {
        "careful" => 1,
        "warning" => 2,
        "critical" => 3,
        _ => 0,
    }
}

fn text<'a>(evt: &'a Value, field: &str) -> &'a str {
    evt.get(field).and_then(Value::as_str).unwrap_or("")
}

/// Elapsed time in at most 8 columns: `43s` / `2m58s` / `1h13m` / `2d04h`.
fn format_duration_compact(seconds: i64) -> String {
    let total = seconds.max(0);
    if total < 60 {
        format!("{total}s")
    } else if total < 3_600 {
        format!("{}m{:02}s", total / 60, total % 60)
    } else if total < 86_400 {
        format!("{}h{:02}m", total / 3_600, (total % 3_600) / 60)
    } else {
        format!("{}d{:02}h", total / 86_400, (total % 86_400) / 3_600)
    }
}

/// Rendered duration, or `None` when it cannot be known (unknown, malformed
/// or future `begin`). Ongoing incidents measure to `now`, resolved ones to
/// `end`; a `partial` incident is prefixed `>` (its duration is a lower bound).
fn duration(inc: &Incident, now_secs: i64) -> Option<String> {
    let begin = parse_iso8601_utc(inc.begin.as_deref()?)?;
    let end = inc
        .end
        .as_deref()
        .and_then(parse_iso8601_utc)
        .unwrap_or(now_secs);
    let elapsed = end - begin;
    if elapsed < 0 {
        return None;
    }
    let t = format_duration_compact(elapsed);
    Some(if inc.partial { format!(">{t}") } else { t })
}

fn derive(history: &[Value], ongoing: &Ongoing) -> Vec<Incident> {
    let mut open: HashMap<IncidentKey, usize> = HashMap::new();
    let mut incidents: Vec<Incident> = Vec::new();

    for evt in history {
        let key: IncidentKey = (
            text(evt, "plugin").to_owned(),
            evt.get("key").and_then(Value::as_str).map(str::to_owned),
            text(evt, "field").to_owned(),
        );
        let level = text(evt, "level");
        let ts = Some(text(evt, "ts").to_owned()).filter(|t| !t.is_empty());
        let prominent = evt
            .get("prominent")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if level == "ok" {
            if let Some(i) = open.remove(&key) {
                incidents[i].end = ts;
                incidents[i].ongoing = false;
            }
            continue;
        }
        match open.get(&key) {
            None => {
                // A first surviving transition that did not come from `ok`
                // means the opening one was evicted: `begin` is a lower bound.
                // An `is_initial` event IS the start (found already in alert).
                let previous = evt
                    .get("previous_level")
                    .and_then(Value::as_str)
                    .unwrap_or("ok");
                let initial = evt
                    .get("is_initial")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                open.insert(key.clone(), incidents.len());
                incidents.push(Incident {
                    key,
                    level: level.to_owned(),
                    begin: ts,
                    end: None,
                    ongoing: true,
                    partial: previous != "ok" && !initial,
                    prominent,
                });
            }
            Some(&i) => {
                let inc = &mut incidents[i];
                if rank(level) > rank(&inc.level) {
                    inc.level = level.to_owned();
                }
                inc.prominent |= prominent;
            }
        }
    }

    // The engine is the authority on what is still active.
    for (key, &i) in &open {
        match ongoing.get(key) {
            // The engine says recovered but no `→ ok` event survives: trust
            // it and close the incident with an unknown end.
            None => incidents[i].ongoing = false,
            Some((committed, _)) if rank(committed) > rank(&incidents[i].level) => {
                incidents[i].level = committed.clone();
            }
            Some(_) => {}
        }
    }
    // Alerts the engine reports active but the journal no longer covers at
    // all: without this the block would silently drop a live alert.
    for (key, (committed, _)) in ongoing {
        if !open.contains_key(key) {
            incidents.push(Incident {
                key: key.clone(),
                level: committed.clone(),
                begin: None,
                end: None,
                ongoing: true,
                partial: true,
                prominent: false,
            });
        }
    }
    // The engine remembers when every ACTIVE incident opened, even once the
    // journal evicted the opening event: its instant wins.
    for inc in incidents.iter_mut().filter(|i| i.ongoing) {
        if let Some((_, Some(since))) = ongoing.get(&inc.key) {
            inc.begin = Some(since.clone());
            inc.partial = false;
        }
    }
    // Ongoing first, newest first within each group (two stable passes).
    incidents.sort_by(|a, b| b.begin.cmp(&a.begin));
    incidents.sort_by_key(|i| !i.ongoing);
    incidents
}

/// The `{is_initializing, incidents}` envelope. `is_initializing` tells
/// warm-up from an all-clear: an empty `incidents` list while it is `true` is
/// not a healthy system. `top`/`top_sort` are always empty/`null` — there is
/// no process plugin — and kept for payload-shape parity.
pub fn incidents_payload(
    history: &[Value],
    ongoing: &Ongoing,
    initializing: bool,
    now: SystemTime,
) -> Value {
    let now_secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let list: Vec<Value> = derive(history, ongoing)
        .into_iter()
        .map(|inc| {
            let duration = duration(&inc, now_secs);
            json!({
                "plugin": inc.key.0,
                "key": inc.key.1,
                "field": inc.key.2,
                "level": inc.level,
                "begin": inc.begin,
                "end": inc.end,
                "ongoing": inc.ongoing,
                "partial": inc.partial,
                "prominent": inc.prominent,
                "top": [],
                "top_sort": null,
                "duration": duration,
            })
        })
        .collect();
    json!({ "is_initializing": initializing, "incidents": list })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::iso8601_utc;

    fn at(secs: u64) -> String {
        iso8601_utc(UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }

    fn evt(ts: u64, key: Option<&str>, level: &str, prev: &str, initial: bool) -> Value {
        json!({
            "ts": at(ts), "plugin": "fs", "key": key, "field": "percent",
            "level": level, "previous_level": prev, "value": 1.0,
            "prominent": false, "is_initial": initial, "hostname": "h",
        })
    }

    fn payload(history: &[Value], ongoing: &Ongoing, now: u64) -> Value {
        incidents_payload(
            history,
            ongoing,
            false,
            UNIX_EPOCH + std::time::Duration::from_secs(now),
        )
    }

    fn k(key: &str) -> IncidentKey {
        ("fs".into(), Some(key.into()), "percent".into())
    }

    #[test]
    fn opens_escalates_and_closes_one_incident() {
        let h = [
            evt(100, Some("/"), "warning", "ok", false),
            evt(160, Some("/"), "critical", "warning", false),
            evt(220, Some("/"), "ok", "critical", false),
        ];
        let p = payload(&h, &Ongoing::new(), 1_000);
        let inc = &p["incidents"].as_array().unwrap();
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0]["level"], "critical"); // escalation, monotonic
        assert_eq!(inc[0]["begin"], at(100));
        assert_eq!(inc[0]["end"], at(220));
        assert_eq!(inc[0]["ongoing"], false);
        assert_eq!(inc[0]["partial"], false);
        assert_eq!(inc[0]["duration"], "2m00s"); // frozen at end, not now
        assert_eq!(inc[0]["top"], json!([]));
        assert!(inc[0]["top_sort"].is_null());
    }

    #[test]
    fn level_stays_critical_after_de_escalation() {
        let h = [
            evt(100, None, "critical", "ok", false),
            evt(130, None, "warning", "critical", false),
        ];
        let ongoing = Ongoing::from([(
            ("fs".into(), None, "percent".into()),
            ("warning".into(), Some(at(100))),
        )]);
        let p = payload(&h, &ongoing, 200);
        assert_eq!(p["incidents"][0]["level"], "critical");
        assert_eq!(p["incidents"][0]["ongoing"], true);
        assert_eq!(p["incidents"][0]["duration"], "1m40s"); // to now
    }

    #[test]
    fn evicted_opening_is_partial_unless_is_initial() {
        let h = [evt(100, Some("/a"), "critical", "warning", false)];
        let p = payload(&h, &Ongoing::new(), 5_000);
        assert_eq!(p["incidents"][0]["partial"], true);
        let h = [evt(100, Some("/a"), "critical", "ok", true)];
        assert_eq!(
            payload(&h, &Ongoing::new(), 5_000)["incidents"][0]["partial"],
            false
        );
        // previous != ok AND is_initial: still the start.
        let h = [evt(100, Some("/a"), "critical", "warning", true)];
        assert_eq!(
            payload(&h, &Ongoing::new(), 5_000)["incidents"][0]["partial"],
            false
        );
    }

    #[test]
    fn partial_duration_is_a_lower_bound() {
        let h = [evt(100, Some("/a"), "critical", "warning", false)];
        let ongoing = Ongoing::from([(k("/a"), ("critical".into(), None))]);
        assert_eq!(
            payload(&h, &ongoing, 100 + 3_600 + 13 * 60)["incidents"][0]["duration"],
            ">1h13m"
        );
    }

    #[test]
    fn engine_says_recovered_closes_with_unknown_end() {
        let h = [evt(100, Some("/"), "warning", "ok", false)];
        let p = payload(&h, &Ongoing::new(), 500);
        assert_eq!(p["incidents"][0]["ongoing"], false);
        assert!(p["incidents"][0]["end"].is_null());
        // Now-based duration for a closed incident with unknown end.
        assert_eq!(p["incidents"][0]["duration"], "6m40s");
    }

    #[test]
    fn engine_authority_adds_alerts_the_journal_forgot() {
        let ongoing = Ongoing::from([(k("/old"), ("warning".into(), Some(at(50))))]);
        let p = payload(&[], &ongoing, 110);
        let inc = &p["incidents"][0];
        assert_eq!(inc["key"], "/old");
        assert_eq!(inc["level"], "warning");
        assert_eq!(inc["begin"], at(50)); // engine's instant wins
        assert_eq!(inc["partial"], false); // a known start is not a lower bound
        assert_eq!(inc["duration"], "1m00s");
        assert_eq!(inc["prominent"], false);
    }

    #[test]
    fn unknown_begin_has_a_null_duration() {
        let ongoing = Ongoing::from([(k("/x"), ("warning".into(), None))]);
        let p = payload(&[], &ongoing, 110);
        assert!(p["incidents"][0]["begin"].is_null());
        assert!(p["incidents"][0]["duration"].is_null());
        // Future begin (clock skew) is unknown too, never a negative number.
        let ongoing = Ongoing::from([(k("/x"), ("warning".into(), Some(at(9_999))))]);
        assert!(payload(&[], &ongoing, 110)["incidents"][0]["duration"].is_null());
    }

    #[test]
    fn ongoing_first_then_newest_first() {
        let h = [
            evt(100, Some("/a"), "warning", "ok", false),
            evt(110, Some("/a"), "ok", "warning", false), // resolved, oldest
            evt(200, Some("/b"), "warning", "ok", false),
            evt(210, Some("/b"), "ok", "warning", false), // resolved, newer
            evt(150, Some("/c"), "critical", "ok", false), // ongoing, older
            evt(300, Some("/d"), "warning", "ok", false), // ongoing, newest
        ];
        let ongoing = Ongoing::from([
            (k("/c"), ("critical".into(), Some(at(150)))),
            (k("/d"), ("warning".into(), Some(at(300)))),
        ]);
        let p = payload(&h, &ongoing, 400);
        let keys: Vec<&str> = p["incidents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["/d", "/c", "/b", "/a"]);
    }

    #[test]
    fn reopening_after_close_is_a_new_incident() {
        let h = [
            evt(100, Some("/"), "warning", "ok", false),
            evt(120, Some("/"), "ok", "warning", false),
            evt(200, Some("/"), "critical", "ok", false),
        ];
        let ongoing = Ongoing::from([(k("/"), ("critical".into(), Some(at(200))))]);
        let p = payload(&h, &ongoing, 260);
        assert_eq!(p["incidents"].as_array().unwrap().len(), 2);
        assert_eq!(p["incidents"][0]["ongoing"], true);
        assert_eq!(p["incidents"][1]["ongoing"], false);
    }

    #[test]
    fn duration_formats() {
        assert_eq!(format_duration_compact(43), "43s");
        assert_eq!(format_duration_compact(178), "2m58s");
        assert_eq!(format_duration_compact(3_600 + 13 * 60), "1h13m");
        assert_eq!(format_duration_compact(2 * 86_400 + 4 * 3_600), "2d04h");
        assert_eq!(format_duration_compact(-5), "0s");
    }

    #[test]
    fn envelope_carries_the_initializing_flag() {
        let p = incidents_payload(&[], &Ongoing::new(), true, SystemTime::now());
        assert_eq!(p, json!({ "is_initializing": true, "incidents": [] }));
    }
}
