# v0.3.0 — Alerting design spec

> Status: approved design, pre-implementation. Scope locked to **alerting
> only** (the v0.3.0 headline). `/api/5/<plugin>/info`, `sensors`,
> `/api/5/config`, JWT/Bearer and in-binary TLS stay deferred.

## 1. Goal

Close the last payload-parity gap with Glances v5: populate the per-field
`_levels` decoration and serve `/api/5/alert`, reproducing the behaviour of
the reference implementation
[`glances/alerts_v5.py`](https://github.com/nicolargo/glances/blob/develop-v5/glances/alerts_v5.py)
within the constraints of the lazy-collection engine (§3) and the footprint
mandate.

Two non-negotiable constraints frame every decision below:

- **Conservatism on defaults.** No built-in thresholds. With the default
  config, `_levels` stays `{}`, `/api/5/alert` returns `[]`, and the alert
  path is effectively free (a single HashMap lookup per cycle, then an early
  return). Alerting is opt-in via configured thresholds — no new default
  behaviour for unconfigured users.
- **Footprint.** The only new persistent state is a bounded
  `VecDeque<Event>` (≤ `history_size`, default 200) plus a hysteresis map
  whose entries exist only for `(plugin, key, field)` triples that have a
  configured threshold. Both are pruned (§6).

## 2. Reference model (Glances v5)

Verified against `alerts_v5.py` and `plugins/plugin/model.py` on
`develop-v5`. The model is **event-based**, not interval-based:

- **Per-field level** comes from `get_alert(value)`: compare the value to the
  configured `careful`/`warning`/`critical` limits → one of
  `ok|careful|warning|critical`. Limits resolve hierarchically
  (`{stat}_{level}` then `{plugin}_{level}`). **No hardcoded defaults** in the
  base model — they live in `glances.conf`.
- **Hysteresis** (`_reconcile`): an observed level must persist for
  `min_duration` (default `_DEFAULT_MIN_DURATION_SECONDS = 5.0`) before the
  transition is committed. State per `(plugin, key, field)`:
  `_AlertState { committed_level, pending_level, pending_since, has_committed }`.
- **Event journal**: each committed transition appends an event to
  `deque(maxlen=history_size)` (`_DEFAULT_HISTORY_SIZE = 200`). `_build_event`
  fields: `ts, plugin, key, field, level, previous_level, value, prominent,
  is_initial, hostname`. `is_initial = not state.has_committed` (first commit
  out of the default `ok`). `prominent` defaults `True`.
- **Endpoint**: `get_history()` returns `list(self._history)` (most-recent
  last). No separate `end` timestamp — a return to `ok` is just another event.
- **Known reference defect**: Glances does **not** garbage-collect `_state`
  for disappeared collection items — a slow leak. glances-rs deliberately does
  better (§6).

## 3. Architecture

### 3.1 Where evaluation lives — approach (A), chosen

A shared `Alerts` component in `AppState`, fed by each plugin loop at publish
time. Rejected alternatives:

- **(B)** levels computed in-loop, journal in `AppState` — splits state that
  wants to be co-located (the hysteresis map and the history both must survive
  idle/wake and are written together). Looser layering, no benefit.
- **(C)** lazy evaluation at request time, no per-cycle work — **incompatible
  with hysteresis**: `min_duration` persistence cannot be reconstructed from a
  single stored sample, and transitions between requests would be lost.
  Rejected.

(A) honours the §3/§5 layering: I/O stays in the plugin loops, the alert state
is one explicit cross-cutting component, the single source of truth — the same
shape as the collector registry.

### 3.2 Why the state cannot live in the plugin loop

Plugin loops are independent, own a stateless-object `P::State`, and **stop
at idle** — `P::State` is dropped (`collector.rs::plugin_loop`). The
hysteresis map and the history must survive an idle→wake cycle, so they live
in `AppState`, not in `P::State`. This is the core consequence of the lazy
model and the reason `Alerts` is a new shared component.

### 3.3 Integration point

`envelope()` is called *inside* each plugin's `collect()`, baking
`_levels: {}` today. The wiring goes in `collector.rs::plugin_loop`, between
`collect()` and `publish()` — **plugins are not touched**:

```
let mut value = plugin.collect(&mut state).await;
app.alerts.observe(id, &mut value);   // reads field values, rewrites _levels,
app.publish(id, value).await;         // updates hysteresis + history in place
```

`observe` reads the freshly collected field values back out of the envelope
`Value`, looks up the plugin's configured thresholds, computes levels, runs
`_reconcile`, appends committed transitions to the history, and **rewrites the
`_levels` key in place** before publication. A plugin with no configured
thresholds early-returns after the config lookup (the conservatism/footprint
guarantee).

## 4. Components and contracts

### 4.1 `src/alerts.rs` (new)

```rust
pub struct Alerts {
    inner: Mutex<AlertsInner>,   // history + state behind one lock
    hostname: String,            // sysinfo::System::host_name(), captured once
    history_size: usize,         // [alerts].history_size, default 200
    min_duration: HashMap<PluginId, Duration>, // per-plugin effective window
                                 // ([plugins.<p>].min_duration_seconds ??
                                 //  [alerts].min_duration_seconds (5.0)),
                                 // precomputed from Config at construction
}

struct AlertsInner {
    history: VecDeque<Event>,                       // bounded maxlen = history_size
    state: HashMap<StateKey, AlertState>,           // hysteresis, pruned (§6)
    last_seen: HashMap<PluginId, Instant>,          // idle-gap detection (§5.2)
}

type StateKey = (PluginId, Option<String>, String); // (plugin, item key, field)

struct AlertState {
    committed_level: Level,
    pending_level: Option<Level>,
    pending_since: Option<Instant>,
    has_committed: bool,
}
```

One `Mutex` guards `AlertsInner` — the critical section is a per-cycle field
walk, short and uncontended (one active loop touches one plugin's entries).
`observe(&self, id, value: &mut Value)` and `history(&self) -> Vec<Event>`
are the only public methods.

For collection plugins, `observe` extracts each item's primary-key **value**
from the `data` items (the value is in the payload; only the key *name*
metadata was omitted from the envelope). It gets the key field name from a
per-plugin descriptor — `PluginId::key_field() -> Option<&'static str>`
(`None` for scalar plugins; `network` → `interface_name`, `fs` → `mnt_point`,
`diskio` → `disk_name`). That key value builds the `StateKey`, indexes
`thresholds_by_item`, and drives the §6 stale-key pruning.

### 4.2 `Event` (parity with `_build_event`)

`{ ts, plugin, key, field, level, previous_level, value, prominent,
is_initial, hostname }`. `ts` is ISO 8601 UTC. `key` is `null` for scalar
plugins, the item primary-key value for collection plugins. Serialized as the
public view returned by `/api/5/alert` (never a raw internal struct — §7).

### 4.3 `_levels` in the envelope

**`_levels` is always top-level in the envelope** (never inside `data` items).
Verified against `alerts_v5.py::_observations` and
`base_v5.py::_compute_levels_for_item` on develop-v5: scalar and collection
plugins differ only in the nesting depth. Each leaf entry is
`{ "level": <str>, "prominent": <bool> }`. `prominent` is a **static per-field
property** (§4.6), not config — it must be emitted to match Glances
byte-for-byte (the renderer uses it for highlight mode); it is *not* omitted.

- **Scalar plugins** (`cpu`, `mem`, `load`, `system`, `uptime`, `memswap`):
  keyed by field name —
  ```json
  "_levels": { "percent": { "level": "careful", "prominent": true } }
  ```
  Emitted for every **alertable** field (§4.6) that has a configured threshold,
  including the `ok` level (so a client sees the field return to `ok`). Fields
  that are not alertable, or alertable with no configured threshold, are absent
  from `_levels`.
- **Collection plugins** (`network`, `fs`, `diskio`): keyed by the
  **stringified primary-key value**, then field name —
  ```json
  "_levels": {
    "/":     { "percent": { "level": "critical", "prominent": false } },
    "/home": { "percent": { "level": "ok",       "prominent": false } }
  }
  ```
  i.e. `_levels[str(pk_value)][field] = { "level": <str>, "prominent": <bool> }`.
  `_observations` joins these back to `payload["data"]` items via
  `plugin._primary_key` — in glances-rs the pk field is `PluginId::key_field()`
  (§4.1). Only items present in the current sample, and only alertable fields
  with a configured threshold (global or per-item, §4.5), appear.

`envelope()` produces the `_levels: {}` placeholder as today; the loop's
`observe` step rebuilds it **fresh from the current sample each cycle** and
overwrites it before publication — so `_levels` itself can never leak stale
items (the §6 pruning concern is only the internal hysteresis `state` map).
The shape is frozen field-for-field in `docs/api.md` §4/§5 during
implementation.

### 4.4 Route `/api/5/alert`

A new handler in `api/mod.rs`, added to the `/api/5` sub-router under the same
§7 security layers (auth, CORS, trusted host) as the other API routes. It
**does not wake or wait** — it reads the accumulated history only (like
`pluginslist`, it is cheap and side-effect free). Returns `200` with a JSON
array, `[]` when empty. It never returns `503`.

### 4.5 Config

Extend `PluginConfig` (in `config.rs`). Thresholds are declared at **two
levels**, mirroring Glances' global `{field}_{level}` and specific
`{key}_{field}_{level}`:

```toml
# Scalar plugin — keyed by field name
[plugins.cpu.thresholds.total]
careful  = 70.0
warning  = 80.0
critical = 90.0

# Collection plugin, GLOBAL — applies to every item, keyed by field name
[plugins.fs.thresholds.percent]
careful  = 70.0
warning  = 80.0
critical = 90.0      # any subset of the three keys is valid

# Collection plugin, SPECIFIC — keyed by item primary key, then field name
[plugins.fs.thresholds_by_item."/".percent]
critical = 95.0      # overrides only `critical` for mount point "/"

# Optional per-plugin hysteresis window — uniform across all items (§5.3)
[plugins.fs]
min_duration_seconds = 10.0   # overrides the global [alerts] default for fs
```

```rust
pub struct PluginConfig {
    // …existing fields…
    pub thresholds: HashMap<String, Thresholds>,                          // global, keyed by field
    pub thresholds_by_item: HashMap<String, HashMap<String, Thresholds>>, // item key -> field -> thresholds
    pub min_duration_seconds: Option<f64>,                                // per-plugin override; uniform over items
}
pub struct Thresholds { careful: Option<f64>, warning: Option<f64>, critical: Option<f64> }
```

**Resolution (per-limit merge, faithful to `get_limit`).** For
`(item, field)`, each limit `careful`/`warning`/`critical` resolves
independently: the item-specific value
(`thresholds_by_item[item][field].<limit>`) wins when set, otherwise the
global (`thresholds[field].<limit>`), otherwise that limit is unset. A
specific override therefore overrides **only the limit keys it declares** and
inherits the rest from global — it does *not* replace the whole field block.
Scalar plugins use `thresholds` only (`thresholds_by_item` is empty/ignored).
The effective `Thresholds` is computed once per `(item, field)` at observation
time.

A new global section:

```toml
[alerts]
history_size = 200            # default
min_duration_seconds = 5.0    # default
```

Validation: thresholds, when present, must be finite and ordered
`careful ≤ warning ≤ critical` for whichever subset is set. Validate both each
declared block **and** the merged effective set for every declared
`(item, field)` pair — both global and `thresholds_by_item` are fully known at
config load, so a partial override that merges into an out-of-order set fails
closed at startup with a clear message (reuse the existing `validate()`
pattern). `history_size ≥ 1`; `min_duration_seconds ≥ 0` for both the global
`[alerts]` value and any per-plugin `[plugins.<name>]` override.

### 4.6 Alertable-field metadata (the "watched" set)

In Glances v5 only fields flagged `watched: True` in `fields_description`
produce `_levels`; each such field also declares a static `prominent` flag and,
for rates, a `normalize_by` divisor and `watch_direction`. glances-rs mirrors
this with a small **static per-plugin table** — the alertable allow-list — that
the `observe` step consults:

```rust
enum Direction { High, Low }            // alert on high vs. low values

struct AlertField {
    field: &'static str,         // e.g. "percent", "bytes_recv"
    prominent: bool,             // replicated from Glances (mem.percent=true,
                                 //   fs.percent=false, network.*=false, …)
    direction: Direction,        // watch_direction; "high" for every v0.3.0
                                 //   field (verified across the 9 v5 schemas),
                                 //   but the engine handles both (§5.1)
    normalize_by: Option<&'static str>, // divisor field, or None for direct compare
}
fn alert_fields(id: PluginId) -> &'static [AlertField];
```

Only fields in this table emit `_levels`, and only when a threshold is
configured (we ship **no** `default_thresholds`, §1 — the conservatism
divergence from Glances, whose schema ships defaults). The `prominent` values
are copied verbatim from the develop-v5 schemas for UI parity. The exact
per-plugin allow-list is enumerated during implementation against develop-v5;
the headline fields: `mem.percent` (prominent true), `fs.percent` (false),
`cpu` load fields, `load.min1/5/15`, `network.bytes_recv/bytes_sent`
(normalize_by, false) + `errors_in/out` (false), `memswap.percent`,
`diskio.read_bytes/write_bytes`.

**`normalize_by` (chosen in-scope for v0.3.0).** When a field declares a
divisor, the level is computed against `value / divisor`; if the divisor is
absent, `0`, or non-finite, the level entry is **skipped** (no `_levels` entry,
no event) — exactly Glances' "unknown link speed" semantics. This requires the
**`network` plugin to expose a new field** `bytes_speed_rate_per_sec` =
`speed_mbits × 1e6 / 8 / 2` (Mbit/s → bytes/s, full-duplex per-direction
split), `0` when unknown. Source: `/sys/class/net/<iface>/speed` on Linux
(degrade to `0` off-Linux / virtual / down interfaces). This is a
**payload-parity addition** to `network` (docs/api.md §5.4), independent of the
alert path but required by it. Thresholds for normalized fields are **ratios in
[0, 1]** (e.g. `bytes_recv.warning = 0.8` = 80 % of per-direction capacity),
unlike the direct percentage thresholds of `mem`/`fs`.

## 5. Lazy-model decisions

### 5.1 Level computation

`level(value)` = highest breached limit among the **effective** subset for
that `(item, field)` (resolved per §4.5: item-specific over global, per-limit),
following `watch_direction`. **High** (`value ≥ careful → careful`, `≥ warning
→ warning`, `≥ critical → critical`, else `ok`) and **Low** (the mirror:
`value ≤ careful → careful`, … `≤ critical → critical`, else `ok`) are both
implemented in `compute_level(value, thresholds, direction)`. Every v0.3.0
alertable field is `High` (verified across the nine v5 schemas — none is `Low`);
the `Low` path is exercised by unit tests so the per-field contract is complete
and correct the day a low-direction field is added. The compared quantity is
the field value directly for most fields, or `value / divisor` when the field
declares `normalize_by` (§4.6); a missing or zero divisor **skips** the field
(no entry). Direct fields are already the percentages/counters Glances compares;
normalized fields compare a ratio in `[0, 1]` against ratio thresholds.

**Raw level vs. debounced events.** This is the value written into `_levels`
— the **raw, instantaneous** level, recomputed every cycle. The `min_duration`
hysteresis (§5.2) gates **only the event journal** (`/api/5/alert`), never
`_levels`. This matches Glances: `_observations` reads `_levels` raw, and
`_reconcile` debounces the transitions that become history events. So a brief
spike shows immediately in `_levels` but only produces an `/alert` event if it
persists past `min_duration`.

### 5.2 Idle-gap rule (deliberate divergence, documented §6.2)

`observe` records `last_seen[plugin] = now` each cycle. If the gap since the
previous observation for that plugin exceeds **2 × the plugin's refresh
period** (i.e. the loop had stopped and re-woken), the pending state is reset
(`pending_level = None`, `pending_since = None`) before reconciling, so a
stale `pending_since` from before the idle gap cannot insta-commit on wake.
`committed_level` is **kept** (no spurious `is_initial` re-fire). Consequence,
to be documented as a §6.2-style divergence in `docs/api.md`: with sporadic
polling a transient breach shorter than `min_duration` of *sustained active*
observation may never commit — acceptable (no client watching → no alert
needed), and a direct result of the lazy contract.

### 5.3 min_duration scope (v0.3.0)

Two levels: the global default `[alerts].min_duration_seconds` (5.0) and an
optional **per-plugin** override `[plugins.<name>].min_duration_seconds`. The
effective `min_duration` is resolved once per plugin
(`plugins[plugin].min_duration_seconds.unwrap_or(global)`) and applied
**uniformly to every item** of a collection plugin — there is deliberately no
per-item `min_duration`. Unlike **thresholds** (global + per-item, §4.5),
`min_duration` is uniform within a plugin: all of a plugin's `(item, field)`
hysteresis windows share the same duration. The finer per-field / per-level /
per-item hierarchy of the reference
(`{pk}_{field}_{level}_min_duration_seconds` …) is **deferred** (YAGNI) and
can be layered on later without changing the journal or the envelope.

## 6. §8.1 cleanup — better than the reference

The reference leaks `_state` entries for disappeared collection items. Per
ARCHITECTURE.md §8.1, glances-rs must not. On each collection-plugin cycle,
`observe` prunes from `state` (and `last_seen`-adjacent maps) every
`(plugin, key, field)` whose `key` is absent from the current sample — the
same "current sample only" discipline as `network`'s `previous`. This is
called out as a code comment so a future change does not reintroduce the
phantom-level leak. Scalar-plugin entries (`key = None`) are stable and need
no pruning. The history `VecDeque` is independently bounded by `history_size`.

## 7. Security

`/api/5/alert` sits behind the existing §7 stack. The event payload is an
explicit *public* view (no raw internal struct, §7 layering). Alert events
expose only metric metadata (plugin, field, level, value, timestamp,
hostname) — no credentials or paths — so no additional public-view filter is
needed (unlike `/api/5/config`, which is why that route stays deferred).

## 8. Tests

- **`alerts.rs` unit tests**: level computation per limit subset, for **both**
  `Direction::High` and `Direction::Low` (the `Low` mirror: low value →
  breach), even though no v0.3.0 field is `Low`; `normalize_by` transform
  (`value / divisor`) and
  **skip** when the divisor is absent / `0` / non-finite; only alertable
  (`watched`) fields emit `_levels`, each with its static `prominent` flag
  (`mem.percent` true, `fs.percent` false); `_reconcile` hysteresis (no commit
  before `min_duration`, commit after, return-to-ok event, `is_initial` only on
  first commit); idle-gap reset; history bounded at `history_size` (ring
  eviction); §6 stale-key pruning for collection items.
- **`network` plugin**: `bytes_speed_rate_per_sec` = `speed_mbits × 1e6 / 8 / 2`
  from a captured `/sys/class/net/<iface>/speed` sample; `0` when speed is
  absent / `-1` (virtual or down interface), which then skips the rate fields'
  levels.
- **Config**: thresholds parse (scalar `thresholds` and collection
  `thresholds_by_item`); **two-level resolution** — a per-item override sets
  only `critical` and inherits `careful`/`warning` from global (per-limit
  merge); ordering validation rejects `careful > warning` both in a declared
  block and in a merged global+item effective set; `[alerts]` defaults applied
  when absent; per-plugin `min_duration_seconds` overrides the global default
  and applies uniformly to every item of a collection plugin.
- **Integration** (`tests/`): with thresholds configured, a breaching value
  populates `_levels` in the plugin payload and, after `min_duration`,
  produces an event at `/api/5/alert`; with no thresholds, `_levels` stays
  `{}` and `/api/5/alert` returns `[]`; `/api/5/alert` never wakes a collector
  (active-collector count unchanged) and is reachable under auth.

## 9. Footprint acceptance

`scripts/footprint.sh` re-run for v0.3.0: with the **default config (no
thresholds)** RSS/CPU must be indistinguishable from v0.2.0 — the alert path
early-returns. A separate run with thresholds on every plugin measures the
worst-case overhead (per-cycle field walk + bounded history). Both recorded in
a `docs/footprint-audit-v0.3.0.md`, the Phase 9 pattern. Any regression on the
default-config axis is a blocker — **except** the one unconditional addition:
`network` now reads `/sys/class/net/<iface>/speed` each cycle for
`bytes_speed_rate_per_sec` (a payload field, collected regardless of alert
config). Measure and record that delta separately; it should be a single small
`/sys` read per interface per cycle (cache the parse if it shows up).

## 10. Out of scope (restated)

Per-field/per-level/per-item `min_duration` overrides; built-in
`default_thresholds` (config-only — the §1 conservatism divergence);
categorical (set-membership) thresholds — every v0.3.0 alertable field is
numeric (`watch_direction` itself **is** implemented, §5.1, with no `Low` field
yet); operator-configurable `prominent` (the flag is static per field, copied
from Glances — not a config key); `/api/5/<plugin>/info`, `sensors`,
`/api/5/config`, JWT/Bearer, in-binary TLS.

## 11. Authoritative-doc updates this entails

- `ARCHITECTURE.md`: new §on the `Alerts` component (alongside §5.1 state
  primitives); resolve the §8.1 "when alerting is added later" note; record
  the idle-gap divergence.
- `docs/api.md`: `/api/5/alert` route + payload; `_levels` shape (top-level
  for both; scalar keyed by field, collection keyed by `str(pk)` then field;
  leaf `{ "level", "prominent" }` — §4.3/§4.6) frozen against develop-v5; the
  new `network.bytes_speed_rate_per_sec` field in §5.4; the §6.2-style lazy
  divergence note.
- `DEVELOPMENT_PLAN.md`: open the v0.3.0 phase; move alerting out of
  "deferred"; note the `network` payload-parity addition.
