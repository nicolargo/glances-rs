//! Linux-specific stat sources giving the full Glances v5 field set, which
//! `sysinfo`'s public API does not expose (it parses `/proc/stat`
//! internally but keeps the per-category breakdown private).
//!
//! The parsers take the file contents as `&str`, so they are unit-tested
//! against captured samples without touching the real filesystem.

use super::round1;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// /proc/stat — CPU
// ---------------------------------------------------------------------------

/// Cumulative CPU jiffies from the aggregate `cpu` line, plus the
/// cumulative event counters used for the rate fields.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuSample {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
    pub guest: u64,
    pub guest_nice: u64,
    pub ctxt: u64,
    pub intr: u64,
    pub softirq_total: u64,
}

pub fn read_proc_stat() -> Option<CpuSample> {
    std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| parse_proc_stat(&s))
}

pub fn parse_proc_stat(content: &str) -> Option<CpuSample> {
    let mut s = CpuSample::default();
    let mut seen_cpu = false;
    for line in content.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("cpu") => {
                // user nice system idle iowait irq softirq steal guest guest_nice
                let v: Vec<u64> = it.filter_map(|x| x.parse().ok()).collect();
                s.user = *v.first()?;
                s.nice = *v.get(1)?;
                s.system = *v.get(2)?;
                s.idle = *v.get(3)?;
                s.iowait = v.get(4).copied().unwrap_or(0);
                s.irq = v.get(5).copied().unwrap_or(0);
                s.softirq = v.get(6).copied().unwrap_or(0);
                s.steal = v.get(7).copied().unwrap_or(0);
                s.guest = v.get(8).copied().unwrap_or(0);
                s.guest_nice = v.get(9).copied().unwrap_or(0);
                seen_cpu = true;
            }
            Some("ctxt") => s.ctxt = it.next().and_then(|x| x.parse().ok()).unwrap_or(0),
            Some("intr") => s.intr = it.next().and_then(|x| x.parse().ok()).unwrap_or(0),
            Some("softirq") => {
                s.softirq_total = it.next().and_then(|x| x.parse().ok()).unwrap_or(0)
            }
            _ => {}
        }
    }
    seen_cpu.then_some(s)
}

/// Per-category CPU percentages from two cumulative samples, following the
/// psutil/Glances semantics: the denominator is the total jiffy delta, and
/// `guest`/`guest_nice` are subtracted from `user`/`nice` because the
/// kernel already counts guest time inside user time. `total` is the busy
/// share, i.e. everything except `idle` (iowait counts as busy, matching
/// Glances).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuPercents {
    pub total: f64,
    pub user: f64,
    pub nice: f64,
    pub system: f64,
    pub idle: f64,
    pub iowait: f64,
    pub irq: f64,
    pub steal: f64,
    pub guest: f64,
}

pub fn cpu_percents(prev: &CpuSample, cur: &CpuSample) -> CpuPercents {
    let user = cur.user.saturating_sub(prev.user) as f64;
    let nice = cur.nice.saturating_sub(prev.nice) as f64;
    let system = cur.system.saturating_sub(prev.system) as f64;
    let idle = cur.idle.saturating_sub(prev.idle) as f64;
    let iowait = cur.iowait.saturating_sub(prev.iowait) as f64;
    let irq = cur.irq.saturating_sub(prev.irq) as f64;
    let softirq = cur.softirq.saturating_sub(prev.softirq) as f64;
    let steal = cur.steal.saturating_sub(prev.steal) as f64;
    let guest = cur.guest.saturating_sub(prev.guest) as f64;
    let guest_nice = cur.guest_nice.saturating_sub(prev.guest_nice) as f64;

    let total = user + nice + system + idle + iowait + irq + softirq + steal;
    if total <= 0.0 {
        return CpuPercents::default();
    }
    let pct = |x: f64| round1(x / total * 100.0);
    CpuPercents {
        total: round1((total - idle) / total * 100.0),
        user: pct((user - guest).max(0.0)),
        nice: pct((nice - guest_nice).max(0.0)),
        system: pct(system),
        idle: pct(idle),
        iowait: pct(iowait),
        irq: pct(irq),
        steal: pct(steal),
        guest: pct(guest),
    }
}

// ---------------------------------------------------------------------------
// /proc/meminfo — memory
// ---------------------------------------------------------------------------

/// Memory figures in bytes, mirroring `psutil.virtual_memory()` on Linux.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemInfo {
    pub total: u64,
    pub available: u64,
    pub free: u64,
    pub buffers: u64,
    pub cached: u64,
    pub active: u64,
    pub inactive: u64,
    pub used: u64,
    pub percent: f64,
}

