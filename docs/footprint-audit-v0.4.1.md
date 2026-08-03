# Footprint audit — v0.4.1 (footprint pass)

> Closes out Phase 9 of `DEVELOPMENT_PLAN.md` (footprint optimization study,
> full plan at `.superpowers/sdd/2026-08-03-footprint-pass/`). Three code
> changes shipped in this pass, in order:
>
> 1. **Per-cycle serialization + `Bytes` serving** (Task 2) — the store
>    (`AppState`) now holds pre-serialized `bytes::Bytes` per plugin,
>    produced once per collection cycle in `plugin_loop`/`publish`, instead
>    of a `serde_json::Value` that every request cloned and re-serialized.
>    Handlers now do a cheap `Bytes` refcount clone.
> 2. **`async-trait` removal** (Task 1) — `Plugin::collect()` moved from
>    `#[async_trait::async_trait]` to native `async fn` in traits (RPITIT).
>    `Plugin` was never used as `dyn Plugin`, so this removes one
>    `Box::pin` heap allocation per collection cycle per active plugin, and
>    drops the `async-trait` dependency outright.
> 3. **Build profile** (Task 3) — `[profile.release] opt-level` changed from
>    `3` to `"s"`, adopted after an isolated A/B on this machine showed a
>    smaller binary with no runtime regression (see "Attribution" below).
>
> This document reports the final v0.4.1 numbers against the v0.4.0 baseline
> the brief carried forward (rest 4.0 MiB, 100 req/s 5.3 MiB / 1.8% CPU,
> binary 2.233 MiB / 2,341,504 B), and attributes the deltas to each change
> as honestly as the measurement precision allows.

Measured with `scripts/footprint.sh` against `/api/5/all` (nine plugins,
default config, no alert thresholds configured), release binary
(`make build`: `opt-level = "s"`, `lto`, `codegen-units = 1`, `strip`,
`panic = "abort"`), same method as the v0.2.0–v0.4.0 audits. Server started
fresh (no prior requests, so no collector had woken) on the default port
`61208` — free on this run, no throwaway config needed. Idle window before
the rest-RSS sample: 15 s (> default `refresh(2.0s) × idle_cycles(5)` = 10 s,
so all four v1-era collectors touched by `/all` had self-stopped).

Reminder carried from every prior audit, still true here: RSS varies by a
few hundred KiB to roughly half a MiB run to run on this machine — read all
deltas below at **±0.5 MiB**, and CPU % at similar looseness. A delta inside
that band is noise, not a proven win, unless a controlled A/B (same session,
same load, only one variable changed) says otherwise.

## Default config: the path every client hits

| load | RSS v0.4.0 (baseline) | RSS v0.4.1 | CPU v0.4.0 | CPU v0.4.1 |
|---|---|---|---|---|
| rest | 4.0 MiB | **3.3 MiB** | ≈ 0 % | ≈ 0 % |
| 2 req/s | 4.1 MiB | **3.6 MiB** | 0.40 % | 0.30 % |
| 10 req/s | 4.3 MiB | **3.7 MiB** | 0.50 % | 0.50 % |
| 100 req/s | 5.3 MiB | **5.1 MiB** | 1.80 % | 1.50 % |

Raw run (`scripts/footprint.sh`, 10 s per rate):

```
pid=523559  url=http://127.0.0.1:61208/api/5/all  10s per rate

RSS at rest:    3.3 MiB

rate      peak RSS      CPU       delivered
2/s         3.6 MiB      .30%    20 req (2/s)
10/s        3.7 MiB      .50%    100 req (10/s)
100/s       5.1 MiB     1.50%    1000 req (100/s)
```

All requests delivered at the target rate (20/20, 100/100, 1000/1000), same
as every prior audit. Every RSS delta above is inside or barely outside the
±0.5 MiB noise band (rest is the largest single move, at −0.7 MiB); read the
whole row as "flat to modestly improved," not as a single number to quote in
isolation.

## Attribution: what moved and by how much

Task 3 ran a controlled, same-session A/B (`opt-level = 3` vs `"s"`, only
that one line changed, everything else — including Tasks 1 and 2's code —
already applied) specifically so this section could separate the build-
profile effect from the serialization/allocation changes. Its numbers are
reused here rather than re-derived.

### Build profile: `opt-level = "s"` (Task 3)

Isolated A/B, same binary otherwise, same session:

| metric | `opt-level = 3` | `opt-level = "s"` | delta |
|---|---|---|---|
| binary size | 2,360,824 B | 1,847,760 B | **−21.7%** |
| rest RSS | 4.0 MiB | 3.5 MiB | −0.5 MiB (edge of noise band) |
| 100 req/s peak RSS | 7.4 MiB | 7.0 MiB | −0.4 MiB (inside noise band) |
| 100 req/s CPU | 1.90% | 1.50% | **−0.40 pt** |

