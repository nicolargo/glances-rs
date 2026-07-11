# glances-rs — REST API contract (v1)

> **Purpose.** This document freezes the observable API of `glances-rs` v1
> against the Glances v5 REST contract (`routes_v5.py` on the `develop-v5`
> branch), and records the Phase 1 spike findings that constrain the
> implementation. Once frozen, changes to payload shapes are breaking changes.

---

## 1. Routes

| Route                    | Method | Response                                | Status codes |
|--------------------------|--------|-----------------------------------------|--------------|
| `/api/5/{plugin}`        | GET    | The plugin's payload in the v5 envelope (§4) | `200`, `404` unknown plugin, `503` collection did not start in time |
| `/api/5/all`             | GET    | Object: `{ "<plugin>": <envelope>, … }` | `200` (possibly partial — see §3) |
| `/api/5/pluginslist`     | GET    | Sorted array of plugin names: `["cpu","diskio","fs","load","mem","memswap","network","system","uptime"]` | `200` |
| `/api/5/alert`           | GET    | Array of alert events, most-recent last (§8.3) | `200` always, `[]` when empty; never `503` — read-only, does not wake or wait on a collector |
| `/status`                | GET    | Empty body                              | `200`; never wakes plugins, never requires auth |
| `/healthz`               | GET    | Empty body                              | `200`; never wakes plugins, never requires auth |

Glances v5 routes **not** implemented in v1 (deliberate, ARCHITECTURE.md §6.1):
`/api/5/token` (Basic auth only), `/api/5/{plugin}/info`, `/api/5/config`.

**Security (ARCHITECTURE.md §7).** The `/api/5/*` routes sit behind, in order,
a CORS layer, a trusted-`Host` check and HTTP Basic auth. Added status codes:

- `401` — password configured but credentials missing or wrong; the response
  carries `WWW-Authenticate: Basic realm="glances-rs"`. When no password is
  configured the layer allows the request, which is safe only because the
  §7.1 startup check guarantees a loopback bind in that case.
- `400` — the `Host` header is present but not in `[security].trusted_hosts`
  (default `localhost`, `127.0.0.1`). A missing `Host` is allowed.

The probes (`/status`, `/healthz`) are outside this stack — no auth, no host
check, no wake-up.

## 2. Divergences from Glances v5 (documented contract differences)

| Behaviour | Glances v5 | glances-rs |
|---|---|---|
| Known plugin, no data yet | `200` with `null` body | **Waits** for the first collection cycle; `503` if it does not arrive within the guard timeout. A `200` always carries real data. |
| Auth | Basic + Bearer token (`/token`) | Basic only (v1). |
| `_levels` (alert thresholds) | Per-field threshold metadata | Populated per §8 Alerting. `{}` when no threshold is configured for that plugin (the default, config-only — no built-in thresholds). |
| Platform-specific fields | Present per platform/psutil | **Full field parity on Linux** (the primary target); macOS/Windows degrade to the portable subset `sysinfo` exposes. Clients treat absent fields as "not available", exactly as with Glances' platform-specific fields. |

> **Field parity (Linux).** `glances-rs` reads `/proc/stat`, `/proc/meminfo`,
> `/sys/class/net`, `/proc/vmstat` and `/proc/diskstats` directly so the Linux
> payloads match the Glances v5 field set. JSON object key *order* may differ —
> objects are unordered, so this is not a contract difference.

## 3. `/all` partial-failure policy

`/api/5/all` wakes all plugins concurrently and returns `200` with every
plugin that produced data; a plugin that exceeded the guard timeout is
**absent from the object** rather than failing the whole response
(ARCHITECTURE.md §6.3). Clients needing per-plugin failure semantics should
query `/api/5/{plugin}` and rely on `503`.

## 4. Response envelope and rate convention (Glances v5)

**Every** plugin response is wrapped in the v5 envelope, which adds two
top-level keys to the plugin's stats:

- `time_since_update` — measured seconds (float) since the plugin's previous
  cycle (real `Instant` elapsed, never the nominal refresh — ARCHITECTURE.md
  §5.4); `0.0` on the very first cycle of an instantaneous plugin.
- `_levels` — alert-threshold decoration (§8 Alerting), rebuilt fresh from the
  current sample every cycle. `{}` when the plugin has no configured
  threshold.