pub fn read_meminfo() -> Option<MemInfo> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .map(|s| parse_meminfo(&s))
}

pub fn parse_meminfo(content: &str) -> MemInfo {
    let mut kv: HashMap<&str, u64> = HashMap::new();
    for line in content.lines() {
        // e.g. "MemTotal:       16461176 kB"
        if let Some((key, rest)) = line.split_once(':')
            && let Some(num) = rest.split_whitespace().next()
            && let Ok(value) = num.parse::<u64>()
        {
            kv.insert(key, value * 1024); // kB -> bytes
        }
    }
    let g = |k: &str| kv.get(k).copied().unwrap_or(0);

    let total = g("MemTotal");
    let free = g("MemFree");
    let available = if kv.contains_key("MemAvailable") {
        g("MemAvailable")
    } else {
        free
    };
    let buffers = g("Buffers");
    // psutil: cached = Cached + SReclaimable.
    let cached = g("Cached") + g("SReclaimable");
    // psutil: used = total - free - cached - buffers.
    let used = total
        .saturating_sub(free)
        .saturating_sub(cached)
        .saturating_sub(buffers);
    let percent = if total == 0 {
        0.0
    } else {
        round1(total.saturating_sub(available) as f64 / total as f64 * 100.0)
    };
    MemInfo {
        total,
        available,
        free,
        buffers,
        cached,
        active: g("Active"),
        inactive: g("Inactive"),
        used,
        percent,
    }
}

// ---------------------------------------------------------------------------
// /sys/class/net — interface status
// ---------------------------------------------------------------------------

/// Administrative status and link speed of one interface.
pub struct IfaceMeta {
    pub is_up: bool,
    /// Link speed in bits per second, 0 when unknown (Glances multiplies
    /// the Mbps value reported by the kernel by 1048576).
    pub speed: u64,
    /// Per-direction bandwidth capacity in bytes/s for `normalize_by` (spec
    /// §4.6): `mbps * 1e6 / 8 / 2`. `0` when the link speed is unknown.
    pub bytes_speed_rate_per_sec: u64,
}

/// Decimal-Mbit per-direction byte capacity, matching Glances v5's
/// `bytes_speed_rate_per_sec`. Note this uses 1e6 (not the 1_048_576 of the
/// `speed` field) — Glances deliberately scales the two fields differently.
pub(crate) fn speed_capacity_bytes_per_dir(mbps: u64) -> u64 {
    mbps * 1_000_000 / 8 / 2
}

pub fn read_iface_meta(name: &str) -> IfaceMeta {
    let base = format!("/sys/class/net/{name}");
    // IFF_UP (0x1) of the interface flags — the administrative "up" bit.
    let is_up = std::fs::read_to_string(format!("{base}/flags"))
        .ok()
        .and_then(|s| u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .map(|flags| flags & 0x1 != 0)
        .unwrap_or(false);
    // `/sys` speed is Mbps; -1 or an error means unknown -> 0.
    let mbps = std::fs::read_to_string(format!("{base}/speed"))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&mbps| mbps > 0)
        .map(|mbps| mbps as u64)
        .unwrap_or(0);
    let speed = mbps * 1_048_576;
    let bytes_speed_rate_per_sec = speed_capacity_bytes_per_dir(mbps);
    IfaceMeta {
        is_up,
        speed,
        bytes_speed_rate_per_sec,
    }
}

// ---------------------------------------------------------------------------
// /proc/meminfo + /proc/vmstat — swap
// ---------------------------------------------------------------------------

/// Swap figures in bytes, mirroring `psutil.swap_memory()` on Linux. `sin`
/// and `sout` are **cumulative** byte counters (pages swapped in/out since
/// boot × page size), reported raw exactly as Glances does — the client
/// derives a rate from successive samples and `time_since_update`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SwapInfo {
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub percent: f64,
    pub sin: u64,
    pub sout: u64,
}

pub fn read_swap() -> Option<SwapInfo> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let vmstat = std::fs::read_to_string("/proc/vmstat").ok()?;
    Some(parse_swap(&meminfo, &vmstat, page_size()))
}

/// `sysconf(_SC_PAGESIZE)`, falling back to 4 KiB if it ever returns ≤ 0.
fn page_size() -> u64 {
    // SAFETY: sysconf with a constant name has no preconditions and no
    // memory effects; it returns a `long`.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v > 0 { v as u64 } else { 4096 }
}

