#!/usr/bin/env bash
# Measure a monitoring server's footprint: resident memory at rest and the
# peak memory + CPU it costs under a polling load. Reads /proc directly, so
# no external benchmarking tool is required (Linux only).
#
# Usage:
#   scripts/footprint.sh <pid> <url> [duration_seconds] [concurrency]
#
# Example — glances-rs (let it idle first so collectors have stopped):
#   ./target/release/glances-rs &
#   scripts/footprint.sh "$(pgrep -n glances-rs)" http://127.0.0.1:61208/api/5/all
#
# Example — Glances web server:
#   glances -w &
#   scripts/footprint.sh "$(pgrep -n glances)" http://127.0.0.1:61208/api/4/all
#
# Run both on the SAME machine for a meaningful comparison.
set -euo pipefail

pid="${1:?usage: footprint.sh <pid> <url> [duration] [concurrency]}"
url="${2:?missing url}"
duration="${3:-15}"
concurrency="${4:-8}"

clk_tck="$(getconf CLK_TCK)"

rss_kb() { awk '/^VmRSS:/ {print $2}' "/proc/$1/status"; }
cpu_jiffies() { awk '{print $14 + $15}' "/proc/$1/stat"; } # utime + stime

[ -d "/proc/$pid" ] || { echo "no process with pid $pid" >&2; exit 1; }

printf 'pid=%s url=%s duration=%ss concurrency=%s\n\n' "$pid" "$url" "$duration" "$concurrency"

rest_kb="$(rss_kb "$pid")"
printf 'RSS at rest:        %6.1f MiB\n' "$(echo "$rest_kb / 1024" | bc -l)"

# Drive a steady polling load with parallel curl loops.
cpu0="$(cpu_jiffies "$pid")"
stop="$(( $(date +%s) + duration ))"
for _ in $(seq "$concurrency"); do
  ( while [ "$(date +%s)" -lt "$stop" ]; do curl -s -o /dev/null "$url" || true; done ) &
done

peak_kb="$rest_kb"
while [ "$(date +%s)" -lt "$stop" ]; do
  cur="$(rss_kb "$pid" 2>/dev/null || echo "$peak_kb")"
  [ "$cur" -gt "$peak_kb" ] && peak_kb="$cur"
  sleep 0.2
done
wait 2>/dev/null || true
cpu1="$(cpu_jiffies "$pid")"

cpu_pct="$(echo "scale=1; ($cpu1 - $cpu0) / $clk_tck / $duration * 100" | bc -l)"
printf 'RSS under load:     %6.1f MiB (peak)\n' "$(echo "$peak_kb / 1024" | bc -l)"
printf 'CPU under load:     %6.1f %% (1 core = 100%%)\n' "$cpu_pct"
