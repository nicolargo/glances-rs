# Footprint audit — v0.4.2 + Glances-v5 alignment

> Re-measurement after the work of this branch (`claude/gifted-allen-px4qlt`):
> new payload fields (`mem.shared/slab`, `network` cumulative/errors/drops,
> `diskio` times/cumulative), the `/{plugin}/{field}` and `/top/{n}` routes,
> JSON `/status`/`/healthz`, and `POST /api/5/token` (two new crates, `hmac`
> and `sha2`). Version string is still `0.4.2` in `Cargo.toml`.

Method unchanged from the earlier audits: `scripts/footprint.sh` against
`/api/5/all` (nine plugins, default config, no thresholds), release profile
(`opt-level = "s"`, `lto`, `codegen-units = 1`, `strip`, `panic = "abort"`),
10 s per rate, server idle 16 s before the rest sample. Same VM, same session
for every row below. Linux x86-64, `rustc` 1.94+.

## A/B: before vs after this branch (same session, two runs each)

"Before" is commit `5ef59f9` (v0.4.2 merge), "after" is `4488b13`.

| | before run 1 | after run 1 | before run 2 | after run 2 |
|---|---|---|---|---|
| rest | 3.4 MiB | 3.5 MiB | 3.5 MiB | 3.5 MiB |
| 2 req/s | 3.6 MiB / 0 % | 3.7 MiB / 0 % | 3.6 MiB / 0.10 % | 3.7 MiB / 0.10 % |
| 10 req/s | 3.8 MiB / 0.30 % | 3.9 MiB / 0.30 % | 3.8 MiB / 0.10 % | 4.0 MiB / 0.20 % |
| 100 req/s | 5.1 MiB / 1.20 % | 5.8 MiB / 1.30 % | 4.7 MiB / 1.40 % | 5.3 MiB / 1.30 % |

| | before | after | delta |
|---|---|---|---|
| binary | 1,835,888 B (1.751 MiB) | 1,905,000 B (1.817 MiB) | **+69,112 B (+3.8 %)** |

Reading (noise band ≈ ±0.5 MiB, CPU % equally loose):

- **Rest and low load: unchanged** (≤ 0.2 MiB, inside noise). The lazy design
  still holds: nothing runs without a client.
- **100 req/s: +0.4 to +0.7 MiB** in both pairs, the only consistent upward
  move. Plausible cause: larger `/all` payloads (about 14 extra numbers per
  network interface and per disk), which the pre-serialized `Bytes` store keeps
  and every in-flight response holds. CPU did not move.
- **Binary +3.8 %**: `hmac` + `sha2` and their small dependency tree, the new
  routes and fields. Still below the v0.4.0 binary (2.233 MiB).
- The new per-cycle work is one extra `/proc/net/dev` read for the network
  plugin on Linux (drops); not separable from noise in CPU.

## glances-rs vs Glances v5 (`develop`, 5.0.0b1)

Same machine, same nine plugins. Glances launched as
`glances -w --disable-webui --disable-history --disable-plugins all
--enable-plugins cpu,load,mem,network,system,uptime,memswap,fs,diskio`
(Glances v5 flags; the table in the README used Glances 4.5.5 until now).

| Polling load | glances-rs RSS / CPU | Glances v5 RSS / CPU |
|---|---|---|
| rest (no client) | 3.5 MiB | 62.3 MiB |
| 2 req/s | 3.7 MiB / 0.10 % | 62.5 MiB / 0.80 % |
| 10 req/s | 3.9 MiB / 0.20 % | 62.5 MiB / 1.60 % |
| 100 req/s | 5.1 MiB / 1.30 % | 63.0 MiB / 9.90 % |

Ratio: ~18× less memory at rest, ~12× at 100 req/s, ~8× less CPU at 100 req/s.
Glances v5 `develop` uses far less memory than 4.5.5 did (≈107 MiB), so the
gap narrowed. Glances' CPU at rest (its scheduler collects continuously) was
not sampled by this script.

Caveats: one VM, one run per Glances row, Glances 5.0.0b1 is a beta, and the
sampler (`curl` per request) is itself CPU-bound at 100 req/s.