/// `page_size` is passed in (not read here) so the parser stays pure and
/// testable without touching `sysconf`.
pub fn parse_swap(meminfo: &str, vmstat: &str, page_size: u64) -> SwapInfo {
    let (mut total, mut free) = (0u64, 0u64);
    for line in meminfo.lines() {
        if let Some((key, rest)) = line.split_once(':')
            && let Some(num) = rest.split_whitespace().next()
            && let Ok(value) = num.parse::<u64>()
        {
            match key {
                "SwapTotal" => total = value * 1024, // kB -> bytes
                "SwapFree" => free = value * 1024,
                _ => {}
            }
        }
    }

    let (mut pswpin, mut pswpout) = (0u64, 0u64);
    for line in vmstat.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("pswpin") => pswpin = it.next().and_then(|x| x.parse().ok()).unwrap_or(0),
            Some("pswpout") => pswpout = it.next().and_then(|x| x.parse().ok()).unwrap_or(0),
            _ => {}
        }
    }

    let used = total.saturating_sub(free);
    let percent = if total == 0 {
        0.0
    } else {
        round1(used as f64 / total as f64 * 100.0)
    };
    SwapInfo {
        total,
        used,
        free,
        percent,
        sin: pswpin.saturating_mul(page_size),
        sout: pswpout.saturating_mul(page_size),
    }
}

// ---------------------------------------------------------------------------
// /proc/diskstats — per-disk I/O counters
// ---------------------------------------------------------------------------

/// Cumulative `(read_count, write_count, read_bytes, write_bytes)` for one
/// disk. `*_bytes` are sectors × 512 (the fixed `/proc/diskstats` sector
/// size, as psutil uses), independent of the device's physical sector size.
pub type DiskIoCounters = (u64, u64, u64, u64);

pub fn read_diskstats() -> Option<std::collections::HashMap<String, DiskIoCounters>> {
    std::fs::read_to_string("/proc/diskstats")
        .ok()
        .map(|s| parse_diskstats(&s))
}

pub fn parse_diskstats(content: &str) -> HashMap<String, DiskIoCounters> {
    let mut map = HashMap::new();
    for line in content.lines() {
        // major minor name reads merged sectors_read ms_read writes merged
        // sectors_written ms_write ... (≥ 14 base fields).
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let num = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        let read_count = num(3);
        let read_bytes = num(5).saturating_mul(512);
        let write_count = num(7);
        let write_bytes = num(9).saturating_mul(512);
        map.insert(
            f[2].to_string(),
            (read_count, write_count, read_bytes, write_bytes),
        );
    }
    map
}

// ---------------------------------------------------------------------------
// /etc/os-release — Linux distribution
// ---------------------------------------------------------------------------

/// `linux_distro` string ("NAME VERSION_ID") from `/etc/os-release`, the same
/// two fields Glances combines. `None` when the file is unreadable.
pub fn read_os_release() -> Option<String> {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .map(|s| parse_os_release(&s))
}

