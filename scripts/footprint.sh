#!/usr/bin/env bash
# Measure a monitoring server's footprint: resident memory at rest, then the
# peak RSS and CPU it costs under a *rate-controlled* polling load at several
# request rates. Reads /proc directly, so no external benchmarking tool is
# required (Linux only).
#
# Usage:
#   scripts/footprint.sh <pid> <url> [rates="2 10 100"] [seconds_per_rate=10]
#
# The rates mirror real clients: 2 req/s is the default Glances WebUI/TUI
# refresh; 10 and 100 req/s stand in for heavier polling.
#
# Example — glances-rs (let it idle first so collectors have stopped):
#   ./target/release/glances-rs &
#   scripts/footprint.sh "$(pgrep -n glances-rs)" http://127.0.0.1:61208/api/5/all
#
# Example — Glances scoped to the same four plugins:
#   glances --disable-plugins all --enable-plugins cpu,load,mem,network \
#           --disable-history --disable-webui -w &
#   scripts/footprint.sh "$(pgrep -f glances)" http://127.0.0.1:61208/api/4/all
#
# Run both on the SAME machine for a meaningful comparison.
set -uo pipefail

pid="${1:?usage: footprint.sh <pid> <url> [rates] [seconds_per_rate]}"
url="${2:?missing url}"
rates="${3:-2 10 100}"
secs="${4:-10}"

clk="$(getconf CLK_TCK)"
rss_kb() { awk '/^VmRSS:/ {print $2}' "/proc/$1/status"; }
cpu_jiffies() { awk '{print $14 + $15}' "/proc/$1/stat"; } # utime + stime
mib() { echo "scale=1; $1 / 1024" | bc -l; }

[ -d "/proc/$pid" ] || { echo "no process with pid $pid" >&2; exit 1; }

printf 'pid=%s  url=%s  %ss per rate\n\n' "$pid" "$url" "$secs"
printf 'RSS at rest:  %5s MiB\n\n' "$(mib "$(rss_kb "$pid")")"
printf '%-9s %-13s %-9s %s\n' 'rate' 'peak RSS' 'CPU' 'delivered'

tmp="$(mktemp)"
for rate in $rates; do
  : > "$tmp"
  cpu0="$(cpu_jiffies "$pid")"
  peak_kb="$(rss_kb "$pid")"
  end="$(( $(date +%s) + secs ))"
  while [ "$(date +%s)" -lt "$end" ]; do
    sec_end="$(( $(date +%s) + 1 ))"
    # Fire `rate` requests for this 1-second window.
    for _ in $(seq "$rate"); do
      ( curl -s -o /dev/null --max-time 5 "$url" && printf 'x' >> "$tmp" ) &
    done
    # Sample RSS while the window drains.
    while [ "$(date +%s)" -lt "$sec_end" ]; do
      cur="$(rss_kb "$pid" 2>/dev/null || echo "$peak_kb")"
      [ "${cur:-0}" -gt "$peak_kb" ] && peak_kb="$cur"
      sleep 0.2
    done
  done
  wait 2>/dev/null || true
  cpu1="$(cpu_jiffies "$pid")"
  # Multiply before dividing: with bc's fixed scale, dividing first would
  # truncate small intermediate results to zero.
  cpu_pct="$(echo "scale=2; ($cpu1 - $cpu0) * 100 / $clk / $secs" | bc -l)"
  delivered="$(wc -c < "$tmp" | tr -d ' ')"
  printf '%-9s %5s MiB    %5s%%    %s req (%s/s)\n' \
    "${rate}/s" "$(mib "$peak_kb")" "$cpu_pct" "$delivered" "$(( delivered / secs ))"
done
rm -f "$tmp"