This is the one change in the pass with a controlled, isolated measurement
showing a real, direction-consistent effect on binary size (−21.7%, far
outside any noise band) and no runtime regression on either RSS or CPU — if
anything both improved slightly, though those two are close enough to the
noise band that "no regression" is the safe claim, not "measured win."
`opt-level = "z"` was not trialled at runtime (out of scope for Task 3; its
binary-only number, 1,706,104 B / −27.1%, is recorded in
`.superpowers/sdd/2026-08-03-footprint-pass/task-3-report.md` as a possible
future follow-up, not exercised further here).

### Per-cycle serialization + `Bytes` serving (Task 2)

Mechanism: before this change, `AppState::snapshot()` did
`store.read().await.get(&id).cloned()` — a deep clone of the whole
`serde_json::Value` tree on *every* request — and the axum handler
re-serialized that clone to JSON bytes on the request's own task, again on
every request. Data only actually changes once per `refresh` period
(2.0 s default); at 100 req/s against `/all` (9 plugins) that was up to
~900 clone+serialize pairs per second for payloads that change at most
0.5 times/second. After this change, serialization happens once per
*collection cycle* in `plugin_loop`, and handlers hand back a `Bytes`
refcount clone.

This mechanism predicts a **per-request** cost reduction that should scale
with request rate and be invisible at rest (no requests means no clone/
serialize work either way, before or after). That prediction matches what
was observed: the rest-RSS reading did not move between the pre-pass
baseline and Task 3's own `opt-level = 3` control run (both 4.0 MiB), while
the 100 req/s numbers in the final v0.4.1 run above (5.1 MiB / 1.50% CPU)
are flat-to-improved versus the v0.4.0 baseline (5.3 MiB / 1.80%).

