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
| `/status`                | GET    | Empty body                              | `200`; never wakes plugins, never requires auth |
| `/healthz`               | GET    | Empty body                              | `200`; never wakes plugins, never requires auth |

Glances v5 routes **not** implemented in v1 (deliberate, ARCHITECTURE.md §6.1):
`/api/5/token` (Basic auth only), `/api/5/{plugin}/info`, `/api/5/alert`,
`/api/5/config`.

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
| `_levels` (alert thresholds) | Per-field threshold metadata | Present but **always `{}`** until alerting lands (v0.3.0; ARCHITECTURE.md §6.1, §8.1). The envelope key is there, the content is empty. |
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

Every plugin response is wrapped in the v5 envelope. The wrapping depends on
the plugin's stat type:

- **Object plugins** (`mem`, `cpu`, `load`, `system`, `uptime`, `memswap`) —
  the stat fields sit at the **top level**, next to `_levels`:
  ```json
  { "seconds": 71988, "_levels": {} }
  ```
- **Collection plugins** (`network`, `fs`, `diskio`) — the per-item array is
  placed under a **`data`** key, with `_levels` alongside it:
  ```json
  { "data": [ { … }, { … } ], "_levels": {} }
  ```

`_levels` is alert-threshold metadata, present on every plugin but **always
`{}`** until alerting lands (v0.3.0).

**`time_since_update` appears only on rate plugins, always at the top level.**
Glances emits it only on the plugins that derive a rate (matching its
`_manage_rate` decorator), and never on the instantaneous ones. It is always a
single value at the **top level of the envelope** — never per-item:

- `cpu` — next to the stat fields (object plugin).
- `network`, `diskio` — next to `data` (one value for the whole array, **not**
  one per item).
- `mem`, `load`, `system`, `uptime`, `fs`, `memswap` — **absent** (`fs` and the
  rest are instantaneous; `memswap`'s `sin`/`sout` are rates but Glances does
  not surface a `time_since_update` for it).

The value is the measured seconds (float) since the plugin's previous cycle
(real `Instant` elapsed, never the nominal refresh — ARCHITECTURE.md §5.4).

**Rate fields are plain per-second rates.** A cumulative-counter field `X`
marked *rate* (network `bytes_*`, diskio `read_*`/`write_*`, memswap
`sin`/`sout`, cpu `ctx_switches`/`interrupts`/`soft_interrupts`) is reported as
a single value — the counter delta divided by the measured interval, to 1
decimal. There is **no** `X_gauge` or `X_rate_per_sec` companion (that was the
v4 shape).

## 5. Payload schemas

> Each example shows the plugin's **stats**; per §4 every response is wrapped
> in the envelope — object plugins keep their fields at the top level,
> collection plugins are nested under `data`, and both gain `_levels`.
> `time_since_update` appears only on the rate plugins (`cpu`, `network`,
> `diskio`), always at the top level. The envelope is shown explicitly where it
> matters (the collection plugins and `uptime`).

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
      "is_up":          true
    }
  ],
  "time_since_update": 2.004,
  "_levels": {}
}
```

- `bytes_recv`/`bytes_sent`/`bytes_all` are **per-second rates** (bytes/s, 1
  decimal). No `_gauge`/`_rate_per_sec` companions.
- `time_since_update` is a single **top-level** value (the measured interval the
  rates were computed over) — not repeated inside each item.
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
  "_levels": {}
}
```

- A single `seconds` field (integer seconds since boot) plus `_levels` —
  the Glances v5 REST shape. No `time_since_update` (uptime is instantaneous).
  (The v4 shape was a bare `str(timedelta)` string; v5 serializes the integer.)
- Same on every platform (`sysinfo::System::uptime`).

### 5.7 `memswap` — object, part-rate

```json
{
  "total":             4294963200,
  "used":              1073737728,
  "free":              3221225472,
  "percent":           25.0,
  "sin":               2048.0,
  "sout":              512.0
}
```

- `total`/`used`/`free` in bytes; `percent = used / total * 100`
  (`used = total - free`), `0.0` when there is no swap.
- `sin`/`sout` are **per-second rates** (bytes/s, 1 decimal): the cumulative
  `/proc/vmstat` page-swap counters diffed over the measured interval (§4).
  `0.0` on the first cycle (the warm-up baseline). Unlike `cpu`, `memswap`
  carries **no** `time_since_update` — Glances does not surface one here.
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
  "_levels": {}
}
```

- No `time_since_update`: `fs` is instantaneous (no rate).
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
  over the top-level `time_since_update`. `*_bytes` derive from sectors × 512. A
  disk absent from the previous sample is skipped for one cycle; a removed disk
  drops out immediately (§8.1).
- `time_since_update` is a single **top-level** value (like `network`), not
  repeated inside each item. **Linux only** — off Linux the `data` array is
  empty and there is no `time_since_update`.
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
