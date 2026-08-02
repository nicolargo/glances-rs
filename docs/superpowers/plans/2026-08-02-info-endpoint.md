# `/api/5/<plugin>/info` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve `GET /api/5/<plugin>/info` — the per-plugin field schema
(`fields_description`) of Glances v5 — from a single unified static metadata
table that also becomes the alerting engine's source of truth.

**Architecture:** One new module `src/plugins/fields.rs` holds a `FieldInfo`
table describing **every field each plugin emits**. `alerts.rs` stops owning
`AlertField` and derives its watched set from `fields(id).filter(watched)`. A
new inert axum route serialises the table (plus configured thresholds) as JSON.

**Tech Stack:** Rust 2024, axum 0.8, serde_json, tokio `current_thread`. No new
dependency.

## Global Constraints

- **Source of truth = the maintainer's live `develop-v5` `/info` dumps** (all
  nine plugins), captured in the design spec
  `docs/superpowers/specs/2026-08-02-info-endpoint-design.md`. The `develop-v5`
  *repo* is stale and must NOT be used for metadata. Descriptions/units below
  are copied verbatim from those dumps.
- **Footprint is the project's reason to exist.** `fields.rs` is `&'static`
  (zero runtime allocation); `/info` allocates only per call. Default config
  (no thresholds) must stay indistinguishable from v0.3.0.
- **`/info` is inert:** never wakes a collector, never reads the store or
  registry, never waits, never returns `503`. Unknown/disabled plugin → `404`
  (glances-rs convention; Glances uses `400` — pre-existing divergence).
- **Whitelist keys only:** `description`, `unit` (always); `short_name`,
  `primary_key`, `rate`, `internal`, `watched`, `watch_direction`,
  `prominent`, `default_thresholds`, `normalize_by` (conditional). **Never**
  emit `history` or `strict_thresholds` (features glances-rs does not
  implement), nor `min_symbol`/`mmm`/`optional`.
- **`default_thresholds` reflects *configured* global thresholds** only
  (`[plugins.<p>].thresholds.<field>`), `Some` limits only, key absent when
  unconfigured. Never ships built-in defaults (v0.3.0 config-only stance).
- **Field-set invariant:** every emitted data key (bar envelope
  `time_since_update`/`_levels`) is described in `/info` (all platforms); every
  `/info` key bar `alias` is emitted (Linux only).
- Run `cargo fmt --all` before every commit; `make lint` and `make test` gate
  each task. `Cargo.lock` stays in sync.
- Commit-message trailer on every commit:
  `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r`.

---

## Task 1: `plugins::fields` — unified table + behaviour-neutral `alerts.rs` refactor

Introduce the full metadata table carrying the **current v0.3.0** alert
attributes verbatim, and make `alerts.rs` derive its watched set from it.
Zero behavioural change — the existing `alerts.rs` suite is the gate.

**Files:**
- Create: `src/plugins/fields.rs`
- Modify: `src/plugins/mod.rs` (add `pub mod fields;`)
- Modify: `src/alerts.rs` (remove `AlertField`/`af`/`*_FIELDS`/`Direction`/`alert_fields` body; import from `plugins::fields`; retype `level_for`/`build_event`; adjust `observe` loops)

**Interfaces:**
- Produces: `crate::plugins::fields::{Unit, Direction, FieldInfo, fields, alert_fields}`.
  - `pub fn fields(id: PluginId) -> &'static [FieldInfo]` — every emitted field.
  - `pub(crate) fn alert_fields(id: PluginId) -> impl Iterator<Item = &'static FieldInfo>` — the `watched` subset, zero-allocation (no per-cycle heap use on the default path).
  - `Unit::as_str(self) -> &'static str`, `Direction::as_str(self) -> &'static str`.
  - `FieldInfo` public fields: `field, description, unit, short_name, primary_key, rate, internal, watched, direction, prominent, normalize_by`.
- Consumes: `crate::plugins::PluginId` and `PluginId::key_field()`.

