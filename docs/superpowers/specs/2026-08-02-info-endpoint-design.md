# v0.4.0 — `/api/5/<plugin>/info` (field schema introspection)

> **Theme.** Expose the per-plugin field schema (`fields_description`) of
> Glances v5 as `GET /api/5/<plugin>/info`, keyed by field name. Read-only,
> inert, config-aware. Single new route; no new collection; one static
> metadata table that also becomes the single source of truth for the
> alerting engine's alertable-field set.
>
> **Authoritative contract.** Glances v5 `develop-v5`. The route is newer than
> the pushed branch — its shape is fixed by the maintainer's running test
> server (curl samples in §2), which supersedes the repo where they diverge.

---

## 1. Conservatism & scope

`/info` is additive and read-only: it introduces no new collection, no new
dependency, and does not touch the lazy-wake-up engine (§3) or any plugin's
`collect()`. The route itself is `&'static` data and allocates only when
called (like `/api/5/alert`). The one deliberate runtime change is the bounded
**alerting parity fix** (§7, Phase 1b): five alert attributes are corrected to
match Glances `/info`, which alters `_levels`/event output — and, for
`normalize_by="cpucore"`, level computation — but only for operators who have
configured `cpu`/`load` thresholds. Default config (no thresholds) is unchanged.

In scope:

- `GET /api/5/<plugin>/info` for all nine plugins.
- A unified static field-metadata table (`FieldInfo`) describing **every field
  each plugin emits**, replacing the alerting engine's `AlertField` table as
  the single source of truth.
- A bounded alerting parity fix aligning five alert attributes with Glances
  `/info` (§7).

Out of scope (unchanged from the v0.3.0 deferral list): the `sensors` plugin;
`/api/5/config`; JWT/Bearer auth; in-binary TLS. No per-item routes
(`/<item>/description`, `/<item>/unit`) — Glances v5's current shape is the
bulk `/info` dict, which is what we mirror.

---

## 2. Reference contract (Glances v5 `develop-v5`)

`/info` returns an object keyed by **field name** (never by collection item),
in a stable order. Each field carries a **whitelist** of keys — `min_symbol`,
`mmm`, `optional` from the internal `fields_description` are **not** serialised.
Envelope keys (`time_since_update`, `_levels`) are **not** fields and never
appear in `/info`.

| key | type | when present |
|---|---|---|
| `description` | string | always |
| `unit` | string | always (`bytes`, `percent`, `bytespers`, `bitpersecond`, `number`, `float`, `bool`, `string`, `seconds`) |
| `short_name` | string | when the field defines one |
| `primary_key` | `true` | only on a collection plugin's key field |
| `rate` | `true` | on per-second rate fields |
| `internal` | `true` | on fields the UI hides but the API still exposes (e.g. `cpucore`, `fs.fs_type`, `diskio.read_count`) |
| `watched` | `true` | on alertable fields |
| `watch_direction` | `"high"` / `"low"` | on watched fields |
| `prominent` | bool | on watched fields |
| `default_thresholds` | `{careful?, warning?, critical?}` | see §3 (glances-rs semantics differ) |
| `normalize_by` | string | on fields compared as `value / divisor` (`cpucore` or `bytes_speed_rate_per_sec`) |

**Omitted by design:** Glances also emits `history: true` and
`strict_thresholds: true` on some fields. glances-rs implements neither the
per-plugin history route nor strict-threshold semantics, so it does **not**
emit those keys — `/info` never advertises a capability the server lacks.

### Reference sample — scalar plugin (`mem`, maintainer's server)

```json
{"total":{"description":"Total physical memory available.","unit":"bytes"},
 "available":{"description":"Actual amount of available memory ...","unit":"bytes","short_name":"avail"},
 "percent":{"description":"Percentage usage calculated as (total - available) / total * 100.",
            "unit":"percent","watched":true,"watch_direction":"high","prominent":true,
            "default_thresholds":{"careful":50.0,"warning":70.0,"critical":90.0}},
 "used":{"description":"...","unit":"bytes"}, "free":{"description":"...","unit":"bytes"}, ...}
```

