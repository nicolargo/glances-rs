# glances-rs — Development Plan

> **Purpose.** This plan turns [ARCHITECTURE.md](ARCHITECTURE.md) into an
> ordered sequence of implementation phases. Each phase ends in a state that
> compiles, passes its tests, and can be committed — no phase leaves the tree
> broken. Section references (`§n`) point to ARCHITECTURE.md.
>
> **Status:** pre-implementation companion to ARCHITECTURE.md.

---

## Guiding principles for the ordering

1. **De-risk first.** The only real unknown is the `sysinfo` behaviour
   (CPU warm-up delay, counter semantics across platforms — §4). It is
   validated by a throwaway spike *before* any architecture code depends
   on it.
2. **Build the engine with the simplest plugin.** The wake-up state machine
   (§3, §5) is the heart of the project. It is developed and tested with
   `mem` (`State = ()`) so that engine bugs and plugin bugs never mix.
3. **Security is a phase, not an afterthought** — but it comes *after* the
   API exists, because every security mechanism (§7) is middleware over
   routes that must already work.
4. **Footprint is measured, not assumed.** The project's reason to exist is
   the footprint; the final phase measures it against Glances and the
   numbers go in the README.

---

## Phase 0 — Repository bootstrap

**Goal:** a compiling, CI-checked skeleton.

- [x] `cargo init` — single binary crate, `lib.rs` + tiny `main.rs` (§9).
- [x] `Cargo.toml`: dependencies from the §4 stack table
      (tokio, axum, tower-http, sysinfo, serde/serde_json, constant_time_eq,
      base64, toml, async-trait, tracing/tracing-subscriber);
      `[profile.release]` block exactly as §9 (including `panic = "abort"`,
      revisited in Phase 7).
- [x] Commit `Cargo.lock` (binary crate).
- [x] Module skeleton matching the §9 tree (`config`, `server`, `state`,
      `collector`, `api/`, `plugins/`) — empty modules, `run()` returns
      immediately.
- [x] CI (GitHub Actions): `cargo fmt --check`, `cargo clippy -- -D warnings`,
      `cargo test`, release build, on the three targets
      (Linux primary, macOS, Windows — §1).
- [x] Minimal `README.md` (one paragraph + link to ARCHITECTURE.md).

**Exit criteria:** `cargo build` and CI green on all three platforms.

---

## Phase 1 — `sysinfo` spike & API contract freeze

**Goal:** close the open questions of §10 that block design, with a
throwaway prototype (a `examples/spike.rs` or a temporary bin — deleted at
the end of the phase, conclusions recorded).

- [x] **Verify `sysinfo`'s minimum CPU-refresh interval** on Linux
      (`MINIMUM_CPU_UPDATE_INTERVAL`) and confirm the ~200 ms warm-up
      assumption of §5.5. Record the chosen warm-up constant.
- [x] Verify network counters are cumulative `u64` on all three platforms
      and observe interface appearance/disappearance behaviour.
- [x] Check `load` availability per platform (expected: absent/degraded on
      Windows — §8) and decide the degraded payload shape.
- [x] **Freeze the JSON payload shape of each v1 plugin** (`mem`, `cpu`,
      `load`, `network`) against the Glances v5 REST contract (§1 layer 1).
      Record the four schemas in `docs/api.md`, including the documented
      divergence: `503` instead of `200 null` (§6.2).
- [x] Decide the config discovery order (§10). Proposal to validate:
      CLI `--config` flag → `GLANCES_RS_CONFIG` env var →
      `./glances-rs.toml` → `$XDG_CONFIG_HOME/glances-rs/config.toml` →
      `/etc/glances-rs/config.toml`.

**Exit criteria:** `docs/api.md` exists with the four payload schemas;
warm-up constant and config discovery order recorded; spike code removed.

---

## Phase 2 — Config, server shell, probes

**Goal:** a server that starts, enforces the §7.1 posture, and answers
probes — no plugins yet.

- [x] `config.rs`: typed TOML config (`bind`, `port`, `password`, per-plugin
      `refresh`, `idle_timeout`, CORS allow-list, trusted hosts, network
      `show`/`hide` regexes), defaults per ARCHITECTURE
      (bind `127.0.0.1`, refresh 2 s, idle ≈ 5 cycles — §3).
