# glances-rs — Architecture & Design Decisions

> **Purpose of this document.** It is the single source of truth for the
> design of `glances-rs`. It is written to brief a fresh Claude Code session
> before implementation, and to onboard human contributors. It records not
> just *what* was decided but *why*, so that future changes are made with the
> original trade-offs in view.
>
> **Status:** pre-implementation. No code exists yet. This document describes
> the intended v1.

---

## 1. Project context

`glances-rs` is a ground-up reimplementation of a **lightweight monitoring
server**, inspired by the `develop-v5` branch of
[Glances](https://github.com/nicolargo/glances).

Glances v5 is and remains a Python application. Its server mode has a known
weakness: a non-negligible CPU/RAM footprint when running unattended on a
server, partly because its collection scheduler runs continuously even with no
client connected (acknowledged in the Glances v5 architecture-decisions
document, section 7.1).

`glances-rs` exists to solve that specific problem: provide the same observable
REST API with the **smallest possible CPU and RAM footprint**.

This is a separate project in its own Git repository — **not** a fork or a
subdirectory of Glances.

### Hard requirements (from the project brief)

- Dedicated Git repository, independent of Glances.
- Conform to the REST API defined in the Glances v5 architecture-decisions
  document.
- Fine-grained configuration: expose only the stats the operator wants.
- Minimise CPU and RAM footprint above all else.
- Ship as a **single binary** for easy deployment.
- Cross-platform: **Linux (primary target)**, macOS, Windows.

### Reference document

The Glances v5 architecture-decisions document is a reference, **not** a
specification to follow literally. It describes Python implementation choices
(asyncio, FastAPI, uvicorn, psutil, ConfigParser). Treat it in three layers:

1. **Contract — must respect.** Anything observable by a client: REST routes,
   payload shapes, status-code semantics, the security model. Mainly section 4
   of that document.
2. **Reusable design — adapt, don't copy.** Collect/store/render separation,
   per-plugin scheduling with a configurable refresh time, alert thresholds.
   Rust will express these with types and traits instead of runtime dicts.
3. **Python-specific — ignore.** asyncio internals, `importlib` auto-discovery,
   `ConfigParser`, the TUI threading model.

---

## 2. Language choice — Rust

**Decision: Rust.**

Reasoning, in the order the trade-off was worked through:

- The project is a **performance-critical system tool**, and its entire reason
  to exist is a **smaller footprint than the Python original**. A
  garbage-collected runtime (Go, Java, C#) contradicts that goal: GC implies a
  baseline memory overhead and periodic CPU pauses. This is exactly the metric
  the project must win on, so a GC language is the wrong default here.
- The stated priority is **robustness and long-term maintainability** over
  raw development speed. This is what separates Rust from C/C++: Rust's
  ownership model turns memory-safety and data-race bugs into *compile-time*
  errors, instead of relying on developer discipline. Refactoring six months
  later is guided by the compiler.
- The realistic finalists were Rust and Go. Go would have been faster to
  develop and is excellent at single-binary cross-platform delivery — but it
  concedes the GC footprint, i.e. precisely the metric this project optimises
  for. Go was therefore rejected *for this project specifically*.
- The main argument against Rust — its learning curve — is neutralised here:
  the developer has broad cross-language experience.

---

## 3. Collection architecture — lazy with wake-up (state machine)

This is the core design decision. A monitoring server spends most of its time
**not** being queried; the question is whether it collects anyway.

Three models were considered:

- **A — Permanent collection** (the Glances v5 model). A background loop
  samples every plugin at its refresh interval, regardless of client activity.
  Rejected: it reproduces the very footprint flaw the project targets.
- **B — Pure lazy collection.** No background loop; each request reads the
  sources directly and responds. Rejected: **rate metrics are impossible**
  this way. CPU %, network throughput and disk I/O are derived from
  *cumulative counters* — they need two samples spaced in time. A single
  isolated request has no previous sample to diff against.
- **C — Lazy with wake-up (CHOSEN).** No collection while idle. The first
  request starts the collection loop; it keeps running while there is traffic;
  after a period of inactivity it stops and the process goes back to sleep.

**Decision: option C.** It is the only model that satisfies both constraints —
near-zero footprint at rest *and* correct rate metrics under load.

The server is therefore a **state machine**: each plugin is independently
`Idle` or `Active` (see section 5 for why per-plugin and not global).

### Three behavioural decisions on top of C

1. **First request waits for a full cycle.** The server must never return empty
   or `null` data. When a plugin is woken, the triggering request *waits* until
   the first collection cycle has been published, bounded by a guard timeout.
   If no cycle arrives within the timeout, the request fails with `503`.
   *(This is a deliberate divergence from Glances, which returns `200 null`
   for a known-but-unpopulated plugin — see section 6.)*
2. **The store is kept in memory while asleep.** When a plugin's collector
   stops, its last snapshot is intentionally retained. The memory cost is
   negligible (a few KB) and it means rates can be computed immediately on the
   next wake-up. Do **not** clear the store on sleep.
3. **Wake-up is per-plugin** (see section 5).

### Tuning parameters

- **Refresh time** — per plugin, configurable. Default 2 s.
- **Idle timeout** — how long without a request before a plugin's collector
  stops. Configurable; default ≈ 5 refresh cycles.

---

## 4. Stack

| Concern        | Choice                | Note                                            |
|----------------|-----------------------|-------------------------------------------------|
| Async runtime  | **Tokio**             | Dominant; provides `spawn` + `CancellationToken`.|
| HTTP framework | **axum**              | Tokio-native; `tower` middleware; `State` extractor.|
| Middleware     | **tower-http**        | CORS layer; compression/timeout available later.|
| System metrics | **sysinfo**           | psutil-equivalent; Linux/macOS/Windows.         |
| Serialization  | **serde / serde_json**| Typed structs replace Glances' runtime field dicts.|
| Password check | **constant_time_eq**  | Timing-attack-safe comparison.                  |
| Basic decoding | **base64**            | Decodes the `Authorization: Basic` header.      |
| Config format  | **toml**              | Idiomatic in Rust; deserializes into structs.   |
| Async traits   | **async-trait**       | `async fn` in the `Plugin` trait.               |
| Logging        | **tracing / tracing-subscriber** | Structured, leveled logs; `RUST_LOG` control.|

**Deliberately excluded from v1:** `jsonwebtoken` (Basic-only auth — section 7),
`schemars` (`/info` route deferred — section 6), any TLS crate (TLS delegated
to a reverse proxy — section 7).

### sysinfo caveat

`sysinfo` has its **own** two-refresh requirement for CPU usage (a minimum
delay between `refresh_cpu_usage()` calls for the percentage to be valid). The
`cpu` plugin therefore has two warm-up mechanisms layered: the project's own
(section 5) and `sysinfo`'s internal one. The project's warm-up delay must
respect `sysinfo`'s minimum. Verify on an early prototype.

---

## 5. The collection layer

### 5.1 Shared state

Three different synchronization primitives, each matched to its access pattern
— do not collapse them into one lock:

- **`RwLock<HashMap<PluginId, Value>>`** — the store. Many readers (HTTP
  handlers), one writer (the loop). Use the **Tokio** `RwLock`, not `std`'s, so
  it never blocks the async runtime.
- **`AtomicI64`** — per-plugin "last request" timestamp. Written on every
  request; must be lock-free and cheap.
- **`Mutex<HashMap<PluginId, Collector>>`** — the registry of active
  collectors. The mutex guards only the rare `Idle -> Active` transition.

### 5.2 Per-plugin wake-up

Wake-up granularity is **per plugin (strict): one plugin = one task**, with its
own `CancellationToken`, its own `last_request` timestamp, its own loop.

Trade-off accepted: plugins that read the same `sysinfo` source cause redundant
refreshes. This is tolerated in v1 for a uniform model. If profiling later
shows it matters, the remedy is a "shared sampler" (Glances doc §3.7) — sibling
plugins sharing one source read. That can be added **without** touching the
wake-up architecture.

Consequence: `/api/5/all` on a fully idle server must wake every plugin. Wake
them **concurrently** (`join_all`), so `/all` waits for the slowest plugin, not
the sum. The slowest will always be a rate plugin (~200 ms warm-up).

### 5.3 The `Plugin` trait

```rust
#[async_trait::async_trait]
trait Plugin: Send + Sync {
    /// Inter-cycle memory. `()` for an instantaneous plugin,
    /// a raw-sample type for a rate plugin.
    type State: Default + Send;

    fn id(&self) -> PluginId;
    fn refresh(&self) -> std::time::Duration;

    /// One collection cycle. Receives the previous cycle's state by
    /// `&mut`, updates it, returns the public JSON. No lock needed:
    /// `state` is exclusive by construction (see 5.4).
    async fn collect(&self, state: &mut Self::State) -> serde_json::Value;
}
```

### 5.4 Rate calculation

Rate metrics (CPU %, network throughput) are computed by diffing two samples.
The inter-cycle state lives **as a local variable inside the plugin's loop
task**, passed to `collect()` by `&mut`. Because only that one task touches it,
the compiler guarantees exclusive access with **no lock at all**. Plugins are
therefore stateless objects — trivially testable by feeding two samples.

Three mandatory safeguards in the diff logic:

- **Counter rollback** — on reboot or 32-bit counter wrap, the new counter can
  be *lower* than the old one. Naive `u64` subtraction panics (debug) or yields
  a huge bogus number (release). Use **`saturating_sub`** — clamps to 0.
- **Appearing items** — a network interface present in the current sample but
  not the previous one has no reference; skip it this cycle (it gets a rate
  next cycle). Implemented via `?` on the `Option` from the previous-sample
  lookup.
- **Real elapsed time** — divide by the *measured* interval between two
  `std::time::Instant`s, never by the configured refresh time. Real cycles are
  never exactly the nominal value. `Instant` is also monotonic — immune to
  system clock changes.

### 5.5 Warm-up (rate plugins)

A rate plugin **self-bootstraps**: on its first cycle (when previous state is
empty) it takes a sample, sleeps briefly (~200 ms), takes a second sample, and
returns a real rate immediately. The `sleep` is on the cold path only; all
subsequent cycles are normal. This keeps the "first request waits for a cycle"
promise (section 3) honest — the first response carries real rates, not an
empty list. The knowledge "I need two samples" stays entirely inside the
plugin; the loop and the wake-up machinery know nothing about it.

---

## 6. The REST API layer

### 6.1 v1 routes

| Route                  | Purpose                                  |
|------------------------|------------------------------------------|
| `GET /api/5/:plugin`   | One plugin's stats (`mem`/`cpu`/`load`/`network`). |
| `GET /api/5/all`       | All plugins at once.                     |
| `GET /api/5/pluginslist` | List of available plugin names.        |
| `GET /status`          | Liveness probe.                          |
| `GET /healthz`         | Liveness probe.                          |

A **single** dynamic route `/api/5/:plugin` handles all four plugins; the
handler validates the name (`&str -> PluginId`) and returns `404` for unknown
names.

**Deferred (not in v1):**

- `/api/5/<plugin>/info` — returns a plugin's field schema. Deferred because
  Rust's typed structs erase field metadata at compile time, so this route
  would require *recreating* a schema (hand-written, or via `schemars`).
  Revisit when a concrete consumer needs it. *(Note: `/pluginslist` is kept
  because it is cheap — just names, no metadata.)*
- `/api/5/alert` — no alerting in v1.
- `/api/5/config` — depends on the config layer; also the source of a known
  Glances CVE (see 7). Defer until it can be done safely.

### 6.2 Status-code grid (deliberate divergence from Glances)

- `200` + real data — the normal case. The store is always populated, because
  the request waited for the first cycle.
- `404` — unknown plugin.
- `503` — `ensure_plugin` exceeded the guard timeout: collection did not start.

Glances uses `200 null` for "known plugin, no data yet". `glances-rs` **never**
does this — it waits instead. `503` replaces the transient `200 null`. This
divergence must be documented in the public API docs.

### 6.3 `/all` partial-failure policy

If one plugin times out, `/all` returns `200` with the other plugins present
and the failed one simply absent — it does **not** fail the whole response. An
aggregate route should not collapse for one component. (Validate this choice;
the alternative is `503` whenever any plugin is missing.)

### 6.4 Probes are inert

`/status` and `/healthz` must **not** trigger plugin wake-up and must **not**
require auth. A probe that woke collection would keep the server permanently
awake and defeat the lazy model. Probes live in a separate sub-router, outside
the middleware stack, so no auth layer can ever cover them by accident.

---

## 7. Security

The Glances doc (§8) lists real CVEs. Each mechanism below is framed as the
flaw it closes.

### 7.1 Default posture — closed by default

**Decision: closed by default.** The server binds to `127.0.0.1` by default.
Binding to a non-loopback address is an explicit operator choice that
**requires** a configured password.

The startup check has four cases; the critical one is the last:

| Bind         | Password | Result                                    |
|--------------|----------|-------------------------------------------|
| loopback     | none     | OK — the default; unreachable externally. |
| loopback     | set      | OK — extra belt.                          |
| non-loopback | set      | OK — an assumed, protected deployment.    |
| non-loopback | none     | **Refuse to start** — hard error, not a warning. |

The hard refusal is what *makes* "closed by default" a guarantee rather than an
intention. A log warning (the Glances approach) scrolls past and is ignored; a
refusal forces a conscious decision. This is the single most important line of
the security layer.

### 7.2 Authentication — Basic only in v1

**Decision: HTTP Basic only.** JWT/Bearer is deferred (removes the
`jsonwebtoken` dependency and the entire `alg: none` validation pitfall).

- Auth is a `tower-http` middleware on the `/api/5/` sub-router — one control
  point, not per-handler.
- Password comparison **must** use `constant_time_eq` — a naive `==` leaks the
  secret byte-by-byte via a timing side channel.
- `401` responses must include the `WWW-Authenticate: Basic realm="..."`
  header.
- When no password is configured, the auth middleware allows the request —
  this is safe *only* because the 7.1 startup check has already proven the
  bind is loopback. Comment this invariant in the code; it looks like a hole
  otherwise.

### 7.3 CORS — allow-list, never wildcard

`Access-Control-Allow-Origin: *` combined with authentication is a Glances CVE.
Use a configurable **allow-list** of explicit origins, empty by default. If the
API is only consumed by non-browser clients (scripts, Prometheus), CORS can
stay fully closed.

### 7.4 Host validation

Validate the incoming `Host` header against a list of expected hosts (a simple
middleware). Default list: `localhost` + `127.0.0.1`, extendable by config.
Closes the spoofed-`Host` injection vector (Glances §8).

### 7.5 TLS — out of scope for v1

No in-binary TLS. Terminating TLS in the binary adds dependencies and
certificate management. Standard practice is a reverse proxy (nginx, Caddy) in
front. **Document** that network exposure must go through a TLS proxy: Basic
auth sends a base64-encoded (not encrypted) password, so the non-loopback case
is only safe over TLS.

### 7.6 General rule for future routes

A route never serializes a raw internal struct. It serializes an explicitly
filtered *public* view. (This is how the Glances `/config` CVE was fixed — a
`as_dict_secure()` filter.) When `/config` is eventually added, it needs its
own public struct, distinct from the internal config.

---

## 8. v1 scope — plugins

**v1 plugins: `mem`, `cpu`, `load`, `network`.** This is the "first deployable
tool" scope — the four dimensions of machine health, matching Glances Phase 1.

| Plugin    | Kind                  | Notes                                       |
|-----------|-----------------------|---------------------------------------------|
| `mem`     | instantaneous, scalar | `State = ()`. Simplest plugin.              |
| `load`    | instantaneous, scalar | Unix `load average`. Degraded/absent on Windows — acceptable. |
| `cpu`     | rate, scalar          | Two warm-up mechanisms layered (see 4).     |
| `network` | rate, **collection**  | One item per interface; introduces the collection category. |

**Deferred:** `fs` (disk space) — a second collection plugin; goes with the
next "collection" iteration. Alerting, and any non-Phase-1 plugin, also
deferred.

### 8.1 Collection plugins (`network`)

A collection plugin returns a JSON **array** of items, and the set of items
changes at runtime (interfaces appear/disappear).

- **Primary key** — a stable per-item id. For `network`, the interface name.
  In Rust this is simply the key of the samples `HashMap`.
- **`show`/`hide` filtering** — configurable regex allow/deny lists on the
  primary key, applied **inside `collect()` before** rate computation, so a
  hidden item neither appears in JSON nor costs a diff. This directly serves
  the "expose only necessary stats" requirement.
- **Disappearing items — immediate removal.** An interface gone from the
  current sample drops out of the output immediately (no "stale" grace
  period). This is free: `rates()` iterates over the current sample.
  - **Critical companion rule:** the inter-cycle state must memorize **only
    the current sample** (`state.previous = Some(now)`), never a merge of old
    and new. Otherwise dead interfaces accumulate in `previous` forever — a
    slow memory leak, ironic for a footprint-focused project. This must be a
    code comment so a future "optimization" does not reintroduce the leak.
  - When alerting is added later, per-item alert levels (`_levels`) must be
    cleaned up the same way — no fantom levels for dead interfaces.

---

## 9. Repository structure

**Decision: a single binary crate**, organized into modules. The component
boundaries (collection, plugins, API) are still settling; modules give logical
separation without freezing those boundaries the way separate crates would. A
multi-crate workspace would be over-engineering at this size.

Variant adopted: `lib.rs` + a tiny `main.rs` in the same crate — a trivial,
testable entry point without workspace ceremony.

```
glances-rs/
├── Cargo.toml
├── Cargo.lock              # committed (this is a binary)
├── README.md
├── ARCHITECTURE.md         # this document
└── src/
    ├── main.rs             # tiny: load config, call run()
    ├── lib.rs              # module declarations, exposes run()
    ├── config.rs           # config struct (bind, password, refresh, show/hide…)
    ├── server.rs           # axum Router construction, startup, 7.1 check
    ├── state.rs            # AppState, Collector, CollectorView
    ├── collector.rs        # ensure_plugin, plugin_loop
    ├── api/
    │   ├── mod.rs          # route handlers
    │   └── security.rs     # auth, trusted-host, check_security
    └── plugins/
        ├── mod.rs          # Plugin trait, PluginId
        ├── mem.rs
        ├── cpu.rs
        ├── load.rs
        └── network.rs
```

This layout is module organization — freely rearrangeable as the code takes
shape.

### `[profile.release]`

```toml
[profile.release]
opt-level = 3
lto = true            # smaller, faster binary
codegen-units = 1     # slower build, better result
strip = true          # drop debug symbols — lighter binary
panic = "abort"       # smaller binary; process aborts on panic
```

These settings directly serve the single-binary requirement. Note
`panic = "abort"`: on a panic the process stops without stack unwinding —
acceptable for a supervised server, but a deliberate choice. Drop that line if
unwinding robustness is preferred.

> **Resolved (Phase 7): keep `panic = "abort"`.** The deployment model is a
> supervised service (systemd/Docker restart policies), so a fatal panic
> turns into a clean restart rather than a corrupt half-state. It also keeps
> the binary smaller (no unwinding tables), which serves the footprint goal.
> The alternative — unwinding so a panicking collector task can't take down
> the server — was weighed and rejected: it would leave a *stale registry
> entry* for the dead collector (its `watch` sender dropped), making that
> plugin answer `503` forever, i.e. a silent partial failure. For a
> monitoring tool a loud crash-and-restart is a better signal than a
> silently broken plugin, and the collection paths are written to not panic
> (`saturating_sub`, `?`, `unwrap_or`). Revisit only if per-plugin panic
> isolation (with registry cleanup on task death) is later deemed worth it.

### Async runtime — `current_thread`

`main` runs Tokio's **`current_thread`** runtime, not the multi-thread default.

> **Resolved (Phase 9): single-threaded runtime.** `#[tokio::main]` defaults to
> the multi-thread runtime, which spawns *one worker per core* — 16 idle OS
> threads on a 16-core host — for a workload whose heaviest measured load is
> ~2 % CPU. The work is trivial and I/O-bound (a few `/proc`/`sysinfo` reads
> plus a few KB of JSON per cycle), so that parallelism buys nothing and costs
> RSS (worker stacks touched under load). The §5.2 concurrent `/all` wake is
> *async concurrency*, not parallelism, and runs identically on one thread.
> Measured on a 16-core host, same nine plugins: **−18 % RSS at rest and −47 %
> RSS under 100 req/s** (12.1 → 5.5 MiB) versus the multi-thread build, with
> equal-or-lower CPU. The `tokio` `rt-multi-thread` feature is dropped (only
> `rt` is needed), which also shrinks the binary. Revisit only if a future
> plugin introduces genuinely CPU-bound, parallelisable collection.

## 10. Open questions for implementation

All resolved during implementation (see `DEVELOPMENT_PLAN.md` for the phase
that closed each):

- ~~Verify the `sysinfo` minimum CPU-refresh delay~~ → 200 ms; warm-up set to
  250 ms (Phase 1, `docs/api.md` §6).
- ~~Confirm the `/all` partial-failure policy~~ → `200` with the failed plugin
  absent (Phase 5, §6.3).
- ~~Confirm `panic = "abort"`~~ → kept; rationale recorded in §9 (Phase 7).
- ~~Decide the exact JSON shape of each plugin's payload~~ → full Glances v5
  parity on Linux, frozen in `docs/api.md` §5 (Phase 1, extended Phase 4.1).
- ~~Choose the config file location / discovery order~~ → frozen in
  `docs/api.md` §7 (Phase 1).

---

## 11. Summary of key decisions

| # | Decision                          | Rationale                              |
|---|-----------------------------------|----------------------------------------|
| 1 | Rust                              | No-GC footprint; compile-time safety.  |
| 2 | Lazy collection with wake-up (C)  | Near-zero idle footprint + correct rates. |
| 3 | First request waits a full cycle  | Never serve `null`/empty.              |
| 4 | Store kept in memory while asleep | Negligible cost; rates ready on wake.  |
| 5 | Per-plugin wake-up (strict)       | Uniform model; shared-sampler later.   |
| 6 | Rate state local to the loop task | Lock-free by construction.             |
| 7 | Plugin self-bootstrap (warm-up)   | Keeps the "wait a cycle" promise honest. |
| 8 | v1 = mem, cpu, load, network      | First deployable tool; Glances Phase 1.|
| 9 | Disappearing items removed at once| Minimal; but `previous` must store `now` only. |
|10 | Closed by default (refuse to start)| Turns intent into a guarantee.        |
|11 | Basic auth only in v1             | Drops JWT dependency and pitfalls.     |
|12 | TLS via reverse proxy             | Keeps the binary minimal.              |
|13 | Single binary crate, modules      | Boundaries still settling.             |