pub fn parse_os_release(content: &str) -> String {
    let mut name = "";
    let mut version = "";
    for line in content.lines() {
        // Values may be double-quoted (`NAME="Ubuntu"`); trim the quotes.
        if let Some(v) = line.strip_prefix("NAME=") {
            name = v.trim().trim_matches('"');
        } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
            version = v.trim().trim_matches('"');
        }
    }
    format!("{name} {version}").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "\
cpu  990 10 4260 214257 735 0 15 1 5 0
cpu0 495 5 2130 107128 367 0 7 0 2 0
intr 227275 1 2 3
ctxt 456736
btime 1700000000
processes 1757
procs_running 1
softirq 35833 100 200
";

    #[test]
    fn parse_proc_stat_reads_aggregate_line_and_counters() {
        let s = parse_proc_stat(STAT).unwrap();
        assert_eq!(s.user, 990);
        assert_eq!(s.nice, 10);
        assert_eq!(s.system, 4260);
        assert_eq!(s.idle, 214257);
        assert_eq!(s.iowait, 735);
        assert_eq!(s.softirq, 15);
        assert_eq!(s.steal, 1);
        assert_eq!(s.guest, 5);
        assert_eq!(s.ctxt, 456736);
        assert_eq!(s.intr, 227275);
        assert_eq!(s.softirq_total, 35833);
    }

    #[test]
    fn cpu_percents_sum_with_idle_to_about_100() {
        let prev = CpuSample {
            user: 100,
            system: 100,
            idle: 800,
            ..Default::default()
        };
        let cur = CpuSample {
            user: 200,
            system: 150,
            idle: 1650,
            ..Default::default()
        };
        // deltas: user 100, system 50, idle 850, total 1000
        let p = cpu_percents(&prev, &cur);
        assert_eq!(p.user, 10.0);
        assert_eq!(p.system, 5.0);
        assert_eq!(p.idle, 85.0);
        assert_eq!(p.total, 15.0); // 100 - idle
    }

    #[test]
    fn cpu_percents_identical_samples_are_zero() {
        let s = CpuSample {
            user: 5,
            idle: 5,
            ..Default::default()
        };
        assert_eq!(cpu_percents(&s, &s), CpuPercents::default());
    }

    #[test]
    fn parse_meminfo_follows_psutil_formulas() {
        let content = "\
MemTotal:       1000 kB
MemFree:         400 kB
MemAvailable:    600 kB
Buffers:          50 kB
Cached:          100 kB
SReclaimable:     20 kB
Active:          200 kB
Inactive:        150 kB
";
        let m = parse_meminfo(content);
        assert_eq!(m.total, 1000 * 1024);
        assert_eq!(m.free, 400 * 1024);
        assert_eq!(m.available, 600 * 1024);
        assert_eq!(m.buffers, 50 * 1024);
        assert_eq!(m.cached, (100 + 20) * 1024); // Cached + SReclaimable
        assert_eq!(m.used, (1000 - 400 - 120 - 50) * 1024); // total-free-cached-buffers
        assert_eq!(m.active, 200 * 1024);
        assert_eq!(m.percent, 40.0); // (1000-600)/1000
    }

    #[test]
    fn parse_meminfo_falls_back_to_free_without_memavailable() {
        let m = parse_meminfo("MemTotal: 1000 kB\nMemFree: 400 kB\n");
        assert_eq!(m.available, 400 * 1024);
    }

    #[test]
    fn parse_diskstats_reads_counts_and_sector_bytes() {
        // Real-shaped lines: sda has 14 base fields, loop0 too.
        let content = "\
   8       0 sda 1000 50 4000 120 2000 30 8000 200 0 300 420
 259       0 nvme0n1 5 0 40 1 7 0 80 2 0 3 6
";
        let m = parse_diskstats(content);
        let sda = m.get("sda").unwrap();
        // read_count, write_count, read_bytes (4000*512), write_bytes (8000*512)
        assert_eq!(*sda, (1000, 2000, 4000 * 512, 8000 * 512));
        assert!(m.contains_key("nvme0n1"));
    }

    #[test]
    fn parse_diskstats_skips_short_lines() {
        assert!(parse_diskstats("8 0 sda 1 2 3\n").is_empty());
    }

    #[test]
    fn parse_swap_reads_bytes_and_cumulative_counters() {
        let meminfo = "\
MemTotal:       1000 kB
SwapTotal:      2000 kB
SwapFree:        500 kB
";
        let vmstat = "\
nr_free_pages 12345
pswpin 10
pswpout 7
";
        // page_size = 4096: sin = 10*4096, sout = 7*4096.
        let s = parse_swap(meminfo, vmstat, 4096);
        assert_eq!(s.total, 2000 * 1024);
        assert_eq!(s.free, 500 * 1024);
        assert_eq!(s.used, (2000 - 500) * 1024);
        assert_eq!(s.percent, 75.0); // 1500/2000
        assert_eq!(s.sin, 10 * 4096);
        assert_eq!(s.sout, 7 * 4096);
    }

    #[test]
    fn parse_swap_without_swap_is_all_zero() {
        let s = parse_swap("MemTotal: 1000 kB\n", "pswpin 0\n", 4096);
        assert_eq!(s.total, 0);
        assert_eq!(s.percent, 0.0);
    }

    #[test]
    fn parse_os_release_combines_name_and_version_id() {
        let content = "\
PRETTY_NAME=\"Ubuntu 22.04.3 LTS\"
NAME=\"Ubuntu\"
VERSION_ID=\"22.04\"
ID=ubuntu
";
        assert_eq!(parse_os_release(content), "Ubuntu 22.04");
    }

    #[test]
    fn parse_os_release_tolerates_unquoted_and_missing_fields() {
        assert_eq!(parse_os_release("NAME=Arch\n"), "Arch");
        assert_eq!(parse_os_release(""), "");
    }

    #[test]
    fn bytes_speed_rate_per_sec_is_decimal_per_direction() {
        // 1000 Mbit/s full-duplex -> 1000 * 1e6 / 8 / 2 = 62_500_000 B/s per dir.
        assert_eq!(super::speed_capacity_bytes_per_dir(1000), 62_500_000);
        // unknown / down link -> 0.
        assert_eq!(super::speed_capacity_bytes_per_dir(0), 0);
    }
}