- [ ] **Step 1: Write the failing guard test** (in `src/plugins/fields.rs` `#[cfg(test)]`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::PluginId;

    // The watched subset must equal the v0.3.0 AlertField table field-for-field
    // (field, prominent, direction, normalize_by). Proves this refactor is
    // behaviour-neutral. Phase 1b updates the five rows it later corrects.
    #[test]
    fn watched_subset_matches_v030_alertfield() {
        // (plugin, field, prominent, normalize_by) — the v0.3.0 alertable set.
        let expected: &[(PluginId, &str, bool, Option<&str>)] = &[
            (PluginId::Mem, "percent", true, None),
            (PluginId::Cpu, "total", true, None),
            (PluginId::Cpu, "system", false, None),
            (PluginId::Cpu, "user", false, None),
            (PluginId::Cpu, "iowait", false, None),
            (PluginId::Cpu, "steal", true, None),
            (PluginId::Cpu, "ctx_switches", true, None),
            (PluginId::Load, "min5", false, None),
            (PluginId::Load, "min15", true, None),
            (PluginId::MemSwap, "percent", true, None),
            (PluginId::MemSwap, "sin", false, None),
            (PluginId::MemSwap, "sout", false, None),
            (PluginId::Diskio, "read_bytes", false, None),
            (PluginId::Diskio, "write_bytes", false, None),
            (PluginId::Fs, "percent", false, None),
            (PluginId::Network, "bytes_recv", false, Some("bytes_speed_rate_per_sec")),
            (PluginId::Network, "bytes_sent", false, Some("bytes_speed_rate_per_sec")),
        ];
        let mut got: Vec<(PluginId, &str, bool, Option<&str>)> = Vec::new();
        for id in PluginId::ALL {
            for f in alert_fields(id) {
                assert!(f.watched);
                assert_eq!(f.direction, Direction::High); // every v0.3.0 field is High
                got.push((id, f.field, f.prominent, f.normalize_by));
            }
        }
        got.sort_by_key(|t| (t.0.as_str(), t.1));
        let mut want = expected.to_vec();
        want.sort_by_key(|t| (t.0.as_str(), t.1));
        assert_eq!(got, want);
    }

    #[test]
    fn primary_key_matches_key_field() {
        for id in PluginId::ALL {
            let pk: Vec<&str> = fields(id).iter().filter(|f| f.primary_key).map(|f| f.field).collect();
            match id.key_field() {
                Some(k) => assert_eq!(pk, vec![k], "{} pk", id.as_str()),
                None => assert!(pk.is_empty(), "{} has no pk", id.as_str()),
            }
        }
    }

    #[test]
    fn every_plugin_has_fields() {
        for id in PluginId::ALL {
            assert!(!fields(id).is_empty(), "{} has fields", id.as_str());
        }
    }

    #[test]
    fn unit_strings() {
        assert_eq!(Unit::Bytes.as_str(), "bytes");
        assert_eq!(Unit::BytesPerSec.as_str(), "bytespers");
        assert_eq!(Unit::BitPerSec.as_str(), "bitpersecond");
        assert_eq!(Unit::Float.as_str(), "float");
        assert_eq!(Unit::Seconds.as_str(), "seconds");
    }

    #[test]
    fn direction_strings() {
        assert_eq!(Direction::High.as_str(), "high");
        assert_eq!(Direction::Low.as_str(), "low");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib plugins::fields 2>&1 | head`
Expected: FAIL — module `fields` does not exist.

- [ ] **Step 3: Write `src/plugins/fields.rs`** (types, builder, all nine tables — current v0.3.0 alert attributes)

```rust
//! Static per-plugin field metadata: the single source of truth for both the
//! `/api/5/<plugin>/info` schema route and the alerting engine's alertable
//! set. Every value is copied verbatim from the maintainer's live Glances v5
//! `develop-v5` `/info` output (the repo `fields_description` is stale). All
//! data is `&'static` — zero runtime allocation (footprint mandate).

use super::PluginId;

/// Threshold ladder direction. `High`: alert as the value rises; `Low`: alert
/// as it falls. Every current field is `High`; `Low` is engine-complete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    High,
    Low,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::High => "high",
            Direction::Low => "low",
        }
    }
}

/// Field unit vocabulary, verbatim from Glances v5 `/info`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    Bytes,
    Percent,
    BytesPerSec,
    BitPerSec,
    Number,
    Float,
    Bool,
    StringT,
    Seconds,
}

impl Unit {
    pub fn as_str(self) -> &'static str {
        match self {
            Unit::Bytes => "bytes",
            Unit::Percent => "percent",
            Unit::BytesPerSec => "bytespers",
            Unit::BitPerSec => "bitpersecond",
            Unit::Number => "number",
            Unit::Float => "float",
            Unit::Bool => "bool",
            Unit::StringT => "string",
            Unit::Seconds => "seconds",
        }
    }
}

/// One emitted field's static metadata. `watched`/`direction`/`prominent`/
/// `normalize_by` drive the alerting engine; the rest is pure schema for
/// `/info`.
pub struct FieldInfo {
    pub field: &'static str,
    pub description: &'static str,
    pub unit: Unit,
    pub short_name: Option<&'static str>,
    pub primary_key: bool,
    pub rate: bool,
    pub internal: bool,
    pub watched: bool,
    pub direction: Direction,
    pub prominent: bool,
    pub normalize_by: Option<&'static str>,
}

impl FieldInfo {
    const fn new(field: &'static str, description: &'static str, unit: Unit) -> Self {
        Self {
            field,
            description,
            unit,
            short_name: None,
            primary_key: false,
            rate: false,
            internal: false,
            watched: false,
            direction: Direction::High,
            prominent: false,
            normalize_by: None,
        }
    }
    const fn short(mut self, s: &'static str) -> Self {
        self.short_name = Some(s);
        self
    }
    const fn pk(mut self) -> Self {
        self.primary_key = true;
        self
    }
    const fn rate(mut self) -> Self {
        self.rate = true;
        self
    }
    const fn internal(mut self) -> Self {
        self.internal = true;
        self
    }
    /// Alertable, `High` direction, with the given prominence.
    const fn watched_high(mut self, prominent: bool) -> Self {
        self.watched = true;
        self.direction = Direction::High;
        self.prominent = prominent;
        self
    }
    const fn normalize(mut self, by: &'static str) -> Self {
        self.normalize_by = Some(by);
        self
    }
}

