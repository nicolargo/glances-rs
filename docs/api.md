# glances-rs — REST API contract (v1)

> **Purpose.** This document freezes the observable API of `glances-rs` v1
> against the Glances v5 REST contract (`routes_v5.py` on the `develop-v5`
> branch), and records the Phase 1 spike findings that constrain the
> implementation. Once frozen, changes to payload shapes are breaking changes.

---

## 1. Routes

| Route                    | Method | Response                                | Status codes |
|--------------------------|--------|-----------------------------------------|--------------|
| `/api/5/{plugin}`        | GET    | The plugin's payload (object or array)  | `200`, `404` unknown plugin, `503` collection did not start in time |
| `/api/5/all`             | GET    | Object: `{ "<plugin>": <payload>, … }`  | `200` (possibly partial — see §3) |
| `/api/5/pluginslist`     | GET    | Sorted array of plugin names: `["cpu","load","mem","network"]` | `200` |
| `/status`                | GET    | Empty body                              | `200`; never wakes plugins, never requires auth |
| `/healthz`               | GET    | Empty body                              | `200`; never wakes plugins, never requires auth |

Glances v5 routes **not** implemented in v1 (deliberate, ARCHITECTURE.md §6.1):
`/api/5/token` (Basic auth only), `/api/5/{plugin}/info`, `/api/5/alert`,
`/api/5/config`.

## 2. Divergences from Glances v5 (documented contract differences)

| Behaviour | Glances v5 | glances-rs |
|---|---|---|
| Known plugin, no data yet | `200` with `null` body | **Waits** for the first collection cycle; `503` if it does not arrive within the guard timeout. A `200` always carries real data. |
| Auth | Basic + Bearer token (`/token`) | Basic only (v1). |
| Optional fields | Present per platform/psutil | Subset per `sysinfo` capability — see per-plugin notes below. Clients must treat absent optional fields as "not available", exactly as with Glances' platform-specific fields. |

## 3. `/all` partial-failure policy

`/api/5/all` wakes all plugins concurrently and returns `200` with every
plugin that produced data; a plugin that exceeded the guard timeout is
**absent from the object** rather than failing the whole response
(ARCHITECTURE.md §6.3). Clients needing per-plugin failure semantics should
query `/api/5/{plugin}` and rely on `503`.

## 4. Rate-field convention (inherited from Glances)

For every cumulative-counter field `X` marked *rate* below, the payload
carries three values plus a shared timestamp field:

- `X` — delta of the counter over the last interval;
- `X_gauge` — the raw cumulative counter;
- `X_rate_per_sec` — `X / time_since_update`;
- `time_since_update` — measured seconds (float) between the two samples
  (real `Instant` elapsed, never the nominal refresh — ARCHITECTURE.md §5.4).

## 5. Payload schemas

### 5.1 `mem` — object, instantaneous

```json
{
  "total":     16856244224,
  "available": 16233152512,
  "percent":   3.7,
  "used":      623091712,
  "free":      15423582208
}
```

- `percent = (total - available) / total * 100` — the Glances formula.
- All sizes in bytes (`u64`).
- Glances optional platform fields (`active`, `inactive`, `buffers`,
  `cached`, `wired`, `shared`) are **omitted in v1**: `sysinfo` does not
  expose them. They are optional in Glances too, so clients already
  tolerate their absence.

### 5.2 `load` — object, instantaneous

```json
{
  "min1":    0.25,
  "min5":    0.19,
  "min15":   0.09,
  "cpucore": 4
}
```

- Full field parity with Glances v5.
- **Windows:** `sysinfo` emulates a load average through PDH performance
  counters; values may legitimately be `0.0`. The payload shape is the same
  on every platform (degraded values, not absent fields).

### 5.3 `cpu` — object, rate

```json
{
  "total":             12.5,
  "cpucore":           4,
  "time_since_update": 2.004
}
```

- `total` — global CPU usage percent (all cores combined), the headline
  Glances field.
- **v1 subset:** Glances' `user`/`system`/`idle`/`iowait`/`steal`/… split
  and the `ctx_switches`/`interrupts` counters are **not exposed by
  `sysinfo`** and are omitted in v1. Adding the split later (e.g. by reading
  `/proc/stat` on Linux) extends the object without breaking it.

### 5.4 `network` — **array** of objects (collection plugin), rate

One element per interface; primary key `interface_name`:

```json
[
  {
    "interface_name":          "eth0",
    "bytes_recv":              1024,
    "bytes_recv_gauge":        1548273,
    "bytes_recv_rate_per_sec": 511.2,
    "bytes_sent":              2048,
    "bytes_sent_gauge":        5153559,
    "bytes_sent_rate_per_sec": 1022.4,
    "bytes_all":               3072,
    "bytes_all_gauge":         6701832,
    "bytes_all_rate_per_sec":  1533.6,
    "time_since_update":       2.004
  }
]
```

- Interfaces filtered by the configured `show`/`hide` regexes on
  `interface_name`, applied before rate computation. No filtering by
  default (loopback included, as in Glances).
- An interface that just appeared is absent for one cycle (no previous
  sample to diff against); an interface that disappeared drops out
  immediately.
- Glances fields `alias`, `speed`, `is_up` are **omitted in v1** (`sysinfo`
  does not expose them).

---

## 6. Phase 1 spike findings (sysinfo 0.38, recorded 2026-06-12)

The spike (`examples/spike.rs`, removed at the end of Phase 1 — see git
history) verified on Linux, with per-platform constants checked in the
`sysinfo` sources:

1. **`MINIMUM_CPU_UPDATE_INTERVAL` = 200 ms** on Linux, macOS and Windows
   (100 ms on BSD). Below it, `refresh_cpu_usage()` is **silently skipped**
   and the reading keeps the bogus first-refresh value (measured: 9.4%
   constant for delays 0–100 ms vs ~1% real usage at ≥200 ms). A too-short
   warm-up does not error — it returns wrong data. This is why the warm-up
   delay is a hard constraint, not an optimization.
2. **CPU warm-up constant: `250 ms`** — `MINIMUM_CPU_UPDATE_INTERVAL` plus
   a 50 ms margin against timer jitter, since the failure mode at the exact
   boundary is silent (point 1). Satisfies ARCHITECTURE.md §5.5 (~200 ms).
3. **Network counters** are cumulative `u64` per interface name
   (`total_received()` / `total_transmitted()`), keyed by interface name —
   the primary-key design of ARCHITECTURE.md §8.1 maps directly.
4. **Load average** works natively on Linux (and macOS via `getloadavg`);
   on Windows sysinfo provides a PDH-based emulation that can return zeros
   → degraded values, identical shape (§5.2 above).
5. **Memory** exposes exactly the v1 subset needed (`total`, `available`,
   `used`, `free`); `percent` is computed with the Glances formula.

## 7. Configuration discovery order (frozen)

First match wins:

1. `--config <path>` CLI flag;
2. `GLANCES_RS_CONFIG` environment variable;
3. `./glances-rs.toml` (current directory);
4. `$XDG_CONFIG_HOME/glances-rs/config.toml`
   (`~/.config/glances-rs/config.toml` if `XDG_CONFIG_HOME` is unset);
5. `/etc/glances-rs/config.toml` (Unix only).

No file found ⇒ built-in defaults (loopback bind, no password, refresh 2 s,
idle timeout 5 cycles). A path given by flag or env var that does not exist
is a **startup error**, not a silent fallback.
