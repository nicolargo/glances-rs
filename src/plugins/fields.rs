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
    FieldInfo::new(
        "hr_name",
        "Human readable operating system name.",
        Unit::StringT,
    ),
];

const UPTIME_FIELDS: &[FieldInfo] = &[FieldInfo::new(
    "seconds",
    "Seconds elapsed since the system booted.",
    Unit::Seconds,
)];

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
    FieldInfo::new(
        "device_name",
        "Device name backing the filesystem (e.g. /dev/sda1).",
        Unit::StringT,
    ),
    FieldInfo::new(
        "fs_type",
        "File system type (e.g. ext4, xfs, btrfs).",
        Unit::StringT,
    )
    .internal(),
    FieldInfo::new(
        "size",
        "Total size of the filesystem in bytes.",
        Unit::Bytes,
    ),
    FieldInfo::new("used", "Used size in bytes.", Unit::Bytes),
    FieldInfo::new("free", "Free size in bytes.", Unit::Bytes),
    FieldInfo::new(
        "percent",
        "Filesystem usage as a percentage of total size.",
        Unit::Percent,
    )
    .watched_high(false),
    FieldInfo::new(
        "alias",
        "Operator-defined display alias for the mount point; present only when configured.",
        Unit::StringT,
    ),
];

const DISKIO_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("disk_name", "Disk name (e.g. sda, nvme0n1).", Unit::StringT).pk(),
    FieldInfo::new(
        "read_count",
        "Read operations per second (rate of psutil read_count counter).",
        Unit::Number,
    )
    .rate()
    .internal(),
    FieldInfo::new(
        "write_count",
        "Write operations per second (rate of psutil write_count counter).",
        Unit::Number,
    )
    .rate()
    .internal(),
    FieldInfo::new(
        "read_bytes",
        "Bytes read per second (rate of psutil read_bytes counter).",
        Unit::BytesPerSec,
    )
    .rate()
    .watched_high(false),
    FieldInfo::new(
        "write_bytes",
        "Bytes written per second (rate of psutil write_bytes counter).",
        Unit::BytesPerSec,
    )
    .rate()
    .watched_high(false),
    FieldInfo::new(
        "alias",
        "Operator-defined display alias for the disk; present only when configured.",
        Unit::StringT,
    ),
];

const NETWORK_FIELDS: &[FieldInfo] = &[
    FieldInfo::new("interface_name", "Network interface name.", Unit::StringT).pk(),
    FieldInfo::new(
        "bytes_recv",
        "Bytes received per second.",
        Unit::BytesPerSec,
    )
    .rate()
    .watched_high(false)
    .normalize("bytes_speed_rate_per_sec"),
    FieldInfo::new("bytes_sent", "Bytes sent per second.", Unit::BytesPerSec)
        .rate()
        .watched_high(false)
        .normalize("bytes_speed_rate_per_sec"),
    FieldInfo::new(
        "bytes_all",
        "Total bytes received and sent per second (bytes_recv + bytes_sent).",
        Unit::BytesPerSec,
    )
    .rate(),
    FieldInfo::new(
        "alias",
        "Operator-defined display alias for the interface; present only when configured.",
        Unit::StringT,
    ),
    FieldInfo::new("is_up", "Whether the interface is up.", Unit::Bool),
    FieldInfo::new(
        "speed",
        "Maximum interface link speed in bits per second (0 when the OS does not report it).",
        Unit::BitPerSec,
    ),
    FieldInfo::new(
        "bytes_speed_rate_per_sec",
        "Estimated per-direction bandwidth capacity in bytes/s. Computed from the interface link speed (Mbit/s) under a full-duplex split assumption: speed_mbits * 1e6 / 8 / 2. Returns 0 when the OS does not report a link speed (loopback, virtual interfaces) — in which case threshold normalisation is skipped for bytes_recv / bytes_sent.",
        Unit::BytesPerSec,
    ),
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
            (
                PluginId::Network,
                "bytes_recv",
                false,
                Some("bytes_speed_rate_per_sec"),
            ),
            (
                PluginId::Network,
                "bytes_sent",
                false,
                Some("bytes_speed_rate_per_sec"),
            ),
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
            let pk: Vec<&str> = fields(id)
                .iter()
                .filter(|f| f.primary_key)
                .map(|f| f.field)
                .collect();
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