The wrapping depends on the plugin's stat type:

- **Object plugins** (`mem`, `cpu`, `load`, `system`, `uptime`, `memswap`) —
  the stat fields sit at the **top level**, next to `time_since_update` and
  `_levels`. `_levels` is keyed by field name:
  ```json
  { "seconds": 71988, "time_since_update": 2.004, "_levels": {} }
  ```
  ```json
  { "percent": 92.1, "time_since_update": 2.004,
    "_levels": { "percent": { "level": "critical", "prominent": true } } }
  ```
- **Collection plugins** (`network`, `fs`, `diskio`) — the per-item array is
  placed under a **`data`** key; `_levels` is keyed by the stringified
  primary-key value, then field name (§8.2):
  ```json
  { "data": [ { … }, { … } ], "time_since_update": 2.004, "_levels": {} }
  ```

**Rate fields are plain per-second rates.** A cumulative-counter field `X`
marked *rate* (network `bytes_*`, diskio `read_*`/`write_*`, memswap
`sin`/`sout`, cpu `ctx_switches`/`interrupts`/`soft_interrupts`) is reported as
a single value — the counter delta divided by `time_since_update`, to 1
decimal. There is **no** `X_gauge` or `X_rate_per_sec` companion (that was the
v4 shape); the per-item objects of a collection plugin carry no
`time_since_update` either — it lives once at the envelope top level.

## 5. Payload schemas

> Each example shows the plugin's **stats**; per §4 every response is wrapped
> in the envelope — object plugins gain top-level `time_since_update` and
> `_levels`, collection plugins are nested under `data`. Only the envelope is
> shown explicitly where it matters (the collection plugins and `uptime`).

### 5.1 `mem` — object, instantaneous

```json
{
  "total":     16856244224,
  "available": 16233152512,
  "percent":   3.7,
  "used":      623091712,
  "free":      15423582208,
  "active":    31532032,
  "inactive":  552156160,
  "buffers":   16388096,
  "cached":    366714880
}
```

- `percent = (total - available) / total * 100` — the Glances formula.
- All sizes in bytes (`u64`).
- **Linux** (`/proc/meminfo`, psutil formulas): `cached = Cached +
  SReclaimable`, `used = total - free - cached - buffers`. The
  `active`/`inactive`/`buffers`/`cached` fields are present.