### Reference sample — collection + rate plugin (`network`, maintainer's server)

```json
{"interface_name":{"description":"Network interface name.","unit":"string","primary_key":true},
 "bytes_recv":{"description":"Bytes received per second.","unit":"bytespers","rate":true,
               "watched":true,"watch_direction":"high","prominent":false,
               "default_thresholds":{"careful":0.7,"warning":0.8,"critical":0.9},
               "normalize_by":"bytes_speed_rate_per_sec"},
 "bytes_sent":{... same shape ...},
 "errors_in":{"description":"Receive errors per second.","unit":"number","rate":true,
              "watched":true,"watch_direction":"high","prominent":false,
              "default_thresholds":{"careful":1.0,"warning":5.0,"critical":20.0}},
 "errors_out":{...}, "dropped_in":{"...","unit":"number","rate":true},
 "dropped_out":{...},
 "bytes_speed_rate_per_sec":{"description":"Estimated per-direction bandwidth capacity ...","unit":"bytespers"},
 "is_up":{"description":"Whether the interface is up.","unit":"bool"}}
```

---

## 3. `default_thresholds` — glances-rs semantics (config-reflecting)

Glances ships built-in default thresholds and always emits them on watched
fields. **glances-rs ships none** (config-only alerting — the deliberate
v0.3.0 conservatism divergence, §5.1 of the alerting spec). Decision for
`/info`:

- `default_thresholds` reflects the **effectively configured global
  thresholds** for that field: `[plugins.<p>].thresholds.<field>`.
- Emitted only when configured; the key is **absent** otherwise.
- Only the limits set to `Some` appear (`careful`/`warning`/`critical` are
  each optional in glances-rs config); a partial threshold yields a partial
  object.
- Per-item overrides (`thresholds_by_item`) are **not** reflected — `/info`
  is a per-field schema, and item overrides are item-specific, outside it.

Consequence, documented: a field may carry `watched: true` **without**
`default_thresholds` (alertable but unconfigured) — a direct, intended result
of the no-defaults + reflect-config decisions. `watched` /
`watch_direction` / `prominent` / `rate` / `normalize_by` / `unit` /
`description` / `primary_key` / `short_name` are all **static** and always
emitted for their field regardless of config; only `default_thresholds` is
dynamic.

---

## 4. Architecture — unified `FieldInfo` table

New module `src/plugins/fields.rs`: the single source of static field
metadata. It describes **every emitted field**, not just watched ones.

```rust
// as_str() strings come from the maintainer's /info dumps (§7 source note).
pub enum Unit { Bytes, Percent, BytesPerSec, BitPerSec, Number, Float, Bool, StringT, Seconds }
impl Unit { pub fn as_str(&self) -> &'static str { /* "bytes", "percent", "bytespers", … */ } }

pub struct FieldInfo {
    pub field: &'static str,
    pub description: &'static str,
    pub unit: Unit,
    pub short_name: Option<&'static str>,
    pub primary_key: bool,           // true on the collection key field (cross-check key_field())
    pub rate: bool,
    pub internal: bool,              // Glances `internal: true` (UI-hidden, API-exposed)
    pub watched: bool,
    pub direction: Direction,        // moved here from alerts.rs
    pub prominent: bool,
    pub normalize_by: Option<&'static str>,
}

pub fn fields(id: PluginId) -> &'static [FieldInfo];
```

- `Direction` (currently in `alerts.rs`) **moves to** `plugins::fields`;
  `alerts.rs` imports it.
- `alerts.rs` no longer defines `AlertField`. `alert_fields(id)` becomes a
  view over the watched subset:
  `fields(id).iter().filter(|f| f.watched)`. The alerting code that reads
  `field` / `prominent` / `direction` / `normalize_by` reads them off
  `FieldInfo` unchanged (same field names, same types).

**Layering.** Field metadata is plugin-domain schema, so it lives under
`plugins::`, not in `api/` or `alerts/`. Both `api/` (the `/info` handler) and
`alerts.rs` depend on it; neither owns it.

### Phased delivery (medium risk → phased, reversible)