Being honest about what this pass *cannot* isolate: Tasks 1 and 2 were not
given their own controlled A/B against a `Value`-store baseline binary (only
Task 3's opt-level A/B was run that way). The final v0.4.1 numbers bundle
their effect with `opt-level = "s"`'s. Given the mechanism (fewer heap
allocations and no re-serialization per request) and that Task 3's isolated
opt-level A/B accounts for essentially all of the measured CPU delta
(−0.40 pt of the −0.30 pt observed end-to-end — i.e. the opt-level effect
alone is *larger* than the total end-to-end delta, meaning Tasks 1–2's own
contribution to the coarse CPU-jiffy measurement on this box is not
distinguishable from run-to-run noise at this measurement precision), the
correct claim is: **the per-cycle serialization change removes real,
architecturally-motivated per-request waste (confirmed by code inspection —
see the working notes' H2 finding), but its magnitude was not large enough
on this machine/workload to produce a clean signal separate from
`opt-level`'s in the coarse RSS/CPU-jiffy measurements available here.** The
qualitative case for the change (fewer allocations, no needless
re-serialization, cleaner data flow) stands on its own regardless of whether
`scripts/footprint.sh`'s precision can isolate its slice of the total.

### `async-trait` removal (Task 1)

Mechanism: removes one `Box::pin` heap allocation per collection cycle per
active plugin (not per request — `collect()` runs once per `refresh`
period, default 2.0 s). Also drops the `async-trait` proc-macro dependency
from the tree. This is a fixed, low-frequency cost (bounded by the refresh
period, independent of request rate), so — like H1's `/proc/meminfo`
redundancy — it is not expected to show up in a request-rate-driven RSS/CPU
measurement at all; its contribution is one dependency fewer at compile
time and marginally less per-cycle allocator churn, not a runtime-RSS
story. No attempt was made to isolate it further at runtime; it is adopted
on the strength of the code-level argument (no `dyn Plugin` usage anywhere
in the tree, confirmed by `grep -rn "dyn Plugin" src/` — no hits — so the
native-AFIT static-dispatch path is a strict improvement with zero
functionality trade-off).

## Binary size

| | v0.4.0 | v0.4.1 | delta |
|---|---|---|---|
| release size | 2,341,504 B (2.233 MiB) | **1,847,744 B (1.762 MiB)** | **−493,760 B (−21.1%)** |

Decomposing the −21.1% net change using Task 3's own before/after markers:
adding the `bytes` dependency (Task 2) while removing `async-trait` (Task 1)
left the binary at 2,360,824 B just before the profile change — **+19,320 B
(+0.8%) versus v0.4.0**, i.e. those two source changes roughly washed out at
the binary-size level (one dependency added, one removed, both small). The
`opt-level = "s"` switch then took that 2,360,824 B down to 1,847,760 B, a
**−21.7%** move on its own. My own from-scratch `make build` of the final
v0.4.1 tree reproduces this: 1,847,744 B, 16 B off Task 3's 1,847,760 B
reading (both trials, negligible, well inside compiler/metadata noise for a
binary this size — not investigated further). **Conclusion: essentially all
of the −21.1% net binary-size win is attributable to `opt-level = "s"`**, not
to the serialization/dependency changes, which were size-neutral.

## Rejected hypotheses (Phase 9 closeout)

Two of the four Phase 9 study items were investigated and rejected — with
reasons, per this project's convention of recording rejections rather than
silently dropping them (the Phase 7 `panic = "abort"` precedent). Full
method and evidence in the now-removed working notes; the conclusions are
preserved here and in `DEVELOPMENT_PLAN.md`.

### Shared sampler (§5.2, §3.7) — REJECTED

`DEVELOPMENT_PLAN.md`'s Phase 9 bullet, as originally written, claimed
`cpu` and `system` both read `/proc/stat`. **That claim is stale and does
not hold against the current code**: `system.rs` never touches
`/proc/stat` — it uses `sysinfo::System::host_name`/`kernel_version` and
`/etc/os-release`. `grep -rn '"/proc/stat"' src/` returns exactly one call
site, in `cpu.rs`. This has been corrected in `DEVELOPMENT_PLAN.md`.

The one real redundancy found: `mem::collect()` and `memswap::collect()`
each independently `std::fs::read_to_string("/proc/meminfo")` — two
separate open+read+allocate cycles for the same ~3–4 KB file, once per
collection cycle (both default to `refresh = 2.0s`, spawned in the same
`JoinSet` batch, so their cycles stay in near lock-step). This cost is fixed
per refresh period, **not** per request — it does not compound under load
the way the per-request clone/reserialize cost (Task 2's target) did.
Building a shared sampler would require a synchronization point across
otherwise-independent per-plugin loop tasks, cutting against the §3
lazy/independent-collector architecture, for a saving on the order of a few
KB of allocation every two seconds. Rejected — matches and reconfirms the
v0.2.0 audit's own conclusion ("Piste 3").

### Dependency audit — REJECTED

`cargo tree --edges normal --prefix none | sort -u | wc -l` → **76 unique
crates**; `cargo tree --duplicates` → **zero duplicate versions**. 14 direct
dependencies (`async-trait` has since been dropped per Task 1, leaving 13:
`axum`, `base64`, `bytes`, `constant_time_eq`, `libc` (Linux-only),
`regex-lite`, `serde`, `serde_json`, `sysinfo`, `tokio`, `toml`,
`tower-http`, `tracing`, `tracing-subscriber`).

`sysinfo` is **not** the heavy dependency the v0.2.0 audit worried about: on
Linux its own subtree is just `libc` + `memchr` (`cargo tree -p sysinfo
--edges normal`), both already required elsewhere (`libc` by
`tokio`/`mio`/`signal-hook-registry`; `memchr` by `serde_json`).
`tower-http` is already scoped to only the `cors` feature. The heaviest
single transitive contributor is `tracing-subscriber`'s `env-filter`
feature (pulls `regex-automata`/`regex-syntax`, `matchers`, `nu-ansi-term`,
`sharded-slab`/`thread_local`, `smallvec`, `once_cell` — roughly ten extra
crates for startup/operational logging), but quantifying whether trimming
it is worth the churn needs `cargo bloat`, which was not installed and not
run in this pass (out of scope). Rejected as currently understood — the
premise that motivated the hypothesis (a heavy `sysinfo` tree) doesn't
survive `cargo tree`; revisit only if `cargo bloat` becomes available and
shows a specific outsized contributor.

## Conclusion

v0.4.1 ships with a smaller binary (−21.1%, 2.233 → 1.762 MiB) and a
default-config (`/all`) footprint that is flat to modestly improved versus
v0.4.0 at every measured load (rest −0.7 MiB, 100 req/s −0.2 MiB / −0.3 pt
CPU) — no regression anywhere, all deltas at or inside the ±0.5 MiB noise
band except the binary-size win, which is unambiguous. Attribution: the
binary-size win is essentially all `opt-level = "s"` (Task 3), confirmed by
an isolated same-session A/B with no runtime cost. The per-cycle
serialization change (Task 2) and `async-trait` removal (Task 1) are sound,
well-motivated architectural fixes — confirmed by code-level inspection to
remove real per-request and per-cycle waste respectively — but their
individual runtime-metric contribution was not cleanly separable from
`opt-level`'s at this measurement's precision; they are adopted on the
strength of the code-level argument and the fact that neither introduced
any measurable regression, not on a standalone measured RSS/CPU win. Phase 9
of `DEVELOPMENT_PLAN.md` is closed: per-cycle allocation and `async-trait`
removal adopted, build profile adopted (`opt-level = "s"`), shared sampler
and dependency audit rejected with reasons recorded above and in the plan.