- **macOS/Windows:** degrade to `total`/`available`/`percent`/`used`/`free`
  (the four extra fields absent), as `sysinfo` does not expose them.
  Glances marks them platform-specific too, so clients already tolerate
  their absence. `wired`/`shared` (macOS/BSD only in Glances) are not
  emitted.

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
  "total":             2.7,
  "user":              1.9,
  "system":            0.7,
  "idle":              97.3,
  "nice":              0.0,
  "iowait":            0.1,
  "irq":               0.0,
  "steal":             0.0,
  "guest":             0.0,
  "ctx_switches":      1637.7,
  "interrupts":        1158.6,
  "soft_interrupts":   742.0,
  "syscalls":          0.0,
  "cpucore":           16,
  "time_since_update": 2.004
}
```

- `total` — busy share, `100 - idle` (iowait counts as busy, matching
  Glances). The percentages are derived by diffing two `/proc/stat`
  samples; `guest`/`guest_nice` are subtracted from `user`/`nice` to avoid
  the kernel's double counting.
- `ctx_switches`/`interrupts`/`soft_interrupts` are **rates** (per second)
  from the cumulative `ctxt`/`intr`/`softirq` counters. `syscalls` is `0.0`
  on Linux, exactly as psutil reports.
- **Linux:** full field set as above. **macOS/Windows:** degrade to
  `total`/`cpucore`/`time_since_update` (`sysinfo`'s `global_cpu_usage`),
  with the §5.5 warm-up against `sysinfo`'s minimum refresh interval.

### 5.4 `network` — collection plugin (items under `data`), rate

One element per interface; primary key `interface_name`:

```json
{
  "data": [
    {
      "interface_name": "eth0",
      "bytes_recv":     511.2,
      "bytes_sent":     1022.4,
      "bytes_all":      1533.6,
      "speed":          0,
      "bytes_speed_rate_per_sec": 62500000,
      "is_up":          true
    }
  ],
  "time_since_update": 2.004,
  "_levels": {}
}
```

- `bytes_recv`/`bytes_sent`/`bytes_all` are **per-second rates** (bytes/s, 1
  decimal). No `_gauge`/`_rate_per_sec` companions.
- Interfaces filtered by the configured `show`/`hide` regexes on
  `interface_name`, applied before rate computation. **Default hide:**
  `docker.*` and `lo` (set an explicit `hide` in config to override).
- An interface that just appeared is absent for one cycle (no previous
  sample to diff against); an interface that disappeared drops out
  immediately.
- `alias` from `[plugins.network].alias` (a `name = "alias"` table) is added
  to an item **only when configured** for it (absent otherwise), as in
  Glances v5.
- **Linux:** `is_up` (from the interface `IFF_UP` flag) and `speed` (link
  speed in bits/s — Mbps × 1048576, `0` when unknown) are added, both from
  `/sys/class/net`. **macOS/Windows:** `is_up`/`speed` are omitted
  (`sysinfo` does not expose them).
- `bytes_speed_rate_per_sec` — the per-direction link capacity in bytes/s,
  used as the `normalize_by` divisor for the `bytes_recv`/`bytes_sent` alert
  thresholds (§8.1): `speed_mbits × 1e6 / 8 / 2` (decimal Mbit/s → bytes/s,
  halved for full-duplex per-direction capacity — note this is a different
  scale than `speed`, which uses the binary 1048576 factor). **Linux only**,
  read from `/sys/class/net/<iface>/speed`; `0` when the link speed is
  unknown, the interface is down, or off-Linux (the field is present and `0`
  on Linux, entirely absent on macOS/Windows, matching `is_up`/`speed`).
  Collected unconditionally, independent of whether any threshold is
  configured.

### 5.5 `system` — object, instantaneous

```json
{
  "os_name":      "Linux",
  "hostname":     "server1",
  "platform":     "64bit",
  "os_version":   "6.18.5",
  "linux_distro": "Ubuntu 22.04",
  "hr_name":      "Ubuntu 22.04 64bit / Linux 6.18.5"
}
```

- `os_name` is the capitalized OS family (`platform.system()` in Glances:
  `Linux`/`Windows`/`Darwin`/…); `platform` is the pointer width
  (`64bit`/`32bit`); `os_version` is the kernel release on Linux.
- **Linux:** `linux_distro` is `NAME VERSION_ID` from `/etc/os-release`, and
  `hr_name` is composed as `"{linux_distro} {platform} / {os_name}
  {os_version}"` — the Glances format.
- **macOS/Windows:** `linux_distro` is omitted; `hr_name` degrades to
  `"{os_name} {os_version} {platform}"`, as Glances does off Linux.

### 5.6 `uptime` — object, instantaneous

```json
{
  "seconds": 71988,
  "time_since_update": 2.004,
  "_levels": {}
}
```

- A single `seconds` field (integer seconds since boot) plus the envelope —
  the Glances v5 REST shape. (The v4 shape was a bare `str(timedelta)` string;
  v5 serializes the integer.)
- Same on every platform (`sysinfo::System::uptime`).

### 5.7 `memswap` — object, part-rate

```json
{
  "total":             4294963200,
  "used":              1073737728,
  "free":              3221225472,
  "percent":           25.0,
  "sin":               2048.0,
  "sout":              512.0,
  "time_since_update": 2.004
}
```

- `total`/`used`/`free` in bytes; `percent = used / total * 100`
  (`used = total - free`), `0.0` when there is no swap.
- `sin`/`sout` are **per-second rates** (bytes/s, 1 decimal): the cumulative
  `/proc/vmstat` page-swap counters diffed over `time_since_update` (§4). `0.0`
  on the first cycle (the warm-up baseline).
- **Linux** (`/proc/meminfo` + `/proc/vmstat`): full field set; `sin`/`sout`
  use the kernel page size (`sysconf(_SC_PAGESIZE)`). **macOS/Windows:**
  degrade to `total`/`used`/`free`/`percent`; `sin`/`sout` are omitted
  (`sysinfo` does not expose the swap counters).

### 5.8 `fs` — collection plugin (items under `data`), instantaneous

One element per mounted filesystem; primary key `mnt_point`:

```json
{
  "data": [
    {
      "device_name": "/dev/vda1",
      "fs_type":     "ext4",
      "mnt_point":   "/",
      "size":        270553174016,
      "used":        240020131840,
      "free":        30533042176,
      "percent":     88.7
    }
  ],
  "time_since_update": 2.004,
  "_levels": {}
}
```

- All sizes in bytes; `free` is the space available to the caller,
  `used = size - free`, `percent = used / size * 100` (1 decimal). This
  slightly overstates usage versus psutil's root-reserve-aware percent (which
  excludes blocks reserved for root); the gap is the reserved fraction. It
  will be revisited when alerting (v0.3.0) needs exact thresholds.
- Filesystems are filtered by the configured `show`/`hide` regexes on
  `mnt_point`. **Default hide:** `/boot.*` and `.*/snap.*`. `alias` from
  `[plugins.fs].alias` keyed by mount point is added **only when configured**.
- **Omitted vs. Glances:** `key` (the primary-key name, dropped for every
  collection plugin here — see `network`) and `options` (mount flags;
  `sysinfo` does not expose them). Same payload on every platform.

### 5.9 `diskio` — collection plugin (items under `data`), rate

One element per disk; primary key `disk_name`:

```json
{
  "data": [
    {
      "disk_name":   "sda",
      "read_count":  6.0,
      "write_count": 20.0,
      "read_bytes":  24576.0,
      "write_bytes": 81920.0
    }
  ],
  "time_since_update": 2.004,
  "_levels": {}
}
```

- `read_count`/`write_count`/`read_bytes`/`write_bytes` are **per-second
  rates** (1 decimal), diffed from the cumulative `/proc/diskstats` counters
  over `time_since_update` (§4). `*_bytes` derive from sectors × 512. A disk
  absent from the previous sample is skipped for one cycle; a removed disk
  drops out immediately (§8.1).
- Disks are filtered by the configured `show`/`hide` regexes on `disk_name`.
  **Default hide:** `loop.*` and `/dev/loop.*`. `alias` from
  `[plugins.diskio].alias` is added **only when configured**.
- **Linux only** (`/proc/diskstats`). `sysinfo` exposes no per-disk I/O, so
  **macOS/Windows return an empty `data` array**. `read_time`/`write_time` and
  the derived latency fields (present in Glances) are omitted.

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

## 8. Alerting (v0.3.0)

Closes the last payload-parity gap with Glances v5: per-field `_levels`
decoration and the `/api/5/alert` event journal. **Conservatism on
defaults:** there are **no built-in thresholds**. With the default config,
every plugin's `_levels` is `{}` and `/api/5/alert` returns `[]` — alerting
is entirely opt-in via configured thresholds.

### 8.1 Configuration

Thresholds are declared per plugin, at two levels — a global default for
every item and an optional per-item override, merged **per limit key**
(item-specific wins only for the limits it declares; the rest fall back to
global):

```toml
# Scalar plugin — keyed by field name
[plugins.mem.thresholds.percent]
careful  = 70.0
warning  = 80.0
critical = 90.0

# Collection plugin, global — applies to every item, keyed by field name
[plugins.fs.thresholds.percent]
careful  = 70.0
warning  = 80.0

# Collection plugin, specific — keyed by item primary key, then field name.
# Overrides only the limit keys it declares; the rest are inherited from
# the global block above.
[plugins.fs.thresholds_by_item."/".percent]
critical = 95.0

# Optional per-plugin hysteresis window, uniform across all of that
# plugin's items — overrides the global [alerts] default below.
[plugins.fs]
min_duration_seconds = 10.0

[alerts]
history_size          = 200   # default; max retained events (ring buffer)
min_duration_seconds  = 5.0   # default; global hysteresis window
```

Any subset of `careful`/`warning`/`critical` is valid. Declared and merged
threshold sets are validated at startup: all present limits must be finite
and satisfy `careful <= warning <= critical`; `alerts.history_size >= 1`;
`min_duration_seconds >= 0` (global and any per-plugin override). An invalid
config is a startup error, not a silent clamp.