- **Phase 1 (behaviour-neutral refactor):** introduce `fields.rs` with the
  full per-plugin tables, carrying the **current** v0.3.0 alert attributes
  verbatim; make the alerting path derive its alertable set from the watched
  subset; delete `AlertField`. **No behavioural change** — the entire existing
  `alerts.rs` unit + integration suite is the gate and must stay green. A guard
  test asserts the watched subset of `fields(id)` equals the pre-refactor
  `AlertField` set field-for-field (field, prominent, direction, normalize_by).
- **Phase 1b (alerting parity fix, behaviour-changing, isolated):** adjust the
  five diverging alert attributes to match Glances `/info` (§7). This is the
  only step that changes alerting output; it lands as its own reviewable delta
  with updated alerting tests, *after* Phase 1 has proven the refactor neutral.
- **Phase 2 (new feature):** add the `/info` route consuming `fields(id)`.
  Lands only after the table is in place and the parity fix is committed.

---

## 5. Handler semantics

`GET /api/5/<plugin>/info`:

- **Routing.** New axum route `"/api/5/{plugin}/info"` alongside the existing
  `"/api/5/{plugin}"`, under the same auth/CORS/trusted-host stack.
- **Inert.** Reads only `fields(id)` + `config`. Never wakes a collector,
  never reads the store or the registry, never waits, never `503`. Same
  read-only class as `/api/5/alert`.
- **Unknown plugin → `404`** (glances-rs convention: `PluginId::parse`
  failure, consistent with the data route; Glances returns `400` — a
  pre-existing documented divergence).
- **Body.** For each `FieldInfo` in table order, build an object with the
  whitelist keys of §2: always `description` + `unit.as_str()`; then
  `short_name`, `primary_key: true`, `rate: true`, and for watched fields
  `watched: true` + `watch_direction` + `prominent` + `normalize_by`; then
  `default_thresholds` per §3 (config lookup). Field iteration order follows
  the static table (stable, mirrors emission order).

---

## 6. Field-set invariant & the `network` divergence

`/info` describes **the fields glances-rs emits**, not Glances' field list.
So glances-rs `network/info` lists `interface_name`, `bytes_recv`,
`bytes_sent`, `bytes_all`, `is_up`, `speed`, `bytes_speed_rate_per_sec`, and
`alias` (conditional) — and **omits** `errors_*`/`dropped_*` (not collected).
It authors descriptions for `bytes_all`/`speed`/`alias` (glances-rs fields
absent from Glances `/info`).

**Invariant (tested).** Two directions, split by platform because several
fields are Linux-only (`mem.active/inactive/buffers/cached`,
`network.is_up/speed/bytes_speed_rate_per_sec`, `memswap.sin/sout`, all of
`diskio`):

- **All platforms — no undocumented field:** every emitted data key (except
  the envelope keys `time_since_update` / `_levels`) appears in `/info`. This
  is the load-bearing direction and always holds.
- **Linux — no phantom field:** every `/info` key except the conditional
  `alias` appears in the emitted data. Tested `#[cfg(target_os = "linux")]`,
  where the full field set is present.

Cross-platform tests use `mem` (scalar) and `fs` (collection) for the
always-true direction; the Linux-only direction covers the platform-gated
plugins.

---

## 7. Parity & sourcing

**Source of truth = the maintainer's running `develop-v5` server**, whose
`/info` output was captured for all nine plugins. The `develop-v5` *repo* is
stale (its `fields_description` dicts carry neither the alert attributes nor
the `bytespers` units the server now returns), so the captured dumps override
the repo field-for-field. `unit` uses the server vocabulary verbatim
(`bytes`, `percent`, `bytespers`, `bitpersecond`, `number`, `float`, `bool`,
`string`, `seconds`); the `Unit` enum's `as_str()` is the mapping and the
parity anchor. Fields glances-rs emits that have no Glances entry
(`network.bytes_all`, `network.speed`, `fs.alias`, `diskio.alias`) get
authored descriptions.

**Alerting parity fix (decided — Phase 1b).** glances-rs's v0.3.0 alert
attributes diverge from the server `/info` on exactly five fields; all are
corrected to match, and no field is added to or removed from any plugin's
watched set:

| field | v0.3.0 | corrected to |
|---|---|---|
| `cpu.iowait` | `prominent=false` | `prominent=true` |
| `cpu.steal` | `prominent=true` | `prominent=false` |
| `cpu.ctx_switches` | `prominent=true`, no normalize | `prominent=false`, `normalize_by="cpucore"` |
| `load.min5` | no normalize | `normalize_by="cpucore"` |
| `load.min15` | no normalize | `normalize_by="cpucore"` |

`prominent` changes only affect the `_levels` / event decoration.
`normalize_by="cpucore"` **changes level computation** (value compared per
core) for those fields when a threshold is configured — a genuine parity fix
(load average is meaningfully per-core). The divisor `cpucore` is already in
the `cpu` and `load` payloads, so the existing `level_for` normalize path
(spec §5, alerting) works unchanged. The `default_thresholds` divergence
(config-only, §3) is the one deliberate, retained difference.

---

## 8. Tests

**Unit (`plugins::fields`), Phase 1:**
- `fields(id)` non-empty for all nine plugins (including `system`/`uptime`,
  which have emitted fields even though nothing is watched).
- Guard: the watched subset of `fields(id)` equals the pre-refactor
  `AlertField` set for every plugin (field, prominent, direction,
  normalize_by) — proves the Phase-1 refactor is behaviour-neutral.
- `primary_key: true` appears on exactly the field named by `key_field(id)`
  for each collection plugin, and on no field of a scalar plugin.
- `Unit::as_str()` maps each variant to its Glances string.

**Unit (`alerts.rs`), Phase 1b (parity fix):**
- The guard test is updated to the corrected attributes (the five rows of §7).
- A `normalize_by="cpucore"` case: with `cpucore` in the payload and a
  configured `load.min15` threshold, the level is computed on `value / cpucore`
  (not raw); the existing normalize tests gain a cpucore analogue.

**Integration (`tests/info.rs`, via `oneshot`), Phase 2:**
- `mem/info` matches the §2 scalar shape (scalar, no `primary_key`,
  `percent` watched/prominent).
- `network/info` keyed by field name; `interface_name` has `primary_key:true`;
  `bytes_recv` has `rate:true` + `watched:true` + `normalize_by`.
- `cpu/info` carries `internal:true` on `cpucore`, and no `history`/
  `strict_thresholds` key on any field.
- `default_thresholds` **absent** with default config; **present** (and
  partial-aware) after configuring `[plugins.mem].thresholds.percent`.
- Field-set invariant (§6) for at least one scalar and one collection plugin.
- Unknown plugin → `404`.
- `/info` never wakes a collector (`active_collectors() == 0` after the call).
- Reachable under auth (behind Basic when a password is configured; probes-
  style test).

Existing `alerts.rs` suite stays green through Phase 1 (the neutral gate) and
is updated only in Phase 1b for the five corrected attributes.

---

## 9. Footprint

Expected **indistinguishable from v0.3.0** under default config: `fields.rs` is
`&'static` (zero runtime allocation, a few KiB of binary), and `/info`
allocates only per call (like `/api/5/alert`). No new dependency. Acceptance
gate: `scripts/footprint.sh` re-baseline vs v0.3.0 — rest/2/10/100 req/s RSS
and CPU within run-to-run variance; binary size delta from the metadata table
recorded.

---

## 10. Docs & version

- `Cargo.toml`: 0.3.0 → 0.4.0.
- `docs/api.md`: new `/api/5/<plugin>/info` section — the key whitelist, the
  config-reflecting `default_thresholds` semantics, the `404`-on-unknown
  divergence, and the field-set invariant.
- `ARCHITECTURE.md`: record `plugins::fields` as the single source of static
  field metadata; note `AlertField` is retired and the alerting engine now
  derives its alertable set from the watched subset.
- `DEVELOPMENT_PLAN.md`: v0.4.0 section; move `/info` out of the deferred list.
- `docs/footprint-audit-v0.4.0.md`: the §9 re-baseline.
