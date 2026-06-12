//! Phase 1 spike (DEVELOPMENT_PLAN.md) — throwaway code, deleted at the end
//! of the phase. Verifies `sysinfo` behaviour before the engine depends on it:
//!
//! 1. `MINIMUM_CPU_UPDATE_INTERVAL` value and the CPU warm-up delay actually
//!    needed for a valid first percentage.
//! 2. Network counters: cumulative `u64` semantics, per-interface keys.
//! 3. Load average availability.
//! 4. Memory fields available for the `mem` payload.
//!
//! Run with: `cargo run --example spike`

use std::time::{Duration, Instant};
use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, Networks, System};

fn main() {
    println!("=== sysinfo 0.38 spike ===\n");

    println!(
        "MINIMUM_CPU_UPDATE_INTERVAL = {:?}\n",
        MINIMUM_CPU_UPDATE_INTERVAL
    );

    cpu_warmup();
    network_counters();
    load_average();
    memory();
}

/// CPU usage needs two refreshes separated by at least
/// MINIMUM_CPU_UPDATE_INTERVAL. Measure what different delays yield.
fn cpu_warmup() {
    println!("--- CPU warm-up ---");
    for delay_ms in [0u64, 50, 100, 200, 250, 500] {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        std::thread::sleep(Duration::from_millis(delay_ms));
        sys.refresh_cpu_usage();
        println!(
            "delay {:>3} ms -> global usage = {:5.1}% ({} cores)",
            delay_ms,
            sys.global_cpu_usage(),
            sys.cpus().len()
        );
    }
    println!();
}

/// Network counters must be cumulative u64 per interface name.
fn network_counters() {
    println!("--- Network counters (two samples, 500 ms apart) ---");
    let mut networks = Networks::new_with_refreshed_list();
    let t0 = Instant::now();
    let first: Vec<(String, u64, u64)> = networks
        .iter()
        .map(|(name, data)| {
            (
                name.clone(),
                data.total_received(),
                data.total_transmitted(),
            )
        })
        .collect();
    std::thread::sleep(Duration::from_millis(500));
    networks.refresh(true);
    let elapsed = t0.elapsed().as_secs_f64();
    for (name, data) in networks.iter() {
        let prev = first.iter().find(|(n, _, _)| n == name);
        match prev {
            Some((_, rx0, tx0)) => println!(
                "{name:<12} total_rx={:>12} total_tx={:>12} | drx={:>8} dtx={:>8} over {elapsed:.3}s",
                data.total_received(),
                data.total_transmitted(),
                data.total_received().saturating_sub(*rx0),
                data.total_transmitted().saturating_sub(*tx0),
            ),
            None => println!("{name:<12} appeared between samples"),
        }
    }
    println!();
}

fn load_average() {
    println!("--- Load average ---");
    let load = System::load_average();
    println!(
        "min1={:.2} min5={:.2} min15={:.2} (cpucore={})\n",
        load.one,
        load.five,
        load.fifteen,
        System::new_all().cpus().len()
    );
}

fn memory() {
    println!("--- Memory ---");
    let mut sys = System::new();
    sys.refresh_memory();
    let (total, avail, used, free) = (
        sys.total_memory(),
        sys.available_memory(),
        sys.used_memory(),
        sys.free_memory(),
    );
    println!("total     = {total:>14} B");
    println!("available = {avail:>14} B");
    println!("used      = {used:>14} B");
    println!("free      = {free:>14} B");
    println!(
        "percent   = {:.1}% ((total-available)/total, the Glances formula)",
        (total - avail) as f64 / total as f64 * 100.0
    );
}