Only a static, per-plugin allow-list of fields is alertable (mirroring
Glances' `watched` flag) — an unlisted field never produces `_levels` even
if a threshold happens to be configured for it. Each alertable field also
has a static `prominent` flag (copied from Glances, not configurable) used
by clients for highlight rendering, and a `watch_direction`: `high` (breach
when the value rises above a limit) or `low` (breach when it falls below).
Every v0.3.0 alertable field is `high` — the `low` direction is implemented
and unit-tested but not yet used by any shipped field.

Some fields declare `normalize_by`: the level is computed against
`value / divisor` instead of the raw value, with thresholds expressed as a
ratio in `[0, 1]` instead of a direct percentage/count. `network`'s
`bytes_recv`/`bytes_sent` use this against the new `bytes_speed_rate_per_sec`
field (§5.4): if the divisor is absent, `0`, or non-finite (unknown link
speed), that field's `_levels` entry — and any event — is skipped for that
cycle, matching Glances' "unknown link speed" semantics.

### 8.2 `_levels` shape

`_levels` is always a **top-level** envelope key (never inside `data`
items), rebuilt fresh from the current sample every cycle — a field/item
absent from the current sample cannot leave a stale `_levels` entry behind.
Each leaf is `{ "level": "ok"|"careful"|"warning"|"critical", "prominent":
<bool> }`. An entry is emitted only for a field that is both alertable
(§8.1) and has a resolved threshold — unconfigured or non-alertable fields
are simply absent, including the `ok` case (so a client can observe a
return to `ok`, it is not just omitted for "healthy").

- **Object plugins** — keyed by field name:
  ```json
  "_levels": { "percent": { "level": "careful", "prominent": true } }
  ```
- **Collection plugins** — keyed by the **stringified primary-key value**,
  then field name:
  ```json
  "_levels": {
    "/":     { "percent": { "level": "critical", "prominent": false } },
    "/home": { "percent": { "level": "ok",       "prominent": false } }
  }
  ```
  Only items present in the current sample appear.

`_levels` carries the **raw, instantaneous** level, recomputed every cycle —
it is not debounced. `min_duration` hysteresis (§8.1) gates only whether a
transition is *journaled* as an `/api/5/alert` event, so a brief spike is
visible in `_levels` immediately even if it never produces an event.

### 8.3 `/api/5/alert`

`GET /api/5/alert` returns the accumulated event journal as a JSON array,
most-recent last, `[]` when empty. It sits behind the same auth/CORS/
trusted-host stack as the other `/api/5/*` routes (§1), but unlike them it
**never wakes or waits on a collector** and **never returns `503`** — it is
a cheap read of in-memory state, like `pluginslist`.

Each event:

```json
{
  "ts":              "2026-06-14T12:34:56Z",
  "plugin":          "fs",
  "key":             "/",
  "field":           "percent",
  "level":           "critical",
  "previous_level":  "warning",
  "value":           95.0,
  "prominent":       false,
  "is_initial":      false,
  "hostname":        "server1"
}
```

- `ts` — ISO 8601 UTC, second precision.
- `key` — `null` for a scalar plugin, the item's primary-key value
  (stringified) for a collection plugin.
- `value` — the raw (undivided) field value, even for a `normalize_by`
  field.
- `is_initial` — `true` only for the very first committed level a
  `(plugin, key, field)` triple ever reaches (i.e. no `ok` was committed
  before it); `false` for every later transition, including a return to
  `ok`.
- An event is journaled only once an observed level has persisted for the
  effective `min_duration` (§8.1); a transient breach that never persists
  long enough produces no event, even though it was visible in `_levels`.
- The journal is a ring buffer bounded by `[alerts].history_size` — the
  oldest event is dropped once the bound is exceeded.

**Lazy-model divergence.** Because collection only happens while a plugin
is `Active` (ARCHITECTURE.md §3), the event journal only accrues while a
client is actively polling that plugin. A breach that starts and clears
entirely during an idle gap (collector stopped, no request) produces no
event — there is no background sampling to observe it. On re-wake, a
still-stale pending state from before the gap is reset (it cannot
insta-commit on the first post-wake sample), but the last **committed**
level is preserved, so a wake does not spuriously re-fire an `is_initial`
event. This is a direct, deliberate consequence of the lazy-collection
contract (ARCHITECTURE.md §3.2, §5.2): with sporadic polling, a breach
shorter than `min_duration` of *sustained active* observation may simply
never commit — no client was watching closely enough for an alert to be
meaningful.
