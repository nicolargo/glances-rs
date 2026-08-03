# Footprint audit — v0.4.0 (`/info` endpoint)

> Acceptance gate for the `/info` field-schema feature
> (`.superpowers/sdd/2026-08-02-info-endpoint/`). v0.4.0 adds
> `GET /api/5/<plugin>/info` (static field metadata, mirroring Glances v5),
> backed by a new `plugins::fields` module with a unified `&'static
> [FieldInfo]` table per plugin. The footprint mandate requires that the
> **default request path** (`/api/5/all`, the one every client hits on every
> poll) stay indistinguishable from v0.3.0, since `/info` is a new, separate,
> low-traffic route that does not touch it.

Measured with `scripts/footprint.sh` against `/api/5/all` (nine plugins,
default config, no alert thresholds configured), release binary
(`make build`: `lto`, `codegen-units = 1`, `strip`, `panic = "abort"`), same
method as the v0.2.0/v0.3.0 audits. glances-rs was started fresh (no prior
requests, so no collector had woken yet) on a non-default port to avoid a
conflict with an unrelated Python Glances process already bound to
`61208` on the test machine; this does not affect the measurement.

Reminder from the v0.3.0 audit, still true here: RSS varies by a few hundred
KiB run to run (v0.2.0 itself recorded 5.5 and 6.4 MiB @100 req/s across
different sub-runs) — read all numbers below at ±0.5 MiB, and CPU % at
similar looseness.

## Default config: the path that must stay free

| load | RSS v0.3.0 (ref) | RSS v0.4.0 | CPU v0.3.0 | CPU v0.4.0 |
|---|---|---|---|---|
| rest | 3.3 MiB | **3.4 MiB** | ≈ 0 % | ≈ 0 % |
| 2 req/s | 3.9 MiB | 4.0 MiB | 0.20 % | 0.10 % |
| 10 req/s | 3.9 MiB | 4.0 MiB | 0.40 % | 0.50 % |
| 100 req/s | 6.1 MiB | **6.4 MiB** | 1.60 % | 1.30 % |

Raw run (`scripts/footprint.sh`, 10 s per rate):

```
RSS at rest:    3.4 MiB

rate      peak RSS      CPU       delivered
2/s         4.0 MiB      .10%    20 req (2/s)
10/s        4.0 MiB      .50%    100 req (10/s)
100/s       6.4 MiB     1.30%    1000 req (100/s)
```

**Verdict: no measurable regression.** Every delta (rest +0.1 MiB, 100 req/s
+0.3 MiB, CPU swinging ±0.2–0.3 pt either direction) sits inside the run-to-run
noise band called out above — the same band the v0.3.0 audit itself used to
wave off a +0.6 MiB delta at 100 req/s. There is no trend consistent with a
real cost: CPU at 100 req/s is actually *lower* than v0.3.0's reading. This is
the expected result, not a coincidence — see "why" below.

## Why: `/info` is not on the `/all` path

`GET /api/5/<plugin>/info` is registered as its own route
(`src/api/mod.rs:28`, `.route("/api/5/{plugin}/info", get(plugin_info))`),
separate from `.route("/api/5/all", get(all_stats))` (`src/api/mod.rs:26`).
`all_stats` walks the plugin registry and serializes live collector state; it
never calls into `plugins::fields`. The `&'static [FieldInfo]` table
(`src/plugins/fields.rs`) is only read when a client explicitly requests
`/<plugin>/info` — a schema lookup, not part of any collection cycle. So:

- the per-cycle collection loop is untouched (no new field reads, no new
  allocations);
- the `/all` and `/<plugin>` hot paths serialize the same structs as v0.3.0;
- `/info` responses are built from `&'static` data with no per-cycle upkeep —
  the only cost is the JSON serialization of that static table, paid once per
  `/info` request, never per `/all` request.

Confirmed on the release binary: `curl /api/5/all` returns the same
plugin-keyed object as v0.3.0 (no `info` key, no field-schema payload mixed
in); `curl /api/5/mem/info` returns the static schema
(`{"active":{"description":...,"unit":"bytes"},...}`) independently.

## Binary

| | v0.3.0 | v0.4.0 |
|---|---|---|
| release size | 2.2 MiB | **2.2 MiB** (2,341,504 bytes) |

No measurable growth at the audit's usual precision. `plugins::fields.rs`
(402 lines) is a static table of short `&'static str` descriptions/units
compiled into `.rodata` — a few KiB, well under the rounding threshold that
would move the reported figure. No new dependency was added (the route reuses
the existing `serde_json`/axum machinery).

## Conclusion

The gate is held: **default config (`/all`, no thresholds) is indistinguishable
from v0.3.0** within run-to-run noise, binary size is unchanged at this
precision, and no new dependency was introduced. `/info` is a genuinely
separate, opt-in route — it does not touch the collection loop and cannot
regress the path every polling client actually uses. No blocking regression.