const MEM_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("total", "Total physical memory available.", Unit::Bytes),
    FieldInfo::new("available", "Actual amount of available memory that can be given instantly to processes that request more memory; calculated by summing different memory values depending on the platform (e.g. free + buffers + cached on Linux). Suitable for monitoring actual memory usage in a cross-platform fashion.", Unit::Bytes).short("avail"),
    FieldInfo::new("percent", "Percentage usage calculated as (total - available) / total * 100.", Unit::Percent).watched_high(true),
    FieldInfo::new("used", "Memory used, calculated differently depending on the platform and designed for informational purposes only.", Unit::Bytes),
    FieldInfo::new("free", "Memory not being used at all (zeroed) that is readily available; note that this does not reflect the actual memory available — use `available` instead.", Unit::Bytes),
    FieldInfo::new("active", "(UNIX) Memory currently in use or very recently used, resident in RAM.", Unit::Bytes),
    FieldInfo::new("inactive", "(UNIX) Memory that is marked as not used.", Unit::Bytes).short("inacti"),
    FieldInfo::new("buffers", "(Linux, BSD) Cache for items like filesystem metadata.", Unit::Bytes).short("buffer"),
    FieldInfo::new("cached", "(Linux, BSD) Cache for various things (including ZFS cache).", Unit::Bytes),
];

const CPU_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("total", "Sum of all CPU percentages (except idle).", Unit::Percent).watched_high(true),
    FieldInfo::new("system", "Percent time spent in kernel space. System CPU time is the time spent running code in the operating system kernel.", Unit::Percent).watched_high(false),
    FieldInfo::new("user", "Percent time spent in user space. User CPU time is the time spent on the processor running the program's code (or code in libraries).", Unit::Percent).watched_high(false),
    FieldInfo::new("iowait", "(Linux) Percent time spent by the CPU waiting for I/O operations to complete.", Unit::Percent).watched_high(false),
    FieldInfo::new("idle", "Percent of CPU not used by any program. Every program or task that runs occupies a certain amount of processing time on the CPU; when the CPU has completed all tasks it is idle.", Unit::Percent),
    FieldInfo::new("irq", "(Linux and BSD) Percent time spent servicing hardware and software interrupts.", Unit::Percent),
    FieldInfo::new("nice", "(UNIX) Percent time occupied by user-level processes with a positive nice value (processes that have been niced down).", Unit::Percent),
    FieldInfo::new("steal", "(Linux) Percentage of time a virtual CPU waits for a real CPU while the hypervisor is servicing another virtual processor.", Unit::Percent).watched_high(true),
    FieldInfo::new("guest", "(Linux) Time spent running a virtual CPU for guest operating systems under the control of the Linux kernel.", Unit::Percent),
    FieldInfo::new("ctx_switches", "Number of context switches (voluntary + involuntary) per second. A context switch is the procedure a CPU follows to change from one task to another while ensuring the tasks do not conflict.", Unit::Number).rate().watched_high(true).short("ctx_sw"),
    FieldInfo::new("interrupts", "Number of interrupts per second.", Unit::Number).rate().short("inter"),
    FieldInfo::new("soft_interrupts", "Number of software interrupts per second. Always 0 on Windows and SunOS.", Unit::Number).rate().short("sw_int"),
    FieldInfo::new("syscalls", "Number of system calls per second. Always 0 on Linux.", Unit::Number).rate(),
    FieldInfo::new("cpucore", "Total number of logical CPU cores.", Unit::Number).internal(),
];

const LOAD_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("min1", "Average number of processes waiting in the run-queue plus those currently executing, over 1 minute.", Unit::Float),
    FieldInfo::new("min5", "Average number of processes waiting in the run-queue plus those currently executing, over 5 minutes.", Unit::Float).watched_high(false),
    FieldInfo::new("min15", "Average number of processes waiting in the run-queue plus those currently executing, over 15 minutes.", Unit::Float).watched_high(true),
    FieldInfo::new("cpucore", "Total number of logical CPU cores.", Unit::Number).internal(),
];

const SYSTEM_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("os_name", "Operating system name.", Unit::StringT),
    FieldInfo::new("hostname", "Hostname.", Unit::StringT),
    FieldInfo::new("platform", "Platform (32 or 64 bits).", Unit::StringT),
    FieldInfo::new("linux_distro", "Linux distribution.", Unit::StringT),
    FieldInfo::new("os_version", "Operating system version.", Unit::StringT),
    FieldInfo::new("hr_name", "Human readable operating system name.", Unit::StringT),
];

const UPTIME_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("seconds", "Seconds elapsed since the system booted.", Unit::Seconds),
];

const MEMSWAP_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("total", "Total swap memory.", Unit::Bytes),
    FieldInfo::new("used", "Used swap memory.", Unit::Bytes),
    FieldInfo::new("free", "Free swap memory.", Unit::Bytes),
    FieldInfo::new("percent", "Used swap memory as a percentage of total.", Unit::Percent).watched_high(true),
    FieldInfo::new("sin", "Bytes the system has swapped in from disk (per second — v4 reports the cumulative counter; v5 converts it to a rate).", Unit::BytesPerSec).rate().watched_high(false),
    FieldInfo::new("sout", "Bytes the system has swapped out to disk (per second — v4 reports the cumulative counter; v5 converts it to a rate).", Unit::BytesPerSec).rate().watched_high(false),
];