- [x] Config discovery per the order frozen in Phase 1.
- [x] `server.rs`: axum `Router` construction and startup.
- [x] **§7.1 startup check** — the four-case bind/password grid; non-loopback
      without password is a **hard startup error**. This is the single most
      important security line; it lands before any route does.
- [x] `/status` and `/healthz` in a **separate sub-router**, outside all
      middleware (§6.4) — they must never trigger wake-up nor require auth.
- [x] `tracing` initialization (`RUST_LOG`).

**Tests:** config parsing (defaults, overrides, bad TOML), all four §7.1
cases (the refusal case asserted as an error), probes respond 200.

**Exit criteria:** binary starts, refuses non-loopback-without-password,
probes green.

---

## Phase 3 — Collection engine + first plugin (`mem`)

**Goal:** the lazy-with-wake-up state machine (§3, §5), proven end-to-end
with the simplest plugin.

- [x] `plugins/mod.rs`: `PluginId` enum (`&str` parsing → `404` semantics)
      and the `Plugin` trait exactly as §5.3 (`type State`, `collect(&mut
      State) -> Value`).
- [x] `state.rs`: `AppState` with the **three distinct primitives** of §5.1 —
      Tokio `RwLock` store, per-plugin `AtomicI64` last-request,
      `Mutex` collector registry. Do not collapse them.
- [x] `collector.rs`:
  - `plugin_loop` — owns the inter-cycle state as a local (§5.4, lock-free),
    publishes to the store, checks `last_request` against `idle_timeout`,
    stops via `CancellationToken`. **The store is retained on stop** (§3.2).
  - `ensure_plugin` — `Idle -> Active` transition under the registry mutex;
    the triggering request **waits for the first published cycle**, bounded
    by a guard timeout → `503` (§3.1, §6.2).
- [x] `plugins/mem.rs` — instantaneous, `State = ()`, payload per the frozen
      schema.
- [x] `api/mod.rs`: `GET /api/5/:plugin` (single dynamic route, §6.1) and
      `GET /api/5/pluginslist`.

