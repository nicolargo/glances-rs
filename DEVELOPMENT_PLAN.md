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

- [ ] `config.rs`: typed TOML config (`bind`, `port`, `password`, per-plugin
      `refresh`, `idle_timeout`, CORS allow-list, trusted hosts, network
      `show`/`hide` regexes), defaults per ARCHITECTURE
      (bind `127.0.0.1`, refresh 2 s, idle ≈ 5 cycles — §3).
- [ ] Config discovery per the order frozen in Phase 1.
- [ ] `server.rs`: axum `Router` construction and startup.
- [ ] **§7.1 startup check** — the four-case bind/password grid; non-loopback
      without password is a **hard startup error**. This is the single most
      important security line; it lands before any route does.
- [ ] `/status` and `/healthz` in a **separate sub-router**, outside all
      middleware (§6.4) — they must never trigger wake-up nor require auth.
- [ ] `tracing` initialization (`RUST_LOG`).

**Tests:** config parsing (defaults, overrides, bad TOML), all four §7.1
cases (the refusal case asserted as an error), probes respond 200.

**Exit criteria:** binary starts, refuses non-loopback-without-password,
probes green.

---

## Phase 3 — Collection engine + first plugin (`mem`)

**Goal:** the lazy-with-wake-up state machine (§3, §5), proven end-to-end
with the simplest plugin.

- [ ] `plugins/mod.rs`: `PluginId` enum (`&str` parsing → `404` semantics)
      and the `Plugin` trait exactly as §5.3 (`type State`, `collect(&mut
      State) -> Value`).
- [ ] `state.rs`: `AppState` with the **three distinct primitives** of §5.1 —
      Tokio `RwLock` store, per-plugin `AtomicI64` last-request,
      `Mutex` collector registry. Do not collapse them.
- [ ] `collector.rs`:
  - `plugin_loop` — owns the inter-cycle state as a local (§5.4, lock-free),
    publishes to the store, checks `last_request` against `idle_timeout`,
    stops via `CancellationToken`. **The store is retained on stop** (§3.2).
  - `ensure_plugin` — `Idle -> Active` transition under the registry mutex;
    the triggering request **waits for the first published cycle**, bounded
    by a guard timeout → `503` (§3.1, §6.2).
- [ ] `plugins/mem.rs` — instantaneous, `State = ()`, payload per the frozen
      schema.
- [ ] `api/mod.rs`: `GET /api/5/:plugin` (single dynamic route, §6.1) and
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

- [ ] `plugins/load.rs` — instantaneous; degraded Windows behaviour as
      decided in Phase 1.
- [ ] `plugins/cpu.rs` — first rate plugin:
  - Self-bootstrap warm-up (sample, ~200 ms sleep, sample — §5.5), delay
    respecting the `sysinfo` minimum verified in Phase 1.
  - The three §5.4 safeguards: `saturating_sub`, skip-on-missing-previous,
    measured `Instant` elapsed (never the nominal refresh).
- [ ] `plugins/network.rs` — first collection plugin (§8.1):
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

## Phase 5 — `/api/5/all`

**Goal:** the aggregate route and its concurrency/partial-failure policy.

- [ ] `GET /api/5/all`: wake all plugins **concurrently** (`join_all`) so
      latency is the slowest plugin, not the sum (§5.2).
- [ ] Partial-failure policy per §6.3: one plugin timing out → `200` with
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

- [ ] Basic auth middleware on the `/api/5/` sub-router only:
  - `base64` decode, comparison via **`constant_time_eq`**.
  - `401` with `WWW-Authenticate: Basic realm="..."`.
  - No-password ⇒ allow, **with the code comment** stating this is safe only
    because the §7.1 startup check proved loopback (§7.2).
- [ ] CORS: explicit allow-list from config, **empty by default**, never
      wildcard (§7.3) — `tower-http` `CorsLayer`.
- [ ] Trusted-`Host` middleware: default `localhost` + `127.0.0.1`,
      extendable by config (§7.4).
- [ ] Documentation (README): non-loopback exposure **must** sit behind a
      TLS reverse proxy — Basic is base64, not encryption (§7.5).

**Tests:** 401 without/with-wrong credentials, 200 with correct ones;
probes reachable without auth even when a password is set; spoofed `Host`
rejected; CORS header absent by default, present for an allow-listed origin.

**Exit criteria:** the §7.1–7.4 grid fully covered by integration tests.

---

## Phase 7 — Footprint validation, hardening, release

**Goal:** prove the project's reason to exist, then ship.

- [ ] **Footprint measurement** (Linux, release build): RSS and CPU at idle
      (after idle-timeout) and under polling load; same scenario against
      Glances v5 server mode. Numbers recorded in the README.
- [ ] Confirm or drop `panic = "abort"` (§10) based on how the supervised
      deployment story looks; record the decision in ARCHITECTURE.md.
- [ ] Light load sanity check (e.g. `oha`/`wrk` on `/api/5/all`) — looking
      for lock contention or store-writer starvation, not raw throughput.
- [ ] `clippy` pedantic pass; `cargo audit` / `cargo deny` in CI.
- [ ] Release workflow: tagged builds producing single static-ish binaries
      for Linux x86_64/aarch64 (musl if practical), macOS, Windows (§1).
- [ ] README completed: install, config reference, API summary +
      divergences (`503` vs `200 null`), TLS-proxy requirement, measured
      footprint numbers.

**Exit criteria:** v0.1.0 tag, binaries attached, README shows measured
footprint vs Glances.

---

## Out of scope (deferred, per ARCHITECTURE.md)

Tracked for later iterations, deliberately **not** in this plan:
`fs` plugin and the next collection iteration (§8); `/api/5/<plugin>/info`,
`/api/5/alert`, `/api/5/config` (§6.1); JWT/Bearer auth (§7.2); in-binary
TLS (§7.5); shared sampler for sibling plugins (§5.2) — only if Phase 7
profiling shows redundant `sysinfo` refreshes actually matter; alerting and
per-item `_levels` cleanup (§8.1).

## Open questions → where they get answered

| Open question (§10)                       | Resolved in |
|-------------------------------------------|-------------|
| `sysinfo` minimum CPU-refresh delay       | Phase 1     |
| Exact JSON payload shapes (Glances contract) | Phase 1  |
| Config file location / discovery order    | Phase 1     |
| `/all` partial-failure policy             | Phase 5     |
| `panic = "abort"`                         | Phase 7     |