const FS_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("mnt_point", "Mount point.", Unit::StringT).pk(),
    FieldInfo::new("device_name", "Device name backing the filesystem (e.g. /dev/sda1).", Unit::StringT),
    FieldInfo::new("fs_type", "File system type (e.g. ext4, xfs, btrfs).", Unit::StringT).internal(),
    FieldInfo::new("size", "Total size of the filesystem in bytes.", Unit::Bytes),
    FieldInfo::new("used", "Used size in bytes.", Unit::Bytes),
    FieldInfo::new("free", "Free size in bytes.", Unit::Bytes),
    FieldInfo::new("percent", "Filesystem usage as a percentage of total size.", Unit::Percent).watched_high(false),
    FieldInfo::new("alias", "Operator-defined display alias for the mount point; present only when configured.", Unit::StringT),
];

const DISKIO_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("disk_name", "Disk name (e.g. sda, nvme0n1).", Unit::StringT).pk(),
    FieldInfo::new("read_count", "Read operations per second (rate of psutil read_count counter).", Unit::Number).rate().internal(),
    FieldInfo::new("write_count", "Write operations per second (rate of psutil write_count counter).", Unit::Number).rate().internal(),
    FieldInfo::new("read_bytes", "Bytes read per second (rate of psutil read_bytes counter).", Unit::BytesPerSec).rate().watched_high(false),
    FieldInfo::new("write_bytes", "Bytes written per second (rate of psutil write_bytes counter).", Unit::BytesPerSec).rate().watched_high(false),
    FieldInfo::new("alias", "Operator-defined display alias for the disk; present only when configured.", Unit::StringT),
];

const NETWORK_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("interface_name", "Network interface name.", Unit::StringT).pk(),
    FieldInfo::new("bytes_recv", "Bytes received per second.", Unit::BytesPerSec).rate().watched_high(false).normalize("bytes_speed_rate_per_sec"),
    FieldInfo::new("bytes_sent", "Bytes sent per second.", Unit::BytesPerSec).rate().watched_high(false).normalize("bytes_speed_rate_per_sec"),
    FieldInfo::new("bytes_all", "Total bytes received and sent per second (bytes_recv + bytes_sent).", Unit::BytesPerSec).rate(),
    FieldInfo::new("alias", "Operator-defined display alias for the interface; present only when configured.", Unit::StringT),
    FieldInfo::new("is_up", "Whether the interface is up.", Unit::Bool),
    FieldInfo::new("speed", "Maximum interface link speed in bits per second (0 when the OS does not report it).", Unit::BitPerSec),
    FieldInfo::new("bytes_speed_rate_per_sec", "Estimated per-direction bandwidth capacity in bytes/s. Computed from the interface link speed (Mbit/s) under a full-duplex split assumption: speed_mbits * 1e6 / 8 / 2. Returns 0 when the OS does not report a link speed (loopback, virtual interfaces) — in which case threshold normalisation is skipped for bytes_recv / bytes_sent.", Unit::BytesPerSec),
];

/// Every field a plugin emits, in stable schema order.
pub fn fields(id: PluginId) -> &'static [FieldInfo] {
    match id {
        PluginId::Mem => MEM_FIELDS,
        PluginId::Cpu => CPU_FIELDS,
        PluginId::Load => LOAD_FIELDS,
        PluginId::Network => NETWORK_FIELDS,
        PluginId::System => SYSTEM_FIELDS,
        PluginId::Uptime => UPTIME_FIELDS,
        PluginId::MemSwap => MEMSWAP_FIELDS,
        PluginId::Fs => FS_FIELDS,
        PluginId::Diskio => DISKIO_FIELDS,
    }
}