**Tests:**
- Unit: `mem::collect` shape; `PluginId` parsing.
- Integration (the engine's contract): first request blocks until data and
  never returns null/empty; second request is served from the store;
  collector stops after `idle_timeout` with no request; store still
  serves the last snapshot after stop; re-wake works; guard timeout → `503`;
  unknown plugin → `404`.

**Exit criteria:** `curl /api/5/mem` returns real data on a cold server;
the process is observably idle (no collection task) after the timeout.

---

## Phase 4 — Rate & collection plugins (`load`, `cpu`, `network`)

**Goal:** the three remaining v1 plugins, in increasing difficulty. The
engine does not change — that's the test of §5.5's claim that warm-up
knowledge stays inside the plugin.

- [x] `plugins/load.rs` — instantaneous; degraded Windows behaviour as
      decided in Phase 1.
- [x] `plugins/cpu.rs` — first rate plugin:
  - Self-bootstrap warm-up (sample, ~200 ms sleep, sample — §5.5), delay
    respecting the `sysinfo` minimum verified in Phase 1.
  - The three §5.4 safeguards: `saturating_sub`, skip-on-missing-previous,
    measured `Instant` elapsed (never the nominal refresh).
- [x] `plugins/network.rs` — first collection plugin (§8.1):
  - `HashMap` keyed by interface name (primary key).
  - `show`/`hide` regex filtering **inside `collect()`, before** rate
    computation.
  - Disappearing interfaces dropped immediately; **`state.previous` stores
    only the current sample, never a merge** — with the mandated code
    comment explaining the leak this prevents.
  - Appearing interfaces skipped for one cycle (`?` on previous lookup).

**Tests:** rate plugins unit-tested by feeding two synthetic samples
(stateless by design — §5.4): nominal rate, counter rollback → 0, appearing
item skipped, disappearing item absent **and absent from `previous`**,
show/hide filtering. Integration: cold `curl /api/5/cpu` returns a real
non-empty rate (warm-up promise).

**Exit criteria:** four plugins live; rate values plausible against
`top`/`iftop` on Linux.

---

## Phase 4.1 — Full field parity with Glances v5 (Linux)

**Goal:** payload shapes identical to Glances v5, field-for-field. Added
after a real comparison showed the v1 field subset diverged from the
Glances contract (ARCHITECTURE.md §1 classes payload shape as "must
respect"). The earlier subset was scoped to `sysinfo`'s public API; the
data is actually available from `/proc` and `/sys` on Linux.

- [x] `plugins/linux.rs`: pure parsers for `/proc/stat`, `/proc/meminfo`,
      `/sys/class/net`, unit-tested against captured samples.
- [x] `cpu`: full breakdown (per-category percentages + ctx_switches /
      interrupts / soft_interrupts rates, syscalls = 0) from `/proc/stat`.
- [x] `mem`: `active`/`inactive`/`buffers`/`cached` from `/proc/meminfo`,
      psutil formulas (`cached = Cached + SReclaimable`, `used = total -
      free - cached - buffers`).
- [x] `network`: `alias` (config), `is_up` + `speed` (`/sys/class/net`).
- [x] macOS/Windows degrade to the portable `sysinfo` subset.
- [x] `_levels` (alert metadata) stays deferred with alerting (§6.1, §8.1)
      — the one remaining structural difference. Documented in docs/api.md.

---

## Phase 5 — `/api/5/all`

**Goal:** the aggregate route and its concurrency/partial-failure policy.

- [x] `GET /api/5/all`: wake all plugins **concurrently** (`join_all`) so
      latency is the slowest plugin, not the sum (§5.2).
- [x] Partial-failure policy per §6.3: one plugin timing out → `200` with
      that plugin absent. **Confirm this choice now** (open question §10);
      if confirmed, document it in `docs/api.md`; if reversed, `503`.

**Tests:** cold `/all` returns all four plugins; latency ≈ slowest warm-up;
with one plugin forced to time out (test plugin or injected guard timeout),
response is `200` with the others present.

**Exit criteria:** cold-start `/all` under ~1 s with all four payloads.

---

## Phase 6 — Security layer (§7)

**Goal:** the full §7 posture on the `/api/5/` sub-router. Probes remain
untouched by construction (§6.4).

- [x] Basic auth middleware on the `/api/5/` sub-router only:
  - `base64` decode, comparison via **`constant_time_eq`**.
  - `401` with `WWW-Authenticate: Basic realm="..."`.
  - No-password ⇒ allow, **with the code comment** stating this is safe only
    because the §7.1 startup check proved loopback (§7.2).
- [x] CORS: explicit allow-list from config, **empty by default**, never
      wildcard (§7.3) — `tower-http` `CorsLayer`.
- [x] Trusted-`Host` middleware: default `localhost` + `127.0.0.1`,
      extendable by config (§7.4).
- [x] Documentation (README): non-loopback exposure **must** sit behind a
      TLS reverse proxy — Basic is base64, not encryption (§7.5).
- [x] **Password via environment variable** (`[server].password_env` names
      the variable; the secret never lives in the config file). Resolved at
      load time; a missing/empty variable, or both `password` and
      `password_env` set, is a hard startup error. No dotenv dependency: the
      `.env` file is supplied by systemd `EnvironmentFile`, Docker, or the
      shell. A didactic "Securing the server" walkthrough was added to the
      README.

**Tests:** 401 without/with-wrong credentials, 200 with correct ones;
probes reachable without auth even when a password is set; spoofed `Host`
rejected; CORS header absent by default, present for an allow-listed origin;
`password_env` resolves / errors on missing-empty-or-both.

**Exit criteria:** the §7.1–7.4 grid fully covered by integration tests.

---

## Phase 7 — Footprint validation, hardening, release

**Goal:** prove the project's reason to exist, then ship.

- [x] **Footprint measurement** (`scripts/footprint.sh`, `/proc`-based,
      rate-controlled load, Glances scoped to the same four plugins):
      glances-rs ≈ 4.5 MiB RSS (≈ 5.6 at 100 req/s), 0.25–2.25 % CPU vs
      Glances 4.5.5 ≈ 69 MiB, 0.5–9 % CPU at 2/10/100 req/s on the same
      machine — ~15× less memory like-for-like. Recorded in the README.
- [x] Confirm or drop `panic = "abort"` → **kept**; rationale (supervised
      deployment, footprint, the stale-registry pitfall of unwinding)
      recorded in ARCHITECTURE.md §9/§10.
- [x] Light load sanity check: the footprint script drives concurrent
      polling on `/api/5/all`; no lock contention or store-writer
      starvation observed — RSS stays flat and CPU near zero.
- [x] `clippy` pedantic pass: run and reviewed. The default `clippy -D
      warnings` stays the CI gate; pedantic's dominant lints here are
      intentional domain casts (`u64`→`f64` for rates/percentages) and
      doc-completeness lints, not adopted to avoid `#[allow]` churn.
      `cargo audit` added as a CI job (`rustsec/audit-check`).
- [x] Release workflow (`.github/workflows/release.yml`): tag-triggered
      builds for Linux x86_64/aarch64 (musl, static), macOS x86_64/aarch64,
      Windows, attached to the release via `upload-rust-binary-action`.
- [x] README completed: quick start, configuration, the didactic "Securing
      the server" guide, API summary + divergences, TLS-proxy requirement,
      measured footprint.

**Exit criteria:** v0.1.0 tag, binaries attached, README shows measured
footprint vs Glances. (Tagging `v0.1.0` is the operator's call — the
release workflow fires on the tag push.)

---

# v0.2.0 — Plugin coverage & footprint pass

> Everything above shipped as **v0.1.0** (engine, the four core plugins,
> security, measured footprint). v0.2.0 has one theme: **widen the plugin
> coverage** with five new plugins, then **revisit the footprint** now that
> there is more collection code to pay for. Alerting (`_levels` +
> `/api/5/alert`), `/api/5/<plugin>/info` and `sensors` are deliberately held
> for v0.3.0 so each release stays small and its footprint stays measurable.
>
> No new architecture: every plugin below is a fresh `Plugin` implementation
> over the existing lazy-wake-up engine (§3, §5) — the engine is *not* touched.
> Plugins are ordered by increasing difficulty (instantaneous → rate →
> collection → collection+rate), the same ramp Phase 3→4 used.

## Phase 8 — New plugins

Each plugin repeats the same checklist, so it is stated once here and not
re-listed per plugin:

- Add the variant to `PluginId` (`as_str`/`parse`/`ALL`) — `parse` keeps the
  `404` semantics (§6.1).
- **Linux first-class, then degrade.** Pure parsers in `plugins/linux.rs`,
  unit-tested against captured `/proc`·`/sys` samples (the Phase 4.1 pattern);
  macOS/Windows fall back to the portable `sysinfo` subset.
- **Freeze the payload** in `docs/api.md` §5, field-for-field against Glances
  v5. `_levels` stays out (deferred with alerting — §6.1, §8.1).
- Wire it into `/api/5/all` (it joins the concurrent wake automatically).

- [x] **`system`** — instantaneous, `State = ()`. Hostname, OS name/version,
      platform, Linux distro (`/etc/os-release`), human-readable name. The
      simplest of the five; lands first to re-establish the rhythm.
- [x] **`uptime`** — instantaneous, `State = ()`. Seconds since boot
      (`sysinfo::System::uptime`). Payload **frozen as a bare JSON string**
      (`str(timedelta)` shape) to match the Glances v5 REST contract —
      `{"seconds": N}` is the Glances *export* shape, not the REST one
      (docs/api.md §5.6).
- [x] **`memswap`** — **part-rate** plugin. `total`/`used`/`free`/`percent` are
      instantaneous; `sin`/`sout` are cumulative byte counters (`/proc/vmstat`
      `pswpin`/`pswpout` × `sysconf(_SC_PAGESIZE)`). **Design call vs. the plan:
      no §5.5 warm-up.** Glances emits `sin`/`sout` *raw* (it does not decorate
      them as a server-side per-second rate), so there is nothing to bootstrap
      and warming up would only add cold-start latency — `State` keeps just the
      previous `Instant` for `time_since_update` (`0.0` on the first cycle, as
      Glances reports). The client derives the rate from two samples. Added a
      Linux-only `libc` dep (already transitive) for the page size; degrades to
      the `sysinfo` swap subset without `sin`/`sout` off Linux (docs/api.md
      §5.7).
- [x] **`fs`** — **collection**, instantaneous (disk *space*, no rate). One
      item per mount point, keyed by `mnt_point`, from `sysinfo::Disks`
      (cross-platform, like `network`). Extracted the `show`/`hide` regex into
      a shared `plugins::filter::KeyFilter` (now used by `network` and `fs`,
      and `diskio` next). No inter-cycle state ⇒ no leak risk. `percent =
      used/size` (a slight approximation of psutil's root-reserve-aware
      percent); `key` and `options` omitted vs. Glances (docs/api.md §5.8).
- [x] **`diskio`** — **collection + rate**, the hardest, landed last. One item
      per disk (`/proc/diskstats`), cumulative `read`/`write` bytes & counts →
      rates, reusing `network`'s `step` shape and `KeyFilter`. Combines all the
      traps: §5.4 (`saturating_sub`, skip a disk absent from the previous
      sample via `?`, measured `Instant`), §5.5 (warm-up), and the §8.1
      anti-leak rule (`state.previous` = current sample only, with the comment).
      **Linux-only** — `sysinfo` has no per-disk I/O, so other platforms return
      an empty array; `read_time`/`write_time`/latency omitted (docs/api.md
      §5.9).

**Tests:** per plugin — Linux parsers against captured samples; rate plugins
(`memswap`, `diskio`) fed two synthetic samples (nominal rate, counter
rollback → 0, appearing item skipped, disappearing item absent **and absent
from `previous`**); `fs`/`diskio` `show`/`hide` filtering; cold
`curl /api/5/<plugin>` returns real non-empty data (the warm-up promise for
the rate ones). Integration: cold `/all` now returns all nine plugins.

**Exit criteria:** nine plugins live (`cpu`, `load`, `mem`, `network`,
`system`, `uptime`, `memswap`, `fs`, `diskio`); values plausible against
`free`/`df`/`iostat` on Linux; `docs/api.md` §5 covers all nine.

### Phase 8.1 — Align the output to the Glances v5 REST API

A review against a live Glances v5 (`develop-v5`) server showed the payloads
followed the **v4** conventions, not v5. Corrected across every plugin
(`docs/api.md` §4 rewritten):

- [x] **Response envelope** — shared `plugins::envelope()`: object plugins gain
      top-level `time_since_update` + `_levels` (`{}` until alerting); collection
      plugins nest their array under `data`. `plugins::Clock` gives instantaneous
      plugins a `time_since_update`.
- [x] **Plain per-second rates** — dropped the v4 `X`/`X_gauge`/`X_rate_per_sec`
      triple; a rate field is now a single per-second value (network `bytes_*`,
      diskio `read_*`/`write_*`, cpu `ctx_switches`/…). **memswap `sin`/`sout`
      are now rates** (warm-up added), not cumulative.
- [x] **uptime** = `{"seconds": <int>}` (the v5 shape; was the v4 timedelta
      string).
- [x] **Default `hide` lists** (`filter::hide_or_default`, replaced by an
      explicit `hide`): network `docker.*,lo`; fs `/boot.*,.*/snap.*`; diskio
      `loop.*,/dev/loop.*`.
- [x] **Conditional `alias`** on collection items — present only when configured
      for that item (was always `null`).

---

## Phase 9 — Footprint optimization study

**Goal:** the footprint is the project's *raison d'être*; five new plugins add
collection code, allocations and `sysinfo`/`/proc` reads, so re-measure and
hunt for regressions. This phase is a **study + measurement + recommendation**
(a spike), not a blanket rewrite — each idea is adopted only if the numbers
justify it, and only if it does not compromise correctness or the §3 lazy
contract.

- [x] **Re-baseline** with `scripts/footprint.sh`: RSS/CPU at rest and under
      2/10/100 req/s on `/all`, now with nine plugins (vs Glances 4.5.5, same
      scope). README footprint table refreshed; full study in
      `docs/footprint-audit-v0.2.0.md`.
- [x] **Async runtime → `current_thread`** (the headline win). The default
      multi-thread runtime spawned one worker per core (16 idle threads) for a
      ~2 % CPU workload. Switched to `current_thread` and dropped tokio's
      `rt-multi-thread` feature: **−18 % RSS at rest, −47 % under 100 req/s**
      (12.1 → 5.5 MiB), binary 2.2 → 2.1 MiB, suite green. Recorded in
      ARCHITECTURE.md §9.
- [ ] **Shared sampler (§5.2)** — now that several plugins read the same
      source (`/proc/stat` for `cpu`+`system`, `/proc/meminfo`·`vmstat` for
      `mem`+`memswap`), measure whether redundant reads/refreshes actually
      cost anything under concurrent `/all`. Implement the §3.7 shared sampler
      **only if** profiling shows it matters — it must not touch the wake-up
      architecture.
- [ ] **Per-cycle allocation** — profile the hot path (the loop publishing a
      fresh `serde_json::Value` every cycle, the `/proc` read buffers).
      Evaluate reusing read buffers across cycles and/or serializing a typed
      public struct directly instead of building a `Value`. Adopt only with a
      measured win.
- [ ] **Binary size / build profile** — revisit `opt-level = 3` vs `"z"/"s"`,
      and whether a lighter allocator helps RSS, against the single-binary and
      footprint goals (§9). Measure both axes (size *and* runtime RSS); keep
      whatever the numbers favour.
- [ ] **Dependency audit** — review the tree for weight that can be dropped or
      feature-gated without losing functionality.

**Tests:** no behavioural change expected — the full suite stays green. Any
adopted optimization keeps the §5.4 safeguards and the §8.1 anti-leak rule
intact; the footprint script is the acceptance gate.

**Exit criteria:** README footprint table refreshed for v0.2.0; each
optimization either adopted with a recorded measured gain or explicitly
rejected with the reason (the Phase 7 `panic = "abort"` precedent — decisions
are recorded, not silently dropped).

**Exit criteria (release):** v0.2.0 tag, binaries attached, `docs/api.md` and
README reflect the nine plugins and the refreshed footprint.

---

# v0.3.0 — Alerting

> Everything above shipped as **v0.2.0** (nine plugins, `current_thread`
> runtime win). v0.3.0 has one theme: **alerting** — close the last
> payload-parity gap with Glances v5 by populating the per-field `_levels`
> decoration and serving `/api/5/alert`, reproducing the behaviour of the
> reference implementation
> [`alerts_v5.py`](https://github.com/nicolargo/glances/blob/develop-v5/glances/alerts_v5.py)
> within the constraints of the lazy-collection engine (§3) and the
> footprint mandate. Full design:
> `docs/superpowers/specs/2026-06-14-alerting-design.md`.
>
> No change to the collection engine itself: alerting is one new shared
> component, `Alerts` (ARCHITECTURE.md §5.6), fed by `plugin_loop` between
> `collect()` and `publish()` — plugins are untouched and know nothing about
> thresholds.

## Phase 10 — Alerting engine

- [x] `src/alerts.rs`: the `Alerts` component — a bounded event journal
      (`VecDeque`, `[alerts].history_size`, default 200) plus a hysteresis
      map (`(plugin, item key, field) -> AlertState`), both behind one
      `std::sync::Mutex` (ARCHITECTURE.md §5.6). Lives in `AppState`, not a
      plugin's `State`, because it must survive an idle→wake cycle (§3.2).
- [x] Two-level threshold config (`config.rs`): `[plugins.<p>].thresholds`
      (global, per field) and `thresholds_by_item` (per item primary key),
      merged **per limit key** — an item override replaces only the limits
      it declares, inheriting the rest from global. `[plugins.<p>]
      .min_duration_seconds` (per-plugin hysteresis override, uniform
      across all of that plugin's items) and the global `[alerts]` section
      (`history_size`, `min_duration_seconds`; defaults 200 / 5.0). Startup
      validation fails closed: finite, ordered (`careful <= warning <=
      critical`) limits for both declared and merged blocks;
      `history_size >= 1`; `min_duration_seconds >= 0`.
- [x] Per-field `_levels`, rebuilt from the current sample every cycle —
      always top-level in the envelope for both object and collection
      plugins (scalar keyed by field name, collection keyed by
      `str(primary_key)` then field name); each leaf
      `{ "level", "prominent" }`. Emitted only for a static, per-plugin
      allow-list of alertable fields that also has a resolved threshold —
      **no built-in defaults** ship (config-only; the deliberate
      conservatism divergence from Glances, whose schema ships defaults).
- [x] `min_duration` hysteresis (`_reconcile` parity): an observed level
      must persist for the effective window before a transition commits and
      is journaled to `/api/5/alert`; `_levels` itself stays the raw,
      instantaneous level, never debounced. Idle-gap reset: a stale pending
      window from before an idle→wake gap cannot insta-commit on the first
      post-wake sample; the last committed level is preserved (no spurious
      `is_initial` re-fire).
- [x] `watch_direction` (`High`/`Low`): the level ladder is direction-aware.
      Every v0.3.0 alertable field is `High`; `Low` is engine-complete and
      unit-tested so the day a low-direction field (e.g. free disk space)
      is added, the contract is already correct.
- [x] `normalize_by`: a field can compare `value / divisor` against a
      ratio-`[0, 1]` threshold instead of the raw value; a missing, zero, or
      non-finite divisor **skips** the field for that cycle (no `_levels`
      entry, no event) — matching Glances' "unknown link speed" semantics.
- [x] `GET /api/5/alert`: the event journal as a JSON array, most-recent
      last, `[]` when empty. Sits behind the same auth/CORS/trusted-host
      stack as the other `/api/5/*` routes, but is read-only — it never
      wakes or waits on a collector, and never returns `503`.
- [x] `network`: new payload field `bytes_speed_rate_per_sec` (per-direction
      link capacity in bytes/s — `speed_mbits × 1e6 / 8 / 2`, `0` when
      unknown, Linux only, from `/sys/class/net/<iface>/speed`) — a
      payload-parity addition needed as the `normalize_by` divisor for
      `bytes_recv`/`bytes_sent`, collected unconditionally regardless of
      whether any threshold is configured (docs/api.md §5.4).
- [x] The §8.1 `_levels` cleanup note is resolved: rebuilt fresh from the
      current sample every cycle, `_levels` can never carry a stale item;
      the internal hysteresis map is pruned for every collection item
      absent from the current sample, on every cycle — deliberately better
      than the Glances v5 reference, which never garbage-collects that
      state (ARCHITECTURE.md §8.1).

**Tests:** unit (`alerts.rs`) — level computation per limit subset for both
directions, the `normalize_by` transform and its skip conditions, hysteresis
commit/debounce/return-to-ok/`is_initial`, idle-gap reset, history ring
eviction, stale-key pruning for collection items; config — threshold
parsing (scalar and `thresholds_by_item`), two-level per-limit merge,
ordering validation on declared and merged blocks, `[alerts]` defaults,
per-plugin `min_duration_seconds`; integration — a configured breaching
value populates `_levels` and, after `min_duration`, produces an
`/api/5/alert` event; an unconfigured plugin stays `_levels: {}` and
`/api/5/alert` returns `[]`; `/api/5/alert` never wakes a collector and is
reachable under auth.

**Exit criteria:** `docs/api.md` §8 and ARCHITECTURE.md §5.6/§8.1 document
the shipped behaviour field-for-field; `make check` green. A footprint
re-baseline against v0.2.0 (spec §9) is still pending: with the default
config (no thresholds) RSS/CPU must be indistinguishable, except for
`network`'s unconditional `bytes_speed_rate_per_sec` addition, which is
measured separately.

---

# v0.4.0 — `/info` (field schema introspection)

> Everything above shipped as **v0.3.0** (alerting). v0.4.0 has one theme:
> expose the per-plugin field schema (`fields_description`) of Glances v5 as
> `GET /api/5/<plugin>/info`, and, in doing so, unify field metadata that had
> started to live in two places (the alerting engine's alertable-field list,
> and what `/info` would otherwise need as its own table). Read-only, inert,
> config-aware; no new collection, no new dependency. Full design:
> `docs/superpowers/specs/2026-08-02-info-endpoint-design.md`.

## Phase 11 — Unified field metadata & the `/info` route

- [x] **Phase 1 (behaviour-neutral refactor).** New module
      `src/plugins/fields.rs`: `FieldInfo`/`fields(id)`, the `&'static`
      per-plugin table describing every field a plugin emits (not just
      watched ones) — description, unit, `short_name`, `primary_key`,
      `rate`, `internal`, plus the alert-only `watched`/`direction`/
      `prominent`/`normalize_by`. `Direction` moved here from `alerts.rs`;
      `alerts.rs` no longer owns a separate `AlertField` table — its
      `alert_fields(id)` becomes a filtered view over `fields(id)`. Carries
      the v0.3.0 alert attributes **verbatim**; no behavioural change. Gate:
      the full existing `alerts.rs` unit/integration suite stays green, plus
      a new guard test asserting the watched subset of `fields(id)` matches
      the pre-refactor `AlertField` set field-for-field (field, prominent,
      direction, normalize_by).
- [x] **Phase 1b (alerting parity fix).** Five alert attributes corrected to
      match the maintainer's live Glances v5 `/info` output (ARCHITECTURE.md
      §5.7): `cpu.iowait` prominent → `true`, `cpu.steal` prominent →
      `false`, `cpu.ctx_switches` prominent → `false` +
      `normalize_by="cpucore"`, `load.min5`/`load.min15` +
      `normalize_by="cpucore"`. No field added to or removed from any
      plugin's watched set. The one behavioural consequence:
      `normalize_by="cpucore"` makes `load`/`ctx_switches` level computation
      per-core (`value / cpucore` vs. threshold) wherever an operator has
      configured `cpu`/`load` thresholds — default config (no thresholds) is
      unaffected. Landed as its own isolated commit, after Phase 1 proved
      the refactor neutral; the guard test and the `normalize_by` unit tests
      were updated to the corrected attributes.
- [x] **Phase 2 (new feature).** `GET /api/5/<plugin>/info`
      (`src/api/mod.rs`): a new axum route alongside `/api/5/{plugin}`,
      under the same auth/CORS/trusted-host stack. Inert — reads only
      `fields(id)` and `config`, never the store or the collector registry;
      never wakes, never waits, never `503`s (same read-only class as
      `/api/5/alert`). Body: an object keyed by field name, table order,
      whitelist keys per `docs/api.md` §9 (`history`/`strict_thresholds` are
      never emitted — glances-rs implements neither). `default_thresholds`
      reflects the operator's configured **global** thresholds only
      (`thresholds_by_item` is not reflected), present only when configured,
      partial-aware. `404` on an unknown or disabled plugin name, matching
      the `/api/5/{plugin}` convention.

**Tests:** unit (`plugins::fields`) — non-empty tables for all nine plugins;
the watched-subset guard against the corrected Glances `/info` attributes;
`primary_key: true` on exactly the field named by `key_field(id)` for
collection plugins, none for scalar plugins; `Unit`/`Direction` string
mappings. Integration (`tests/info.rs`, via `oneshot`) — `mem/info` matches
the scalar shape (no `primary_key`, `percent` watched/prominent, no built-in
`default_thresholds`); `network/info` keyed by field name with
`interface_name.primary_key`, `bytes_recv` rate/watched/`normalize_by`;
`cpu/info` carries `internal: true` on `cpucore` and no field carries
`history`/`strict_thresholds`; `default_thresholds` absent by default,
present and partial-aware once `[plugins.mem.thresholds.percent]` is
configured; unknown plugin → `404`; `/info` never wakes a collector
(`active_collectors() == 0` after the call); the field-set invariant (every
emitted data key is documented in `/info`) for `mem` and `fs`.

**Exit criteria:** `docs/api.md` §9 and ARCHITECTURE.md §5.7 document the
shipped route field-for-field; `make check` green; version bumped to
`0.4.0`. A footprint re-baseline against v0.3.0 is pending — expected
indistinguishable under default config (`fields.rs` is `&'static`, `/info`
allocates only per call, no new dependency).

---

## Out of scope (deferred beyond v0.3.0)

Tracked for later iterations, deliberately **not** in v0.3.0:
the `sensors` plugin (§6.1, §8); `/api/5/config` (§6.1, needs the
public-view filter of §7.6); JWT/Bearer auth (§7.2); in-binary TLS (§7.5).

## Open questions → where they get answered

| Open question (§10)                       | Resolved in |
|-------------------------------------------|-------------|
| `sysinfo` minimum CPU-refresh delay       | Phase 1     |
| Exact JSON payload shapes (Glances contract) | Phase 1  |
| Config file location / discovery order    | Phase 1     |
| `/all` partial-failure policy             | Phase 5     |
| `panic = "abort"`                         | Phase 7     |