/// The alertable subset: fields with `watched: true`, as a **zero-allocation**
/// iterator over the static table (called every cycle before the
/// has-thresholds early-out, so it must not allocate — footprint mandate).
/// Only these emit `_levels`, and only when a threshold is configured.
pub(crate) fn alert_fields(id: PluginId) -> impl Iterator<Item = &'static FieldInfo> {
    fields(id).iter().filter(|f| f.watched)
}
```

- [ ] **Step 4: Add `pub mod fields;` to `src/plugins/mod.rs`**

Add near the other `pub mod` declarations (e.g. after `pub mod filter;`):

```rust
pub mod fields;
```

- [ ] **Step 5: Rewire `src/alerts.rs` to the unified table**

5a. Delete the `Direction` enum definition **and** its `impl` from `alerts.rs`
(now in `fields.rs`). Delete the `AlertField` struct, the `af` const fn, all
`*_FIELDS` consts (`MEM_FIELDS`…`EMPTY_FIELDS`), and the old
`alert_fields` function body — everything from the `/// One alertable field's
static metadata` doc comment through the end of `alert_fields`.

5b. Add the import near the top of `alerts.rs` (with the other `use crate::…`):

```rust
use crate::plugins::fields::{Direction, FieldInfo, alert_fields};
```

(Keep any existing `use` of `PluginId`/`Config`. `Direction` is still used by
`compute_level` and the direction unit tests; `FieldInfo` replaces `AlertField`
in signatures.)

5c. In `observe` (around line 310) replace the `let fields = alert_fields(id); if fields.is_empty()` early-out with a non-consuming, non-allocating check:

```rust
// No alertable fields for this plugin (e.g. system, uptime): nothing to do.
if alert_fields(id).next().is_none() {
    return;
}
```

5d. Change the observations vector element type (around line 357):

```rust
let mut observations: Vec<(Option<String>, &'static FieldInfo, Level, f64)> = Vec::new();
```

5e. In both `observe` branches, change `for af in fields {` to iterate the
watched subset directly (each `af: &'static FieldInfo`):

```rust
for af in alert_fields(id) {
```

(Two occurrences: the scalar branch and the collection branch. Calling
`alert_fields(id)` per branch is free — only one branch runs, and it is a
lazy iterator over a static slice.)

5f. Retype the two helpers:

```rust
fn level_for(
    config: &Config,
    id: PluginId,
    item: Option<&str>,
    af: &FieldInfo,
    stats: &Value,
) -> Option<(Level, f64)> {
```

```rust
fn build_event(
    hostname: &str,
    id: PluginId,
    key: Option<&str>,
    af: &FieldInfo,
    tr: &Transition,
    value: f64,
    ts: SystemTime,
) -> Value {
```

Their bodies are unchanged — `af.field`, `af.prominent`, `af.direction`,
`af.normalize_by` are all present on `FieldInfo`.

5g. Update the in-module test `alert_fields_match_emitted_payload_fields`
(around line 707): `alert_fields` now returns an iterator, so collect first —
`let mem: Vec<&FieldInfo> = alert_fields(PluginId::Mem).collect();` then
`mem.len()`/`mem[0].field`/`mem[0].prominent` compile; likewise
`let net: Vec<&FieldInfo> = alert_fields(PluginId::Network).collect();` before
`net.iter()`, and `alert_fields(PluginId::System).next().is_none()` instead of
`.is_empty()`. Do not add new assertions here (the `fields.rs` guard test owns
the parity check).

- [ ] **Step 6: Run to verify the guard test passes and behaviour is unchanged**

Run: `cargo fmt --all && cargo test --lib 2>&1 | tail -20`
Expected: PASS — the new `fields` tests pass AND the entire pre-existing
`alerts` unit suite passes unchanged (behaviour-neutral).

- [ ] **Step 7: Full gate**

Run: `make lint && make test`
Expected: fmt clean, clippy clean (`-D warnings`), all tests green.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add src/plugins/fields.rs src/plugins/mod.rs src/alerts.rs
git commit -m "refactor(alerts): unify field metadata into plugins::fields

Introduce FieldInfo describing every emitted field for all nine plugins;
alerts.rs derives its watched set from fields(id).filter(watched) and no
longer owns AlertField. Behaviour-neutral: the alerts suite is the gate;
current v0.3.0 alert attributes are carried verbatim.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r"
```

---

## Task 2: Phase 1b — alerting parity fix (five attributes)

Correct the five alert attributes that diverge from Glances `/info`. This is
the **only** behaviour-changing step: `_levels`/event decoration changes, and
`normalize_by="cpucore"` changes level computation for `load`/`ctx_switches`
when a threshold is configured. No field is added to or removed from any
watched set.

**Files:**
- Modify: `src/plugins/fields.rs` (five field literals + the guard test's expected table)
- Modify: `src/alerts.rs` (add a `normalize_by="cpucore"` unit test)

**Interfaces:**
- Consumes: everything from Task 1.
- Produces: no signature change; only data and test changes.

- [ ] **Step 1: Update the guard test's expected table first (make it fail)**

In `src/plugins/fields.rs` `watched_subset_matches_v030_alertfield`, edit the
five diverging rows to the corrected values, and rename the test to
`watched_subset_matches_glances_info`:

```rust
    (PluginId::Cpu, "iowait", true, None),                 // prominent false -> true
    (PluginId::Cpu, "steal", false, None),                 // prominent true  -> false
    (PluginId::Cpu, "ctx_switches", false, Some("cpucore")), // prominent true->false, +normalize
    (PluginId::Load, "min5", false, Some("cpucore")),      // +normalize
    (PluginId::Load, "min15", true, Some("cpucore")),      // +normalize
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib plugins::fields::tests::watched_subset_matches_glances_info 2>&1 | tail`
Expected: FAIL — table still holds the old attributes.

- [ ] **Step 3: Apply the five corrections in the `CPU_FIELDS`/`LOAD_FIELDS` literals**

`CPU_FIELDS`:

```rust
    FieldInfo::new("iowait", "(Linux) Percent time spent by the CPU waiting for I/O operations to complete.", Unit::Percent).watched_high(true),
```
```rust
    FieldInfo::new("steal", "(Linux) Percentage of time a virtual CPU waits for a real CPU while the hypervisor is servicing another virtual processor.", Unit::Percent).watched_high(false),
```
```rust
    FieldInfo::new("ctx_switches", "Number of context switches (voluntary + involuntary) per second. A context switch is the procedure a CPU follows to change from one task to another while ensuring the tasks do not conflict.", Unit::Number).rate().watched_high(false).normalize("cpucore").short("ctx_sw"),
```

`LOAD_FIELDS`:

```rust
    FieldInfo::new("min5", "Average number of processes waiting in the run-queue plus those currently executing, over 5 minutes.", Unit::Float).watched_high(false).normalize("cpucore"),
    FieldInfo::new("min15", "Average number of processes waiting in the run-queue plus those currently executing, over 15 minutes.", Unit::Float).watched_high(true).normalize("cpucore"),
```

- [ ] **Step 4: Add a cpucore-normalisation unit test in `src/alerts.rs`**

Alongside the existing `normalize_by` tests, add one proving level computation
divides by `cpucore` (mirror the network `bytes_speed_rate_per_sec` test):

```rust
#[test]
fn cpucore_normalizes_load_level() {
    // load.min15 threshold on the normalized (per-core) value.
    let config = Config::from_toml(
        "[plugins.load.thresholds.min15]\ncareful = 1.0\nwarning = 2.0\ncritical = 4.0\n",
    )
    .unwrap();
    // 8.0 / 4 cores = 2.0 -> warning (not critical, which raw 8.0 would be).
    let stats = serde_json::json!({ "min15": 8.0, "cpucore": 4 });
    let af = crate::plugins::fields::alert_fields(PluginId::Load)
        .find(|f| f.field == "min15")
        .unwrap();
    let (level, raw) = level_for(&config, PluginId::Load, None, af, &stats).unwrap();
    assert_eq!(level, Level::Warning);
    assert_eq!(raw, 8.0); // event carries the undivided value
}
```

(Adjust the module path to `level_for`/`Level` to match its visibility in the
test module — they are in the same file.)

- [ ] **Step 5: Run the alerting + fields suites**

Run: `cargo fmt --all && cargo test --lib 2>&1 | tail -20`
Expected: PASS — the renamed guard test, the new cpucore test, and the whole
alerts suite green. If any pre-existing alerts test asserted the OLD attribute
of one of the five fields (e.g. an iowait/steal prominent value, or a load
level computed without normalisation), update that assertion to the corrected
value — this is the documented behaviour change.

- [ ] **Step 6: Full gate**

Run: `make lint && make test`

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add src/plugins/fields.rs src/alerts.rs
git commit -m "fix(alerts): align field alert attributes with Glances v5 /info

normalize_by=cpucore on load.min5/min15 and cpu.ctx_switches; correct
prominent on cpu.iowait (true) and cpu.steal (false). Load average is now
alerted per core, matching Glances. Behaviour change is confined to
operators who configure cpu/load thresholds.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r"
```

---

## Task 3: Phase 2 — the `/api/5/<plugin>/info` route

Add the inert schema route consuming `fields(id)`, with configured-threshold
reflection.

**Files:**
- Modify: `src/api/mod.rs` (route + `plugin_info` handler + `configured_thresholds` helper)
- Create: `tests/info.rs`

**Interfaces:**
- Consumes: `crate::plugins::fields::fields`, `FieldInfo`, `PluginId::parse`, `AppState::is_registered`, `config.plugins[…].thresholds: HashMap<String, Thresholds>` where `Thresholds { careful, warning, critical: Option<f64> }`.
- Produces: `GET /api/5/{plugin}/info` → `200` JSON object keyed by field name; `404` on unknown/disabled plugin.

- [ ] **Step 1: Write failing integration tests** (`tests/info.rs`)

```rust
//! `/api/5/<plugin>/info` — static field schema (design spec 2026-08-02).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use glances_rs::config::Config;
use glances_rs::server::build_router;
use glances_rs::state::AppState;
use serde_json::Value;
use tower::ServiceExt;

async fn info(config: Config, plugin: &str) -> (StatusCode, Value) {
    let router = build_router(AppState::new(config));
    let resp = router
        .oneshot(
            Request::get(format!("/api/5/{plugin}/info"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, json)
}

#[tokio::test]
async fn mem_info_scalar_shape() {
    let (status, body) = info(Config::default(), "mem").await;
    assert_eq!(status, StatusCode::OK);
    let percent = &body["percent"];
    assert_eq!(percent["description"], "Percentage usage calculated as (total - available) / total * 100.");
    assert_eq!(percent["unit"], "percent");
    assert_eq!(percent["watched"], true);
    assert_eq!(percent["watch_direction"], "high");
    assert_eq!(percent["prominent"], true);
    // no built-in default thresholds ship (config-only).
    assert!(percent.get("default_thresholds").is_none());
    // short_name only where defined.
    assert_eq!(body["available"]["short_name"], "avail");
    assert!(body["total"].get("short_name").is_none());
    // scalar plugin: no primary_key anywhere.
    assert!(body.as_object().unwrap().values().all(|f| f.get("primary_key").is_none()));
}

#[tokio::test]
async fn network_info_collection_shape() {
    let (status, body) = info(Config::default(), "network").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["interface_name"]["primary_key"], true);
    assert_eq!(body["interface_name"]["unit"], "string");
    let recv = &body["bytes_recv"];
    assert_eq!(recv["rate"], true);
    assert_eq!(recv["watched"], true);
    assert_eq!(recv["prominent"], false);
    assert_eq!(recv["normalize_by"], "bytes_speed_rate_per_sec");
    assert_eq!(recv["unit"], "bytespers");
    // glances-rs-only field is described.
    assert!(body["bytes_all"]["description"].is_string());
}

#[tokio::test]
async fn cpu_info_internal_and_no_unimplemented_keys() {
    let (_, body) = info(Config::default(), "cpu").await;
    assert_eq!(body["cpucore"]["internal"], true);
    for (_, f) in body.as_object().unwrap() {
        assert!(f.get("history").is_none(), "history must not be emitted");
        assert!(f.get("strict_thresholds").is_none(), "strict_thresholds must not be emitted");
    }
    // parity fix visible in /info: ctx_switches normalizes by cpucore.
    assert_eq!(body["ctx_switches"]["normalize_by"], "cpucore");
    assert_eq!(body["iowait"]["prominent"], true);
    assert_eq!(body["steal"]["prominent"], false);
}

#[tokio::test]
async fn default_thresholds_reflects_config() {
    let config = Config::from_toml(
        "[plugins.mem.thresholds.percent]\nwarning = 75.0\ncritical = 90.0\n",
    )
    .unwrap();
    let (_, body) = info(config, "mem").await;
    let dt = &body["percent"]["default_thresholds"];
    assert_eq!(dt["warning"], 75.0);
    assert_eq!(dt["critical"], 90.0);
    // partial: careful was not configured -> absent.
    assert!(dt.get("careful").is_none());
}

#[tokio::test]
async fn unknown_plugin_is_404() {
    let (status, _) = info(Config::default(), "nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn info_never_wakes_a_collector() {
    let app = AppState::new(Config::default());
    let router = build_router(app.clone());
    let _ = router
        .oneshot(Request::get("/api/5/mem/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(app.active_collectors().await, 0);
}

// Load-bearing invariant (all platforms): no emitted field is undocumented.
#[tokio::test]
async fn every_emitted_field_is_documented() {
    for plugin in ["mem", "fs"] {
        let router = build_router(AppState::new(Config::default()));
        let data_resp = router
            .oneshot(Request::get(format!("/api/5/{plugin}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(data_resp.into_body(), usize::MAX).await.unwrap();
        let data: Value = serde_json::from_slice(&bytes).unwrap();
        let (_, schema) = info(Config::default(), plugin).await;
        let described: std::collections::HashSet<&str> =
            schema.as_object().unwrap().keys().map(String::as_str).collect();
        // Scalar: fields at top level. Collection: under "data"[0].
        let sample = data.get("data").and_then(|d| d.get(0)).unwrap_or(&data);
        for key in sample.as_object().unwrap().keys() {
            if key == "time_since_update" || key == "_levels" {
                continue;
            }
            assert!(described.contains(key.as_str()), "{plugin}.{key} undocumented");
        }
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --test info 2>&1 | tail`
Expected: FAIL — route returns `404`/`405` (handler not wired) or JSON mismatch.

- [ ] **Step 3: Add the handler and helper to `src/api/mod.rs`**

Add the import at the top:

```rust
use crate::plugins::fields::{FieldInfo, fields};
```

Register the route inside `api_router`, before the catch-all `/{plugin}`:

```rust
        .route("/api/5/{plugin}/info", get(plugin_info))
```

Add the handler and helper:

```rust
/// `GET /api/5/{plugin}/info` — the static field schema (design spec
/// 2026-08-02). Inert like `pluginslist`/`alert`: no wake, no store, no
/// `503`. `default_thresholds` reflects the operator's configured global
/// thresholds (config-only; no built-in defaults ship).
async fn plugin_info(State(app): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let Some(id) = PluginId::parse(&name).filter(|id| app.is_registered(*id)) else {
        return not_found(&name);
    };
    let mut out = Map::new();
    for fi in fields(id) {
        out.insert(fi.field.to_owned(), field_schema(&app, id, fi));
    }
    Json(Value::Object(out)).into_response()
}

fn field_schema(app: &AppState, id: PluginId, fi: &FieldInfo) -> Value {
    let mut m = Map::new();
    m.insert("description".into(), json!(fi.description));
    m.insert("unit".into(), json!(fi.unit.as_str()));
    if let Some(s) = fi.short_name {
        m.insert("short_name".into(), json!(s));
    }
    if fi.primary_key {
        m.insert("primary_key".into(), json!(true));
    }
    if fi.rate {
        m.insert("rate".into(), json!(true));
    }
    if fi.internal {
        m.insert("internal".into(), json!(true));
    }
    if fi.watched {
        m.insert("watched".into(), json!(true));
        m.insert("watch_direction".into(), json!(fi.direction.as_str()));
        m.insert("prominent".into(), json!(fi.prominent));
        if let Some(dt) = configured_thresholds(app, id, fi.field) {
            m.insert("default_thresholds".into(), dt);
        }
    }
    if let Some(nb) = fi.normalize_by {
        m.insert("normalize_by".into(), json!(nb));
    }
    Value::Object(m)
}

/// The operator's configured global thresholds for `field` (`Some` limits
/// only), or `None` when unconfigured. Per-item overrides are not reflected —
/// `/info` is a per-field schema.
fn configured_thresholds(app: &AppState, id: PluginId, field: &str) -> Option<Value> {
    let t = app.config.plugins.get(id.as_str())?.thresholds.get(field)?;
    let mut m = Map::new();
    if let Some(c) = t.careful {
        m.insert("careful".into(), json!(c));
    }
    if let Some(w) = t.warning {
        m.insert("warning".into(), json!(w));
    }
    if let Some(cr) = t.critical {
        m.insert("critical".into(), json!(cr));
    }
    (!m.is_empty()).then_some(Value::Object(m))
}
```

(Confirm `AppState` exposes `pub config: Config`; the alerting code already
reads `app.config` — reuse the same access. If `config` is private, add a
`pub fn config(&self) -> &Config` accessor and use it.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test info 2>&1 | tail -20`
Expected: PASS — all `info` integration tests green.

- [ ] **Step 5: Full gate**

Run: `make lint && make test`

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add src/api/mod.rs tests/info.rs
git commit -m "feat(api): GET /api/5/<plugin>/info field schema route

Inert schema route serialising plugins::fields with configured-threshold
reflection; keyed by field name, 404 on unknown/disabled plugin, never
wakes a collector.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r"
```

---

## Task 4: Docs & version

**Files:**
- Modify: `Cargo.toml` (version), `Cargo.lock` (version)
- Modify: `docs/api.md` (new `/info` section)
- Modify: `ARCHITECTURE.md` (`plugins::fields` as the single metadata source; `AlertField` retired; the five-attribute parity fix)
- Modify: `DEVELOPMENT_PLAN.md` (v0.4.0 section; remove `/info` from the deferred list)

- [ ] **Step 1: Bump the version**

In `Cargo.toml`: `version = "0.3.0"` → `version = "0.4.0"`. Then:

```bash
cargo build 2>&1 | tail -3   # refresh Cargo.lock
```

- [ ] **Step 2: Document the route in `docs/api.md`**

Add a section documenting `GET /api/5/<plugin>/info`: the key whitelist
(`description`, `unit`, `short_name`, `primary_key`, `rate`, `internal`,
`watched`, `watch_direction`, `prominent`, `default_thresholds`,
`normalize_by`); the omitted `history`/`strict_thresholds`; keyed by field
name; `default_thresholds` = configured global thresholds (config-only); `404`
on unknown/disabled plugin; inert (no wake, no `503`). Show a real `mem/info`
and `network/info` example (copy from the spec §2 samples, trimmed to the
fields glances-rs emits).

- [ ] **Step 3: Record the architecture change in `ARCHITECTURE.md`**

Note that `plugins::fields` is the single source of static field metadata,
consumed by both the `/info` handler and the alerting engine; `AlertField` is
retired. Record the five-attribute alerting parity fix (§7 of the spec) and its
one behavioural consequence (`normalize_by="cpucore"` → per-core alerting for
`load`/`ctx_switches`).

- [ ] **Step 4: Update `DEVELOPMENT_PLAN.md`**

Add a `# v0.4.0 — /info (field schema introspection)` section summarising the
three phases; remove `/api/5/<plugin>/info` from the "Out of scope (deferred
beyond v0.3.0)" list (leave `sensors`, `/api/5/config`, JWT/Bearer, TLS).

- [ ] **Step 5: Gate + commit**

```bash
cargo fmt --all
make lint && make test
git add Cargo.toml Cargo.lock docs/api.md ARCHITECTURE.md DEVELOPMENT_PLAN.md
git commit -m "docs: v0.4.0 /info route, plugins::fields, alerting parity fix

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r"
```

---

## Task 5: Footprint re-baseline

**Files:**
- Create: `docs/footprint-audit-v0.4.0.md`

- [ ] **Step 1: Build release and run the footprint script**

```bash
make build
scripts/footprint.sh 2>&1 | tee /tmp/footprint-v0.4.0.txt
```

- [ ] **Step 2: Record binary size delta**

```bash
ls -l target/release/glances-rs   # compare vs the 2.2 MiB v0.3.0 baseline
```

- [ ] **Step 3: Write `docs/footprint-audit-v0.4.0.md`**

Mirror `docs/footprint-audit-v0.3.0.md`: RSS/CPU at rest and 2/10/100 req/s on
`/all` (default config, no thresholds) vs the v0.3.0 numbers; the binary-size
delta attributable to the `fields.rs` `&'static` table; the conclusion that
default config is indistinguishable from v0.3.0 (the `/info` route allocates
only when called, and `/all` does not touch `fields.rs`). Note `/info` is not
part of `/all`, so the hot path is unchanged.

- [ ] **Step 4: Commit**

```bash
git add docs/footprint-audit-v0.4.0.md
git commit -m "docs: v0.4.0 footprint audit (default config unchanged vs v0.3.0)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01MHRetdr4R9KSFJcbiYGc8r"
```

---

## Self-review notes (for the executor)

- **Spec coverage:** §1→T1/T2 scope; §2 whitelist→T3 handler; §3 default_thresholds→T3 `configured_thresholds`; §4 unified table→T1; §5 handler→T3; §6 invariant→T3 `every_emitted_field_is_documented`; §7 parity fix→T2; §8 tests→T1/T2/T3; §9 footprint→T5; §10 docs→T4.
- **Type consistency:** `alert_fields` returns `impl Iterator<Item = &'static FieldInfo>` (T1, zero-alloc) and is consumed by iteration in `alerts.rs` (T1) and the cpucore test (T2, via `.find`). `FieldInfo` field names are identical across `fields.rs`, `alerts.rs`, and `api/mod.rs`.
- **Footprint:** no per-cycle allocation on any path — `alert_fields` is a lazy iterator over `&'static` data; the default (no-threshold) path early-outs after one `.next()`.
- **Behaviour isolation:** T1 is neutral (guard = identity with v0.3.0); the ONLY behaviour change is T2's five attributes. Keep them in separate commits.
